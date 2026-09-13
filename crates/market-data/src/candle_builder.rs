//! Candle aggregation **from the trade stream**.
//!
//! We do not use exchange-provided klines for live candles. Building from
//! trades means the `buy_volume`/`sell_volume` split is derived the same way as
//! delta and CVD downstream, which is what makes footprint/delta/CVD coherent
//! (`docs/04-MARKET-DATA-ENGINE.md`).
//!
//! A consequence worth knowing: if no trades print inside a bucket, **no candle
//! is emitted** for it -- there is no synthetic "flat" candle. Gap detection
//! (see [`crate::health`]) is what tells you a bucket was missed.

use analytics_core::{Candle, Timeframe, Trade};

/// Aggregates a trade stream into candles at one resolution.
#[derive(Debug, Clone)]
pub struct CandleBuilder {
    symbol: String,
    timeframe: Timeframe,
    current: Option<Candle>,
    emitted: u64,
}

impl CandleBuilder {
    /// A builder with no in-progress candle.
    #[must_use]
    pub fn new(symbol: impl Into<String>, timeframe: Timeframe) -> Self {
        Self {
            symbol: symbol.into(),
            timeframe,
            current: None,
            emitted: 0,
        }
    }

    /// Resolution being built.
    #[must_use]
    pub const fn timeframe(&self) -> Timeframe {
        self.timeframe
    }

    /// The in-progress (not yet closed) candle, if any.
    #[must_use]
    pub const fn current(&self) -> Option<&Candle> {
        self.current.as_ref()
    }

    /// How many candles have been closed so far.
    #[must_use]
    pub const fn emitted(&self) -> u64 {
        self.emitted
    }

    /// Feed one trade.
    ///
    /// Returns the candle that was just closed if this trade started a new
    /// bucket, otherwise `None`.
    ///
    /// Trades are assumed to arrive in chronological order. An out-of-order
    /// trade that predates the current bucket is ignored rather than corrupting
    /// the aggregation.
    pub fn on_trade(&mut self, trade: &Trade) -> Option<Candle> {
        if trade.symbol != self.symbol {
            return None;
        }

        let bucket = self.timeframe.bucket_of(trade.timestamp);
        let mut closed = None;

        match self.current.as_mut() {
            Some(c) if c.open_time == bucket => {
                Self::accumulate(c, trade);
                return None;
            }
            Some(c) => {
                if bucket < c.open_time {
                    // Late trade for an already-closed bucket: drop it.
                    return None;
                }
                closed = self.current.take();
            }
            None => {}
        }

        if closed.is_some() {
            self.emitted += 1;
        }

        self.current = Some(Self::seed(&self.symbol, self.timeframe, bucket, trade));
        closed
    }

    /// Close the in-progress candle without waiting for the next bucket.
    pub fn flush(&mut self) -> Option<Candle> {
        let candle = self.current.take();
        if candle.is_some() {
            self.emitted += 1;
        }
        candle
    }

    fn seed(symbol: &str, timeframe: Timeframe, bucket: i64, trade: &Trade) -> Candle {
        let (buy, sell) = if trade.is_buyer_maker {
            (0.0, trade.quantity)
        } else {
            (trade.quantity, 0.0)
        };

        Candle {
            symbol: symbol.to_string(),
            timeframe,
            open_time: bucket,
            open: trade.price,
            high: trade.price,
            low: trade.price,
            close: trade.price,
            volume: trade.quantity,
            buy_volume: buy,
            sell_volume: sell,
        }
    }

    fn accumulate(candle: &mut Candle, trade: &Trade) {
        candle.high = candle.high.max(trade.price);
        candle.low = candle.low.min(trade.price);
        candle.close = trade.price;
        candle.volume += trade.quantity;
        if trade.is_buyer_maker {
            candle.sell_volume += trade.quantity;
        } else {
            candle.buy_volume += trade.quantity;
        }
    }
}

/// Fans one trade stream out to several resolutions simultaneously.
#[derive(Debug, Clone)]
pub struct MultiTimeframeCandleBuilder {
    builders: Vec<CandleBuilder>,
}

impl MultiTimeframeCandleBuilder {
    /// One builder per supplied timeframe.
    #[must_use]
    pub fn new(symbol: impl Into<String>, timeframes: &[Timeframe]) -> Self {
        let symbol = symbol.into();
        Self {
            builders: timeframes
                .iter()
                .map(|tf| CandleBuilder::new(symbol.clone(), *tf))
                .collect(),
        }
    }

    /// Builders for the standard ladder used by the platform.
    #[must_use]
    pub fn standard(symbol: impl Into<String>) -> Self {
        Self::new(
            symbol,
            &[
                Timeframe::M1,
                Timeframe::M5,
                Timeframe::M15,
                Timeframe::H1,
                Timeframe::H4,
                Timeframe::D1,
            ],
        )
    }

    /// Feed one trade; returns every candle closed by it (0..n, one per
    /// resolution whose bucket just rolled over).
    pub fn on_trade(&mut self, trade: &Trade) -> Vec<Candle> {
        self.builders
            .iter_mut()
            .filter_map(|b| b.on_trade(trade))
            .collect()
    }

    /// Flush all in-progress candles.
    pub fn flush(&mut self) -> Vec<Candle> {
        self.builders.iter_mut().filter_map(|b| b.flush()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60 * 1_000_000_000;

    fn trade_at(ts: i64, price: f64, qty: f64, buyer_maker: bool) -> Trade {
        Trade {
            symbol: "BTCUSDT".into(),
            trade_id: ts as u64,
            price,
            quantity: qty,
            is_buyer_maker: buyer_maker,
            timestamp: ts,
        }
    }

    #[test]
    fn first_trade_seeds_a_candle() {
        let mut b = CandleBuilder::new("BTCUSDT", Timeframe::M1);
        let closed = b.on_trade(&trade_at(MINUTE + 5, 100.0, 1.0, false));
        assert!(closed.is_none());
        let c = b.current().unwrap();
        assert_eq!(c.open_time, MINUTE);
        assert_eq!(c.open, 100.0);
        assert_eq!(c.high, 100.0);
        assert_eq!(c.low, 100.0);
    }

    #[test]
    fn trades_in_same_bucket_accumulate() {
        let mut b = CandleBuilder::new("BTCUSDT", Timeframe::M1);
        b.on_trade(&trade_at(MINUTE + 1, 100.0, 1.0, false));
        b.on_trade(&trade_at(MINUTE + 2, 102.0, 2.0, false));
        b.on_trade(&trade_at(MINUTE + 3, 99.0, 3.0, true));
        let c = b.current().unwrap();
        assert_eq!(c.open, 100.0);
        assert_eq!(c.high, 102.0);
        assert_eq!(c.low, 99.0);
        assert_eq!(c.close, 99.0);
        assert!((c.volume - 6.0).abs() < f64::EPSILON);
        // Two buy-aggressed trades (1+2), one sell-aggressed (3).
        assert!((c.buy_volume - 3.0).abs() < f64::EPSILON);
        assert!((c.sell_volume - 3.0).abs() < f64::EPSILON);
        assert!((c.delta()).abs() < f64::EPSILON);
    }

    #[test]
    fn crossing_a_bucket_closes_the_previous_candle() {
        let mut b = CandleBuilder::new("BTCUSDT", Timeframe::M1);
        b.on_trade(&trade_at(MINUTE + 10, 100.0, 1.0, false));
        let closed = b.on_trade(&trade_at(2 * MINUTE + 10, 110.0, 1.0, false));

        let closed = closed.expect("should have closed the first minute");
        assert_eq!(closed.open_time, MINUTE);
        assert_eq!(closed.close, 100.0);
        assert_eq!(closed.timeframe, Timeframe::M1);

        let current = b.current().unwrap();
        assert_eq!(current.open_time, 2 * MINUTE);
        assert_eq!(current.open, 110.0);
        assert_eq!(b.emitted(), 1);
    }

    #[test]
    fn out_of_order_trade_is_dropped_not_merged() {
        let mut b = CandleBuilder::new("BTCUSDT", Timeframe::M1);
        b.on_trade(&trade_at(5 * MINUTE + 1, 100.0, 1.0, false));
        // Older than the current bucket.
        let closed = b.on_trade(&trade_at(2 * MINUTE + 1, 50.0, 99.0, false));
        assert!(closed.is_none());
        let c = b.current().unwrap();
        assert!(
            (c.volume - 1.0).abs() < f64::EPSILON,
            "late trade must not leak in"
        );
    }

    #[test]
    fn trades_for_another_symbol_are_ignored() {
        let mut b = CandleBuilder::new("BTCUSDT", Timeframe::M1);
        let other = Trade {
            symbol: "ETHUSDT".into(),
            trade_id: 1,
            price: 10.0,
            quantity: 5.0,
            is_buyer_maker: false,
            timestamp: MINUTE,
        };
        assert!(b.on_trade(&other).is_none());
        assert!(b.current().is_none());
    }

    #[test]
    fn flush_closes_the_open_candle() {
        let mut b = CandleBuilder::new("BTCUSDT", Timeframe::M1);
        b.on_trade(&trade_at(MINUTE, 100.0, 1.0, false));
        let c = b.flush().expect("flush should yield the open candle");
        assert_eq!(c.close, 100.0);
        assert!(b.current().is_none());
        assert!(b.flush().is_none());
    }

    #[test]
    fn multi_timeframe_rolls_over_only_the_lower_resolutions() {
        let mut m = MultiTimeframeCandleBuilder::standard("BTCUSDT");
        // 10:00:30
        let t1 = 10 * 60 * MINUTE + 30;
        m.on_trade(&trade_at(t1, 100.0, 1.0, false));

        // 10:01:10 -- only the 1m bucket rolls (5m bucket is still 10:00).
        let closed = m.on_trade(&trade_at(t1 + 70 * 1_000_000_000, 101.0, 1.0, false));
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].timeframe, Timeframe::M1);

        // 10:05:00 -- 1m and 5m both roll.
        let closed = m.on_trade(&trade_at(10 * 60 * MINUTE + 5 * MINUTE, 102.0, 1.0, false));
        let tfs: Vec<Timeframe> = closed.iter().map(|c| c.timeframe).collect();
        assert!(tfs.contains(&Timeframe::M1));
        assert!(tfs.contains(&Timeframe::M5));
        assert!(!tfs.contains(&Timeframe::H1));
    }

    #[test]
    fn candle_volume_matches_trade_volume() {
        let mut b = CandleBuilder::new("BTCUSDT", Timeframe::M1);
        let mut total = 0.0;
        for i in 0..10 {
            let qty = 0.5;
            total += qty;
            #[allow(clippy::cast_precision_loss)]
            b.on_trade(&trade_at(MINUTE + i, 100.0 + (i as f64), qty, i % 2 == 0));
        }
        let c = b.current().unwrap();
        assert!((c.volume - total).abs() < 1e-9);
        assert!((c.buy_volume + c.sell_volume - total).abs() < 1e-9);
    }
}
