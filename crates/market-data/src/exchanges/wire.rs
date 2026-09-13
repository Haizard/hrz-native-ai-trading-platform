//! Binance wire formats.
//!
//! Kept separate from the collector so payload parsing is unit-testable from
//! recorded samples without a socket.
//!
//! Binance sends every field as a **string** for numeric precision, hence the
//! `String` -> `f64` conversion step.

use analytics_core::{Candle, Timeframe, Trade};
use serde::{Deserialize, Serialize};

use crate::error::MarketDataError;

/// Envelope for combined-stream messages (`/stream?streams=...`).
#[derive(Debug, Clone, Deserialize)]
pub struct CombinedMessage {
    /// Stream name, e.g. `btcusdt@trade`.
    pub stream: String,
    /// The payload; shape depends on the stream suffix.
    pub data: serde_json::Value,
}

/// `@trade` payload.
#[derive(Debug, Clone, Deserialize)]
pub struct TradeMessage {
    /// Symbol (`s`).
    #[serde(rename = "s")]
    pub symbol: String,
    /// Trade id (`t`).
    #[serde(rename = "t")]
    pub trade_id: u64,
    /// Price (`p`), as a string.
    #[serde(rename = "p")]
    pub price: String,
    /// Quantity (`q`), as a string.
    #[serde(rename = "q")]
    pub quantity: String,
    /// Trade time in **milliseconds** (`T`).
    #[serde(rename = "T")]
    pub trade_time_ms: i64,
    /// Is the buyer the maker (`m`)?
    #[serde(rename = "m")]
    pub is_buyer_maker: bool,
}

impl TradeMessage {
    /// Convert to the platform's shared `Trade` (nanosecond timestamps).
    ///
    /// # Errors
    /// Returns [`MarketDataError::Normalization`] if price/quantity are not
    /// parseable numbers.
    pub fn to_trade(&self) -> Result<Trade, MarketDataError> {
        let price = parse_f64(&self.price, "price")?;
        let quantity = parse_f64(&self.quantity, "quantity")?;

        Ok(Trade {
            symbol: self.symbol.clone(),
            trade_id: self.trade_id,
            price,
            quantity,
            is_buyer_maker: self.is_buyer_maker,
            timestamp: self.trade_time_ms * 1_000_000,
        })
    }
}

/// A `[price, quantity]` pair as delivered on the wire.
pub type LevelStrings = (String, String);

/// `@depth` diff payload.
#[derive(Debug, Clone, Deserialize)]
pub struct DepthMessage {
    /// Symbol (`s`).
    #[serde(rename = "s")]
    pub symbol: String,
    /// First update id in this event (`U`).
    #[serde(rename = "U")]
    pub first_update_id: u64,
    /// Final update id in this event (`u`).
    #[serde(rename = "u")]
    pub final_update_id: u64,
    /// Bid level changes (`b`). Quantity `"0"` removes the level.
    #[serde(rename = "b")]
    pub bids: Vec<LevelStrings>,
    /// Ask level changes (`a`).
    #[serde(rename = "a")]
    pub asks: Vec<LevelStrings>,
}

/// Response from `GET /api/v3/depth`.
#[derive(Debug, Clone, Deserialize)]
pub struct DepthSnapshotResponse {
    /// `lastUpdateId` -- the snapshot is current as of this id.
    #[serde(rename = "lastUpdateId")]
    pub last_update_id: u64,
    /// Bids as `[price, quantity]`.
    pub bids: Vec<LevelStrings>,
    /// Asks as `[price, quantity]`.
    pub asks: Vec<LevelStrings>,
}

/// One row of `GET /api/v3/klines`.
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
    /// Taker **buy** base-asset volume -- this is what gives us the buy/sell
    /// split without replaying individual trades.
    pub taker_buy_base: f64,
}

impl RawKline {
    /// Parse one kline array.
    ///
    /// # Errors
    /// Returns [`MarketDataError::Normalization`] if the array is too short or
    /// any numeric field is malformed.
    pub fn from_array(values: &[serde_json::Value]) -> Result<Self, MarketDataError> {
        if values.len() < 10 {
            return Err(MarketDataError::Normalization(format!(
                "kline row has {} fields, expected at least 10",
                values.len()
            )));
        }

        Ok(Self {
            open_time_ms: values[0]
                .as_i64()
                .ok_or_else(|| MarketDataError::Normalization("open_time".into()))?,
            open: json_f64(&values[1], "open")?,
            high: json_f64(&values[2], "high")?,
            low: json_f64(&values[3], "low")?,
            close: json_f64(&values[4], "close")?,
            volume: json_f64(&values[5], "volume")?,
            taker_buy_base: json_f64(&values[9], "taker_buy_base")?,
        })
    }

    /// Convert to a `Candle` at `timeframe`, deriving the buy/sell split from
    /// `taker_buy_base`.
    #[must_use]
    pub fn to_candle(&self, symbol: &str, timeframe: Timeframe) -> Candle {
        let buy_volume = self.taker_buy_base.min(self.volume);
        let sell_volume = (self.volume - buy_volume).max(0.0);

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

/// One row of `GET /api/v3/aggTrades`.
#[derive(Debug, Clone, Deserialize)]
pub struct AggTrade {
    /// Aggregate trade id (`a`).
    #[serde(rename = "a")]
    pub agg_id: u64,
    /// Price (`p`).
    #[serde(rename = "p")]
    pub price: String,
    /// Quantity (`q`).
    #[serde(rename = "q")]
    pub quantity: String,
    /// Timestamp in milliseconds (`T`).
    #[serde(rename = "T")]
    pub timestamp_ms: i64,
    /// Is the buyer the maker (`m`)?
    #[serde(rename = "m")]
    pub is_buyer_maker: bool,
}

impl AggTrade {
    /// Convert to the shared `Trade` type.
    ///
    /// # Errors
    /// Returns [`MarketDataError::Normalization`] on unparseable numbers.
    pub fn to_trade(&self, symbol: &str) -> Result<Trade, MarketDataError> {
        Ok(Trade {
            symbol: symbol.to_string(),
            trade_id: self.agg_id,
            price: parse_f64(&self.price, "price")?,
            quantity: parse_f64(&self.quantity, "quantity")?,
            is_buyer_maker: self.is_buyer_maker,
            timestamp: self.timestamp_ms * 1_000_000,
        })
    }
}

/// Outgoing `SUBSCRIBE` / `UNSUBSCRIBE` control message.
#[derive(Debug, Clone, Serialize)]
pub struct SubscribeMessage<'a> {
    /// `"SUBSCRIBE"` or `"UNSUBSCRIBE"`.
    pub method: &'a str,
    /// Stream names to (un)subscribe.
    pub params: Vec<String>,
    /// Arbitrary request id.
    pub id: u64,
}

/// Parse a decimal string into `f64`.
///
/// # Errors
/// Returns [`MarketDataError::Normalization`] if the string is not a number.
pub fn parse_f64(raw: &str, field: &str) -> Result<f64, MarketDataError> {
    raw.parse::<f64>().map_err(|e| {
        MarketDataError::Normalization(format!("{field}: `{raw}` is not a number ({e})"))
    })
}

/// Parse levels from `[["1.0","2.0"], ...]`.
///
/// # Errors
/// Returns [`MarketDataError::Normalization`] if any level is malformed.
pub fn parse_levels(
    levels: &[LevelStrings],
    side: &str,
) -> Result<Vec<(f64, f64)>, MarketDataError> {
    levels
        .iter()
        .map(|(p, q)| {
            Ok((
                parse_f64(p, &format!("{side} price"))?,
                parse_f64(q, &format!("{side} qty"))?,
            ))
        })
        .collect()
}

fn json_f64(value: &serde_json::Value, field: &str) -> Result<f64, MarketDataError> {
    match value {
        serde_json::Value::String(s) => parse_f64(s, field),
        other => other
            .as_f64()
            .ok_or_else(|| MarketDataError::Normalization(format!("{field}: not numeric"))),
    }
}

/// Milliseconds -> nanoseconds.
#[must_use]
pub const fn ms_to_ns(ms: i64) -> i64 {
    ms * 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRADE_JSON: &str = r#"{
        "e":"trade","E":1672515782136,"s":"BTCUSDT","t":12345,
        "p":"0.001","q":"100","T":1672515782136,"m":true,"M":true
    }"#;

    const DEPTH_JSON: &str = r#"{
        "e":"depthUpdate","E":1672515782136,"s":"BTCUSDT",
        "U":157,"u":160,
        "b":[["0.0024","10"],["0.0020","0"]],
        "a":[["0.0026","100"]]
    }"#;

    #[test]
    fn parses_trade_message() {
        let msg: TradeMessage = serde_json::from_str(TRADE_JSON).unwrap();
        assert_eq!(msg.symbol, "BTCUSDT");
        assert_eq!(msg.trade_id, 12345);
        assert!(msg.is_buyer_maker);

        let trade = msg.to_trade().unwrap();
        assert!((trade.price - 0.001).abs() < f64::EPSILON);
        assert!((trade.quantity - 100.0).abs() < f64::EPSILON);
        // ms -> ns
        assert_eq!(trade.timestamp, 1_672_515_782_136_000_000);
        assert_eq!(trade.side(), analytics_core::Side::Sell);
    }

    #[test]
    fn parses_depth_message_and_levels() {
        let msg: DepthMessage = serde_json::from_str(DEPTH_JSON).unwrap();
        assert_eq!(msg.first_update_id, 157);
        assert_eq!(msg.final_update_id, 160);

        let bids = parse_levels(&msg.bids, "bid").unwrap();
        assert_eq!(bids.len(), 2);
        assert!((bids[0].0 - 0.0024).abs() < f64::EPSILON);
        assert!(
            (bids[1].1 - 0.0).abs() < f64::EPSILON,
            "zero qty means remove"
        );

        let asks = parse_levels(&msg.asks, "ask").unwrap();
        assert!((asks[0].0 - 0.0026).abs() < f64::EPSILON);
    }

    #[test]
    fn parses_combined_envelope() {
        let raw = format!(r#"{{"stream":"btcusdt@trade","data":{TRADE_JSON}}}"#);
        let env: CombinedMessage = serde_json::from_str(&raw).unwrap();
        assert_eq!(env.stream, "btcusdt@trade");
        let trade: TradeMessage = serde_json::from_value(env.data).unwrap();
        assert_eq!(trade.trade_id, 12345);
    }

    #[test]
    fn parses_depth_snapshot_response() {
        let raw = r#"{"lastUpdateId":1027024,"bids":[["4.00000000","431.0"]],"asks":[["4.00000200","12.0"]]}"#;
        let snap: DepthSnapshotResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(snap.last_update_id, 1_027_024);
        assert_eq!(snap.bids.len(), 1);
        assert_eq!(snap.asks.len(), 1);
    }

    #[test]
    fn kline_row_splits_volume_by_taker_buy() {
        let row = serde_json::json!([
            1672515780000i64,
            "16800.0",
            "16850.0",
            "16790.0",
            "16840.0",
            "100.0",
            1672515839999i64,
            "1684000.0",
            500,
            "60.0",
            "1008000.0",
            "0"
        ]);
        let values: Vec<serde_json::Value> = row.as_array().unwrap().clone();
        let kline = RawKline::from_array(&values).unwrap();

        assert_eq!(kline.open_time_ms, 1_672_515_780_000);
        assert!((kline.volume - 100.0).abs() < f64::EPSILON);
        assert!((kline.taker_buy_base - 60.0).abs() < f64::EPSILON);

        let candle = kline.to_candle("BTCUSDT", Timeframe::M1);
        assert!((candle.buy_volume - 60.0).abs() < f64::EPSILON);
        assert!((candle.sell_volume - 40.0).abs() < f64::EPSILON);
        assert!((candle.delta() - 20.0).abs() < f64::EPSILON);
        assert_eq!(candle.open_time, 1_672_515_780_000_000_000);
    }

    #[test]
    fn kline_row_rejects_short_arrays() {
        let short = vec![serde_json::json!(1)];
        assert!(RawKline::from_array(&short).is_err());
    }

    #[test]
    fn agg_trade_converts_to_trade() {
        let raw = r#"{"a":26129,"p":"0.01633102","q":"4.70443515","f":27781,"l":27781,"T":1498793709153,"m":true,"M":true}"#;
        let agg: AggTrade = serde_json::from_str(raw).unwrap();
        let trade = agg.to_trade("BNBBTC").unwrap();
        assert_eq!(trade.symbol, "BNBBTC");
        assert_eq!(trade.trade_id, 26129);
        assert!(trade.is_buyer_maker);
        assert_eq!(trade.timestamp, 1_498_793_709_153_000_000);
    }

    #[test]
    fn malformed_numbers_are_reported_not_panicked() {
        let raw = TRADE_JSON.replace("\"0.001\"", "\"not-a-number\"");
        let msg: TradeMessage = serde_json::from_str(&raw).unwrap();
        assert!(matches!(
            msg.to_trade(),
            Err(MarketDataError::Normalization(_))
        ));
    }
}
