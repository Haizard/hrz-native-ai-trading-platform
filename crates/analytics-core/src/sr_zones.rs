//! Support/resistance zones: confirmed swings that keep arriving at one price.
//!
//! ## Why this is separate from [`crate::regions`]
//!
//! `regions` answers "where did the impulse that broke structure start" —
//! supply and demand bands with one origin each. This module answers the other
//! question a chart's levels are drawn from: "where does price keep turning".
//! A level the market has respected three times is a different object from the
//! origin of one impulse, and neither detector can speak for the other.
//!
//! ## The rule, pinned
//!
//! All confirmed swings — highs and lows together — are clustered by price
//! within `tolerance_pct`. A level does not care whether it was made by a high
//! or a low, and mixing them is what lets a broken resistance return as
//! support. One touch is a point; a zone needs at least `min_touches`. Support
//! or resistance is decided against the last close, because that is how the
//! level will be traded.

use serde::{Deserialize, Serialize};

use crate::market_structure::MarketStructure;

/// Tuning for [`detect_sr_zones`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SrZoneConfig {
    /// Cluster tolerance, as a fraction of price: two swings this close are
    /// the same level.
    pub tolerance_pct: f64,
    /// Minimum touches for a cluster to be a zone.
    pub min_touches: usize,
    /// How many zones to report at most, most significant first.
    pub max_zones: usize,
}

impl Default for SrZoneConfig {
    fn default() -> Self {
        Self {
            tolerance_pct: 0.004,
            min_touches: 2,
            max_zones: 6,
        }
    }
}

/// Which side of the market a zone defends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SrKind {
    /// Below price: where swings keep finding buyers.
    Support,
    /// Above price: where swings keep finding sellers.
    Resistance,
}

impl SrKind {
    /// Canonical name, as a chart label spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Support => "support",
            Self::Resistance => "resistance",
        }
    }
}

/// A band where confirmed swings keep clustering.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SrZone {
    /// Which side of the market defends it, against the last close.
    pub kind: SrKind,
    /// The cluster's highest member.
    pub top: f64,
    /// The cluster's lowest member.
    pub bottom: f64,
    /// How many confirmed swings landed in the band.
    pub touches: usize,
    /// The oldest member's timestamp, unix nanoseconds UTC.
    pub first_time: i64,
    /// The newest member's timestamp, unix nanoseconds UTC.
    pub last_time: i64,
}

/// The support/resistance zones of a structure read, most-touched first.
///
/// Significance is touch count, then recency: a zone the market keeps
/// revisiting outranks one it made once and left.
#[must_use]
pub fn detect_sr_zones(
    structure: &MarketStructure,
    last_close: f64,
    config: SrZoneConfig,
) -> Vec<SrZone> {
    let mut members: Vec<(f64, i64)> = structure
        .points
        .iter()
        .map(|p| (p.price, p.timestamp))
        .collect();
    members.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut zones: Vec<SrZone> = Vec::new();
    let mut cluster: Vec<(f64, i64)> = Vec::new();
    let mut cluster_mean = 0.0_f64;

    for member in members {
        let within = !cluster.is_empty()
            && cluster_mean > 0.0
            && (member.0 - cluster_mean).abs() / cluster_mean <= config.tolerance_pct;
        if within {
            cluster.push(member);
            cluster_mean = cluster.iter().map(|m| m.0).sum::<f64>() / cluster.len() as f64;
            continue;
        }
        flush(&cluster, last_close, config, &mut zones);
        cluster.clear();
        cluster_mean = member.0;
        cluster.push(member);
    }
    flush(&cluster, last_close, config, &mut zones);

    zones.sort_by(|a, b| {
        b.touches
            .cmp(&a.touches)
            .then_with(|| b.last_time.cmp(&a.last_time))
    });
    zones.truncate(config.max_zones);
    zones
}

/// One finished cluster: a zone when it has the touches, nothing when not.
fn flush(
    cluster: &[(f64, i64)],
    last_close: f64,
    config: SrZoneConfig,
    zones: &mut Vec<SrZone>,
) {
    if cluster.len() < config.min_touches {
        return;
    }
    let top = cluster.iter().map(|m| m.0).fold(f64::NEG_INFINITY, f64::max);
    let bottom = cluster.iter().map(|m| m.0).fold(f64::INFINITY, f64::min);
    let mean = cluster.iter().map(|m| m.0).sum::<f64>() / cluster.len() as f64;
    let first_time = cluster.iter().map(|m| m.1).fold(i64::MAX, i64::min);
    let last_time = cluster.iter().map(|m| m.1).fold(i64::MIN, i64::max);
    let kind = if mean >= last_close {
        SrKind::Resistance
    } else {
        SrKind::Support
    };
    zones.push(SrZone {
        kind,
        top,
        bottom,
        touches: cluster.len(),
        first_time,
        last_time,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_structure::detect_market_structure;
    use crate::types::{Candle, Timeframe};

    fn candle(i: i64, open: f64, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            symbol: "TEST".into(),
            timeframe: Timeframe::H1,
            open_time: i * 3_600_000_000_000,
            open,
            high,
            low,
            close,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    /// A zigzag between ~97 and ~103, legs of five candles each.
    fn zigzag() -> Vec<Candle> {
        let mut series = Vec::new();
        let mut price = 100.0_f64;
        let mut index = 0_i64;
        for leg in 0..24_i64 {
            let target = if leg % 2 == 0 {
                103.0 + (leg % 3) as f64 * 0.1
            } else {
                97.0 - (leg % 3) as f64 * 0.1
            };
            let step = (target - price) / 5.0;
            for _ in 0..5 {
                let open = price;
                price += step;
                series.push(candle(
                    index,
                    open,
                    open.max(price) + 0.15,
                    open.min(price) - 0.15,
                    price,
                ));
                index += 1;
            }
        }
        series
    }

    #[test]
    fn repeated_swings_at_one_price_become_a_zone() {
        let series = zigzag();
        let structure = detect_market_structure(&series, Default::default());
        assert!(!structure.points.is_empty(), "the zigzag must confirm swings");
        let zones = detect_sr_zones(
            &structure,
            series.last().unwrap().close,
            SrZoneConfig::default(),
        );
        assert!(
            !zones.is_empty(),
            "repeated swings at the same prices must cluster into zones"
        );
        assert!(
            zones.iter().all(|z| z.touches >= 2),
            "a one-touch level is a point, not a zone: {zones:?}"
        );
        assert!(
            zones.iter().all(|z| z.top >= z.bottom),
            "a zone's band is ordered: {zones:?}"
        );
    }

    #[test]
    fn a_zone_below_price_is_support_and_above_is_resistance() {
        let series = zigzag();
        let structure = detect_market_structure(&series, Default::default());
        // Park the read in the middle of the band: the zones above it must be
        // resistance, the zones below it support.
        let zones = detect_sr_zones(&structure, 100.0, SrZoneConfig::default());
        let kinds: Vec<_> = zones.iter().map(|z| z.kind).collect();
        assert!(
            kinds.contains(&SrKind::Support) && kinds.contains(&SrKind::Resistance),
            "a band on each side of price: {zones:?}"
        );
    }

    #[test]
    fn a_structure_with_no_repeated_levels_has_no_zones() {
        // Steadily rising swings never revisit a price.
        let series: Vec<Candle> = (0..40_i64)
            .map(|i| {
                let base = 100.0 + i as f64 * 1.5;
                candle(i, base, base + 1.0, base - 1.0, base + 0.5)
            })
            .collect();
        let structure = detect_market_structure(&series, Default::default());
        let zones = detect_sr_zones(&structure, 160.0, SrZoneConfig::default());
        assert!(
            zones.is_empty(),
            "no price is touched twice, so there is no zone: {zones:?}"
        );
    }
}
