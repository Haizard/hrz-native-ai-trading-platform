//! A deterministic multi-timeframe replay, pinned to exact numbers.
//!
//! ## Why this exists
//!
//! `docs/11` requires the paper trader to produce trades "consistent with what
//! a manual replay of the same period through the backtester would produce".
//! That is only checkable if the replay is *pinned*: the per-timeframe
//! windowing lives in `strategy_runtime::rolling` and is shared with the live
//! paper trader, so a change there silently moves both at once and no unit
//! test would notice.
//!
//! This file replays a synthetic but non-trivial 5m/1h document end to end and
//! asserts every number each trade carries. It is deliberately an integration
//! test: it drives the public API, so it cannot be satisfied by an internal
//! refactor that keeps the units green.
//!
//! The fixture is generated rather than recorded, so it needs no database and
//! no network. If you change `analytics-core`, the numbers here are expected
//! to move -- and moving them is a decision someone should make on purpose,
//! not a test to delete.

use std::collections::BTreeMap;

use analytics_core::resample;
use analytics_core::types::{Candle, Timeframe};
use backtester::replay::{run_backtest, ReplayConfig, ReplayInput};
use strategy_runtime::{RuntimeConfig, StrategyEngine};

const M5: i64 = 5 * 60 * 1_000_000_000;

/// A staircase that contains the setup the document is looking for.
///
/// Three fixture designs fail here, and the reasons are worth recording
/// because each one produces a silently *empty* backtest rather than a loud
/// failure:
///
/// * **A straight line** confirms no swings at all, so every structure and
///   liquidity condition is false.
/// * **A rising series with a sine over it** has every successive trough
///   higher than the last, so no swing low is ever swept.
/// * **Tied highs** -- if `high` is `max(open, close) + constant`, then a peak
///   is both bar `i`'s open and bar `i-1`'s close, both bars get exactly the
///   same high, and no bar is strictly the highest. Swing detection then
///   finds nothing at all.
///
/// So each cycle does the thing the strategy buys: rally, make a swing low,
/// rally again, trade below that low, and close back above it. Keyframes are
/// offsets from the cycle's base, interpolated linearly, and the base rises by
/// [`DRIFT`] each cycle so the coarse view is an uptrend.
///
/// The cycle is 120 bars: long enough that the 1h view sees real up and down
/// legs (a cycle shorter than the context timeframe produces 1h candles whose
/// highs rise monotonically, and therefore no 1h swings either).
const CYCLE: usize = 120;
const DRIFT: f64 = 24.0;
const KEYFRAMES: [(usize, f64); 6] = [
    (0, 0.0),
    (40, 20.0),
    (55, 8.0),
    (95, 34.0),
    (105, 4.0),
    (119, 24.0),
];

fn offset(bar: usize) -> f64 {
    let position = bar % CYCLE;
    for window in KEYFRAMES.windows(2) {
        let ((from_bar, from), (to_bar, to)) = (window[0], window[1]);
        if position >= from_bar && position <= to_bar {
            let span = (to_bar - from_bar) as f64;
            let progress = (position - from_bar) as f64 / span;
            return from + (to - from) * progress;
        }
    }
    // The final keyframe's value is also the next cycle's first offset, so
    // reaching here would mean the shape does not close on itself.
    unreachable!("keyframes must cover the whole cycle")
}

fn price_at(bar: usize) -> f64 {
    let base = 100.0 + DRIFT * (bar / CYCLE) as f64;
    base + offset(bar)
}

fn series(count: usize) -> Vec<Candle> {
    (0..count)
        .map(|i| {
            let open = price_at(i);
            let close = price_at(i + 1);
            // Alternate the range so two consecutive bars never tie -- see the
            // note on `KEYFRAMES`.
            let wiggle = if i % 2 == 0 { 0.3 } else { 0.6 };
            Candle {
                symbol: "BTCUSDT".into(),
                timeframe: Timeframe::M5,
                open_time: i as i64 * M5,
                open,
                high: open.max(close) + wiggle,
                low: open.min(close) - wiggle,
                close,
                volume: 1_000.0,
                buy_volume: 700.0,
                sell_volume: 300.0,
            }
        })
        .collect()
}

/// The document under test.
///
/// Notably it does *not* include `liquidity.swept == "sell_side"`, which is
/// the first condition anyone would reach for. On this fixture that condition
/// and `market_structure.trend == "bullish"` are mutually exclusive, because
/// a sell-side sweep on the entry timeframe is, by definition, a close below
/// the last swing low -- which is exactly the break that flips the coarse
/// structure bearish. The reference strategy gets away with it because its
/// trend is 4h and its entry is 5m: a 5m sweep is noise to a 4h structure.
/// Here the two are only 12x apart, so the sweep moves the coarse view too.
const DOCUMENT: &str = r#"
name: "Golden replay"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  trend: 1h
  entry: 5m
entry:
  direction: long
  all_of:
    - timeframe: trend
      condition: market_structure.trend == "bullish"
    - timeframe: entry
      condition: close > liquidity.swept_level
invalidation:
  - timeframe: entry
    condition: close_below(stop_price)
risk:
  max_risk_pct: 1.0
  stop: "below_sweep_low"
  take_profit:
    type: "risk_multiple"
    value: 2.5
"#;

fn input() -> ReplayInput {
    let m5 = series(1200);
    // Aggregate rather than invent a second series, so the two views can never
    // disagree about where price was.
    let h1 = resample(&m5, Timeframe::H1);
    let mut timeframes = BTreeMap::new();
    timeframes.insert("trend".to_string(), Timeframe::H1);
    timeframes.insert("entry".to_string(), Timeframe::M5);
    let mut candles = BTreeMap::new();
    candles.insert("trend".to_string(), h1);
    candles.insert("entry".to_string(), m5);
    ReplayInput {
        timeframes,
        candles,
    }
}

fn config() -> ReplayConfig {
    ReplayConfig {
        symbol: "BTCUSDT".into(),
        from: 0,
        to: i64::MAX,
        state_window: 200,
        ..ReplayConfig::default()
    }
}

fn report() -> backtester::report::BacktestReport {
    let validated =
        strategy_dsl::parse_and_validate(DOCUMENT).expect("the golden document must validate");
    let mut engine = StrategyEngine::new(&validated, RuntimeConfig::default()).unwrap();
    run_backtest(&mut engine, &input(), &config()).unwrap()
}

/// Compare to ten decimal places: exact equality on floats would make this
/// test fail on a legitimate last-bit change, which is noise, not drift.
fn assert_close(what: &str, actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{what}: expected {expected:.10}, got {actual:.10}"
    );
}

#[test]
fn the_golden_replay_produces_exactly_these_trades() {
    let report = report();

    // The fixture must actually trade, or every assertion below is vacuous.
    assert_eq!(
        report.total_trades,
        3,
        "the golden fixture must produce trades; skips: {:?}",
        &report.skipped_signals[..report.skipped_signals.len().min(5)]
    );

    // Every number a trade carries, not just the ones the report summarises:
    // a windowing bug that moves an entry by one bar changes entry_time and
    // entry_price while leaving the trade count identical.
    let expected = [
        (
            43_200_000_000_000,
            136.0,
            136.0272,
            107.3463,
            207.634_249_999_999_95,
            150_900_000_000_000,
            207.63425,
            2.499_050_733_4,
            3.489_950_687,
            28.6537,
            359,
            "Target",
        ),
        (
            150_900_000_000_000,
            207.5,
            207.5415,
            179.3103,
            277.97425,
            244_500_000_000_000,
            277.97425,
            2.498_527_831_1,
            3.547_394_970_5,
            28.1897,
            312,
            "Target",
        ),
        (
            244_500_000_000_000,
            278.0,
            278.0556,
            227.2863,
            404.78425,
            360_000_000_000_000,
            340.0,
            1.221_452_980_2,
            1.971_853_759_4,
            50.7137,
            385,
            "EndOfData",
        ),
    ];

    for (index, trade) in report.trades.iter().enumerate() {
        let (opened_at, reference, entry, stop, target, closed_at, exit, r, size, risk, bars, why) =
            expected[index];
        assert_eq!(trade.entry_time, opened_at, "trade {index} entry time");
        assert_eq!(trade.exit_time, closed_at, "trade {index} exit time");
        assert_eq!(trade.bars_held, bars, "trade {index} bars held");
        assert_eq!(
            format!("{:?}", trade.exit_trigger),
            why,
            "trade {index} trigger"
        );
        assert_eq!(trade.regime, "bullish", "trade {index} regime");
        assert_close(
            &format!("trade {index} reference"),
            trade.reference_price,
            reference,
        );
        assert_close(&format!("trade {index} entry"), trade.entry_price, entry);
        assert_close(&format!("trade {index} stop"), trade.stop_price, stop);
        assert_close(&format!("trade {index} exit"), trade.exit_price, exit);
        assert_close(&format!("trade {index} r"), trade.r_multiple, r);
        assert_close(&format!("trade {index} size"), trade.size, size);
        assert_close(
            &format!("trade {index} risk/unit"),
            trade.risk_per_unit,
            risk,
        );
        assert_eq!(
            trade.take_profit_price,
            Some(target),
            "trade {index} target"
        );
        assert_eq!(
            trade.entry_reasons,
            vec![
                "market_structure.trend == \"bullish\"".to_string(),
                "close > liquidity.swept_level".to_string(),
            ],
            "trade {index} reasons"
        );
    }

    assert_close("win rate", report.win_rate, 1.0);
    assert!(
        report.profit_factor.is_infinite(),
        "no losing trade, so the profit factor is infinite: got {}",
        report.profit_factor
    );
    assert_close("average R", report.average_r, 2.073_010_514_9);
    assert_close("net return", report.net_return_pct, 6.219_031_544_7);
    assert_close("max drawdown", report.max_drawdown_pct, 0.0);
    assert_close("sharpe", report.sharpe_ratio, 0.284_789_909_4);
}

/// The decision candle count is part of the contract too: it is how many times
/// the strategy was actually asked, and it changes if a timeframe's visibility
/// rule moves by even one bar.
#[test]
fn the_golden_replay_asks_on_every_decision_candle() {
    let validated =
        strategy_dsl::parse_and_validate(DOCUMENT).expect("the golden document must validate");
    let mut engine = StrategyEngine::new(&validated, RuntimeConfig::default()).unwrap();
    let output = backtester::replay::replay(&mut engine, &input(), &config()).unwrap();
    assert_eq!(
        output.candles_processed, 1200,
        "every 5m candle in the window is a decision"
    );
}
