//! Volume profile with memory -- how each level's delta *developed*.
//!
//! A traditional volume profile is a final picture: it shows how much traded
//! at each price, and nothing about the path. But a level that was heavily
//! sold early and heavily bought later is a different animal from one that
//! traded flat all session -- the first *flipped control*, and levels that
//! flip behave differently afterwards. This module keeps, per price level,
//! the per-candle delta history over a session: the "profile with memory"
//! feature, and the structured fact behind theses like *"this level was
//! defended, flipped control at 09:40, and is currently held by buyers."*
//!
//! ## Shape
//!
//! Levels exist only where volume actually traded -- unlike the snapshot
//! volume profile, which keeps empty buckets because an imbalance needs a
//! neighboring level, a memory profile's empty levels carry no history at
//! all. Sessions follow the same **UTC calendar day** rule as
//! [`crate::cvd`], so "the current session's profile" is well-defined for a
//! 24/7 market.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::cvd::session_of;
use crate::types::{Candle, Trade};

/// One candle's delta at one level: a single memory entry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LevelDeltaVisit {
    /// Open time of the candle that touched this level.
    pub open_time: i64,
    /// `ask - bid` at this level within that candle.
    pub delta: f64,
    /// Volume traded at this level within that candle.
    pub volume: f64,
}

/// One price level's accumulating history over a session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LevelMemory {
    /// Bucket midpoint price.
    pub price_level: f64,
    /// Total volume traded at this level in the session.
    pub volume: f64,
    /// Buy-aggressed volume at this level in the session.
    pub buy_volume: f64,
    /// Sell-aggressed volume at this level in the session.
    pub sell_volume: f64,
    /// Per-candle delta history, oldest first. One entry per candle that
    /// touched the level; this is the "memory."
    pub visits: Vec<LevelDeltaVisit>,
    /// Highest single-candle delta this level ever printed.
    pub max_delta: f64,
    /// Lowest single-candle delta this level ever printed.
    pub min_delta: f64,
}

impl LevelMemory {
    /// Session delta at this level: `buy_volume - sell_volume`.
    #[must_use]
    pub fn delta(&self) -> f64 {
        self.buy_volume - self.sell_volume
    }

    /// The side currently dominant at this level, by session delta.
    #[must_use]
    pub fn dominant_side(&self) -> Option<crate::types::Side> {
        match self.delta() {
            d if d > f64::EPSILON => Some(crate::types::Side::Buy),
            d if d < -f64::EPSILON => Some(crate::types::Side::Sell),
            _ => None,
        }
    }

    /// Whether the level's *cumulative* delta changed sign across the
    /// session's visits: control genuinely flipped rather than merely
    /// oscillating around a dominant side.
    ///
    /// Zeros are skipped (a level can trade exactly balanced in one candle);
    /// the flip is defined on nonzero signs only.
    #[must_use]
    pub fn flipped_control(&self) -> bool {
        let mut last_sign = 0_i8;
        for visit in &self.visits {
            let sign = if visit.delta > f64::EPSILON {
                1
            } else if visit.delta < -f64::EPSILON {
                -1
            } else {
                continue;
            };
            if last_sign != 0 && sign != last_sign {
                return true;
            }
            last_sign = sign;
        }
        false
    }
}

/// One session's accumulating profile-with-memory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileMemory {
    /// Open time of the session this profile covers (the UTC day start).
    pub session_open_time: i64,
    /// Width of each price bucket.
    pub bucket_size: f64,
    /// Total session volume across every level.
    pub total_volume: f64,
    /// Levels with actual traded volume, ascending by price.
    pub levels: Vec<LevelMemory>,
}

impl ProfileMemory {
    /// Whether no level traded this session.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// The level nearest `price`, if one exists within half a bucket.
    #[must_use]
    pub fn level_at(&self, price: f64) -> Option<&LevelMemory> {
        let index = (price / self.bucket_size).round();
        self.levels
            .iter()
            .find(|l| (l.price_level / self.bucket_size).round() - index == 0.0)
    }
}

/// Upper bound on tracked levels per session, matching the snapshot profile's
/// resource guard.
const MAX_LEVELS: usize = 100_000;

/// Build the per-session profile-with-memory series.
///
/// `candles` and `trades` must both be sorted ascending by time; trades are
/// bucketed into candles by binary search, exactly as
/// [`crate::footprint::build_footprints`] does, so a footprint and a memory
/// profile over the same window always agree about what traded where.
///
/// One [`ProfileMemory`] per UTC day covered by the input, oldest first; a
/// session with no trades produces no entry.
#[must_use]
pub fn build_profile_memory(
    candles: &[Candle],
    trades: &[Trade],
    bucket_size: f64,
) -> Vec<ProfileMemory> {
    if candles.is_empty() || !bucket_size.is_finite() || bucket_size <= 0.0 {
        return Vec::new();
    }

    let mut sessions: Vec<ProfileMemory> = Vec::new();
    let mut nodes: BTreeMap<i64, LevelMemory> = BTreeMap::new();

    for candle in candles {
        let session = session_of(candle.open_time);
        if sessions.last().map_or(true, |s| s.session_open_time != session) {
            // Flush the previous session's nodes before starting a new one.
            if !nodes.is_empty() {
                if let Some(current) = sessions.last_mut() {
                    flush(&mut nodes, current);
                }
            }
            nodes.clear();
            sessions.push(ProfileMemory {
                session_open_time: session * crate::cvd::NS_PER_DAY,
                bucket_size,
                total_volume: 0.0,
                levels: Vec::new(),
            });
        }
        if nodes.len() >= MAX_LEVELS {
            continue;
        }

        let end = candle.open_time + candle.timeframe.nanos();
        let from = trades.partition_point(|t| t.timestamp < candle.open_time);
        let to = trades.partition_point(|t| t.timestamp < end);

        // Per-level accumulation for this candle only.
        let mut candle_levels: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
        for trade in &trades[from..to] {
            let bucket = (trade.price / bucket_size).round() as i64;
            let entry = candle_levels.entry(bucket).or_insert((0.0, 0.0));
            if trade.is_buyer_maker {
                entry.1 += trade.quantity;
            } else {
                entry.0 += trade.quantity;
            }
        }

        for (bucket, (buy, sell)) in candle_levels {
            let level = nodes.entry(bucket).or_default();
            level.price_level = bucket as f64 * bucket_size;
            level.buy_volume += buy;
            level.sell_volume += sell;
            let delta = buy - sell;
            let volume = buy + sell;
            level.volume += volume;
            level.max_delta = level.max_delta.max(delta);
            level.min_delta = level.min_delta.min(delta);
            level.visits.push(LevelDeltaVisit {
                open_time: candle.open_time,
                delta,
                volume,
            });
        }
    }

    if let Some(current) = sessions.last_mut() {
        flush(&mut nodes, current);
    }
    sessions.into_iter().filter(|s| !s.is_empty()).collect()
}

/// Move the accumulated nodes into a session profile, ascending by price.
fn flush(nodes: &mut BTreeMap<i64, LevelMemory>, profile: &mut ProfileMemory) {
    profile.levels = std::mem::take(nodes).into_values().collect();
    profile.levels.reverse(); // BTreeMap drains descending; restore ascending.
    profile.total_volume = profile.levels.iter().map(|l| l.volume).sum();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Timeframe, Trade};

    fn candle(open_time: i64) -> Candle {
        Candle {
            symbol: "TEST".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: 0.0,
            buy_volume: 0.0,
            sell_volume: 0.0,
        }
    }

    fn trade(price: f64, qty: f64, buyer_maker: bool, ts: i64) -> Trade {
        Trade {
            symbol: "TEST".into(),
            trade_id: ts as u64,
            price,
            quantity: qty,
            is_buyer_maker: buyer_maker,
            timestamp: ts,
        }
    }

    #[test]
    fn visits_accumulate_across_candles() {
        let candles = vec![candle(0), candle(60_000_000_000)];
        let trades = vec![
            trade(100.0, 5.0, false, 1_000),
            trade(100.0, 2.0, true, 61_000_000_000),
        ];

        let sessions = build_profile_memory(&candles, &trades, 1.0);
        assert_eq!(sessions.len(), 1);
        let level = &sessions[0].levels[0];
        assert_eq!(level.visits.len(), 2, "one visit per touching candle");
        assert!((level.volume - 7.0).abs() < 1e-9);
        assert!((level.max_delta - 5.0).abs() < 1e-9);
        assert!((level.min_delta - (-2.0)).abs() < 1e-9);
    }

    #[test]
    fn control_flip_is_detected() {
        let candles = vec![candle(0), candle(60_000_000_000)];
        // Candle 1 sells the level, candle 2 buys it back harder: a flip.
        let trades = vec![
            trade(100.0, 10.0, true, 1_000),
            trade(100.0, 20.0, false, 61_000_000_000),
        ];

        let sessions = build_profile_memory(&candles, &trades, 1.0);
        let level = &sessions[0].levels[0];
        assert!(level.flipped_control());
        assert_eq!(level.dominant_side(), Some(crate::types::Side::Buy));
    }

    #[test]
    fn sessions_reset_on_the_utc_day() {
        let day = crate::cvd::NS_PER_DAY;
        let candles = vec![candle(0), candle(day)];
        let trades = vec![
            trade(100.0, 1.0, false, 1_000),
            trade(100.0, 2.0, false, day + 1_000),
        ];

        let sessions = build_profile_memory(&candles, &trades, 1.0);
        assert_eq!(sessions.len(), 2);
        assert!((sessions[0].levels[0].volume - 1.0).abs() < 1e-9);
        assert!((sessions[1].levels[0].volume - 2.0).abs() < 1e-9);
    }
}
