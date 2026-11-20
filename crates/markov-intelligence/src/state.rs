//! Regime vocabulary and market-feature encoding.
//!
//! A regime is a hidden state the engine infers from observable features. This
//! module is pure data: it never fetches prices and never talks to an agent.

use serde::{Deserialize, Serialize};

/// The five regimes the first engine version models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Regime {
    BullTrend,
    BearTrend,
    Sideways,
    HighVolatility,
    LowVolatility,
}

impl Regime {
    pub const ALL: [Regime; 5] = [
        Regime::BullTrend,
        Regime::BearTrend,
        Regime::Sideways,
        Regime::HighVolatility,
        Regime::LowVolatility,
    ];

    pub const fn index(self) -> usize {
        match self {
            Regime::BullTrend => 0,
            Regime::BearTrend => 1,
            Regime::Sideways => 2,
            Regime::HighVolatility => 3,
            Regime::LowVolatility => 4,
        }
    }

    pub const fn from_index(index: usize) -> Self {
        match index {
            0 => Regime::BullTrend,
            1 => Regime::BearTrend,
            2 => Regime::Sideways,
            3 => Regime::HighVolatility,
            _ => Regime::LowVolatility,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Regime::BullTrend => "bull_trend",
            Regime::BearTrend => "bear_trend",
            Regime::Sideways => "sideways",
            Regime::HighVolatility => "high_volatility",
            Regime::LowVolatility => "low_volatility",
        }
    }
}

/// Observable features used by the Markov engines.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MarketFeatures {
    pub return_1: f64,
    pub atr_pct: f64,
    pub volume_ratio: f64,
    pub momentum: f64,
    pub delta: f64,
    pub distance_from_vwap: f64,
    pub spread: f64,
}

impl MarketFeatures {
    /// Feature vector in stable order (TVTP betas bind to this order).
    pub fn vector(self) -> [f64; 7] {
        [
            self.return_1,
            self.atr_pct,
            self.volume_ratio,
            self.momentum,
            self.delta,
            self.distance_from_vwap,
            self.spread,
        ]
    }

    /// A deterministic classification used as the starting state. Non-finite
    /// features fall back to `Sideways` so a bad feed cannot poison the engine.
    pub fn classify(self, atr_reference: f64) -> Regime {
        let values = self.vector();
        if values.iter().any(|value| !value.is_finite()) {
            return Regime::Sideways;
        }
        if atr_reference.is_finite() && atr_reference > 0.0 && self.atr_pct > atr_reference * 1.5 {
            return Regime::HighVolatility;
        }
        if atr_reference.is_finite() && atr_reference > 0.0 && self.atr_pct < atr_reference * 0.5 {
            return Regime::LowVolatility;
        }
        let direction = self.momentum + self.delta + self.distance_from_vwap;
        if direction > 0.0 {
            Regime::BullTrend
        } else if direction < 0.0 {
            Regime::BearTrend
        } else {
            Regime::Sideways
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regimes_round_trip_through_their_index() {
        for regime in Regime::ALL {
            assert_eq!(Regime::from_index(regime.index()), regime);
        }
    }

    #[test]
    fn bad_features_fall_back_to_sideways() {
        let features = MarketFeatures {
            return_1: f64::NAN,
            atr_pct: 1.0,
            volume_ratio: 1.0,
            momentum: 1.0,
            delta: 1.0,
            distance_from_vwap: 1.0,
            spread: 0.1,
        };
        assert_eq!(features.classify(1.0), Regime::Sideways);
    }

    #[test]
    fn volatility_extremes_are_classified_first() {
        let base = MarketFeatures {
            return_1: 0.0,
            atr_pct: 2.0,
            volume_ratio: 1.0,
            momentum: 1.0,
            delta: 1.0,
            distance_from_vwap: 1.0,
            spread: 0.1,
        };
        assert_eq!(base.classify(1.0), Regime::HighVolatility);
        assert_eq!(
            MarketFeatures {
                atr_pct: 0.2,
                ..base
            }
            .classify(1.0),
            Regime::LowVolatility
        );
    }
}
