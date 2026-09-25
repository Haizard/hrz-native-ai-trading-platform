//! Binance, expressed as a [`WireCodec`].
//!
//! This is a thin adapter, not a rewrite: every payload shape still lives in
//! [`wire`](super::wire), which stays the one place Binance's field names and
//! its string-typed numerics are written down. What is added here is the three
//! things the collector used to hold inline and a second venue cannot share --
//! the subscription spelling, the combined-stream envelope, and the fact that
//! Binance needs no heartbeat because it pings from its own side.

use super::codec::{DepthDiff, Frame, Incoming, Subscription, WireCodec};
use super::wire::{
    self, CombinedMessage, DepthMessage, DepthSnapshotResponse, SubscribeMessage, TradeMessage,
};
use crate::error::MarketDataError;

/// The default combined-stream endpoint.
pub const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/stream";

/// Binance's live wire format.
#[derive(Debug, Clone, Default)]
pub struct BinanceCodec {
    /// Combined-stream endpoint to open.
    ws_url: Option<String>,
}

impl BinanceCodec {
    /// A codec against the default Binance stream endpoint.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A codec against a specific endpoint.
    #[must_use]
    pub fn at(ws_url: impl Into<String>) -> Self {
        Self {
            ws_url: Some(ws_url.into()),
        }
    }

    /// Stream name for the trade feed.
    #[must_use]
    pub fn trade_stream_name(symbol: &str) -> String {
        format!("{}@trade", symbol.to_ascii_lowercase())
    }

    /// Stream name for the depth-diff feed.
    #[must_use]
    pub fn depth_stream_name(symbol: &str) -> String {
        format!("{}@depth@100ms", symbol.to_ascii_lowercase())
    }

    /// Parse a REST depth response into an [`Incoming::BookSnapshot`].
    ///
    /// The REST half stays here rather than moving inside `parse_frame`, because
    /// the collector's resync path fetches it out of band: Binance has no
    /// in-band reset boundary, so a book that failed to bridge needs a real
    /// snapshot, and only the caller knows when to ask for one.
    ///
    /// # Errors
    /// Returns [`MarketDataError::Normalization`] if any level is unusable.
    pub fn snapshot_from_rest(
        response: &DepthSnapshotResponse,
        symbol: &str,
    ) -> Result<Incoming, MarketDataError> {
        Ok(Incoming::BookSnapshot {
            symbol: symbol.to_uppercase(),
            bids: wire::parse_levels(&response.bids, "bid")?,
            asks: wire::parse_levels(&response.asks, "ask")?,
            last_update_id: response.last_update_id,
        })
    }
}

impl WireCodec for BinanceCodec {
    fn name(&self) -> &'static str {
        "binance"
    }

    fn ws_url(&self) -> String {
        self.ws_url
            .clone()
            .unwrap_or_else(|| BINANCE_WS_URL.to_string())
    }

    fn subscribe_payload(&self, subs: &[Subscription]) -> Result<String, MarketDataError> {
        let params: Vec<String> = subs
            .iter()
            .map(|sub| match sub {
                Subscription::Trades { symbol } => Self::trade_stream_name(symbol),
                Subscription::OrderBook { symbol } => Self::depth_stream_name(symbol),
            })
            .collect();

        serde_json::to_string(&SubscribeMessage {
            method: "SUBSCRIBE",
            params,
            id: 1,
        })
        .map_err(|e| MarketDataError::Normalization(e.to_string()))
    }

    /// Binance needs no pong: it pings from its own side and expects no answer.
    ///
    /// Returning `None` is the honest answer and keeps the collector from
    /// sending a control frame the venue never asked for.
    fn heartbeat(&self, _now_ms: u64) -> Option<String> {
        None
    }

    fn parse_frame(&self, text: &str) -> Frame {
        // A subscribe ack, and anything else that is not a combined-stream
        // envelope, lands here. It is a control frame, not a failure -- treating
        // it as one is how a pump dies on its own handshake.
        let Ok(envelope) = serde_json::from_str::<CombinedMessage>(text) else {
            return Frame::Control;
        };

        if envelope.stream.ends_with("@trade") {
            return match serde_json::from_value::<TradeMessage>(envelope.data) {
                Ok(message) => match message.to_trade() {
                    Ok(trade) => Frame::Data(Box::new(Incoming::Trade(trade))),
                    Err(e) => Frame::Malformed(e.to_string()),
                },
                Err(e) => Frame::Malformed(e.to_string()),
            };
        }

        if envelope.stream.contains("@depth") {
            let Ok(message) = serde_json::from_value::<DepthMessage>(envelope.data) else {
                return Frame::Malformed("depth payload did not match the Binance shape".into());
            };

            let mut reason = None;
            let bids = match wire::parse_levels(&message.bids, "bid") {
                Ok(bids) => bids,
                Err(e) => {
                    reason = Some(e.to_string());
                    Vec::new()
                }
            };
            let asks = match wire::parse_levels(&message.asks, "ask") {
                Ok(asks) => asks,
                Err(e) => {
                    reason.get_or_insert(e.to_string());
                    Vec::new()
                }
            };
            if let Some(reason) = reason {
                return Frame::Malformed(reason);
            }

            // Stream names are lowercase; symbols on the bus are uppercase.
            return Frame::Data(Box::new(Incoming::BookDelta(DepthDiff {
                symbol: message.symbol.to_uppercase(),
                // Binance spans a real id range and repeats it in both fields,
                // so both are carried. A venue without a span leaves this equal
                // to `final_update_id` rather than inventing one; see `bybit`.
                first_update_id: message.first_update_id,
                final_update_id: message.final_update_id,
                bids,
                asks,
            })));
        }

        // An envelope on a stream we did not subscribe to. Not fatal, and
        // visible at debug so a venue that adds a field is findable.
        Frame::Unrecognised
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes Binance sends for a `@trade` event.
    const TRADE_FRAME: &str = r#"{"stream":"btcusdt@trade","data":{"e":"trade","E":1672515782136,"s":"BTCUSDT","t":12345,"p":"0.001","q":"100","T":1672515782136,"m":true,"M":true}}"#;

    const DEPTH_FRAME: &str = r#"{"stream":"btcusdt@depth@100ms","data":{"e":"depthUpdate","E":1672515782136,"s":"BTCUSDT","U":157,"u":160,"b":[["0.0024","10"]],"a":[["0.0026","100"]]}}"#;

    fn codec() -> BinanceCodec {
        BinanceCodec::new()
    }

    /// The whole point of `Unrecognised`: a subscribe ack must not end the pump.
    ///
    /// Measured off the real socket -- Binance answers a `SUBSCRIBE` with
    /// `{"result":null,"id":1}`. Decoding that as an error is the dead-book
    /// defect with a different face.
    #[test]
    fn a_subscribe_ack_is_a_control_frame_not_a_failure() {
        assert_eq!(
            codec().parse_frame(r#"{"result":null,"id":1}"#),
            Frame::Control
        );
    }

    #[test]
    fn decodes_a_trade_frame() {
        let Frame::Data(incoming) = codec().parse_frame(TRADE_FRAME) else {
            panic!("a trade frame must decode");
        };
        let Incoming::Trade(trade) = *incoming else {
            panic!("expected a trade");
        };
        assert_eq!(trade.symbol, "BTCUSDT");
        assert_eq!(trade.trade_id, 12345);
        assert_eq!(trade.timestamp, 1_672_515_782_136_000_000);
    }

    /// The symbol the bus is keyed on is upper case, whatever case the stream
    /// name is spelled in.
    #[test]
    fn decodes_a_depth_frame_and_canonicalises_the_symbol() {
        let Frame::Data(incoming) = codec().parse_frame(DEPTH_FRAME) else {
            panic!("a depth frame must decode");
        };
        let Incoming::BookDelta(diff) = *incoming else {
            panic!("expected a depth diff");
        };
        assert_eq!(diff.symbol, "BTCUSDT");
        assert_eq!(diff.first_update_id, 157);
        assert_eq!(diff.final_update_id, 160);
    }

    /// Binance repeats the span in `U` and `u`; both are carried through, and
    /// the control is that they are *not* collapsed to one value.
    #[test]
    fn binance_keeps_its_update_id_span() {
        let Frame::Data(incoming) = codec().parse_frame(DEPTH_FRAME) else {
            panic!("decodes");
        };
        let Incoming::BookDelta(diff) = *incoming else {
            panic!("expected a depth diff");
        };
        assert_ne!(
            diff.first_update_id, diff.final_update_id,
            "a venue that carries a real span must not have it flattened"
        );
    }

    #[test]
    fn a_recognised_topic_that_does_not_decode_is_malformed() {
        let broken = DEPTH_FRAME.replace("\"0.0024\"", "\"not-a-price\"");
        assert!(matches!(codec().parse_frame(&broken), Frame::Malformed(_)));
    }

    /// An envelope for a stream we never asked for is not an error.
    #[test]
    fn an_unknown_stream_is_unrecognised() {
        let frame = r#"{"stream":"btcusdt@kline_1m","data":{"e":"kline"}}"#;
        assert_eq!(codec().parse_frame(frame), Frame::Unrecognised);
    }

    #[test]
    fn subscribes_with_the_combined_stream_spelling() {
        let raw = codec()
            .subscribe_payload(&[
                Subscription::Trades {
                    symbol: "BTCUSDT".into(),
                },
                Subscription::OrderBook {
                    symbol: "BTCUSDT".into(),
                },
            ])
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["method"], "SUBSCRIBE");
        let params: Vec<String> = serde_json::from_value(value["params"].clone()).unwrap();
        assert_eq!(params, vec!["btcusdt@trade", "btcusdt@depth@100ms"]);
    }

    #[test]
    fn binance_sends_no_heartbeat() {
        assert_eq!(codec().heartbeat(0), None);
    }
}
