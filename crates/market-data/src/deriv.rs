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

/// The default WebSocket endpoint (`ws.derivws.com` is the current host;
/// `ws.binaryws.com` is the legacy alias).
pub const DERIV_WS_URL: &str = "wss://ws.derivws.com/websockets/v3";

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
/// the venue has no such resolution.
#[must_use]
pub fn granularity_for(tf: Timeframe) -> Option<u32> {
    let secs = tf.secs();
    u32::try_from(secs).ok().filter(|s| *s > 0)
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
        let granularity = granularity_for(tf).ok_or_else(|| {
            MarketDataError::Normalization(format!("deriv has no granularity for {tf}"))
        })?;
        let url = format!("{}?app_id={}", self.ws_url, self.app_id);
        let (mut socket, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|e| MarketDataError::Normalization(format!("deriv connect failed: {e}")))?;
        let mut request = serde_json::json!({
            "ticks_history": code,
            "adjust_start_time": 1,
            "count": count.clamp(1, 5000),
            "granularity": granularity,
            "style": "candles",
        });
        if end_ns > 0 {
            request["end"] = serde_json::json!(end_ns / 1_000_000_000);
        }
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
        Ok(rows
            .into_iter()
            .map(|r| Candle {
                symbol: display.clone(),
                timeframe: tf,
                open_time: r.epoch * 1_000_000_000,
                open: r.open,
                high: r.high,
                low: r.low,
                close: r.close,
                volume: 0.0,
                buy_volume: 0.0,
                sell_volume: 0.0,
            })
            .collect())
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
    fn granularities_cover_the_venue_set() {
        for tf in [
            Timeframe::M1,
            Timeframe::M3,
            Timeframe::M5,
            Timeframe::M15,
            Timeframe::M30,
            Timeframe::H1,
            Timeframe::H2,
            Timeframe::H4,
            Timeframe::H6,
            Timeframe::H8,
            Timeframe::H12,
            Timeframe::D1,
            Timeframe::D3,
            Timeframe::W1,
        ] {
            assert!(granularity_for(tf).is_some(), "{tf:?}");
        }
        assert_eq!(granularity_for(Timeframe::M1), Some(60));
        assert_eq!(granularity_for(Timeframe::H4), Some(14_400));
        assert_eq!(granularity_for(Timeframe::W1), Some(604_800));
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
