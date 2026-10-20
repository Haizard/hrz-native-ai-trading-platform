//! The Deriv data plane: candles from the Deriv WebSocket API.
//!
//! Deriv speaks one WebSocket JSON dialect for everything (`wss://ws.derivws
//! .com/websockets/v3`, free `app_id`). One-shot history is a
//! `ticks_history` request with `style: "candles"` and a granularity in
//! SECONDS; the same call with `"subscribe": 1` becomes the live candle
//! stream (phase 2.5). Everything in this file is shaped so the rest of the
//! platform cannot tell Deriv apart from any other venue: [`Candle`] in,
//! [`Candle`] out, and the symbol map is the only Deriv-specific knowledge.
//!
//! No API key is required for market data; `DERIV_APP_ID` overrides the
//! public demo id (`1089`) when the platform's own registration lands.

use analytics_core::types::{Candle, Timeframe};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::tungstenite::Message;

use crate::MarketDataError;

/// The current public market-data endpoint. The legacy
/// `ws.derivws.com/websockets/v3` is decommissioned -- Cloudflare returns a
/// bare 520 for it (origin unreachable), which is what every client hit
/// before the move. Public data needs no auth beyond the app id.
pub const DERIV_WS_URL: &str = "wss://api.derivws.com/trading/v1/options/ws/public";

/// The public demo application id. Registered apps substitute their own via
/// `DERIV_APP_ID` -- and MUST before any order ever carries an attribution
/// tag (that is the business model, not a nicety).
pub const DEFAULT_APP_ID: &str = "1089";

/// One instrument the platform serves from Deriv: our display symbol, the
/// Deriv code, and the family (used for feature notes later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivInstrument {
    /// The platform-facing symbol (`Volatility 75`, `Boom 1000`).
    pub symbol: &'static str,
    /// The Deriv API code (`R_75`, `BOOM1000`).
    pub code: &'static str,
    /// `synthetic` or `forex-cfd` -- the broker-product id from `broker.rs`.
    pub product: &'static str,
}

/// The Deriv instruments phase 2 serves. Synthetics first (the user demand),
/// the liquid majors' CFDs beside them.
pub const DERIV_INSTRUMENTS: &[DerivInstrument] = &[
    DerivInstrument { symbol: "Volatility 10", code: "R_10", product: "synthetic" },
    DerivInstrument { symbol: "Volatility 25", code: "R_25", product: "synthetic" },
    DerivInstrument { symbol: "Volatility 50", code: "R_50", product: "synthetic" },
    DerivInstrument { symbol: "Volatility 75", code: "R_75", product: "synthetic" },
    DerivInstrument { symbol: "Volatility 100", code: "R_100", product: "synthetic" },
    DerivInstrument { symbol: "Volatility 10 (1s)", code: "1HZ10V", product: "synthetic" },
    DerivInstrument { symbol: "Volatility 25 (1s)", code: "1HZ25V", product: "synthetic" },
    DerivInstrument { symbol: "Volatility 50 (1s)", code: "1HZ50V", product: "synthetic" },
    DerivInstrument { symbol: "Volatility 75 (1s)", code: "1HZ75V", product: "synthetic" },
    DerivInstrument { symbol: "Volatility 100 (1s)", code: "1HZ100V", product: "synthetic" },
    DerivInstrument { symbol: "Boom 500", code: "BOOM500", product: "synthetic" },
    DerivInstrument { symbol: "Boom 1000", code: "BOOM1000", product: "synthetic" },
    DerivInstrument { symbol: "Crash 500", code: "CRASH500", product: "synthetic" },
    DerivInstrument { symbol: "Crash 1000", code: "CRASH1000", product: "synthetic" },
    DerivInstrument { symbol: "Step Index", code: "stpRNG", product: "synthetic" },
    DerivInstrument { symbol: "Jump 75", code: "JD75", product: "synthetic" },
    DerivInstrument { symbol: "Jump 100", code: "JD100", product: "synthetic" },
    DerivInstrument { symbol: "EURUSD", code: "frxEURUSD", product: "forex-cfd" },
    DerivInstrument { symbol: "GBPUSD", code: "frxGBPUSD", product: "forex-cfd" },
    DerivInstrument { symbol: "USDJPY", code: "frxUSDJPY", product: "forex-cfd" },
    DerivInstrument { symbol: "Gold", code: "frxXAUUSD", product: "forex-cfd" },
];

/// Our display symbol -> the Deriv code, when served.
#[must_use]
pub fn code_for(symbol: &str) -> Option<&'static str> {
    DERIV_INSTRUMENTS
        .iter()
        .find(|i| i.symbol.eq_ignore_ascii_case(symbol))
        .map(|i| i.code)
}

/// The Deriv code -> our display symbol.
#[must_use]
pub fn symbol_for(code: &str) -> Option<&'static str> {
    DERIV_INSTRUMENTS
        .iter()
        .find(|i| i.code == code)
        .map(|i| i.symbol)
}

/// The Deriv granularity for one of our timeframes (seconds), or none when
/// the venue has no such resolution. The current API documents exactly this
/// set -- note the gaps: no 6h, 12h, 3d or 1w candles from the venue
/// itself.
pub const DERIV_GRANULARITIES: &[u32] = &[
    60, 120, 180, 300, 600, 900, 1800, 3600, 7200, 14400, 28800, 86400,
];

/// The Deriv granularity for one of our timeframes, when the venue serves it
/// directly. 6h/12h/3d/1w return None -- the caller resamples those from a
/// finer granularity rather than inventing bars the venue does not have.
#[must_use]
pub fn granularity_for(tf: Timeframe) -> Option<u32> {
    let secs = u32::try_from(tf.secs()).ok()?;
    DERIV_GRANULARITIES.contains(&secs).then_some(secs)
}

/// One live tick from a `style: "ticks"` subscription.
#[derive(Debug, Deserialize)]
pub struct DerivTick {
    /// The quote (a price observation, NOT a trade).
    pub quote: f64,
    /// Tick time, unix seconds.
    pub epoch: i64,
    /// The Deriv symbol code (echoed).
    #[serde(default)]
    #[allow(dead_code)]
    pub symbol: Option<String>,
}

/// The envelope of a live subscription stream: ticks, the history batch,
/// candle updates, or an error.
#[derive(Debug, Deserialize)]
pub struct DerivStreamMsg {
    /// `"tick"`.
    pub tick: Option<DerivTick>,
    /// The initial history batch (`{"prices": [...], "times": [...]}`).
    pub history: Option<DerivTickHistory>,
    /// Any error envelope.
    pub error: Option<DerivError>,
}

/// The tick-history batch (`style: "ticks"`).
#[derive(Debug, Deserialize)]
pub struct DerivTickHistory {
    /// Prices, oldest first.
    pub prices: Vec<f64>,
    /// Unix seconds, aligned with `prices`.
    pub times: Vec<i64>,
}

/// Convert one tick into the builder's internal trade shape: zero quantity
/// (a quote has no volume) and never anything the tape would mistake for a
/// real fill. This is the ONLY conversion a Deriv quote receives -- the tape
/// never sees it.
#[must_use]
pub fn tick_as_builder_trade(tick: &DerivTick, symbol: &str) -> analytics_core::types::Trade {
    analytics_core::types::Trade {
        symbol: symbol.to_string(),
        // Quotes have no venue trade id; the builder's late-trade rule works
        // off timestamps, so a stable zero is honest here.
        trade_id: 0,
        price: tick.quote,
        quantity: 0.0,
        timestamp: tick.epoch * 1_000_000_000,
        is_buyer_maker: false,
    }
}

/// A candles response row from `ticks_history`.
#[derive(Debug, Deserialize)]
struct DerivCandle {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    epoch: i64,
}

/// The envelope: `{"candles": [...], "msg_type": "candles"}` or an error.
#[derive(Debug, Deserialize)]
struct DerivResponse {
    candles: Option<Vec<DerivCandle>>,
    error: Option<DerivError>,
}

/// Deriv's error shape: `{"error": {"code": "...", "message": "..."}}`.
#[derive(Debug, Deserialize)]
struct DerivError {
    code: Option<String>,
    message: Option<String>,
}

/// The client: one-shot candle queries over short-lived WebSocket
/// connections. Deriv is polite about this shape; a persistent multiplexed
/// connection is the streaming phase, not the fetch one.
#[derive(Debug, Clone)]
pub struct DerivClient {
    app_id: String,
    ws_url: String,
}

impl DerivClient {
    /// From the environment: `DERIV_APP_ID`, falling back to the public demo
    /// id so data works before the platform's own registration lands.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            app_id: std::env::var("DERIV_APP_ID").unwrap_or_else(|_| DEFAULT_APP_ID.to_string()),
            ws_url: DERIV_WS_URL.to_string(),
        }
    }

    /// With an explicit endpoint (tests).
    #[must_use]
    pub fn new(app_id: impl Into<String>, ws_url: impl Into<String>) -> Self {
        Self { app_id: app_id.into(), ws_url: ws_url.into() }
    }

    /// Open a Deriv WebSocket (plain tokio-tungstenite: the current public
    /// endpoint does not fingerprint clients -- the old one's 520 was a dead
    /// origin, not a browser check).
    async fn connect(
        &self,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        MarketDataError,
    > {
        let url = format!("{}?app_id={}", self.ws_url, self.app_id);
        let (socket, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|e| MarketDataError::Normalization(format!("deriv connect failed: {e}")))?;
        Ok(socket)
    }

    /// The fetch granularity for a target timeframe: direct when the venue
    /// serves it, otherwise the finer granularity we resample up from
    /// (6h from 2h, 12h from 4h, 3d and 1w from 1d). Returns (fetch_tf,
    /// ratio).
    fn fetch_plan(tf: Timeframe) -> Option<(Timeframe, usize)> {
        if granularity_for(tf).is_some() {
            return Some((tf, 1));
        }
        match tf {
            Timeframe::H6 => Some((Timeframe::H2, 3)),
            Timeframe::H12 => Some((Timeframe::H4, 3)),
            Timeframe::D3 => Some((Timeframe::D1, 3)),
            Timeframe::W1 => Some((Timeframe::D1, 7)),
            _ => None,
        }
    }

    /// One page of candles, newest at the end: `ticks_history` with
    /// `style: "candles"`, `count` bars back from `end` (unix nanos; 0 =
    /// latest). Deriv volumes on synthetics are quote ticks, not trades, so
    /// every bar reports volume zero -- the honest number, and the reason
    /// order flow is refused on these products rather than estimated.
    ///
    /// # Errors
    /// [`MarketDataError::Normalization`] on a Deriv error reply, a closed
    /// socket, or a row that does not parse.
    pub async fn candles(
        &self,
        code: &str,
        tf: Timeframe,
        count: usize,
        end_ns: i64,
    ) -> Result<Vec<Candle>, MarketDataError> {
        let (fetch_tf, ratio) = Self::fetch_plan(tf).ok_or_else(|| {
            MarketDataError::Normalization(format!("deriv has no granularity for {tf}"))
        })?;
        let granularity = granularity_for(fetch_tf).expect("the plan only names direct granularities");
        let mut socket = self.connect().await?;
        // Resampling fetches `ratio` bars per output bar, padded for the
        // partial group at the window's edge.
        let fetch_count = if ratio > 1 {
            count.saturating_mul(ratio).saturating_add(ratio).clamp(1, 5000)
        } else {
            count.clamp(1, 5000)
        };
        let mut request = serde_json::json!({
            "ticks_history": code,
            "adjust_start_time": 1,
            "count": fetch_count,
            "granularity": granularity,
            "style": "candles",
        });
        // `end` is REQUIRED by the current schema (the legacy API defaulted
        // it); a string: "latest" or the epoch seconds.
        request["end"] = serde_json::json!(if end_ns > 0 {
            (end_ns / 1_000_000_000).to_string()
        } else {
            "latest".to_string()
        });
        socket
            .send(Message::Text(request.to_string().into()))
            .await
            .map_err(|e| MarketDataError::Normalization(format!("deriv send failed: {e}")))?;
        // Read until the candles envelope (pings and unrelated pushes first).
        let reply: DerivResponse = loop {
            let Some(frame) = socket.next().await else {
                return Err(MarketDataError::Normalization(
                    "deriv closed the socket before answering".to_string(),
                ));
            };
            let frame = frame.map_err(|e| {
                MarketDataError::Normalization(format!("deriv socket error: {e}"))
            })?;
            let Message::Text(text) = frame else { continue };
            let parsed: DerivResponse = serde_json::from_str(&text).map_err(|e| {
                MarketDataError::Normalization(format!("deriv reply did not parse: {e}"))
            })?;
            if parsed.candles.is_some() || parsed.error.is_some() {
                break parsed;
            }
        };
        if let Some(err) = reply.error {
            return Err(MarketDataError::Normalization(format!(
                "deriv {}: {}",
                err.code.unwrap_or_else(|| "error".to_string()),
                err.message.unwrap_or_else(|| "unknown".to_string())
            )));
        }
        let rows = reply.candles.unwrap_or_default();
        let display = symbol_for(code).unwrap_or(code).to_string();
        let fetched: Vec<Candle> = rows
            .into_iter()
            .map(|r| Candle {
                symbol: display.clone(),
                timeframe: fetch_tf,
                open_time: r.epoch * 1_000_000_000,
                open: r.open,
                high: r.high,
                low: r.low,
                close: r.close,
                volume: 0.0,
                buy_volume: 0.0,
                sell_volume: 0.0,
            })
            .collect();
        if ratio == 1 {
            return Ok(fetched);
        }
        Ok(analytics_core::resample::resample(&fetched, tf))
    }
}

/// What a successful `authorize` call tells us about a token: the account
/// it opens, in Deriv's own words.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DerivAuth {
    /// The account login id (e.g. `CR123456` or `VRTC...` for virtual).
    pub loginid: String,
    /// Account currency, when reported.
    #[serde(default)]
    pub currency: Option<String>,
    /// Whether the account is a virtual (demo) account, when reported.
    #[serde(default)]
    pub is_virtual: Option<f64>,
    /// The token's scopes (`read`, `trade`, ...), when reported.
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// The `authorize` envelope.
#[derive(Debug, Deserialize)]
struct DerivAuthResponse {
    authorize: Option<DerivAuthWire>,
    error: Option<DerivError>,
}

/// The wire shape (extra fields ignored).
#[derive(Debug, Deserialize)]
struct DerivAuthWire {
    loginid: String,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    is_virtual: Option<f64>,
    #[serde(default)]
    scopes: Vec<String>,
}

impl DerivClient {
    /// Subscribe to the live tick stream: `ticks_history` with
    /// `style: "ticks"` and `"subscribe": 1`. The venue sends the recent tick
    /// batch first (used to seed the candle builders so a reconnect does not
    /// restart the forming bars from zero), then streams one tick at a time.
    ///
    /// # Errors
    /// [`MarketDataError::Normalization`] on a subscription refusal or a
    /// transport failure before the batch arrives.
    pub async fn subscribe_ticks(
        &self,
        code: &str,
        history: usize,
    ) -> Result<
        (
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Vec<DerivTick>,
            Vec<DerivTick>,
        ),
        MarketDataError,
    > {
        let mut socket = self.connect().await?;
        let request = serde_json::json!({
            "ticks_history": code,
            "adjust_start_time": 1,
            "count": history.clamp(1, 5000),
            "end": "latest",
            "start": 1,
            "style": "ticks",
            "subscribe": 1,
        });
        socket
            .send(Message::Text(request.to_string().into()))
            .await
            .map_err(|e| MarketDataError::Normalization(format!("deriv subscribe failed: {e}")))?;
        // The batch must arrive before we call the stream live; live ticks
        // that race ahead of it are buffered and replayed after it.
        let mut early = Vec::new();
        let batch = loop {
            let Some(frame) = socket.next().await else {
                return Err(MarketDataError::Normalization(
                    "deriv closed the socket before the tick batch".to_string(),
                ));
            };
            let frame = frame.map_err(|e| {
                MarketDataError::Normalization(format!("deriv socket error: {e}"))
            })?;
            let Message::Text(text) = frame else { continue };
            let parsed: DerivStreamMsg = serde_json::from_str(&text).map_err(|e| {
                MarketDataError::Normalization(format!("deriv reply did not parse: {e}"))
            })?;
            if let Some(err) = parsed.error {
                return Err(MarketDataError::Normalization(format!(
                    "deriv {}: {}",
                    err.code.unwrap_or_else(|| "error".to_string()),
                    err.message.unwrap_or_else(|| "unknown".to_string())
                )));
            }
            if let Some(t) = parsed.tick {
                early.push(t);
            }
            if let Some(h) = parsed.history {
                break h
                    .times
                    .into_iter()
                    .zip(h.prices)
                    .map(|(epoch, quote)| DerivTick { quote, epoch, symbol: None })
                    .collect::<Vec<_>>();
            }
        };
        Ok((socket, batch, early))
    }

    /// The next live tick on an open subscription. Non-tick envelopes
    /// (heartbeats and friends) are skipped; a close or an error envelope ends
    /// the stream.
    ///
    /// # Errors
    /// [`MarketDataError::Normalization`] on a socket error, a venue error
    /// envelope, or a reply that does not parse.
    pub async fn next_tick(
        &self,
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Result<DerivTick, MarketDataError> {
        loop {
            let Some(frame) = socket.next().await else {
                return Err(MarketDataError::Normalization(
                    "deriv closed the tick stream".to_string(),
                ));
            };
            let frame = frame.map_err(|e| {
                MarketDataError::Normalization(format!("deriv socket error: {e}"))
            })?;
            let Message::Text(text) = frame else { continue };
            let parsed: DerivStreamMsg = serde_json::from_str(&text).map_err(|e| {
                MarketDataError::Normalization(format!("deriv reply did not parse: {e}"))
            })?;
            if let Some(err) = parsed.error {
                return Err(MarketDataError::Normalization(format!(
                    "deriv {}: {}",
                    err.code.unwrap_or_else(|| "error".to_string()),
                    err.message.unwrap_or_else(|| "unknown".to_string())
                )));
            }
            if let Some(tick) = parsed.tick {
                return Ok(tick);
            }
        }
    }

    /// Validate an API token: the `authorize` call answers with the account
    /// it opens (and proves the token and the network both work), or Deriv's
    /// refusal. One short-lived connection per check -- the same shape as
    /// [`Self::candles`].
    ///
    /// # Errors
    /// [`MarketDataError::Normalization`] on a Deriv error reply or a
    /// transport failure (the caller distinguishes "bad token" from "cannot
    /// reach the venue" by the error text).
    pub async fn authorize(&self, token: &str) -> Result<DerivAuth, MarketDataError> {
        let mut socket = self.connect().await?;
        socket
            .send(Message::Text(
                serde_json::json!({ "authorize": token }).to_string().into(),
            ))
            .await
            .map_err(|e| MarketDataError::Normalization(format!("deriv send failed: {e}")))?;
        let reply: DerivAuthResponse = loop {
            let Some(frame) = socket.next().await else {
                return Err(MarketDataError::Normalization(
                    "deriv closed the socket before answering".to_string(),
                ));
            };
            let frame = frame.map_err(|e| {
                MarketDataError::Normalization(format!("deriv socket error: {e}"))
            })?;
            let Message::Text(text) = frame else { continue };
            let parsed: DerivAuthResponse = serde_json::from_str(&text).map_err(|e| {
                MarketDataError::Normalization(format!("deriv reply did not parse: {e}"))
            })?;
            if parsed.authorize.is_some() || parsed.error.is_some() {
                break parsed;
            }
        };
        if let Some(err) = reply.error {
            return Err(MarketDataError::Normalization(format!(
                "deriv {}: {}",
                err.code.unwrap_or_else(|| "error".to_string()),
                err.message.unwrap_or_else(|| "unknown".to_string())
            )));
        }
        let wire = reply.authorize.ok_or_else(|| {
            MarketDataError::Normalization("deriv answered without an account".to_string())
        })?;
        Ok(DerivAuth {
            loginid: wire.loginid,
            currency: wire.currency,
            is_virtual: wire.is_virtual,
            scopes: wire.scopes,
        })
    }
}

/// Parse a `Deriv:...` or `DERIV-FX:...` namespace into the Deriv code, when
/// the instrument is one we serve.
#[must_use]
pub fn code_for_namespaced(symbol: &str) -> Option<&'static str> {
    let vs = crate::broker::VenueSymbol::parse(symbol);
    if vs.broker != "deriv" {
        return None;
    }
    code_for(&vs.symbol)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_symbol_map_round_trips() {
        assert_eq!(code_for("Volatility 75"), Some("R_75"));
        assert_eq!(code_for("volatility 75"), Some("R_75"));
        assert_eq!(symbol_for("R_75"), Some("Volatility 75"));
        assert_eq!(code_for("Boom 1000"), Some("BOOM1000"));
        assert_eq!(code_for("Step Index"), Some("stpRNG"));
        assert_eq!(code_for("EURUSD"), Some("frxEURUSD"));
        assert_eq!(code_for("NotAnIndex"), None);
    }

    #[test]
    fn namespaced_symbols_resolve() {
        assert_eq!(code_for_namespaced("DERIV:Volatility 75"), Some("R_75"));
        assert_eq!(code_for_namespaced("DERIV-FX:EURUSD"), Some("frxEURUSD"));
        assert_eq!(code_for_namespaced("BINANCE:BTCUSDT"), None);
        assert_eq!(code_for_namespaced("BTCUSDT"), None);
    }

    #[test]
    fn granularities_match_the_venue_set_and_gaps_plan_a_resample() {
        for tf in [
            Timeframe::M1,
            Timeframe::M3,
            Timeframe::M5,
            Timeframe::M15,
            Timeframe::M30,
            Timeframe::H1,
            Timeframe::H2,
            Timeframe::H4,
            Timeframe::H8,
            Timeframe::D1,
        ] {
            assert!(granularity_for(tf).is_some(), "{tf:?}");
        }
        assert_eq!(granularity_for(Timeframe::M1), Some(60));
        assert_eq!(granularity_for(Timeframe::H4), Some(14_400));
        // The venue's gaps: 6h/12h/3d/1w have no direct candles -- they plan
        // a resample from a finer granularity instead of inventing bars.
        for tf in [Timeframe::H6, Timeframe::H12, Timeframe::D3, Timeframe::W1] {
            assert!(granularity_for(tf).is_none(), "{tf:?}");
            assert!(DerivClient::fetch_plan(tf).is_some(), "{tf:?}");
        }
        assert_eq!(DerivClient::fetch_plan(Timeframe::H6), Some((Timeframe::H2, 3)));
        assert_eq!(DerivClient::fetch_plan(Timeframe::W1), Some((Timeframe::D1, 7)));
    }

    #[test]
    fn a_candles_reply_maps_to_our_shape() {
        // The parse half of candles(): a recorded Deriv envelope becomes our
        // Candle with nanosecond times and zero volumes.
        let body = r#"{"candles":[{"close":105.5,"epoch":1700000000,"high":106.0,"low":104.9,"open":105.0},{"close":106.1,"epoch":1700000060,"high":106.5,"low":105.4,"open":105.5}],"msg_type":"candles"}"#;
        let parsed: DerivResponse = serde_json::from_str(body).expect("parses");
        let rows = parsed.candles.expect("candles present");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].epoch * 1_000_000_000, 1_700_000_000_000_000_000);
        assert!(rows[0].open < rows[0].high);
        // And an error envelope parses as an error.
        let err: DerivResponse = serde_json::from_str(
            r#"{"error":{"code":"InvalidSymbol","message":"Symbol not active"}}"#,
        )
        .expect("parses");
        assert_eq!(err.error.expect("error").code.as_deref(), Some("InvalidSymbol"));
    }

    #[test]
    fn a_tick_becomes_a_zero_volume_builder_trade() {
        let tick = DerivTick { quote: 101.25, epoch: 1_700_000_061, symbol: None };
        let trade = tick_as_builder_trade(&tick, "Volatility 75");
        assert_eq!(trade.symbol, "Volatility 75");
        assert!((trade.price - 101.25).abs() < 1e-9);
        assert_eq!(trade.quantity, 0.0, "a quote has no volume");
        assert_eq!(trade.timestamp, 1_700_000_061_000_000_000);
        assert_eq!(trade.trade_id, 0, "no venue id on a quote");
        // And the stream message envelope parses both shapes.
        let batch: DerivStreamMsg = serde_json::from_str(
            r#"{"history":{"prices":[100.0,100.5],"times":[1700000000,1700000002]},"msg_type":"history"}"#,
        )
        .expect("batch parses");
        assert_eq!(batch.history.expect("batch").prices.len(), 2);
        let live: DerivStreamMsg = serde_json::from_str(
            r#"{"msg_type":"tick","tick":{"epoch":1700000004,"quote":101.0,"symbol":"R_75"}}"#,
        )
        .expect("tick parses");
        assert!((live.tick.expect("tick").quote - 101.0).abs() < 1e-9);
    }

    #[tokio::test]
    #[ignore = "live stream smoke: run explicitly with --include-ignored"]
    async fn live_ticks_stream_for_a_few_seconds() {
        let client = DerivClient::from_env();
        let (mut socket, batch, _early) = client
            .subscribe_ticks("R_75", 10)
            .await
            .expect("subscribe");
        assert!(!batch.is_empty(), "the venue seeds a tick batch");
        let tick = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            client.next_tick(&mut socket),
        )
        .await
        .expect("a live tick within 20s")
        .expect("tick parses");
        assert!(tick.quote > 0.0 && tick.epoch > 0);
    }

    #[test]
    fn an_authorize_reply_maps_the_account() {
        let ok: DerivAuthResponse = serde_json::from_str(
            r#"{"authorize":{"currency":"USD","is_virtual":1,"loginid":"VRTC123456"},"msg_type":"authorize"}"#,
        )
        .expect("parses");
        let wire = ok.authorize.expect("account");
        assert_eq!(wire.loginid, "VRTC123456");
        assert_eq!(wire.currency.as_deref(), Some("USD"));
        let bad: DerivAuthResponse = serde_json::from_str(
            r#"{"error":{"code":"InvalidToken","message":"The token is invalid"}}"#,
        )
        .expect("parses");
        assert_eq!(bad.error.expect("error").code.as_deref(), Some("InvalidToken"));
    }

    #[tokio::test]
    #[ignore = "live venue smoke test: run explicitly with --include-ignored"]
    async fn live_candles_come_back_sane() {
        let client = DerivClient::from_env();
        let candles = client
            .candles("R_75", Timeframe::M1, 60, 0)
            .await
            .expect("live fetch");
        assert_eq!(candles.len(), 60);
        assert!(candles.windows(2).all(|w| w[0].open_time < w[1].open_time));
        assert!(candles.iter().all(|c| c.low <= c.high && c.volume == 0.0));
    }
}
