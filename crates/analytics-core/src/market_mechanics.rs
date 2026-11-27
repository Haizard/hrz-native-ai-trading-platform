//! Deterministic participant, dealer and cross-market mechanics.
//!
//! None of these claims to identify a real institution. They are reproducible
//! proxies: an agent-based simulation with fixed policies, a dealer-hedging
//! proxy from visible volume and delta, and transfer entropy between two
//! instruments' returns. Every output states what it does not know.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::research_methods::transfer_entropy;
use crate::types::Candle;

/// Fixed policies for the agent-based simulation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SimulationConfig {
    /// Bars to simulate.
    pub bars: usize,
    /// Starting price.
    pub start_price: f64,
    /// Market maker inventory target.
    pub maker_target: f64,
    /// Trend-follower momentum window.
    pub trend_window: usize,
    /// Random seed for noise traders.
    pub seed: u64,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            bars: 120,
            start_price: 100.0,
            maker_target: 0.0,
            trend_window: 10,
            seed: 0xA6E5_1234,
        }
    }
}

/// One simulated bar of participant flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulatedBar {
    /// Price after the bar's net order flow.
    pub price: f64,
    /// Net aggressor flow (positive = buying).
    pub net_flow: f64,
    /// Maker inventory after the bar.
    pub maker_inventory: f64,
    /// Per-class order flow, keyed by class name.
    pub class_flow: BTreeMap<String, f64>,
}

struct XorShift(u64);

impl XorShift {
    fn next_f64(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x << 27;
        self.0 = x;
        (((x.wrapping_mul(0x2545_F491_4F6C_DD1D)) >> 11) as f64) / (1u64 << 53) as f64
    }
}

/// Run a deterministic four-class agent-based market simulation.
///
/// Classes: `market_maker` (inventory mean reversion), `trend_follower`
/// (momentum), `noise_trader` (seeded random) and `informed_trader` (a small
/// constant drift). The simulation is a research instrument, not a forecast.
#[must_use]
pub fn simulate_participants(config: &SimulationConfig) -> Vec<SimulatedBar> {
    if config.bars == 0 || config.start_price <= 0.0 || config.trend_window == 0 {
        return Vec::new();
    }
    let mut rng = XorShift(config.seed);
    let mut price = config.start_price;
    let mut inventory = 0.0;
    let mut history = Vec::with_capacity(config.bars);
    let mut out = Vec::with_capacity(config.bars);
    for _ in 0..config.bars {
        let maker = (config.maker_target - inventory) * 0.35;
        let recent = history
            .iter()
            .rev()
            .take(config.trend_window)
            .copied()
            .collect::<Vec<_>>();
        let trend = if recent.len() >= 2 {
            let mean = recent.iter().sum::<f64>() / recent.len() as f64;
            ((price - mean) / mean * 200.0).clamp(-5.0, 5.0)
        } else {
            0.0
        };
        let noise = (rng.next_f64() - 0.5) * 2.0;
        let informed = 0.15;
        let net_flow = (maker + trend + noise + informed).clamp(-10.0, 10.0);
        inventory = (inventory - net_flow).clamp(-1000.0, 1000.0);
        let returns = (net_flow * 0.0005).clamp(-0.02, 0.02);
        price = (price * (1.0 + returns)).max(f64::EPSILON);
        let mut class_flow = BTreeMap::new();
        class_flow.insert("market_maker".to_string(), maker);
        class_flow.insert("trend_follower".to_string(), trend);
        class_flow.insert("noise_trader".to_string(), noise);
        class_flow.insert("informed_trader".to_string(), informed);
        history.push(price);
        out.push(SimulatedBar {
            price,
            net_flow,
            maker_inventory: inventory,
            class_flow,
        });
    }
    out
}

/// Deterministic proxy of participant dominance from visible OHLCV/delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParticipantMix {
    /// Weight attributed to liquidity provision (inventory management).
    pub market_maker: f64,
    /// Weight attributed to momentum following.
    pub trend_follower: f64,
    /// Weight attributed to random/noise flow.
    pub noise_trader: f64,
    /// Weight attributed to persistent directional flow.
    pub informed_trader: f64,
    /// Explicit limitation of the proxy.
    pub limitation: String,
}

/// Estimate a participant mix from completed candles.
///
/// This is a heuristic over the visible aggressor split, body/wick shape and
/// directional persistence. It cannot identify a participant class.
#[must_use]
pub fn estimate_participant_mix(candles: &[Candle]) -> Option<ParticipantMix> {
    if candles.len() < 20 {
        return None;
    }
    let mut volume = 0.0;
    let mut abs_delta = 0.0;
    let mut net_delta = 0.0;
    let mut wick = 0.0;
    let mut body = 0.0;
    for candle in candles {
        let total = candle.volume.max(0.0);
        volume += total;
        let delta = candle.delta();
        abs_delta += delta.abs();
        net_delta += delta;
        let span = (candle.high - candle.low).abs().max(f64::EPSILON);
        body += (candle.close - candle.open).abs();
        wick += span - (candle.close - candle.open).abs();
    }
    if volume <= 0.0 {
        return None;
    }
    let balance = (net_delta.abs() / volume.max(f64::EPSILON)).clamp(0.0, 1.0);
    let churn = (abs_delta / volume.max(f64::EPSILON)).clamp(0.0, 1.0);
    let wick_share = (wick / (body + wick).max(f64::EPSILON)).clamp(0.0, 1.0);
    let informed = balance * (1.0 - wick_share);
    let maker = wick_share * (1.0 - churn);
    let noise = (1.0 - balance) * churn;
    let trend = (1.0 - maker - informed - noise).abs().clamp(0.0, 1.0);
    Some(ParticipantMix {
        market_maker: maker,
        trend_follower: trend,
        noise_trader: noise,
        informed_trader: informed,
        limitation: "heuristic from visible flow; not participant identification".to_string(),
    })
}

/// Dealer hedging proxy from volume shape and aggressor delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DealerState {
    /// Estimated inventory bias: positive means the dealer is long.
    pub exposure: f64,
    /// Direction the dealer would hedge next.
    pub hedge_bias: f64,
    /// Confidence in `[0, 1]`, not a probability of price direction.
    pub confidence: f64,
    /// Explicit limitation.
    pub limitation: String,
}

/// Estimate dealer exposure without options data.
#[must_use]
pub fn estimate_dealer_state(candles: &[Candle]) -> Option<DealerState> {
    let last = candles.last()?;
    if candles.len() < 20 || last.close <= 0.0 {
        return None;
    }
    let high = candles
        .iter()
        .map(|c| c.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let low = candles.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    if !(high > low) {
        return None;
    }
    let position = ((last.close - low) / (high - low)).clamp(0.0, 1.0);
    let delta = last.delta();
    let volume = last.volume.max(f64::EPSILON);
    let exposure = (position - 0.5) * 2.0 - (delta / volume).clamp(-1.0, 1.0);
    let hedge_bias = -exposure;
    let confidence = (1.0 - (exposure.abs() - 1.0).max(0.0)).clamp(0.0, 1.0);
    Some(DealerState {
        exposure,
        hedge_bias,
        confidence,
        limitation: "price/volume proxy; no options positioning or dealer data".to_string(),
    })
}

/// Categorical transfer entropy between two instrument return series.
///
/// Returns `None` when series lengths differ or the data is degenerate. The
/// value is descriptive evidence of information flow, not a significance test.
#[must_use]
pub fn cross_market_transfer_entropy(a: &[Candle], b: &[Candle]) -> Option<f64> {
    let count = a.len().min(b.len());
    if count < 30 {
        return None;
    }
    let a_returns: Vec<f64> = a[..count]
        .windows(2)
        .filter(|pair| pair[0].close > 0.0)
        .map(|pair| (pair[1].close / pair[0].close).ln())
        .collect();
    let b_returns: Vec<f64> = b[..count]
        .windows(2)
        .filter(|pair| pair[0].close > 0.0)
        .map(|pair| (pair[1].close / pair[0].close).ln())
        .collect();
    let n = a_returns.len().min(b_returns.len());
    transfer_entropy(&a_returns[..n], &b_returns[..n], 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles(up: bool) -> Vec<Candle> {
        (0..40)
            .map(|i| {
                let close = if up {
                    100.0 + i as f64
                } else {
                    140.0 - i as f64
                };
                let open = if up { close - 0.5 } else { close + 0.5 };
                Candle {
                    symbol: "TEST".to_string(),
                    timeframe: crate::types::Timeframe::M1,
                    open_time: i * 60_000_000_000,
                    open,
                    high: open.max(close) + 0.5,
                    low: open.min(close) - 0.5,
                    close,
                    volume: 10.0,
                    buy_volume: if up { 7.0 } else { 3.0 },
                    sell_volume: if up { 3.0 } else { 7.0 },
                }
            })
            .collect()
    }

    #[test]
    fn simulation_is_deterministic_and_finite() {
        let config = SimulationConfig::default();
        let first = simulate_participants(&config);
        let second = simulate_participants(&config);
        assert_eq!(first, second);
        assert_eq!(first.len(), config.bars);
        assert!(first
            .iter()
            .all(|bar| bar.price.is_finite() && bar.price > 0.0));
        assert!(first.iter().all(|bar| bar.class_flow.len() == 4));
    }

    #[test]
    fn participant_mix_is_bounded_and_honest() {
        let mix = estimate_participant_mix(&candles(true)).unwrap();
        for value in [
            mix.market_maker,
            mix.trend_follower,
            mix.noise_trader,
            mix.informed_trader,
        ] {
            assert!((0.0..=1.0).contains(&value));
        }
        assert!(mix.limitation.contains("not participant identification"));
        assert!(estimate_participant_mix(&candles(true)[..5]).is_none());
    }

    #[test]
    fn dealer_proxy_reports_its_limitation() {
        let state = estimate_dealer_state(&candles(true)).unwrap();
        assert!(state.confidence >= 0.0 && state.confidence <= 1.0);
        assert!(state.limitation.contains("no options"));
    }

    #[test]
    fn cross_market_transfer_entropy_is_nonnegative() {
        let a = candles(true);
        let b = candles(false);
        let te = cross_market_transfer_entropy(&a, &b).unwrap();
        assert!(te >= 0.0 && te <= 8.0);
    }
}
