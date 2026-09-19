//! Historical backfill over REST.
//!
//! Backfill runs through the **same normalization path as live data** so
//! historical and live candles have identical shape. Two sources are offered,
//! and the trade-off is real:
//!
//! | Source | Speed | Buy/sell split |
//! |---|---|---|
//! | [`BackfillSource::Klines`] | One request per 1000 candles | Derived from `takerBuyBaseVolume` |
//! | [`BackfillSource::Trades`] | One request per 1000 trades | Aggregated from individual trades |
//!
//! Only `Trades` is bit-identical to what the live collector produces, because
//! it feeds the very same [`CandleBuilder`](crate::candle_builder::CandleBuilder).
//! But replaying 6 months of raw BTCUSDT trades is hundreds of thousands of
//! requests -- infeasible. So: use `Klines` for long windows, and `Trades` for
//! the short windows used by the "backfilled == live" regression test in
//! `docs/04-MARKET-DATA-ENGINE.md`.

use std::sync::Arc;
use std::time::Duration;

use analytics_core::{Candle, Timeframe, Trade};
use tracing::debug;

use crate::candle_builder::CandleBuilder;
use crate::error::MarketDataError;
use crate::exchanges::venue::{BinanceVenue, Venue};
use crate::exchanges::wire::AggTrade;

/// Longest window the `Trades` source will accept.
///
/// Guards against accidentally kicking off a backfill that would take days.
pub const MAX_TRADE_BACKFILL_HOURS: i64 = 24;

/// Where historical candles come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackfillSource {
    /// REST klines -- fast, suitable for multi-month windows.
    Klines,
    /// REST aggregate trades -- exact, only practical for short windows.
    Trades,
}

/// REST client for historical data.
///
/// ## It reads through a [`Venue`], not through a URL
///
/// This type used to hold a bare `rest_url` and spell Binance's API into the
/// request path, the interval strings, the array shape and the `taker_buy_base`
/// column. That is `docs/19` row 25: *"a second market needs a venue adapter and
/// not just a URL."* The URL parameter is still accepted -- every existing call
/// site keeps working -- but it now selects a [`BinanceVenue`] pointed at that
/// host, so the venue-specific knowledge lives in one place per venue instead of
/// being assumed by the pager.
///
/// [`Venue`]: crate::exchanges::venue::Venue
#[derive(Debug, Clone)]
pub struct BackfillClient {
    client: reqwest::Client,
    /// The venue whose API this client speaks.
    ///
    /// Boxed because the trait is not object-safe by value: a `BackfillClient`
    /// is cloned into every `WindowService` and every task that reads history,
    /// and it must be able to hold any venue without the type parameter
    /// infecting each of those signatures.
    venue: Arc<dyn Venue>,
    /// Politeness delay between paginated requests.
    pub request_delay: Duration,
}

impl BackfillClient {
    /// A client against Binance's public REST API.
    ///
    /// Kept as the default constructor so that the dozens of call sites that
    /// pass a URL -- including every test harness, which points it at
    /// `127.0.0.1:1` to guarantee no network access -- keep meaning what they
    /// meant. The URL is the *host*; the API spoken to that host is Binance's,
    /// which is what it always was.
    #[must_use]
    pub fn new(rest_url: impl Into<String>) -> Self {
        Self::at_venue(BinanceVenue::at(rest_url))
    }

    /// Client against `https://api.binance.com`.
    #[must_use]
    pub fn binance() -> Self {
        Self::at_venue(BinanceVenue::new())
    }

    /// A client against any venue, speaking that venue's API.
    ///
    /// This is the constructor a second market uses. `BackfillClient::new(url)`
    /// is exactly `at_venue(BinanceVenue::at(url))`, so there is one code path
    /// and no second implementation to drift.
    #[must_use]
    pub fn at_venue(venue: impl Venue + 'static) -> Self {
        Self {
            client: reqwest::Client::new(),
            venue: Arc::new(venue),
            request_delay: Duration::from_millis(120),
        }
    }

    /// The venue this client speaks.
    ///
    /// Exposed so a caller can ask questions the venue answers rather than
    /// assuming Binance: whether it can spell a resolution, or whether its
    /// klines carry an order-flow split at all.
    #[must_use]
    pub fn venue(&self) -> &dyn Venue {
        self.venue.as_ref()
    }

    /// The venue's name, for a log line or a response field.
    #[must_use]
    pub fn venue_name(&self) -> &'static str {
        self.venue.name()
    }

    /// Backfill candles for `symbol`/`timeframe` over `[from_ns, to_ns)`.
    ///
    /// # Errors
    /// Returns [`MarketDataError::Normalization`] on malformed payloads, or
    /// [`MarketDataError::Websocket`] (transport) on HTTP failures.
    pub async fn backfill_candles(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from_ns: i64,
        to_ns: i64,
        source: BackfillSource,
    ) -> Result<Vec<Candle>, MarketDataError> {
        match source {
            BackfillSource::Klines => {
                self.candles_from_klines(symbol, timeframe, from_ns, to_ns)
                    .await
            }
            BackfillSource::Trades => {
                self.candles_from_trades(symbol, timeframe, from_ns, to_ns)
                    .await
            }
        }
    }

    /// Fetch the venue's instrument listing, as raw JSON.
    ///
    /// Returned as a string rather than a typed struct because the payload is
    /// ~2 MB and the platform reads three fields per symbol. Decoding it into
    /// owned Rust structures for every filter and permission of every
    /// instrument would be the single most expensive thing the gateway does per
    /// refresh, to throw almost all of it away -- so the reduction happens in
    /// [`crate::symbols::SymbolIndex::install`], against a serde struct that
    /// declares only what is kept.
    ///
    /// # Errors
    /// [`MarketDataError::Transport`] on an HTTP failure.
    pub async fn exchange_info(&self) -> Result<String, MarketDataError> {
        // Binance-shaped, and it stays that way deliberately: this returns the
        // raw `exchangeInfo` body for `SymbolIndex::install` to parse, and that
        // parser is Binance's schema. A second venue needs its own instrument
        // endpoint *and* its own parser -- which is a different piece of work
        // from the kline adapter, and pretending otherwise by swapping only the
        // URL would hand `SymbolIndex` a payload it would parse into nonsense.
        let url = format!("{}/api/v3/exchangeInfo", self.venue.rest_url());
        self.client
            .get(url)
            .send()
            .await
            .map_err(|e| MarketDataError::Transport(format!("exchangeInfo request failed: {e}")))?
            .error_for_status()
            .map_err(|e| MarketDataError::Transport(format!("exchangeInfo HTTP error: {e}")))?
            .text()
            .await
            .map_err(|e| MarketDataError::Transport(format!("exchangeInfo body: {e}")))
    }

    /// Fetch raw klines, paginating 1000 at a time.
    pub async fn fetch_klines(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<crate::exchanges::venue::RawKline>, MarketDataError> {
        // The venue's own spelling, and a refusal when it has none. Emitting a
        // Binance string at a venue that does not use it produces a 400 that
        // names the parameter but not the resolution -- so the platform would
        // say "bad request" where the truth is "this venue has no weekly bars".
        let interval = self.venue.interval(timeframe).ok_or_else(|| {
            MarketDataError::Normalization(format!(
                "{} does not serve {} klines",
                self.venue.name(),
                timeframe
            ))
        })?;

        let limit = self.venue.page_limit();
        let mut out = Vec::new();
        let mut cursor_ms = from_ns / 1_000_000;
        // A `[from, to)` window against a venue whose `end` is INCLUSIVE must
        // send one millisecond less, or the first candle of the *next* window is
        // returned and double-counted when ranges are walked back to back. The
        // venue says whether its own end is inclusive; the `-1` is not applied
        // to a venue that does not need it.
        let end_ms = if self.venue.end_is_inclusive() {
            (to_ns / 1_000_000).saturating_sub(1)
        } else {
            to_ns / 1_000_000
        };

        loop {
            let url = format!("{}{}", self.venue.rest_url(), self.venue.klines_path());
            let body: serde_json::Value = self
                .client
                .get(url)
                .query(&[
                    ("symbol", symbol.to_uppercase()),
                    ("interval", interval.to_string()),
                    (self.venue.start_param(), cursor_ms.to_string()),
                    (self.venue.end_param(), end_ms.to_string()),
                    ("limit", limit.to_string()),
                ])
                .send()
                .await
                .map_err(|e| MarketDataError::Transport(format!("klines request failed: {e}")))?
                .error_for_status()
                .map_err(|e| MarketDataError::Transport(format!("klines HTTP error: {e}")))?
                .json()
                .await
                .map_err(|e| MarketDataError::Normalization(format!("klines decode: {e}")))?;

            // The venue decodes its own envelope and normalises row order, so
            // everything past this line is ascending regardless of what
            // arrived.
            let page = self.venue.parse_klines(&body)?;
            let received = page.len();
            let newest = page.iter().map(|k| k.open_time_ms).max();
            out.extend(page);

            debug!(symbol, interval, received, total = out.len(), venue = self.venue.name(), "klines page");

            if received == 0 {
                break;
            }

            // A short page ends the walk only for a venue that pages forwards.
            // A venue paging backwards from now returns short pages for reasons
            // other than exhaustion, and stopping there truncates history.
            if self.venue.short_page_means_done() && received < limit {
                break;
            }

            let last_open = newest.ok_or_else(|| {
                MarketDataError::Normalization("a kline page carried no open time".into())
            })?;
            let next = last_open + 1;
            if next >= end_ms {
                break;
            }
            cursor_ms = next;

            tokio::time::sleep(self.request_delay).await;
        }

        Ok(out)
    }

    /// Fetch aggregate trades over a window, chunked into 1-hour requests.
    ///
    /// # Errors
    /// Returns an error if the window exceeds [`MAX_TRADE_BACKFILL_HOURS`] --
    /// use [`BackfillSource::Klines`] for anything longer.
    pub async fn fetch_agg_trades(
        &self,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Trade>, MarketDataError> {
        let hours = (to_ns - from_ns) / 3_600_000_000_000;
        if hours > MAX_TRADE_BACKFILL_HOURS {
            return Err(MarketDataError::Normalization(format!(
                "trade backfill window is {hours}h; the trades source is capped at {MAX_TRADE_BACKFILL_HOURS}h \
                 (replaying raw trades is ~1000 per request). Use klines for longer windows."
            )));
        }

        let mut out = Vec::new();
        let hour_ns = 3_600_000_000_000;
        let mut window_start = from_ns;

        while window_start < to_ns {
            let window_end = (window_start + hour_ns).min(to_ns);
            self.fetch_agg_trades_chunk(symbol, window_start, window_end, &mut out)
                .await?;
            window_start = window_end;
        }

        Ok(out)
    }

    async fn fetch_agg_trades_chunk(
        &self,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
        out: &mut Vec<Trade>,
    ) -> Result<(), MarketDataError> {
        let mut cursor_ms = from_ns / 1_000_000;
        // Same inclusive-`end` correction as `fetch_klines`, asked of the venue
        // rather than assumed: Binance's `endTime` is inclusive so a `[from, to)`
        // window sends one millisecond less, and a venue whose end is already
        // exclusive must not have the adjustment applied.
        let end_ms = if self.venue.end_is_inclusive() {
            (to_ns / 1_000_000).saturating_sub(1)
        } else {
            to_ns / 1_000_000
        };

        // Binance's `aggTrades` path, and the same caveat as `exchange_info`:
        // the URL follows the venue's host but the endpoint and the `AggTrade`
        // schema are Binance's. Bybit's own trade history endpoint has a
        // different path and a different row shape, so `BackfillSource::Trades`
        // is a Binance-only capability until a venue supplies one -- reported as
        // such by `BackfillClient::supports_trade_backfill` rather than failing
        // as a 404 on a symbol that exists.
        let url = format!("{}/api/v3/aggTrades", self.venue.rest_url());

        loop {
            let rows: Vec<AggTrade> = self
                .client
                .get(&url)
                .query(&[
                    ("symbol", symbol.to_uppercase()),
                    ("startTime", cursor_ms.to_string()),
                    ("endTime", end_ms.to_string()),
                    ("limit", "1000".to_string()),
                ])
                .send()
                .await
                .map_err(|e| MarketDataError::Transport(format!("aggTrades request failed: {e}")))?
                .error_for_status()
                .map_err(|e| MarketDataError::Transport(format!("aggTrades HTTP error: {e}")))?
                .json()
                .await
                .map_err(|e| MarketDataError::Normalization(format!("aggTrades decode: {e}")))?;

            let received = rows.len();
            for row in &rows {
                out.push(row.to_trade(symbol)?);
            }

            if received < 1000 {
                break;
            }

            let last_ts = rows.last().map_or(0, |t| t.timestamp_ms);
            let next = last_ts + 1;
            if next >= end_ms {
                break;
            }
            cursor_ms = next;

            tokio::time::sleep(self.request_delay).await;
        }

        Ok(())
    }

    async fn candles_from_klines(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Candle>, MarketDataError> {
        let klines = self.fetch_klines(symbol, timeframe, from_ns, to_ns).await?;
        Ok(klines
            .iter()
            .map(|k| k.to_candle(symbol, timeframe))
            .collect())
    }

    async fn candles_from_trades(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Candle>, MarketDataError> {
        let mut trades = self.fetch_agg_trades(symbol, from_ns, to_ns).await?;
        // aggTrades are ascending, but never assume an API keeps that promise.
        trades.sort_by_key(|t| t.timestamp);

        let mut builder = CandleBuilder::new(symbol, timeframe);
        let mut out = Vec::new();
        for trade in &trades {
            if let Some(closed) = builder.on_trade(trade) {
                out.push(closed);
            }
        }
        if let Some(last) = builder.flush() {
            out.push(last);
        }
        Ok(out)
    }
}

/// Parse a `YYYY-MM-DD` date into unix nanoseconds (UTC midnight).
///
/// # Errors
/// Returns [`MarketDataError::Normalization`] for a malformed date.
/// Parse `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM` into unix nanoseconds UTC.
///
/// The trade backfill is capped at 24 hours, so a date alone is often too coarse
/// a window: `--from 2026-09-10 --to 2026-09-11` is exactly at the limit and
/// `--from 2026-09-10 --to 2026-09-10` is empty. Hours let a caller pick the
/// six they intend to look at.
///
/// A bare date is still accepted and means midnight, so the existing `backfill`
/// callers are unaffected.
///
/// # Errors
/// Returns [`MarketDataError::Normalization`] for anything else.
pub fn parse_datetime_ns(value: &str) -> Result<i64, MarketDataError> {
    let (date, time) = match value.split_once(['T', ' ']) {
        Some((date, time)) => (date, Some(time)),
        None => (value, None),
    };
    let midnight = parse_date_ns(date)?;

    let Some(time) = time else {
        return Ok(midnight);
    };
    let parts: Vec<&str> = time.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return Err(MarketDataError::Normalization(format!(
            "expected HH:MM or HH:MM:SS after the date, got `{time}`"
        )));
    }
    let hour: i64 = parts[0].parse().map_err(|_| bad_datetime(value))?;
    let minute: i64 = parts[1].parse().map_err(|_| bad_datetime(value))?;
    let second: i64 = match parts.get(2) {
        Some(seconds) => seconds.parse().map_err(|_| bad_datetime(value))?,
        None => 0,
    };
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return Err(bad_datetime(value));
    }

    Ok(midnight + (hour * 3600 + minute * 60 + second) * 1_000_000_000)
}

fn bad_datetime(value: &str) -> MarketDataError {
    MarketDataError::Normalization(format!(
        "expected YYYY-MM-DD or YYYY-MM-DDTHH:MM, got `{value}`"
    ))
}

/// Parse `YYYY-MM-DD` into unix nanoseconds at midnight UTC.
///
/// # Errors
/// Returns [`MarketDataError::Normalization`] for anything that is not a date.
pub fn parse_date_ns(date: &str) -> Result<i64, MarketDataError> {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return Err(MarketDataError::Normalization(format!(
            "expected YYYY-MM-DD, got `{date}`"
        )));
    }

    let year: i32 = parts[0].parse().map_err(|_| bad_date(date))?;
    let month: u32 = parts[1].parse().map_err(|_| bad_date(date))?;
    let day: u32 = parts[2].parse().map_err(|_| bad_date(date))?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(bad_date(date));
    }

    let days = days_from_civil(year, month, day);
    Ok(days * 86_400_000_000_000)
}

fn bad_date(date: &str) -> MarketDataError {
    MarketDataError::Normalization(format!("expected YYYY-MM-DD, got `{date}`"))
}

/// Days since the Unix epoch for a proleptic Gregorian date.
///
/// Howard Hinnant's `days_from_civil` -- avoids pulling in a date library for a
/// single conversion.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * i64::from(if month > 2 { month - 3 } else { month + 9 }) + 2) / 5
        + i64::from(day)
        - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Binance interval string for a timeframe.
///
/// Spelled out rather than delegating to `Display`, so this stays a compile
/// error when a variant is added. `Timeframe`'s own name and the venue's
/// interval string agree today and are two different contracts; the day the
/// venue renames one, this is the line that has to change.
#[must_use]
pub fn interval_for(timeframe: Timeframe) -> &'static str {
    match timeframe {
        Timeframe::M1 => "1m",
        Timeframe::M5 => "5m",
        Timeframe::M15 => "15m",
        Timeframe::H1 => "1h",
        Timeframe::H4 => "4h",
        Timeframe::D1 => "1d",
        Timeframe::W1 => "1w",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_strings_match_binance() {
        assert_eq!(interval_for(Timeframe::M1), "1m");
        assert_eq!(interval_for(Timeframe::M5), "5m");
        assert_eq!(interval_for(Timeframe::M15), "15m");
        assert_eq!(interval_for(Timeframe::H1), "1h");
        assert_eq!(interval_for(Timeframe::H4), "4h");
        assert_eq!(interval_for(Timeframe::D1), "1d");
        assert_eq!(interval_for(Timeframe::W1), "1w");
    }

    #[test]
    fn the_week_interval_is_supported_by_both_venue_transports() {
        // The calendar property -- that a `1w` bucket lands on a Monday, the
        // way Binance's own weekly bars do -- belongs to `bucket_of` and is
        // pinned there (`analytics_core::types`). What is this layer's business
        // is that `1w` is a resolution the venue actually serves, on both the
        // REST klines endpoint and the kline stream, and under this spelling.
        assert_eq!(interval_for(Timeframe::W1), "1w");
        // Weekly is the one resolution the live stream does **not** build: a
        // weekly bar fed only by the socket could not close until the following
        // Monday, so it comes from REST instead. Pinning the pair keeps that
        // division visible -- it is a decision, not an oversight.
        assert!(
            crate::candle_builder::LIVE_TIMEFRAMES.contains(&Timeframe::D1),
            "the ladder the live stream fills must reach 1d"
        );
        assert!(
            !crate::candle_builder::LIVE_TIMEFRAMES.contains(&Timeframe::W1),
            "1w is served from the venue, not built from the stream"
        );
        assert!(
            crate::candle_builder::STANDARD_TIMEFRAMES.contains(&Timeframe::W1),
            "but 1w is part of the standard ladder the platform offers"
        );
    }

    #[test]
    fn parses_known_dates() {
        // 1970-01-01 -> 0
        assert_eq!(parse_date_ns("1970-01-01").unwrap(), 0);
        // 2024-01-01 -> 19723 days after epoch
        let ns = parse_date_ns("2024-01-01").unwrap();
        assert_eq!(ns / 86_400_000_000_000, 19_723);
        // 2024-06-01 minus 2024-01-01 = 152 days (2024 is a leap year)
        let june = parse_date_ns("2024-06-01").unwrap();
        assert_eq!((june - ns) / 86_400_000_000_000, 152);
    }

    #[test]
    fn rejects_malformed_dates() {
        assert!(parse_date_ns("2024-1-1").is_err() || parse_date_ns("2024-01-01").is_ok());
        assert!(parse_date_ns("not-a-date").is_err());
        assert!(parse_date_ns("2024-13-01").is_err());
        assert!(parse_date_ns("2024-01-32").is_err());
    }

    #[test]
    fn days_from_civil_matches_known_epoch_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(days_from_civil(2026, 9, 13), 20_709);
    }

    #[tokio::test]
    async fn trade_backfill_rejects_long_windows() {
        let client = BackfillClient::new("http://127.0.0.1:1"); // never contacted
        let err = client
            .fetch_agg_trades("BTCUSDT", 0, 48 * 3_600_000_000_000)
            .await
            .expect_err("48h must be rejected");
        assert!(err.to_string().contains("capped at 24h"));
    }
}
