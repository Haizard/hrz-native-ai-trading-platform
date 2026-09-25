//! Venue REST adapters: the seam that makes `BackfillClient` venue-agnostic.
//!
//! ## Why this module exists
//!
//! `docs/19` row 25 found it and named it: *"`BackfillClient` still speaks
//! Binance's klines/aggTrades shape whatever `MARKET_REST_URL` points at, so a
//! second market needs a venue adapter and not just a URL."*
//!
//! That is the whole defect. `ExchangeCollector` made the **live** side
//! exchange-agnostic -- adding a venue meant implementing a trait -- but the
//! **historical** side had the venue's name written into it: the path
//! `/api/v3/klines`, the interval spelling, the array shape, and the
//! `taker_buy_base` column at index 9. Pointing `MARKET_REST_URL` at a second
//! venue produced a client that asked a different API for Binance's URL and then
//! decoded whatever came back as if it were Binance's.
//!
//! ## What a venue actually differs in
//!
//! Read from the venues' own documentation, not guessed. Bybit's `GET
//! /v5/market/kline` differs from Binance's `GET /api/v3/klines` in five ways,
//! and every one of them fails *silently* if the adapter gets it wrong:
//!
//! | | Binance | Bybit v5 |
//! |---|---|---|
//! | Path | `/api/v3/klines` | `/v5/market/kline` |
//! | Interval | `1m`, `1h`, `1w` | `1`, `60`, `W` |
//! | Envelope | a bare array | `{"result":{"list":[…]}}` |
//! | Row order | ascending by open time | **descending** |
//! | Row width | 12 columns, strings and numbers mixed | 7 columns, **all strings** |
//!
//! A wrong interval is a 400 rather than a wrong answer, but a wrong **row
//! order** is a chart drawn backwards and a wrong **path** is an empty window
//! reported as "the venue has no data". Both read as market conditions.
//!
//! ## The buy/sell split, and why it is a `None`
//!
//! Binance's kline carries `takerBuyBaseAssetVolume` (column 9), which is what
//! gives the platform a buy/sell split without replaying trades. **Bybit's does
//! not** -- it returns turnover, which is a quote-asset figure and cannot be
//! decomposed into a buy and a sell side.
//!
//! So [`RawKline::taker_buy_base`] is an `Option` here, and a venue that cannot
//! supply it says `None`. The alternative -- filling it with `volume / 2` or with
//! `0.0` -- would make every Bybit candle report a 50/50 split or an all-sell
//! split, which is a *fabricated* order-flow reading that a footprint or a delta
//! indicator would then chart as fact. A missing number the caller can see is
//! strictly better than a plausible one that is wrong.

use analytics_core::{Candle, Timeframe};

use crate::error::MarketDataError;

/// One page of klines, plus what the pager needs to decide whether to continue.
///
/// A venue page is not interchangeable with a list of rows: Binance answers
/// "fewer than `limit` means exhausted", while a descending venue's page must be
/// walked backwards, and both need to know the newest open time they saw so the
/// next request can resume exactly.
#[derive(Debug, Clone)]
pub struct KlinePage {
    /// The rows, **normalised to ascending open time**.
    ///
    /// Ascending regardless of how the venue sent them: the pager walks forward,
    /// and `merge` in `window.rs` assumes order. A venue that answers newest-first
    /// is reversed here, once, at the boundary -- not at every call site.
    pub rows: Vec<RawKline>,
    /// Oldest open time in this page, milliseconds.
    pub oldest_ms: Option<i64>,
    /// Newest open time in this page, milliseconds.
    pub newest_ms: Option<i64>,
}

impl KlinePage {
    /// Whether this page was short of a full venue page.
    ///
    /// The pager's stop condition for an ascending venue. A descending venue
    /// stops on its own rule -- see [`Venue::page_is_exhausted`].
    #[must_use]
    pub fn was_short(&self, limit: usize) -> bool {
        self.rows.len() < limit
    }

    /// Whether the page carried nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// One column of a venue's kline row, as an index into its array.
///
/// Named rather than numeric so an adapter's mapping is readable, and so a venue
/// whose columns are in a different order cannot be confused with one whose are
/// the same. Binance puts volume at 5 and taker-buy at 9; Bybit puts volume at 5
/// and has no taker-buy at all.
#[derive(Debug, Clone, Copy)]
pub struct Columns {
    /// Index of the open time.
    pub open_time: usize,
    /// Index of the open price.
    pub open: usize,
    /// Index of the high.
    pub high: usize,
    /// Index of the low.
    pub low: usize,
    /// Index of the close.
    pub close: usize,
    /// Index of the base-asset volume.
    pub volume: usize,
    /// Index of the taker-buy base volume, when the venue supplies one.
    ///
    /// `None` for a venue that does not. Not a default of `0`, because zero
    /// means "every trade was a sell" and that is a claim.
    pub taker_buy_base: Option<usize>,
}

/// A raw kline, normalised across venues.
///
/// Deliberately not the `wire::RawKline` from the Binance module: this one has
/// [`Self::taker_buy_base`] as an `Option`, because that is the field that
/// actually varies. Keeping them separate means the Binance decoder keeps its
/// non-optional field and no existing call site learns about an `Option` it
/// never has to handle.
#[derive(Debug, Clone, PartialEq)]
pub struct RawKline {
    /// Open time in milliseconds.
    pub open_time_ms: i64,
    /// Open price.
    pub open: f64,
    /// High price.
    pub high: f64,
    /// Low price.
    pub low: f64,
    /// Close price.
    pub close: f64,
    /// Total base-asset volume.
    pub volume: f64,
    /// Taker **buy** base-asset volume, when the venue supplies it.
    ///
    /// `None` is a real answer, not a failure -- see the module doc. A consumer
    /// that needs a buy/sell split must decide what to do without one; a
    /// consumer that does not (a plain candlestick chart) is unaffected.
    pub taker_buy_base: Option<f64>,
}

impl RawKline {
    /// Parse one row using a venue's column map.
    ///
    /// # Errors
    /// [`MarketDataError::Normalization`] when the row is too short for the
    /// columns the venue claims, or a required field is not a number.
    pub fn parse(values: &[serde_json::Value], columns: Columns) -> Result<Self, MarketDataError> {
        let widest = [
            Some(columns.open_time),
            Some(columns.open),
            Some(columns.high),
            Some(columns.low),
            Some(columns.close),
            Some(columns.volume),
            columns.taker_buy_base,
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(0);

        if values.len() <= widest {
            return Err(MarketDataError::Normalization(format!(
                "kline row has {} fields, expected at least {}",
                values.len(),
                widest + 1
            )));
        }

        let number = |index: usize, what: &str| json_f64(&values[index], what);

        Ok(Self {
            open_time_ms: json_i64(&values[columns.open_time], "open_time")?,
            open: number(columns.open, "open")?,
            high: number(columns.high, "high")?,
            low: number(columns.low, "low")?,
            close: number(columns.close, "close")?,
            volume: number(columns.volume, "volume")?,
            taker_buy_base: match columns.taker_buy_base {
                Some(index) => Some(number(index, "taker_buy_base")?),
                None => None,
            },
        })
    }

    /// Whether this venue's data can supply a buy/sell split.
    ///
    /// The honest answer to "can I chart delta on this venue", asked of the data
    /// rather than of the venue's name.
    #[must_use]
    pub fn has_order_flow_split(&self) -> bool {
        self.taker_buy_base.is_some()
    }

    /// Convert to a `Candle` at `timeframe`.
    ///
    /// ## What happens when the venue has no order-flow split
    ///
    /// `Candle.buy_volume` and `Candle.sell_volume` are plain `f64`, and every
    /// candle the platform builds has to fill them. Binance supplies
    /// `takerBuyBaseAssetVolume`, so for Binance this is a real measurement.
    /// **Bybit does not**, and there is no honest number to put in its place:
    ///
    /// - `buy = volume, sell = 0` says every trade was a buy.
    /// - `buy = 0, sell = volume` says every trade was a sell.
    /// - `buy = sell = volume / 2` says the order flow was perfectly balanced.
    ///
    /// All three are fabrications, and the third is the most dangerous because it
    /// looks plausible: a volume-delta indicator would chart a flat line and a
    /// reader would take "no imbalance" for a finding about the market. So this
    /// splits **proportionally to the candle's own direction** -- an up candle
    /// attributes the volume to buyers, a down candle to sellers -- and the
    /// caller is expected to have checked [`Self::has_order_flow_split`] first.
    ///
    /// That fallback is *also* not a measurement, and it is documented rather
    /// than silent. The rule the platform follows is that a consumer which needs
    /// real order flow must ask `has_order_flow_split` and refuse to draw a
    /// delta chart when it is false; the split here exists so that a plain
    /// candlestick chart -- which never reads these fields -- still gets a valid
    /// `Candle` rather than a panic or a NaN.
    #[must_use]
    pub fn to_candle(&self, symbol: &str, timeframe: Timeframe) -> Candle {
        let (buy_volume, sell_volume) = match self.taker_buy_base {
            Some(taker_buy) => {
                let buy = taker_buy.min(self.volume);
                (buy, (self.volume - buy).max(0.0))
            }
            // No measurement available. Direction-attributed, which at least
            // agrees with the candle's own reading instead of contradicting it.
            None if self.close >= self.open => (self.volume, 0.0),
            None => (0.0, self.volume),
        };

        Candle {
            symbol: symbol.to_string(),
            timeframe,
            open_time: self.open_time_ms * 1_000_000,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
            buy_volume,
            sell_volume,
        }
    }
}

/// A venue whose REST history `BackfillClient` can read.
///
/// The trait is deliberately about **shape**, not transport: the pager in
/// `backfill.rs` owns the loop, the politeness delay, the window arithmetic and
/// the `[from, to)` convention, because those are the same for every venue and
/// copy-pasting them per venue is how the `-1ms` inclusive-end fix would get
/// applied to one and forgotten on the other. A venue supplies its URL, its
/// spellings, its column map and its row order.
pub trait Venue: Send + Sync + std::fmt::Debug {
    /// Lowercase venue name, e.g. `"binance"`. Matches [`ExchangeCollector::name`]
    /// so the live and historical halves agree.
    ///
    /// [`ExchangeCollector::name`]: super::ExchangeCollector::name
    fn name(&self) -> &'static str;

    /// REST base URL, no trailing slash.
    fn rest_url(&self) -> &str;

    /// Path for a kline request, including the leading slash.
    fn klines_path(&self) -> &'static str;

    /// The venue's own spelling for `timeframe`.
    ///
    /// `None` when the venue cannot serve that resolution. Binance spells them
    /// `1m`/`1h`/`1w`; Bybit spells them `1`/`60`/`W`. A venue that lacks one
    /// must say so rather than emit a Binance string and let the request 400 --
    /// the difference is a refusal that names the resolution and an error that
    /// says "bad request".
    fn interval(&self, timeframe: Timeframe) -> Option<&'static str>;

    /// Column map for this venue's kline rows.
    fn columns(&self) -> Columns;

    /// Whether rows arrive newest-first and must be reversed.
    fn newest_first(&self) -> bool;

    /// Whether the venue wraps its rows in an envelope.
    ///
    /// `true` means the rows are at `result.list` rather than at the root.
    fn wraps_in_envelope(&self) -> bool;

    /// Rows the venue will return in one page.
    fn page_limit(&self) -> usize;

    /// Query parameter name for the window start.
    fn start_param(&self) -> &'static str;

    /// Query parameter name for the window end.
    fn end_param(&self) -> &'static str;

    /// Whether the venue's `end` parameter is inclusive.
    ///
    /// Binance's is inclusive, so a `[from, to)` window must send `to - 1ms` or
    /// the first candle of the *next* window is returned and double-counted when
    /// ranges are walked back to back. Recorded per venue because it is a venue
    /// fact, and the day a second venue differs the `-1` must not be applied to
    /// both.
    fn end_is_inclusive(&self) -> bool;

    /// Whether a short page means the window is exhausted.
    ///
    /// True for an ascending venue: pages are walked forwards and a short page
    /// at the end is the final one. A venue that pages **backwards from now**
    /// returns short pages for reasons other than exhaustion, and stopping on
    /// them truncates the history silently -- which is a chart missing its older
    /// half and looking perfectly healthy.
    fn short_page_means_done(&self) -> bool {
        true
    }

    /// Read a venue response body into rows.
    ///
    /// # Errors
    /// [`MarketDataError::Normalization`] when the envelope is missing or a row
    /// cannot be parsed.
    fn parse_klines(&self, body: &serde_json::Value) -> Result<Vec<RawKline>, MarketDataError> {
        let rows = if self.wraps_in_envelope() {
            body.get("result")
                .and_then(|result| result.get("list"))
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    MarketDataError::Normalization(format!(
                        "{}: response has no `result.list`",
                        self.name()
                    ))
                })?
        } else {
            body.as_array().ok_or_else(|| {
                MarketDataError::Normalization(format!(
                    "{}: expected an array of klines",
                    self.name()
                ))
            })?
        };

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let values = row.as_array().ok_or_else(|| {
                MarketDataError::Normalization(format!(
                    "{}: a kline row is not an array",
                    self.name()
                ))
            })?;
            out.push(RawKline::parse(values, self.columns())?);
        }

        if self.newest_first() {
            out.reverse();
        }
        Ok(out)
    }
}

/// `f64` from a JSON number **or** a JSON string.
///
/// Venues disagree: Binance sends a mixed row (numbers for prices, a number for
/// the open time, strings for the trailing aggregates) and Bybit v5 sends **every
/// field as a string** for precision. A decoder that assumed one would refuse the
/// other with "expected f64, got string" -- and that error would be raised at
/// parse time, on a row that looks perfectly fine in the venue's own docs.
fn json_f64(value: &serde_json::Value, what: &str) -> Result<f64, MarketDataError> {
    if let Some(number) = value.as_f64() {
        return Ok(number);
    }
    if let Some(text) = value.as_str() {
        return text.parse::<f64>().map_err(|_| {
            MarketDataError::Normalization(format!("{what}: `{text}` is not a number"))
        });
    }
    Err(MarketDataError::Normalization(format!(
        "{what}: expected a number or a numeric string, got {value}"
    )))
}

/// `i64` from a JSON number **or** a JSON string.
///
/// Bybit sends the open time as `"1670608800000"`. Parsing that as a string via
/// `as_i64()` returns `None`, which the Binance decoder would have reported as a
/// missing `open_time` -- on a payload that has one.
fn json_i64(value: &serde_json::Value, what: &str) -> Result<i64, MarketDataError> {
    if let Some(number) = value.as_i64() {
        return Ok(number);
    }
    if let Some(text) = value.as_str() {
        return text.parse::<i64>().map_err(|_| {
            MarketDataError::Normalization(format!("{what}: `{text}` is not an integer"))
        });
    }
    Err(MarketDataError::Normalization(format!(
        "{what}: expected an integer or a numeric string, got {value}"
    )))
}

/// Binance's spot REST API.
#[derive(Debug, Clone, Default)]
pub struct BinanceVenue {
    /// Base URL, overridable for a test or a regional mirror.
    pub rest_url: String,
}

impl BinanceVenue {
    /// Against `https://api.binance.com`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rest_url: "https://api.binance.com".to_string(),
        }
    }

    /// Against an arbitrary base URL.
    #[must_use]
    pub fn at(rest_url: impl Into<String>) -> Self {
        Self {
            rest_url: rest_url.into(),
        }
    }
}

impl Venue for BinanceVenue {
    fn name(&self) -> &'static str {
        "binance"
    }

    fn rest_url(&self) -> &str {
        &self.rest_url
    }

    fn klines_path(&self) -> &'static str {
        "/api/v3/klines"
    }

    fn interval(&self, timeframe: Timeframe) -> Option<&'static str> {
        Some(crate::backfill::interval_for(timeframe))
    }

    fn columns(&self) -> Columns {
        Columns {
            open_time: 0,
            open: 1,
            high: 2,
            low: 3,
            close: 4,
            volume: 5,
            // Column 9 is `takerBuyBaseAssetVolume`, which is what gives the
            // platform an order-flow split without replaying trades.
            taker_buy_base: Some(9),
        }
    }

    fn newest_first(&self) -> bool {
        false
    }

    fn wraps_in_envelope(&self) -> bool {
        false
    }

    fn page_limit(&self) -> usize {
        1000
    }

    fn start_param(&self) -> &'static str {
        "startTime"
    }

    fn end_param(&self) -> &'static str {
        "endTime"
    }

    fn end_is_inclusive(&self) -> bool {
        // Binance treats `endTime` as INCLUSIVE, hence the `-1ms` in the pager.
        true
    }
}

/// Bybit's v5 public REST API.
#[derive(Debug, Clone, Default)]
pub struct BybitVenue {
    /// Base URL.
    pub rest_url: String,
    /// Product type: `spot`, `linear` or `inverse`.
    ///
    /// Explicit rather than defaulted, because Bybit's own default is `linear`
    /// -- a perpetual -- and a platform charting a *spot* market that silently
    /// received perpetual candles would be drawing a different instrument at a
    /// similar price. The difference is real (funding, no expiry, different open
    /// interest) even when the two prices agree to the cent.
    pub category: String,
}

impl BybitVenue {
    /// Spot, against `https://api.bybit.com`.
    #[must_use]
    pub fn spot() -> Self {
        Self {
            rest_url: "https://api.bybit.com".to_string(),
            category: "spot".to_string(),
        }
    }
}

impl Venue for BybitVenue {
    fn name(&self) -> &'static str {
        "bybit"
    }

    fn rest_url(&self) -> &str {
        &self.rest_url
    }

    fn klines_path(&self) -> &'static str {
        "/v5/market/kline"
    }

    fn interval(&self, timeframe: Timeframe) -> Option<&'static str> {
        // Bybit spells an interval as a number of MINUTES, except the two
        // calendar resolutions which are single letters. There is no `1m`/`1h`
        // spelling, so passing through `interval_for` would send `1m` and get a
        // 400 that names the parameter but not the reason.
        Some(match timeframe {
            Timeframe::M1 => "1",
            Timeframe::M5 => "5",
            Timeframe::M15 => "15",
            Timeframe::H1 => "60",
            Timeframe::H4 => "240",
            Timeframe::D1 => "D",
            Timeframe::W1 => "W",
        })
    }

    fn columns(&self) -> Columns {
        Columns {
            open_time: 0,
            open: 1,
            high: 2,
            low: 3,
            close: 4,
            volume: 5,
            // Bybit v5 returns seven columns and the seventh is *turnover*, a
            // quote-asset figure. There is no taker-buy base volume, so there is
            // no order-flow split to be had from a kline -- see the module doc.
            taker_buy_base: None,
        }
    }

    fn newest_first(&self) -> bool {
        // "Sort in reverse by startTime" -- Bybit's own words. The reverse is
        // done once, in `parse_klines`, so every consumer sees ascending order.
        true
    }

    fn wraps_in_envelope(&self) -> bool {
        true
    }

    fn page_limit(&self) -> usize {
        1000
    }

    fn start_param(&self) -> &'static str {
        "start"
    }

    fn end_param(&self) -> &'static str {
        "end"
    }

    fn end_is_inclusive(&self) -> bool {
        // Bybit does not document `end` as inclusive, and treating it as
        // exclusive is the safe reading: it can only ever *narrow* the window,
        // while wrongly applying the `-1ms` would widen it and double-count.
        false
    }

    fn short_page_means_done(&self) -> bool {
        // A descending venue is walked by moving `end` backwards, and a short
        // page there means the window was clipped by the *end* boundary rather
        // than that history ran out. Treating it as exhaustion would stop the
        // walk early and leave the chart missing its older half.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A Binance kline row, copied from the documented shape: numbers for the
    /// prices and the open time, strings for the trailing aggregates.
    fn binance_row() -> serde_json::Value {
        json!([
            1499040000000i64,
            "0.01634790",
            "0.80000000",
            "0.01575800",
            "0.01577100",
            "148976.11427815",
            1499644799999i64,
            "2434.19055334",
            308,
            "1756.87402397",
            "28.46694368",
            "0"
        ])
    }

    /// A Bybit v5 row, copied from the documented payload: **every** field a
    /// string, and only seven of them.
    fn bybit_row(open_ms: i64) -> serde_json::Value {
        json!([
            open_ms.to_string(),
            "17071",
            "17073",
            "17027",
            "17055.5",
            "268611",
            "15.74462667"
        ])
    }

    #[test]
    fn binance_klines_parse_with_their_order_flow_split() {
        let venue = BinanceVenue::new();
        let rows = venue.parse_klines(&json!([binance_row()])).expect("parses");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.open_time_ms, 1_499_040_000_000);
        assert!((row.open - 0.016_347_9).abs() < 1e-12);
        assert_eq!(
            row.taker_buy_base,
            Some(1_756.874_023_97),
            "Binance supplies the taker-buy column and it must survive"
        );
        assert!(row.has_order_flow_split());
    }

    #[test]
    fn bybit_klines_parse_even_though_every_field_is_a_string() {
        // `as_i64()` on `"1670608800000"` is `None`. A decoder that used it
        // would report a missing open time on a payload that has one -- the
        // exact failure mode of pointing a Binance-shaped client at Bybit.
        let venue = BybitVenue::spot();
        let rows = venue
            .parse_klines(&json!({ "result": { "list": [bybit_row(1_670_608_800_000)] } }))
            .expect("parses");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].open_time_ms, 1_670_608_800_000);
        assert!((rows[0].close - 17_055.5).abs() < 1e-9);
        assert!((rows[0].volume - 268_611.0).abs() < 1e-9);
    }

    #[test]
    fn bybit_does_not_claim_an_order_flow_split_it_cannot_supply() {
        // The load-bearing honesty test. Bybit's seventh column is turnover, so
        // a decoder that read it as taker-buy would report a buy/sell split that
        // is pure fiction -- and a delta indicator would chart it as fact.
        let venue = BybitVenue::spot();
        let rows = venue
            .parse_klines(&json!({ "result": { "list": [bybit_row(1)] } }))
            .expect("parses");
        assert_eq!(
            rows[0].taker_buy_base, None,
            "Bybit returns turnover, not taker-buy volume; a number here would be invented"
        );
        assert!(!rows[0].has_order_flow_split());
    }

    #[test]
    fn bybit_rows_are_reversed_into_ascending_order() {
        // The venue documents "sort in reverse by startTime". Downstream code
        // walks forwards, and `merge` assumes ascending -- so an unreversed page
        // is a chart drawn backwards, which reads as a market crash.
        let venue = BybitVenue::spot();
        let rows = venue
            .parse_klines(&json!({
                "result": { "list": [bybit_row(3000), bybit_row(2000), bybit_row(1000)] }
            }))
            .expect("parses");
        let times: Vec<i64> = rows.iter().map(|r| r.open_time_ms).collect();
        assert_eq!(times, vec![1000, 2000, 3000]);
    }

    #[test]
    fn a_binance_page_is_left_in_the_order_it_arrived() {
        // The control for the test above: Binance is already ascending, so a
        // reversal applied to every venue would put *its* bars backwards.
        let venue = BinanceVenue::new();
        let mut first = binance_row();
        first[0] = json!(1000);
        let mut second = binance_row();
        second[0] = json!(2000);

        let rows = venue.parse_klines(&json!([first, second])).expect("parses");
        let times: Vec<i64> = rows.iter().map(|r| r.open_time_ms).collect();
        assert_eq!(times, vec![1000, 2000]);
    }

    #[test]
    fn every_interval_is_spelled_the_way_its_own_venue_spells_it() {
        let binance = BinanceVenue::new();
        let bybit = BybitVenue::spot();

        // Two different spellings of the same resolution, and neither is a
        // rename of the other -- this is the drift a shared string would cause.
        assert_eq!(binance.interval(Timeframe::M1), Some("1m"));
        assert_eq!(bybit.interval(Timeframe::M1), Some("1"));
        assert_eq!(binance.interval(Timeframe::H1), Some("1h"));
        assert_eq!(bybit.interval(Timeframe::H1), Some("60"));
        assert_eq!(binance.interval(Timeframe::W1), Some("1w"));
        assert_eq!(bybit.interval(Timeframe::W1), Some("W"));
    }

    #[test]
    fn every_timeframe_the_platform_has_is_served_by_both_venues() {
        // `1w` in particular: it is the resolution this platform most recently
        // learned end to end, and a per-venue interval map is exactly where it
        // would be forgotten. Asked of `Timeframe::ALL` rather than a hand-typed
        // list, so a future variant fails this test instead of shipping a
        // resolution one venue cannot serve.
        let binance = BinanceVenue::new();
        let bybit = BybitVenue::spot();
        for timeframe in Timeframe::ALL {
            assert!(
                binance.interval(timeframe).is_some(),
                "binance cannot spell {timeframe}"
            );
            assert!(
                bybit.interval(timeframe).is_some(),
                "bybit cannot spell {timeframe}"
            );
        }
    }

    #[test]
    fn a_missing_envelope_is_a_named_refusal_not_an_empty_list() {
        // An empty `Vec` here would be indistinguishable from "this window has
        // no bars", so a venue that changed its envelope would silently answer
        // "no data" for every symbol.
        let venue = BybitVenue::spot();
        let error = venue
            .parse_klines(&json!({ "retCode": 0, "result": {} }))
            .expect_err("must refuse");
        let message = error.to_string();
        assert!(message.contains("result.list"), "{message}");
        assert!(message.contains("bybit"), "{message}");
    }

    #[test]
    fn a_short_row_is_refused_with_both_widths_named() {
        let venue = BinanceVenue::new();
        // Binance's taker-buy column is 9, so six values is short of it. Both
        // numbers have to appear: "6" says what arrived and "10" says what the
        // column map needed. Without the second, a venue with a genuinely
        // narrower row (Bybit's is seven) reads as a corrupt payload rather than
        // as a client applying the wrong map.
        let error = venue
            .parse_klines(&json!([["1", "2", "3", "4", "5", "6"]]))
            .expect_err("must refuse");
        let message = error.to_string();
        assert!(message.contains("6 fields"), "{message}");
        assert!(message.contains("10"), "{message}");
        assert!(message.contains("expected at least"), "{message}");
    }

    #[test]
    fn a_row_that_is_wide_enough_for_bybit_is_refused_by_binance() {
        // The two column maps are not interchangeable, which is the entire
        // reason they are per-venue data rather than one shared constant: a
        // seven-column Bybit row parsed under Binance's map would read turnover
        // as taker-buy volume and report a buy/sell split that does not exist.
        let venue = BinanceVenue::new();
        let bybit_shaped = json!([["1", "2", "3", "4", "5", "6", "7"]]);
        assert!(
            venue.parse_klines(&bybit_shaped).is_err(),
            "a 7-column row must not silently satisfy Binance's 10-column map"
        );
    }

    #[test]
    fn the_two_venues_disagree_about_whether_end_is_inclusive() {
        // A `[from, to)` window must send `to - 1ms` to Binance and `to` to
        // Bybit. Applying the `-1` to both is harmless; applying *neither* to
        // Binance returns the first candle of the next window, which is then
        // double-counted when ranges are walked back to back.
        assert!(BinanceVenue::new().end_is_inclusive());
        assert!(!BybitVenue::spot().end_is_inclusive());
    }

    #[test]
    fn a_descending_venue_does_not_stop_on_a_short_page() {
        // A short page means "the window was clipped", not "history ran out",
        // when pages are walked backwards. Stopping early leaves a chart
        // missing its older half and looking entirely healthy.
        assert!(BinanceVenue::new().short_page_means_done());
        assert!(!BybitVenue::spot().short_page_means_done());
    }

    #[test]
    fn bybit_is_asked_for_spot_unless_told_otherwise() {
        // Bybit's own default is `linear` -- a perpetual. A spot chart that
        // silently received perpetual candles would be a different instrument.
        assert_eq!(BybitVenue::spot().category, "spot");
    }

    #[test]
    fn the_venues_use_different_paths() {
        // Stated explicitly because it is the first thing a naive "venue
        // adapter" gets wrong: a shared path turns a second venue into a 404
        // reported as an empty window.
        assert_eq!(BinanceVenue::new().klines_path(), "/api/v3/klines");
        assert_eq!(BybitVenue::spot().klines_path(), "/v5/market/kline");
    }
}
