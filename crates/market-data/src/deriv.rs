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
