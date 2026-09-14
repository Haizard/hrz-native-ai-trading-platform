//! Performance reporting (`docs/07-BACKTESTING-ENGINE.md`).
//!
//! The report is the artifact that makes an AI thesis checkable, so it carries
//! more than the spec's field list: every completed trade with the conditions
//! that opened and closed it, plus an explicit statement of the fill
//! assumptions the numbers rest on. A win rate is only meaningful next to the
//! assumptions that produced it.
//!
//! ## R units, stated plainly
//!
//! `net_return_pct` and `max_drawdown_pct` are named as the spec names them,
//! but Phase 3 reports **R multiples**, not percentages. One R is the risk a
//! trade accepted when it was decided. Summing R across trades gives a curve
//! that is independent of account size and free of any compounding assumption;
//! calling it a "percent" would imply a compounding model that does not exist
//! here. [`FillAssumptions::return_units`] says so in the report itself, so
//! nobody has to read this comment to interpret the number.

use serde::{Deserialize, Serialize};
use strategy_dsl::StrategyDocument;
// The fill model lives with the runtime so the paper trader and the replay
// cannot drift apart; these are re-exported for callers that know them as
// report types.
pub use strategy_runtime::{FillAssumptions, TradeRecord};

/// Nanoseconds in a Julian year, for annualizing the Sharpe ratio.
const NANOS_PER_YEAR: f64 = 365.25 * 86_400.0 * 1_000_000_000.0;

/// The metrics, separated from the trades that produced them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    /// Number of completed trades.
    pub total_trades: u32,
    /// Fraction of trades that made money.
    pub win_rate: f64,
    /// Gross profit divided by gross loss, in R.
    ///
    /// Infinite when nothing lost money. Note that JSON renders a non-finite
    /// float as `null`, so a consumer should treat `null` as "no losses".
    pub profit_factor: f64,
    /// Total R gained or lost.
    pub net_return_pct: f64,
    /// Largest peak-to-trough fall of the cumulative R curve, as a positive
    /// number.
    pub max_drawdown_pct: f64,
    /// Per-trade Sharpe, annualized by the trade rate of the window.
    pub sharpe_ratio: f64,
    /// Mean R per trade.
    pub average_r: f64,
}

impl Metrics {
    /// All-zero metrics, for a run that produced no trades.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            total_trades: 0,
            win_rate: 0.0,
            profit_factor: 0.0,
            net_return_pct: 0.0,
            max_drawdown_pct: 0.0,
            sharpe_ratio: 0.0,
            average_r: 0.0,
        }
    }
}

/// The full report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacktestReport {
    /// Strategy name, from the document.
    pub strategy: String,
    /// Document version.
    pub version: String,
    /// Symbol traded.
    pub symbol: String,
    /// Start of the window (unix nanos).
    pub from: i64,
    /// End of the window (unix nanos).
    pub to: i64,
    /// The timeframe decisions were made on.
    pub decision_timeframe: String,
    /// Every completed trade.
    pub trades: Vec<TradeRecord>,
    /// Number of completed trades.
    pub total_trades: u32,
    /// Fraction of trades that made money.
    pub win_rate: f64,
    /// Gross profit divided by gross loss.
    pub profit_factor: f64,
    /// Total R, in R units.
    pub net_return_pct: f64,
    /// Maximum drawdown of the cumulative R curve, in R units.
    pub max_drawdown_pct: f64,
    /// Per-trade Sharpe, annualized.
    pub sharpe_ratio: f64,
    /// Mean R per trade.
    pub average_r: f64,
    /// The timeframe the run executed on.
    pub best_timeframe: Option<String>,
    /// The regime with the worst mean R, when at least two regimes appeared.
    pub worst_regime: Option<String>,
    /// Setups that fired but never became trades, with reasons. Empty is the
    /// happy case; a long list usually means a stop rule needs a longer warm-up.
    pub skipped_signals: Vec<String>,
    /// Count of skipped signals, for a quick read.
    pub skipped_signals_count: u32,
    /// The assumptions behind every number above.
    pub assumptions: FillAssumptions,
}

impl BacktestReport {
    /// Whether the run produced no trades at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total_trades == 0
    }

    /// A one-line summary, for the CLI.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return format!(
                "no trades over {} candle(s) of {} on {}",
                self.decision_timeframe, self.symbol, self.decision_timeframe
            );
        }
        format!(
            "{} trades, {:.1}% win rate, {:.2}R net, {:.2}R max drawdown, PF {:.2}, mean {:.3}R",
            self.total_trades,
            self.win_rate * 100.0,
            self.net_return_pct,
            self.max_drawdown_pct,
            self.profit_factor,
            self.average_r,
        )
    }
}

/// Compute the metrics from a set of trades.
///
/// `window_nanos` is the length of the backtest window, used only to annualize
/// the Sharpe ratio.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn compute_metrics(trades: &[TradeRecord], window_nanos: i64) -> Metrics {
    if trades.is_empty() {
        return Metrics::empty();
    }

    let total = trades.len();
    let wins = trades.iter().filter(|t| t.is_win()).count();

    let gross_profit: f64 = trades
        .iter()
        .filter(|t| t.is_win())
        .map(|t| t.r_multiple)
        .sum();
    let gross_loss: f64 = trades
        .iter()
        .filter(|t| t.is_loss())
        .map(|t| -t.r_multiple)
        .sum();

    let profit_factor = if gross_loss > 0.0 {
        gross_profit / gross_loss
    } else if gross_profit > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };

    let net: f64 = trades.iter().map(|t| t.r_multiple).sum();
    let average = net / total as f64;

    // Peak-to-trough of the cumulative R curve, starting from flat.
    let mut peak = 0.0_f64;
    let mut cumulative = 0.0_f64;
    let mut max_drawdown = 0.0_f64;
    for trade in trades {
        cumulative += trade.r_multiple;
        peak = peak.max(cumulative);
        max_drawdown = max_drawdown.max(peak - cumulative);
    }

    let sharpe = annualized_sharpe(trades, average, window_nanos);

    Metrics {
        total_trades: u32::try_from(total).unwrap_or(u32::MAX),
        win_rate: wins as f64 / total as f64,
        profit_factor,
        net_return_pct: net,
        max_drawdown_pct: max_drawdown,
        sharpe_ratio: sharpe,
        average_r: average,
    }
}

/// Per-trade Sharpe, annualized by how often the strategy actually traded.
///
/// Sample standard deviation, `n - 1`. A single trade has no dispersion to
/// measure, and a zero-variance run has no meaningful ratio, so both report
/// zero rather than an infinity.
#[allow(clippy::cast_precision_loss)]
fn annualized_sharpe(trades: &[TradeRecord], mean: f64, window_nanos: i64) -> f64 {
    let n = trades.len();
    if n < 2 || window_nanos <= 0 {
        return 0.0;
    }

    let variance = trades
        .iter()
        .map(|t| {
            let deviation = t.r_multiple - mean;
            deviation * deviation
        })
        .sum::<f64>()
        / (n - 1) as f64;

    let std_dev = variance.sqrt();
    if !std_dev.is_finite() || std_dev <= 0.0 {
        return 0.0;
    }

    let years = window_nanos as f64 / NANOS_PER_YEAR;
    if years <= 0.0 {
        return 0.0;
    }
    let trades_per_year = n as f64 / years;

    (mean / std_dev) * trades_per_year.sqrt()
}

/// The regime with the worst mean R.
///
/// `None` unless at least two distinct regimes appeared: "worst regime" is a
/// comparison, and naming the only one as the worst says nothing.
#[must_use]
pub fn worst_regime(trades: &[TradeRecord]) -> Option<String> {
    use std::collections::BTreeMap;

    let mut totals: BTreeMap<&str, (f64, usize)> = BTreeMap::new();
    for trade in trades {
        let entry = totals.entry(trade.regime.as_str()).or_insert((0.0, 0));
        entry.0 += trade.r_multiple;
        entry.1 += 1;
    }

    if totals.len() < 2 {
        return None;
    }

    totals
        .into_iter()
        .map(|(regime, (sum, count))| (regime, sum / count as f64))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(regime, _)| regime.to_string())
}

/// Assemble a report from its parts.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_report(
    document: &StrategyDocument,
    symbol: &str,
    from: i64,
    to: i64,
    decision_timeframe: &str,
    trades: Vec<TradeRecord>,
    skipped_signals: Vec<String>,
    assumptions: FillAssumptions,
) -> BacktestReport {
    let metrics = compute_metrics(&trades, to.saturating_sub(from));
    let skipped_signals_count = u32::try_from(skipped_signals.len()).unwrap_or(u32::MAX);
    let worst = worst_regime(&trades);

    BacktestReport {
        strategy: document.name.clone(),
        version: document.version.clone(),
        symbol: symbol.to_string(),
        from,
        to,
        decision_timeframe: decision_timeframe.to_string(),
        total_trades: metrics.total_trades,
        win_rate: metrics.win_rate,
        profit_factor: metrics.profit_factor,
        net_return_pct: metrics.net_return_pct,
        max_drawdown_pct: metrics.max_drawdown_pct,
        sharpe_ratio: metrics.sharpe_ratio,
        average_r: metrics.average_r,
        best_timeframe: Some(decision_timeframe.to_string()),
        worst_regime: worst,
        trades,
        skipped_signals,
        skipped_signals_count,
        assumptions,
    }
}

/// Parameter optimization / walk-forward, stubbed as the spec requires.
///
/// The spec is explicit: define the trait now so a grid or walk-forward search
/// slots in without a redesign, but leave the search algorithm until
/// single-run backtesting is solid. A `threshold(1500)` in a document marks the
/// numbers a sweep would vary, which is why that function exists.
pub trait ParameterSweep {
    /// Candidate documents derived from a base document.
    fn candidate_documents(&self, base: &StrategyDocument) -> Vec<StrategyDocument>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use strategy_dsl::{parse, Direction};
    use strategy_runtime::ExitTrigger;

    const SAMPLE: &str = r#"
name: "Test"
version: "1.0"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
entry:
  all_of:
    - timeframe: entry
      condition: delta > threshold(1500)
risk:
  max_risk_pct: 1.0
  stop: below_swing_low
invalidation:
  - timeframe: entry
    condition: close_below(vwap)
"#;

    const DAY: i64 = 86_400 * 1_000_000_000;

    fn trade(r: f64, regime: &str, trigger: ExitTrigger) -> TradeRecord {
        TradeRecord {
            direction: Direction::Long,
            entry_time: 0,
            entry_price: 100.0,
            exit_time: DAY,
            exit_price: 100.0 + r * 5.0,
            reference_price: 100.0,
            stop_price: 95.0,
            take_profit_price: Some(110.0),
            size: 20.0,
            risk_per_unit: 5.0,
            r_multiple: r,
            bars_held: 3,
            exit_trigger: trigger,
            entry_reasons: vec!["delta".into()],
            exit_reasons: Vec::new(),
            regime: regime.to_string(),
        }
    }

    #[test]
    fn an_empty_run_reports_zeros_not_nans() {
        let metrics = compute_metrics(&[], 30 * DAY);
        assert_eq!(metrics, Metrics::empty());
        assert!(metrics.win_rate.is_finite());
        assert!(metrics.profit_factor.is_finite());
    }

    #[test]
    fn win_rate_and_profit_factor_are_computed_in_r() {
        // +2R, +1R, -1R, -1R: 2 wins of 4, gross 3R against 2R.
        let trades = vec![
            trade(2.0, "bullish", ExitTrigger::Target),
            trade(1.0, "bullish", ExitTrigger::Target),
            trade(-1.0, "bearish", ExitTrigger::Stop),
            trade(-1.0, "bearish", ExitTrigger::Stop),
        ];
        let metrics = compute_metrics(&trades, 90 * DAY);

        assert_eq!(metrics.total_trades, 4);
        assert!((metrics.win_rate - 0.5).abs() < 1e-9);
        assert!((metrics.profit_factor - 1.5).abs() < 1e-9);
        assert!((metrics.net_return_pct - 1.0).abs() < 1e-9);
        assert!((metrics.average_r - 0.25).abs() < 1e-9);
    }

    #[test]
    fn a_run_with_no_losses_reports_an_infinite_profit_factor() {
        let trades = vec![trade(1.0, "bullish", ExitTrigger::Target)];
        let metrics = compute_metrics(&trades, 30 * DAY);
        assert!(metrics.profit_factor.is_infinite());
        assert!((metrics.win_rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn max_drawdown_is_the_deepest_peak_to_trough_of_the_r_curve() {
        // Curve: +1, +3, +2, 0, +2. Peak 3, trough 0 -> drawdown 3.
        let trades = vec![
            trade(1.0, "bullish", ExitTrigger::Target),
            trade(2.0, "bullish", ExitTrigger::Target),
            trade(-1.0, "bearish", ExitTrigger::Stop),
            trade(-2.0, "bearish", ExitTrigger::Stop),
            trade(2.0, "bullish", ExitTrigger::Target),
        ];
        let metrics = compute_metrics(&trades, 90 * DAY);
        assert!((metrics.max_drawdown_pct - 3.0).abs() < 1e-9, "{metrics:?}");
    }

    #[test]
    fn a_monotonic_winning_curve_has_no_drawdown() {
        let trades = vec![
            trade(1.0, "bullish", ExitTrigger::Target),
            trade(1.0, "bullish", ExitTrigger::Target),
        ];
        assert!((compute_metrics(&trades, 30 * DAY).max_drawdown_pct).abs() < 1e-9);
    }

    #[test]
    fn sharpe_is_zero_without_dispersion_or_a_single_trade() {
        assert!(
            (compute_metrics(&[trade(1.0, "bullish", ExitTrigger::Target)], 30 * DAY).sharpe_ratio)
                .abs()
                < 1e-9
        );

        let identical = vec![
            trade(1.0, "bullish", ExitTrigger::Target),
            trade(1.0, "bullish", ExitTrigger::Target),
        ];
        assert!((compute_metrics(&identical, 30 * DAY).sharpe_ratio).abs() < 1e-9);
    }

    #[test]
    fn sharpe_is_positive_for_a_profitable_run_with_variance() {
        let trades = vec![
            trade(2.0, "bullish", ExitTrigger::Target),
            trade(-1.0, "bearish", ExitTrigger::Stop),
            trade(3.0, "bullish", ExitTrigger::Target),
            trade(-1.0, "bearish", ExitTrigger::Stop),
        ];
        let metrics = compute_metrics(&trades, 365 * DAY);
        assert!(metrics.sharpe_ratio > 0.0, "{metrics:?}");
    }

    #[test]
    fn worst_regime_needs_something_to_compare() {
        let single = vec![trade(1.0, "bullish", ExitTrigger::Target)];
        assert_eq!(
            worst_regime(&single),
            None,
            "one regime is not a comparison"
        );

        let mixed = vec![
            trade(2.0, "bullish", ExitTrigger::Target),
            trade(-1.0, "bearish", ExitTrigger::Stop),
            trade(-1.0, "bearish", ExitTrigger::Stop),
        ];
        assert_eq!(worst_regime(&mixed).as_deref(), Some("bearish"));
    }

    #[test]
    fn the_report_serializes_to_json_for_the_cli() {
        let document = parse(SAMPLE).unwrap();
        let report = build_report(
            &document,
            "BTCUSDT",
            0,
            90 * DAY,
            "entry",
            vec![trade(2.0, "bullish", ExitTrigger::Target)],
            vec!["no swing low yet".into()],
            FillAssumptions::default(),
        );

        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"strategy\":\"Test\""), "{json}");
        assert!(json.contains("\"total_trades\":1"), "{json}");
        assert!(json.contains("R multiples"), "{json}");
        assert_eq!(report.skipped_signals_count, 1);
        assert_eq!(report.best_timeframe.as_deref(), Some("entry"));
        assert!(!report.is_empty());
    }

    #[test]
    fn the_summary_is_honest_about_an_empty_run() {
        let document = parse(SAMPLE).unwrap();
        let report = build_report(
            &document,
            "BTCUSDT",
            0,
            DAY,
            "entry",
            Vec::new(),
            Vec::new(),
            FillAssumptions::default(),
        );
        assert!(report.is_empty());
        assert!(
            report.summary().contains("no trades"),
            "{}",
            report.summary()
        );
    }

    #[test]
    fn the_assumptions_state_that_results_do_not_compound() {
        let assumptions = FillAssumptions::default();
        assert!(assumptions.compounding.contains("none"));
        assert!(assumptions.return_units.contains("R multiples"));
        assert!(assumptions.ambiguous_bar.contains("stop"));
    }
}
