//! Market-wide scans (`docs/19`, the "market-wide scanners" gap).
//!
//! ## What a scan is, and what it is not
//!
//! A scan ranks *many* symbols by one measurement. It is **not** a data source:
//! it does not fill a buffer, it does not open a feed, and it keeps nothing after
//! it answers. That is the whole reason it can exist under the 6 GB rule and
//! under the RAM budget at the same time -- see [`scan`] for the memory argument.
//!
//! The distinction matters for what a user can do with the result. A scan tells
//! them *where to look*; opening one of its rows is what makes that symbol a
//! chart, and a chart goes through the normal path (buffer, feed, on-demand
//! backfill). A scan that tried to be a chart would have to hold candles for
//! everything it ranked, which is the thing this design refuses.
//!
//! ## Concurrency, and why it is bounded
//!
//! Symbols are visited with a fixed number of permits rather than one request per
//! symbol or one request at a time:
//!
//! * **Unbounded** would fire three hundred venue requests at once, which is a
//!   rate-limit ban and, worse, a ban that arrives as a burst of failures the
//!   caller cannot attribute to the scan.
//! * **Serial** is honest but unusably slow -- three hundred round trips at
//!   ~150 ms is most of a minute for a ranking that is stale by the time it ends.
//!
//! [`DEFAULT_CONCURRENCY`] is deliberately small. Binance's REST budget is
//! per-IP and shared with every chart on the page, so a scan that saturates it
//! makes the *interactive* charts slow, which is the wrong trade: a scan is a
//! background question.
//!
//! ## A failure is a row, not a hole
//!
//! A symbol that could not be fetched produces [`ScanRow::failed`] rather than
//! being dropped. A ranking of 40 out of 50 symbols is a claim about 40 symbols
//! presented as if it were the market, and the user has no way to tell. The
//! result carries the failures, counts them, and the summary says so.

use std::sync::Arc;

use analytics_core::indicators::{atr_percent, rsi};
use analytics_core::types::Timeframe;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::window::WindowService;

/// The period every indicator here uses.
///
/// 14 is the conventional default and the one a chart draws, which is the
/// property that matters: a scan is a *pointer* to a chart, so a number that
/// disagreed with the chart it opens would send the user to a setup that is not
/// there. Changing it means changing it in both places or in neither.
pub const INDICATOR_PERIOD: usize = 14;

/// How many symbols may be in flight at once.
///
/// Shared with the interactive charts' REST budget, which is why it is small
/// rather than optimal for the scan alone. See the module docs.
pub const DEFAULT_CONCURRENCY: usize = 4;

/// Most symbols one scan will visit, whatever the caller asks for.
///
/// A ceiling rather than a validation error: a caller that asks for more gets a
/// truncated scan and a note saying so, because a scan that refuses outright
/// teaches the user nothing about which symbols it *could* have covered.
pub const MAX_SCAN_SYMBOLS: usize = 200;

/// Bars pulled per symbol. Enough for the slowest indicator here (RSI-14) to
/// have real history behind its first value, and small enough that a symbol's
/// footprint is bounded regardless of how many symbols the scan visits.
pub const BARS_PER_SYMBOL: usize = 300;

/// What to measure across the market.
///
/// A closed enum rather than a free-form expression, for the same reason
/// [`analytics_core::concepts`] is validated: a scan name nobody can compute
/// must be **refused**, not stored and then never produced. Each variant is one
/// implementation, and adding one is a variant plus an arm in [`measure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanMetric {
    /// Latest RSI. Ranks by RSI descending, so overbought leads.
    Rsi,
    /// Most recent ATR as a percentage of price -- a volatility ranking that is
    /// comparable across instruments, which raw ATR is not: 500 points of ATR
    /// is enormous on BTCUSDT and impossible on a coin trading at 0.02.
    AtrPercent,
    /// Percentage change over the window. The blunt one, kept because it is the
    /// only measure here a user can check by eye against the chart.
    ChangePercent,
}

impl ScanMetric {
    /// Every metric, for a client that wants to build a menu.
    pub const ALL: [Self; 3] = [Self::Rsi, Self::AtrPercent, Self::ChangePercent];

    /// The wire name, which is also the shell's label key.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rsi => "rsi",
            Self::AtrPercent => "atr_percent",
            Self::ChangePercent => "change_percent",
        }
    }

    /// What the number means, for a result that has to explain itself.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rsi => "RSI",
            Self::AtrPercent => "ATR % of price",
            Self::ChangePercent => "Change %",
        }
    }

    /// Whether a higher value is more interesting.
    ///
    /// The ranking direction, and it belongs to the metric rather than the
    /// caller: sorting RSI ascending would put the most oversold first under a
    /// heading that says "highest RSI". A caller that wants the other end reads
    /// the array backwards.
    #[must_use]
    pub const fn higher_is_first(self) -> bool {
        true
    }
}

impl std::str::FromStr for ScanMetric {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "rsi" => Ok(Self::Rsi),
            "atr_percent" | "atr" => Ok(Self::AtrPercent),
            "change_percent" | "change" => Ok(Self::ChangePercent),
            other => Err(format!(
                "`{other}` is not a scan this platform can run. Try one of: {}.",
                Self::ALL
                    .iter()
                    .map(|m| m.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

/// One symbol's place in the ranking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanRow {
    /// The instrument.
    pub symbol: String,
    /// The measured value, when it could be measured.
    ///
    /// `None` on a failure, and `None` on a symbol with too little history --
    /// which is a real case on a young listing and must not be reported as zero.
    /// A zero RSI and "RSI could not be computed" are different claims, and only
    /// one of them means the symbol is worth opening.
    pub value: Option<f64>,
    /// The bar the value was measured on, in unix nanoseconds.
    ///
    /// Carried so a ranking cannot be read as "now" without evidence: a symbol
    /// whose newest bar is hours old still has an RSI, and it is the RSI of
    /// hours ago.
    pub as_of_ns: Option<i64>,
    /// How many bars the measurement actually had.
    pub bars: usize,
    /// Why the symbol has no value, when it has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ScanRow {
    /// A measured row.
    #[must_use]
    pub fn measured(symbol: &str, value: f64, as_of_ns: i64, bars: usize) -> Self {
        Self {
            symbol: symbol.to_uppercase(),
            value: Some(value),
            as_of_ns: Some(as_of_ns),
            bars,
            error: None,
        }
    }

    /// A row that could not be measured, named rather than dropped.
    ///
    /// Public because the scanner's callers build these too -- and because the
    /// point of the type is that a failure is representable *in the result*
    /// rather than only in a log nobody reads.
    #[must_use]
    pub fn failed(symbol: &str, reason: impl Into<String>) -> Self {
        Self {
            symbol: symbol.to_uppercase(),
            value: None,
            as_of_ns: None,
            bars: 0,
            error: Some(reason.into()),
        }
    }

    /// Whether this row carries a measurement.
    #[must_use]
    pub fn is_measured(&self) -> bool {
        self.value.is_some()
    }
}

/// The whole result of one scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanResult {
    /// What was measured.
    pub metric: ScanMetric,
    /// The resolution every symbol was measured on.
    pub timeframe: Timeframe,
    /// Measured rows, best first.
    pub rows: Vec<ScanRow>,
    /// Symbols the scan could not measure at all, with the reason.
    ///
    /// Separate from `rows` so a caller cannot mistake "we skipped it" for "it
    /// ranked last", which is the same distinction [`ScanRow::failed`] makes one
    /// level down.
    pub failures: Vec<ScanRow>,
    /// Symbols the request named that the ceiling dropped.
    pub skipped: Vec<String>,
    /// How many symbols were asked for. Reported so a truncated scan is visibly
    /// truncated rather than looking like the whole market.
    pub requested: usize,
}

impl ScanResult {
    /// How many symbols came back with a number.
    #[must_use]
    pub fn measured(&self) -> usize {
        self.rows.len()
    }

    /// Whether every requested symbol produced a value.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty() && self.skipped.is_empty()
    }

    /// One sentence describing what the scan covered.
    ///
    /// Written here rather than in the shell for the same reason the capability
    /// warning is: it names the *consequences* of an incomplete scan ("40 of 50
    /// symbols") rather than the mechanism ("skipped: 10"), and a shell that
    /// composed it would drift from the counts it is describing.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = format!(
            "{} ranked by {} on {}",
            format_count(self.rows.len(), "symbol"),
            self.metric.label(),
            self.timeframe.as_str()
        );
        let unmeasured = self.failures.len() + self.skipped.len();
        if unmeasured > 0 {
            out.push_str(&format!(
                "; {} of {} could not be measured",
                unmeasured, self.requested
            ));
            if !self.skipped.is_empty() {
                out.push_str(&format!(" ({} over the {} symbol ceiling)", self.skipped.len(), MAX_SCAN_SYMBOLS));
            }
            if !self.failures.is_empty() {
                out.push_str(" (see the failures for why)");
            }
        }
        out
    }
}

fn format_count(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Scan `symbols`, returning them ranked by `metric`.
///
/// ## The memory argument
///
/// Symbols are visited one at a time behind a permit. Each one's candles are
/// fetched, measured and **dropped** before the next is fetched, so the peak
/// footprint is `concurrency * BARS_PER_SYMBOL` bars regardless of how many
/// symbols the scan covers. Three hundred symbols cost the same as three.
///
/// This is the whole reason the function takes a count rather than keeping what
/// it reads. The alternative -- hold every symbol's window, then rank -- would be
/// ~30 MB resident for three hundred symbols on one timeframe, and would grow
/// without bound the moment a user asked for a second resolution.
///
/// Note it reads through [`WindowService`] and not the venue directly: a symbol
/// the user already has on a chart is answered from the buffer, so a scan over
/// what is on screen is nearly free and does not spend REST budget at all.
pub async fn scan(
    windows: &Arc<WindowService>,
    symbols: &[String],
    timeframe: Timeframe,
    metric: ScanMetric,
    now_ns: i64,
) -> ScanResult {
    scan_with_concurrency(windows, symbols, timeframe, metric, now_ns, DEFAULT_CONCURRENCY).await
}

/// [`scan`] with an explicit concurrency, so a test can prove the permit logic
/// without racing a real venue.
pub async fn scan_with_concurrency(
    windows: &Arc<WindowService>,
    symbols: &[String],
    timeframe: Timeframe,
    metric: ScanMetric,
    now_ns: i64,
    concurrency: usize,
) -> ScanResult {
    let requested = symbols.len();
    // Truncated, not refused. A caller that asks for more than the ceiling gets
    // the first `MAX_SCAN_SYMBOLS` and a list of what it did not get, so the
    // result can say "40 of 250" instead of quietly answering about 40.
    let wanted: Vec<String> = symbols.iter().take(MAX_SCAN_SYMBOLS).cloned().collect();
    let skipped: Vec<String> = symbols
        .iter()
        .skip(MAX_SCAN_SYMBOLS)
        .map(|s| s.to_uppercase())
        .collect();

    // The window every symbol is measured over: the last `BARS_PER_SYMBOL` bars
    // ending now. One window for all of them, so the ranking compares like with
    // like -- a per-symbol window fitted to however much history exists would
    // rank a young listing on three bars against an old one on three hundred.
    let width = timeframe.nanos().max(1);
    let span = i64::try_from(BARS_PER_SYMBOL)
        .unwrap_or(i64::MAX)
        .saturating_mul(width);
    let to_ns = now_ns;
    let from_ns = to_ns.saturating_sub(span);

    let permits = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut tasks = Vec::with_capacity(wanted.len());

    for symbol in &wanted {
        let permits = Arc::clone(&permits);
        let windows = Arc::clone(windows);
        let symbol = symbol.clone();
        tasks.push(tokio::spawn(async move {
            // Held for the whole fetch, so the permit bounds *requests in flight*
            // rather than tasks started -- which is what the venue's rate limit
            // is actually about.
            let _permit = permits.acquire_owned().await;
            measure_symbol(&windows, &symbol, timeframe, metric, from_ns, to_ns).await
        }));
    }

    let mut rows = Vec::with_capacity(tasks.len());
    let mut failures = Vec::new();
    for (index, task) in tasks.into_iter().enumerate() {
        let measured = match task.await {
            Ok(row) => row,
            // A panicked or cancelled task is still a symbol the user asked
            // about. Dropping it would silently shrink the ranking; naming it
            // keeps `requested` honest.
            Err(e) => Err(format!("the scan task for this symbol failed: {e}")),
        };
        match measured {
            Ok(row) => rows.push(row),
            Err(reason) => failures.push(ScanRow::failed(&wanted[index], reason)),
        }
    }

    // Best first. `total_cmp` rather than `partial_cmp().unwrap()`: a `NaN` that
    // slipped through would panic a sort, and a scan is not worth taking the
    // process down for.
    rows.sort_by(|a, b| {
        let (left, right) = (a.value.unwrap_or(f64::MIN), b.value.unwrap_or(f64::MIN));
        if metric.higher_is_first() {
            right.total_cmp(&left)
        } else {
            left.total_cmp(&right)
        }
    });

    ScanResult {
        metric,
        timeframe,
        rows,
        failures,
        skipped,
        requested,
    }
}

/// Fetch one symbol's window and measure it.
async fn measure_symbol(
    windows: &Arc<WindowService>,
    symbol: &str,
    timeframe: Timeframe,
    metric: ScanMetric,
    from_ns: i64,
    to_ns: i64,
) -> Result<ScanRow, String> {
    let window = windows
        .candles(symbol, timeframe, from_ns, to_ns)
        .await
        .map_err(|e| e.to_string())?;

    let Some(last) = window.candles.last() else {
        // Not a failure of the request -- the venue answered and had nothing.
        // Reported as a reason rather than as a zero, because a symbol with no
        // bars in the window has no RSI and saying it has one of zero is a
        // claim the data does not support.
        return Err(format!(
            "the venue returned no {} bars for this symbol in the window",
            timeframe.as_str()
        ));
    };
    let as_of = last.open_time;

    let value = measure(&window.candles, metric).ok_or_else(|| {
        format!(
            "{} bars is not enough to compute {}",
            window.candles.len(),
            metric.label()
        )
    })?;

    if !value.is_finite() {
        return Err(format!("{} came out non-finite", metric.label()));
    }

    Ok(ScanRow::measured(
        symbol,
        value,
        as_of,
        window.candles.len(),
    ))
}

/// The measurement itself.
///
/// One function with an arm per metric, returning `Option` rather than a default:
/// "not enough history" is a real answer and a zero would be a lie about it.
///
/// The indicators return one entry per input with `None` for their warm-up, so
/// `.last()` is the newest value and a `None` there is exactly "not enough
/// history yet". That convention is what lets this function be this short -- the
/// alternative, scanning backwards for the newest `Some`, would silently rank a
/// symbol on a stale indicator value.
fn measure(candles: &[analytics_core::Candle], metric: ScanMetric) -> Option<f64> {
    let last = candles.last()?;
    match metric {
        ScanMetric::Rsi => {
            // Wilder's RSI-14 over the closes, matching the period a chart shows.
            // A scan whose RSI disagreed with the RSI on the chart it opens would
            // be worse than no scan.
            let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
            rsi(&closes, INDICATOR_PERIOD).last().copied().flatten()
        }
        ScanMetric::AtrPercent => {
            // `atr_percent` rather than dividing `atr` by the close here: the
            // library already owns that conversion, and a second copy of it is a
            // second definition of the metric.
            atr_percent(candles, INDICATOR_PERIOD).last().copied().flatten()
        }
        ScanMetric::ChangePercent => {
            let first = candles.first()?;
            if first.close == 0.0 {
                return None;
            }
            Some((last.close - first.close) / first.close * 100.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles(count: usize, start: f64, step: f64) -> Vec<analytics_core::Candle> {
        // An *absolute* +-1.0 range on a series that starts at `start`. Good
        // enough for "a number comes out"; wrong for any claim about scale,
        // because +-1.0 is 0.004% of 50,000 and 10,000% of 0.02.
        candle_series(count, start, step, 1.0, 1.0)
    }

    /// The same, but with both the drift *and* the envelope expressed as a
    /// fraction of price, so two series at different price levels are the same
    /// shape and can be compared like for like.
    fn candles_scaled(count: usize, start: f64) -> Vec<analytics_core::Candle> {
        candle_series(count, start, start * 0.00001, start * 0.00002, start * 0.00001)
    }

    fn candle_series(
        count: usize,
        start: f64,
        step: f64,
        range: f64,
        body: f64,
    ) -> Vec<analytics_core::Candle> {
        (0..count)
            .map(|i| {
                let base = start + (i as f64) * step;
                analytics_core::Candle {
                    symbol: "BTCUSDT".into(),
                    timeframe: Timeframe::M5,
                    open_time: (i as i64) * 300_000_000_000,
                    open: base,
                    high: base + range,
                    low: base - range,
                    close: base + body,
                    volume: 10.0,
                    buy_volume: 6.0,
                    sell_volume: 4.0,
                }
            })
            .collect()
    }

    #[test]
    fn a_measured_row_carries_when_its_number_is_from() {
        // A ranking read as "now" is wrong the moment one symbol's newest bar is
        // old -- which happens on a thin listing while a liquid one is live.
        let row = ScanRow::measured("btcusdt", 71.5, 1_700_000_000_000_000_000, 300);
        assert_eq!(row.symbol, "BTCUSDT");
        assert_eq!(row.as_of_ns, Some(1_700_000_000_000_000_000));
        assert_eq!(row.bars, 300);
        assert!(row.is_measured());
    }

    #[test]
    fn a_metric_name_round_trips_through_its_wire_form() {
        // The shell sends this string and switches on it for a column heading.
        // A rename is not a compile error anywhere -- it is a 422 the user sees
        // as "that scan does not exist".
        for metric in ScanMetric::ALL {
            let json = serde_json::to_value(metric).expect("serializes");
            assert_eq!(json, metric.name(), "{metric:?} renamed on the wire");
            assert_eq!(metric.name().parse::<ScanMetric>(), Ok(metric));
        }
    }

    #[test]
    fn every_metric_has_a_label_and_ranks_somewhere() {
        for metric in ScanMetric::ALL {
            assert!(!metric.label().is_empty(), "{metric:?} has no label");
            // Reference `higher_is_first` so the property is exercised rather
            // than merely declared.
            let _ = metric.higher_is_first();
        }
    }

    #[test]
    fn an_unknown_metric_is_refused_with_the_ones_that_exist() {
        // The message has to name the alternatives: a refusal a user cannot act
        // on is a dead end, and the vocabulary is small enough to list.
        let error = "gann_angle".parse::<ScanMetric>().expect_err("must refuse");
        assert!(error.contains("gann_angle"), "{error}");
        for metric in ScanMetric::ALL {
            assert!(error.contains(metric.name()), "{error} omits {metric:?}");
        }
    }

    #[test]
    fn a_metric_spelled_with_a_dash_or_a_space_still_parses() {
        // A query string is written by hand before it is written by the shell.
        assert_eq!("atr-percent".parse::<ScanMetric>(), Ok(ScanMetric::AtrPercent));
        assert_eq!(" ATR ".parse::<ScanMetric>(), Ok(ScanMetric::AtrPercent));
        assert_eq!("CHANGE".parse::<ScanMetric>(), Ok(ScanMetric::ChangePercent));
    }

    #[test]
    fn rsi_needs_enough_history_and_says_so_instead_of_returning_zero() {
        // Three bars cannot produce an RSI-14. Zero is the dangerous answer: it
        // reads as "maximally oversold", which is the most interesting value the
        // column can hold.
        assert_eq!(measure(&candles(3, 100.0, 1.0), ScanMetric::Rsi), None);
        assert!(
            measure(&candles(60, 100.0, 1.0), ScanMetric::Rsi).is_some(),
            "sixty bars is enough for RSI-14"
        );
    }

    #[test]
    fn an_empty_window_measures_nothing() {
        for metric in ScanMetric::ALL {
            assert_eq!(measure(&[], metric), None, "{metric:?}");
        }
    }

    #[test]
    fn atr_percent_comes_back_as_a_fraction_not_a_number_out_of_a_hundred() {
        // The library's name says "percent" and its arithmetic is `value / close`
        // -- a **fraction**, not `* 100`. Pinned because the label is on the wire:
        // a shell that printed `0.004` under a heading reading "%" would be wrong
        // by two orders of magnitude and look entirely plausible.
        //
        // This series moves ~1.0 per 5-minute bar on a ~100 price, so the ATR is
        // a bit under 1% -- as a fraction, that is a bit under 0.01.
        let series = candles(60, 100.0, 1.0);
        let value = measure(&series, ScanMetric::AtrPercent).expect("computes");
        assert!(
            value > 0.0 && value < 0.1,
            "expected a fraction near 0.01, got {value}"
        );
    }

    #[test]
    fn atr_percent_is_scale_free_and_raw_atr_is_not() {
        // The reason this metric divides by price: 500 points of ATR is enormous
        // on BTCUSDT and impossible on a coin at 0.02, so ranking raw ATR would
        // rank by price level and call it volatility.
        //
        // "Scale free" is a claim about the *same relative move*, so the two
        // inputs are the same shape at prices 2,500,000x apart. Note the candles
        // are built locally: the shared helper's +-1.0 range is an absolute
        // constant, which is 0.004% of 50,000 and 10,000% of 0.02.
        let expensive = candles_scaled(60, 50_000.0);
        let cheap = candles_scaled(60, 0.02); // the same shape, 2,500,000x cheaper

        let a = measure(&expensive, ScanMetric::AtrPercent).expect("computes");
        let b = measure(&cheap, ScanMetric::AtrPercent).expect("computes");

        assert!(
            (a - b).abs() < 1e-9,
            "the same proportional move must score the same at any price: ATR% {a} at 50,000 vs {b} at 0.02"
        );

        // The control: raw ATR on those same series differs by five orders of
        // magnitude, which is what makes ATR% the only rankable form.
        let raw_a = analytics_core::indicators::atr(&expensive, 14)
            .last()
            .copied()
            .flatten()
            .expect("computes");
        let raw_b = analytics_core::indicators::atr(&cheap, 14)
            .last()
            .copied()
            .flatten()
            .expect("computes");
        assert!(
            raw_a > raw_b * 1_000_000.0,
            "raw ATR must be dominated by price level -- that is the bug this metric avoids: {raw_a} vs {raw_b}"
        );
    }

    #[test]
    fn a_change_percent_is_the_whole_window_not_the_last_bar() {
        // 100 -> 110 over the series is +10%, whatever the last candle did.
        let series = candles(10, 100.0, 1.0);
        let change = measure(&series, ScanMetric::ChangePercent).expect("computes");
        let first = series.first().unwrap().close;
        let last = series.last().unwrap().close;
        assert!(
            (change - (last - first) / first * 100.0).abs() < 1e-9,
            "got {change}"
        );
    }

    #[test]
    fn a_zero_price_does_not_produce_an_infinity() {
        // A venue that reports a zero close is broken, and `x / 0` is `inf`,
        // which would sort to the top of a volatility ranking and look like the
        // most volatile instrument in the market.
        let mut series = candles(60, 100.0, 1.0);
        for candle in &mut series {
            candle.close = 0.0;
        }
        assert_eq!(measure(&series, ScanMetric::AtrPercent), None);
        assert_eq!(measure(&series, ScanMetric::ChangePercent), None);
    }

    #[test]
    fn a_failed_row_is_named_not_dropped() {
        let row = ScanRow::failed("ethusdt", "the venue did not answer");
        assert_eq!(row.symbol, "ETHUSDT", "symbols are normalised like the rest");
        assert!(!row.is_measured());
        assert_eq!(row.value, None);
        assert!(row.error.is_some());
    }

    fn result(rows: usize, failures: usize, skipped: usize) -> ScanResult {
        ScanResult {
            metric: ScanMetric::Rsi,
            timeframe: Timeframe::H1,
            rows: (0..rows)
                .map(|i| ScanRow::measured(&format!("S{i}"), i as f64, 0, 300))
                .collect(),
            failures: (0..failures)
                .map(|i| ScanRow::failed(&format!("F{i}"), "no data"))
                .collect(),
            skipped: (0..skipped).map(|i| format!("K{i}")).collect(),
            requested: rows + failures + skipped,
        }
    }

    #[test]
    fn a_complete_scan_says_so_without_qualification() {
        let scan = result(50, 0, 0);
        assert!(scan.is_complete());
        assert_eq!(scan.summary(), "50 symbols ranked by RSI on 1h");
    }

    #[test]
    fn an_incomplete_scan_states_how_incomplete() {
        // The number that matters to a reader is "45 of 50", not "skipped: 5"
        // -- one is a fact about the market they are looking at, the other is a
        // fact about the scanner.
        let scan = result(40, 5, 5);
        assert!(!scan.is_complete());
        let summary = scan.summary();
        assert!(summary.starts_with("40 symbols ranked by RSI on 1h"), "{summary}");
        assert!(summary.contains("10 of 50 could not be measured"), "{summary}");
        assert!(summary.contains("5 over the 200 symbol ceiling"), "{summary}");
        assert!(summary.contains("see the failures for why"), "{summary}");
    }

    #[test]
    fn a_scan_that_only_failed_says_where_to_look_and_not_about_a_ceiling() {
        // The two reasons to be incomplete are different problems with different
        // fixes, so a summary that mentioned both when only one applied would
        // send the reader looking at the wrong limit.
        let only_failures = result(40, 10, 0).summary();
        assert!(only_failures.contains("10 of 50 could not be measured"), "{only_failures}");
        assert!(only_failures.contains("see the failures for why"), "{only_failures}");
        assert!(!only_failures.contains("ceiling"), "{only_failures}");

        let only_skipped = result(40, 0, 10).summary();
        assert!(only_skipped.contains("10 of 50 could not be measured"), "{only_skipped}");
        assert!(only_skipped.contains("10 over the 200 symbol ceiling"), "{only_skipped}");
        assert!(!only_skipped.contains("failures for why"), "{only_skipped}");
    }

    #[test]
    fn a_single_symbol_is_not_pluralised() {
        assert_eq!(result(1, 0, 0).summary(), "1 symbol ranked by RSI on 1h");
    }

    #[test]
    fn the_scan_result_wire_shape_is_pinned() {
        // Three consumers: the route, the shell's table, and the AI's tool
        // result. A rename here is a column that renders blank.
        let json = serde_json::to_value(ScanRow::measured("BTCUSDT", 71.5, 123, 300))
            .expect("serializes");
        assert_eq!(json["symbol"], "BTCUSDT");
        assert_eq!(json["value"], 71.5);
        assert_eq!(json["as_of_ns"], 123);
        assert_eq!(json["bars"], 300);
        assert!(json["error"].is_null(), "a measured row has no error");

        let failed = serde_json::to_value(ScanRow::failed("ETHUSDT", "no data")).expect("ok");
        assert!(failed["value"].is_null());
        assert_eq!(failed["error"], "no data");

        let scan = serde_json::to_value(result(1, 0, 0)).expect("ok");
        assert_eq!(scan["metric"], "rsi");
        assert_eq!(scan["timeframe"], "1h");
        assert!(scan["rows"].is_array());
        assert!(scan["failures"].is_array());
        assert!(scan["skipped"].is_array());
        assert_eq!(scan["requested"], 1);
    }
}
