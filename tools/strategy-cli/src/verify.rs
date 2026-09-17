//! Independent verification of stored backtest trades (`docs/19` row 13).
//!
//! ## What this is for
//!
//! Phase 3's exit criterion in `02-ROADMAP.md` says the backtester's output was
//! verified by "manually-verified spot checks". For a long time no such check
//! existed anywhere in the repository — the phrase appears in the roadmap and
//! nowhere else — and `reports/` had records for Phases 5 and 6 only.
//!
//! That matters more than it sounds, because the check that *does* exist cannot
//! stand in for it. `crates/backtester/tests/replay_golden.rs` compares against
//! a fixture that was **generated from this implementation**. Its own header
//! says so. A golden file detects a *changed* implementation; it can never
//! detect a wrong one, because it was told what the answer should be by the
//! thing under test.
//!
//! So this module asks a different question: **given the trades the backtester
//! recorded, does the market data in the candle table actually support them?**
//! The candle table is an artefact of the collector and the backfill, not of
//! the backtester, so it is genuinely external to the thing being checked.
//!
//! ## What it does and does not establish
//!
//! It establishes, per trade:
//!
//! * the stop and target sit on the correct side of the reference price;
//! * `risk_per_unit` equals the distance it claims to be;
//! * `entry_price` is the open of the candle at `entry_time`, moved against the
//!   trade by exactly the recorded slippage;
//! * the candle before `entry_time` closed at `reference_price` — that is, the
//!   entry is on the bar *after* the decision and there is no look-ahead;
//! * `r_multiple` is the arithmetic its own inputs imply;
//! * `bars_held` agrees with the timestamps;
//! * `exit_time` is a candle **close**, and the candle there genuinely touched
//!   the level the exit trigger names.
//!
//! It does **not** establish that the strategy is profitable, that the fills are
//! realistic, that the sizing is right, or that the trades are ones a human
//! would take. It checks internal consistency plus one external fact per trade:
//! *the level it says it exited at was actually reached in the data*.
//!
//! ## Why a sample rather than every trade
//!
//! The database costs about a second per statement and a six-month run has
//! hundreds of trades, so the default is a spread sample across the run rather
//! than the whole set. Every check is a property of one trade, so a failure
//! anywhere is a real failure; a clean sample is evidence, not proof. The report
//! says which trades were checked so the claim is bounded by what was read.

use std::collections::HashMap;
use std::str::FromStr;

use analytics_core::{Candle, Timeframe};
use anyhow::{bail, Context, Result};
use backtester::report::{BacktestReport, TradeRecord};
use db::Database;
use strategy_runtime::ExitTrigger;

/// One check's verdict on one trade.
#[derive(Debug)]
struct Finding {
    check: &'static str,
    detail: String,
}

/// What was verified about one trade.
#[derive(Debug)]
struct TradeVerdict {
    ordinal: usize,
    direction: &'static str,
    entry_time: i64,
    entry_price: f64,
    exit_time: i64,
    exit_price: f64,
    r_multiple: f64,
    exit_trigger: String,
    findings: Vec<Finding>,
}

impl TradeVerdict {
    fn passed(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Relative tolerance for float comparisons that should be exact.
///
/// The values travel through JSONB, so a `f64` written as `10.0` can come back
/// as `10.000000000000002`. Absolute tolerance is wrong here because prices
/// range over four orders of magnitude; `1e-9` relative is far tighter than any
/// real discrepancy and far looser than JSON round-trip noise.
const REL_TOL: f64 = 1e-9;

fn close_enough(a: f64, b: f64) -> bool {
    let scale = a.abs().max(b.abs()).max(1.0);
    (a - b).abs() <= REL_TOL * scale
}

/// Read the newest stored run for `symbol` and verify a sample of its trades.
///
/// Returns `true` when every checked trade passed. The caller turns that into
/// an exit code, so this is usable as a gate rather than only as a report.
///
/// # Errors
/// Returns an error if the database cannot be reached, no run exists, the stored
/// report cannot be parsed, or the candle window cannot be loaded. A *failed
/// check* is not an error — it is a `false` return with the detail printed.
/// Read a run and verify a sample of its trades.
///
/// `report_path` reads a JSON report written by `backtest run --report-out`;
/// `symbol` reads the newest stored run for that symbol that has trades. Exactly
/// one must be given.
///
/// Returns `true` when every checked trade passed. The caller turns that into an
/// exit code, so this is usable as a gate rather than only as a report.
///
/// # Errors
/// Returns an error if the source cannot be read, the report cannot be parsed,
/// or the candle window cannot be loaded. A *failed check* is not an error — it
/// is a `false` return with the detail printed.
pub async fn run(
    report_path: Option<&str>,
    symbol: Option<&str>,
    sample: usize,
    timeframe_override: Option<&str>,
) -> Result<bool> {
    let database = Database::from_env()
        .await
        .context("connecting to the database (DATABASE_URL)")?;

    let (label, report) = match (report_path, symbol) {
        (Some(path), None) => (path.to_string(), read_report_file(path)?),
        (None, Some(symbol)) => {
            let (label, report) = read_stored_run(&database, symbol).await?;
            (label, report)
        }
        (None, None) => bail!("pass either --report <path> or --symbol <symbol>"),
        (Some(_), Some(_)) => bail!("--report and --symbol are mutually exclusive"),
    };

    // Before anything that reads a trade: `discover_timeframe` probes one, and a
    // run with no trades is a legitimate thing to have stored.
    if report.trades.is_empty() {
        bail!("{label} recorded no trades, so there is nothing to verify");
    }

    let timeframe = match timeframe_override {
        Some(raw) => {
            Timeframe::from_str(raw).with_context(|| format!("unknown timeframe {raw}"))?
        }
        None => discover_timeframe(&database, &report).await?,
    };

    println!("source     {label}");
    println!("strategy   {} v{}", report.strategy, report.version);
    println!("symbol     {}", report.symbol);
    println!("window     {} .. {}", report.from, report.to);
    println!(
        "decides on {timeframe} (the report names it {:?}, which is a declared name \
         and not a resolution)",
        report.decision_timeframe
    );
    println!("trades     {} completed", report.total_trades);
    println!(
        "reported   win rate {:.4}, PF {:.4}, avg R {:.4}",
        report.win_rate, report.profit_factor, report.average_r
    );
    println!(
        "assumptions slippage {} bps, entry: {}",
        report.assumptions.slippage_bps, report.assumptions.entry_fill
    );
    println!();

    // One query for the whole window, not one per trade. The managed database
    // costs about a second per statement, and a per-trade lookup over a sample
    // of fifty would be a minute of round trips for data already in memory.
    //
    // The window is padded by one candle on each side: the entry bar is the one
    // after the decision bar, and the exit bar is the one whose *close* is
    // stamped, so the check needs neighbours the trade's own timestamps do not
    // name.
    let pad = timeframe.nanos();
    let candles = db::repositories::load_candles(
        database.pool(),
        &report.symbol,
        timeframe,
        report.from - pad,
        report.to + 2 * pad,
    )
    .await
    .context("loading the candle window the trades were made in")?;

    if candles.is_empty() {
        bail!(
            "no {:?} candles stored for {} between {} and {}; the trades cannot be \
             checked against data that is not there",
            timeframe,
            report.symbol,
            report.from,
            report.to
        );
    }

    let index: HashMap<i64, &Candle> = candles.iter().map(|c| (c.open_time, c)).collect();
    println!(
        "candles    {} {:?} bars loaded for the window",
        candles.len(),
        timeframe
    );
    println!();

    let picks = spread(report.trades.len(), sample);
    let mut verdicts = Vec::with_capacity(picks.len());
    for &ordinal in &picks {
        let trade = &report.trades[ordinal];
        verdicts.push(check_one(ordinal, trade, &index, timeframe, &report));
    }

    print_verdicts(&verdicts);
    report_summary(&verdicts, &report)
}

/// Parse a report JSON file written by `backtest run --report-out`.
fn read_report_file(path: &str) -> Result<BacktestReport> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading the report at {path}"))?;
    serde_json::from_str(&text)
        .with_context(|| format!("{path} is not a BacktestReport this build understands"))
}

/// Read the newest stored run for `symbol` that recorded a trade.
///
/// ## Why two lookups
///
/// The first answers "what is the newest run", the second "what is the newest
/// run with something to check". When they differ the operator is told, because
/// silently verifying an older run while they believe the newest one was checked
/// is exactly the quiet substitution this command exists to prevent.
async fn read_stored_run(database: &Database, symbol: &str) -> Result<(String, BacktestReport)> {
    let newest = db::strategies::newest_backtest_for_symbol(database.pool(), symbol, false)
        .await?
        .with_context(|| format!("no stored backtest for {symbol}; run one first"))?;

    let row = db::strategies::newest_backtest_for_symbol(database.pool(), symbol, true)
        .await?
        .with_context(|| {
            format!(
                "no stored run for {symbol} recorded a trade, so there is nothing to verify. \
                 The newest run ({}) has an empty trade list. Pass --report to check a report \
                 written to disk instead.",
                newest.id
            )
        })?;

    if row.id != newest.id {
        println!(
            "note: the newest run for {symbol} ({}) recorded no trades; verifying {} \
             instead, the newest one that did.",
            newest.id, row.id
        );
    }

    let report = serde_json::from_value(row.report.clone()).with_context(|| {
        format!(
            "the stored report in backtests.{} is not a BacktestReport this build understands",
            row.id
        )
    })?;
    Ok((format!("backtests.{}", row.id), report))
}

/// Work out which resolution a stored run traded on.
///
/// ## Why this is not just a field read
///
/// `BacktestReport::decision_timeframe` holds the **declared name** from the
/// strategy document — `"entry"`, `"trend"` — not a resolution. Reading it as
/// one fails with `unknown timeframe 'entry'`, which is exactly what happened
/// the first time this command ran. The report simply does not record the
/// resolution, and guessing from the name is impossible.
///
/// So it is derived from the candle table, which does know: the resolutions with
/// a bar opening at the first trade's entry instant are the candidates. Every
/// timeframe shares a grid at a UTC day boundary, so a trade entered at midnight
/// matches all six — hence the second filter, which keeps only resolutions whose
/// bar length divides every trade's holding period exactly. Ambiguity that
/// survives both is reported rather than resolved, and `--timeframe` overrides
/// the whole thing.
async fn discover_timeframe(database: &Database, report: &BacktestReport) -> Result<Timeframe> {
    // Any trade works as the probe; the middle one is used so a single
    // mis-timestamped first or last trade cannot mislead the search.
    let probe = &report.trades[report.trades.len() / 2];

    let raw = db::repositories::timeframes_with_candle_at(
        database.pool(),
        &report.symbol,
        probe.entry_time,
    )
    .await?;

    if raw.is_empty() {
        bail!(
            "no candle opens at {} for {} -- the run's trades are not on a stored bar \
             boundary, so the resolution cannot be derived. Pass --timeframe.",
            probe.entry_time,
            report.symbol
        );
    }

    let mut candidates: Vec<Timeframe> = raw
        .iter()
        .filter_map(|s| Timeframe::from_str(s).ok())
        .collect();

    // One candidate needs no disambiguation, and filtering it here would hide a
    // real fault behind a confusing message. An off-grid exit timestamp makes the
    // holding period indivisible at *every* resolution, so a filter-first version
    // of this function reported "no resolution explains the holding period" when
    // the actual problem was one bad trade -- and the per-trade check that says
    // so never ran. Deferring to it is what turns that into a named finding.
    if let [only] = candidates.as_slice() {
        return Ok(*only);
    }

    // A trade's holding period is a whole number of bars at the resolution it was
    // made on, and nothing else. Applied to every trade rather than the probe, so
    // a resolution that happens to fit one trade but not the run is discarded.
    candidates.retain(|tf| {
        let nanos = tf.nanos();
        report
            .trades
            .iter()
            .all(|t| (t.exit_time - t.entry_time).rem_euclid(nanos) == 0)
    });

    match candidates.as_slice() {
        [only] => Ok(*only),
        [] => bail!(
            "no resolution in the candle table explains every trade's holding period \
             (candidates at the entry instant: {raw:?}). Pass --timeframe."
        ),
        many => bail!(
            "several resolutions fit this run ({:?}); pass --timeframe to say which \
             the trades were made on. The stored report names the decision timeframe \
             only as {:?}, which is a declared name and not a resolution.",
            many.iter().map(ToString::to_string).collect::<Vec<_>>(),
            report.decision_timeframe
        ),
    }
}

/// Indices spread evenly across the run, including the first and last trade.
///
/// A contiguous slice would sample one regime; the point of a spot check is to
/// touch the whole run, and the first and last trades are where an off-by-one in
/// the window handling would show up.
fn spread(total: usize, wanted: usize) -> Vec<usize> {
    if total == 0 {
        return Vec::new();
    }
    let wanted = wanted.clamp(1, total);
    if wanted == 1 {
        return vec![0];
    }
    let mut picks: Vec<usize> = (0..wanted)
        .map(|i| i * (total - 1) / (wanted - 1))
        .collect();
    picks.dedup();
    picks
}

/// Every check, against one trade.
fn check_one(
    ordinal: usize,
    trade: &TradeRecord,
    index: &HashMap<i64, &Candle>,
    timeframe: Timeframe,
    report: &BacktestReport,
) -> TradeVerdict {
    let long = trade.direction == strategy_dsl::Direction::Long;
    let mut findings = Vec::new();
    let mut fail = |check: &'static str, detail: String| findings.push(Finding { check, detail });

    // --- Internal consistency: the trade must not contradict itself ---------

    // 1. The stop is on the correct side of the *reference* price. The engine
    //    claims to refuse otherwise, so a violation means either the refusal is
    //    not wired up or the record was written past it.
    let stop_ok = if long {
        trade.stop_price < trade.reference_price
    } else {
        trade.stop_price > trade.reference_price
    };
    if !stop_ok {
        fail(
            "stop side",
            format!(
                "{} stop {} is on the wrong side of reference {}",
                trade.direction, trade.stop_price, trade.reference_price
            ),
        );
    }

    // 2. Risk per unit is the distance it claims. Computed from the *reference*
    //    price, not the fill: `EnterSignal::risk_per_unit` is
    //    `|reference - stop|`, so a checker using the fill price would report a
    //    discrepancy on every trade and be wrong about all of them.
    let claimed_risk = (trade.reference_price - trade.stop_price).abs();
    if !close_enough(trade.risk_per_unit, claimed_risk) {
        fail(
            "risk per unit",
            format!(
                "recorded {} but |reference {} - stop {}| is {}",
                trade.risk_per_unit, trade.reference_price, trade.stop_price, claimed_risk
            ),
        );
    }
    // Written out rather than as `!(risk > 0.0)` because that form is rejected by
    // clippy (`neg_cmp_op_on_partial_ord`) and, more to the point, NaN must be a
    // failure here: `NaN > 0.0` is false, so a NaN risk would slip through a
    // plain `if risk <= 0.0` guard as if it were valid.
    if trade.risk_per_unit.is_nan() || trade.risk_per_unit <= 0.0 {
        fail(
            "risk per unit",
            format!("{} is not a positive risk distance", trade.risk_per_unit),
        );
    }

    // 3. Target on the correct side, when there is one.
    if let Some(target) = trade.take_profit_price {
        let ok = if long {
            target > trade.reference_price
        } else {
            target < trade.reference_price
        };
        if !ok {
            fail(
                "target side",
                format!(
                    "{} target {target} is on the wrong side of reference {}",
                    trade.direction, trade.reference_price
                ),
            );
        }
    }

    // 4. R is the arithmetic its own inputs imply. This is not a re-run of the
    //    strategy -- it is the definition of an R multiple, applied to the
    //    numbers in the record.
    let signed = if long {
        trade.exit_price - trade.entry_price
    } else {
        trade.entry_price - trade.exit_price
    };
    let implied_r = signed / trade.risk_per_unit;
    if !close_enough(trade.r_multiple, implied_r) {
        fail(
            "r multiple",
            format!(
                "recorded {:.10} but {} at {} to {} over risk {:.10} implies {:.10}",
                trade.r_multiple,
                trade.direction,
                trade.entry_price,
                trade.exit_price,
                trade.risk_per_unit,
                implied_r
            ),
        );
    }

    // 5. Bars held agrees with the timestamps. Catches an off-by-one in the
    //    bar accounting, which is invisible from the R side.
    let span = trade.exit_time - trade.entry_time;
    let expected_bars = span / timeframe.nanos();
    if span % timeframe.nanos() != 0 {
        fail(
            "bars held",
            format!(
                "exit {} - entry {} is {span} ns, not a whole number of {:?} bars",
                trade.exit_time, trade.entry_time, timeframe
            ),
        );
    } else if expected_bars as usize != trade.bars_held {
        fail(
            "bars held",
            format!(
                "recorded {} bars but the timestamps span {expected_bars}",
                trade.bars_held
            ),
        );
    }

    // 6. Both timestamps are inside the run's own window. A trade dated outside
    //    the window it was run over is a window-handling bug.
    for (label, at) in [("entry", trade.entry_time), ("exit", trade.exit_time)] {
        if at < report.from || at > report.to + timeframe.nanos() {
            fail(
                "window",
                format!(
                    "{label} {at} is outside the run window {} .. {}",
                    report.from, report.to
                ),
            );
        }
    }

    // --- The external checks: what the candle table says --------------------

    // 7. The entry bar exists and its open, moved against the trade by the
    //    recorded slippage, is the fill price. `entry_time` is a bar *open*
    //    (the simulator fills at the next open), so this is the direct test.
    let slip = report.assumptions.slippage_bps / 10_000.0;
    match index.get(&trade.entry_time) {
        None => fail(
            "entry bar",
            format!(
                "no candle opens at the entry timestamp {} -- the fill is not on a bar \
                 boundary, so it cannot be reconciled with the data",
                trade.entry_time
            ),
        ),
        Some(entry_bar) => {
            let expected_fill = if long {
                entry_bar.open * (1.0 + slip)
            } else {
                entry_bar.open * (1.0 - slip)
            };
            if !close_enough(trade.entry_price, expected_fill) {
                fail(
                    "entry fill",
                    format!(
                        "recorded entry {} but bar {} opened at {} and {} bps against the \
                         trade gives {}",
                        trade.entry_price,
                        trade.entry_time,
                        entry_bar.open,
                        report.assumptions.slippage_bps,
                        expected_fill
                    ),
                );
            }

            // 8. The bar *before* the entry bar closed at the reference price.
            //    This is the no-look-ahead check: the decision was made on the
            //    previous close and filled on this bar's open. If the reference
            //    were the entry bar's own close, the strategy would have traded
            //    on information it could not have had.
            match index.get(&(trade.entry_time - timeframe.nanos())) {
                None => fail(
                    "no look-ahead",
                    format!(
                        "no candle before the entry bar at {}; cannot confirm the decision \
                         was made on a closed bar",
                        trade.entry_time
                    ),
                ),
                Some(previous) => {
                    if !close_enough(previous.close, trade.reference_price) {
                        fail(
                            "no look-ahead",
                            format!(
                                "reference {} is not the close of the bar before the entry \
                                 (bar {} closed at {})",
                                trade.reference_price, previous.open_time, previous.close
                            ),
                        );
                    }
                }
            }
        }
    }

    // 9. The exit bar exists and genuinely touched what the trigger claims.
    //    The simulator stamps a stop or target exit at the bar's *close* time,
    //    so the bar to look up is `exit_time - one bar`.
    let exit_bar_time = trade.exit_time - timeframe.nanos();
    match index.get(&exit_bar_time) {
        None => fail(
            "exit bar",
            format!(
                "no candle opens at {} (exit {} less one {:?} bar) -- the exit timestamp is \
                 not a bar close",
                exit_bar_time, trade.exit_time, timeframe
            ),
        ),
        Some(bar) => match trade.exit_trigger {
            ExitTrigger::Stop => {
                let touched = if long {
                    bar.low <= trade.stop_price
                } else {
                    bar.high >= trade.stop_price
                };
                if !touched {
                    fail(
                        "exit level",
                        format!(
                            "stop exit at {} but the bar {} ({} {} {} {}) never reached it",
                            trade.stop_price, bar.open_time, bar.open, bar.high, bar.low, bar.close
                        ),
                    );
                }
                if !close_enough(trade.exit_price, trade.stop_price) {
                    fail(
                        "exit fill",
                        format!(
                            "stop exit recorded at {} but the stop level is {}; the documented \
                             simplification fills exactly at the level",
                            trade.exit_price, trade.stop_price
                        ),
                    );
                }
            }
            ExitTrigger::Target => match trade.take_profit_price {
                None => fail(
                    "exit level",
                    "a target exit with no target price recorded".to_string(),
                ),
                Some(target) => {
                    let touched = if long {
                        bar.high >= target
                    } else {
                        bar.low <= target
                    };
                    if !touched {
                        fail(
                            "exit level",
                            format!(
                                "target exit at {target} but the bar {} ({} {} {} {}) never \
                                 reached it",
                                bar.open_time, bar.open, bar.high, bar.low, bar.close
                            ),
                        );
                    }
                    if !close_enough(trade.exit_price, target) {
                        fail(
                            "exit fill",
                            format!(
                                "target exit recorded at {} but the target is {target}",
                                trade.exit_price
                            ),
                        );
                    }
                }
            },
            // Condition, time and end-of-data exits are market closes: there is
            // no level they must have touched. What can be checked is that the
            // exit price is inside the bar it happened on, which is the weaker
            // claim the data does support.
            _ => {
                if trade.exit_price < bar.low || trade.exit_price > bar.high {
                    fail(
                        "exit fill",
                        format!(
                            "{} exit at {} is outside the bar {} ({} {} {} {})",
                            trade.exit_trigger.name(),
                            trade.exit_price,
                            bar.open_time,
                            bar.open,
                            bar.high,
                            bar.low,
                            bar.close
                        ),
                    );
                }
            }
        },
    }

    TradeVerdict {
        ordinal,
        direction: if long { "long" } else { "short" },
        entry_time: trade.entry_time,
        entry_price: trade.entry_price,
        exit_time: trade.exit_time,
        exit_price: trade.exit_price,
        r_multiple: trade.r_multiple,
        exit_trigger: trade.exit_trigger.name().to_string(),
        findings,
    }
}

fn print_verdicts(verdicts: &[TradeVerdict]) {
    println!("--- checked trades ---");
    for verdict in verdicts {
        let mark = if verdict.passed() { "ok  " } else { "FAIL" };
        println!(
            "{mark} #{:<4} {:<5} entry {:.4} @ {} -> exit {:.4} @ {}  {:>8.4}R  {}",
            verdict.ordinal,
            verdict.direction,
            verdict.entry_price,
            verdict.entry_time,
            verdict.exit_price,
            verdict.exit_time,
            verdict.r_multiple,
            verdict.exit_trigger,
        );
        for finding in &verdict.findings {
            println!("       {}: {}", finding.check, finding.detail);
        }
    }
    println!();
}

/// Print the tally and return whether everything passed.
fn report_summary(verdicts: &[TradeVerdict], report: &BacktestReport) -> Result<bool> {
    let failed: Vec<&TradeVerdict> = verdicts.iter().filter(|v| !v.passed()).collect();
    let total_findings: usize = verdicts.iter().map(|v| v.findings.len()).sum();

    println!("--- tally ---");
    println!(
        "checked    {} of {} trades ({:.1}% of the run)",
        verdicts.len(),
        report.trades.len(),
        100.0 * verdicts.len() as f64 / report.trades.len().max(1) as f64
    );
    println!("passed     {}", verdicts.len() - failed.len());
    println!(
        "failed     {} ({} finding(s))",
        failed.len(),
        total_findings
    );

    if failed.is_empty() {
        println!();
        println!(
            "Every checked trade is internally consistent and its exit level was reached in \
             the stored candles."
        );
        println!(
            "This is a sample of one run. It does not establish that the strategy is \
             profitable, that the fills are realistic, or that every trade is clean -- only \
             that the ones read here are."
        );
        return Ok(true);
    }

    println!();
    println!("FAILED -- the trades above contradict either themselves or the candle table.");
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_includes_the_ends_and_stays_in_range() {
        assert_eq!(spread(10, 3), vec![0, 4, 9]);
        assert_eq!(spread(10, 10), (0..10).collect::<Vec<_>>());
        assert_eq!(spread(10, 1), vec![0]);
        assert_eq!(spread(1, 5), vec![0]);
        assert!(spread(0, 5).is_empty());
    }

    /// A sample larger than the run must not repeat or exceed the run.
    #[test]
    fn spread_never_leaves_the_run() {
        for total in [1usize, 2, 3, 7, 53] {
            for wanted in [1usize, 2, 5, 50, 500] {
                let picks = spread(total, wanted);
                assert!(!picks.is_empty() || total == 0);
                assert!(
                    picks.iter().all(|&i| i < total),
                    "{total}/{wanted}: {picks:?}"
                );
                assert!(picks.len() <= total);
                let mut sorted = picks.clone();
                sorted.sort_unstable();
                sorted.dedup();
                assert_eq!(sorted, picks, "picks must be strictly increasing");
            }
        }
    }

    #[test]
    fn close_enough_scales_with_magnitude() {
        assert!(close_enough(100.0, 100.0 + 1e-11));
        assert!(!close_enough(100.0, 100.001));
        assert!(close_enough(0.0, 0.0));
        // A cent-level difference at BTC prices is a real difference.
        assert!(!close_enough(64_000.0, 64_000.01));
    }
}
