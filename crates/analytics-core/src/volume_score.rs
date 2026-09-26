//! Volume-weighted aggression score -- how one-sided a candle was, scaled by
//! how unusual its volume was.
//!
//! ## Why this module exists
//!
//! The vocabulary can read `delta`, `volume`, `buy_volume` and `sell_volume`,
//! but every condition on them compares a raw quantity -- and raw quantities
//! are not comparable across symbols, sessions or regimes. "Strong buy
//! pressure" is a *ratio* (who won) weighted by a *ratio* (how much business
//! was done compared with normal), and neither ratio is writable in a grammar
//! with no arithmetic. This module is that measurement, once, tested, shared.
//!
//! ## The formula, and why it is this shape
//!
//! ```text
//! delta_share = (buy_volume - sell_volume) / volume        // [-1, +1]
//! relative    = volume / baseline                          // 1.0 = average
//! strength    = clamp((relative - 1) / 2, 0, 1)            // 1.0 at 3x average
//! score       = delta_share * strength                     // [-1, +1]
//! ```
//!
//! The baseline is the mean volume of the `relative_period` candles **before"
//! the one being scored. Including the candle itself would dilute a spike with
//!
//! Multiplying rather than adding is the whole point: a quiet candle that hit
//! the ask once prints a high `delta_share` but near-zero `strength`, and the
//! score refuses to call it pressure. A 3x-average candle that is entirely
//! buy-aggressed is a full `+1.0`. The sign is always the aggressor, so the
//! score composes with the same `==` / `>` conditions every other numeric
//! field uses.

use serde::{Deserialize, Serialize};

use crate::types::Candle;

/// Default lookback for the volume baseline.
pub const DEFAULT_RELATIVE_PERIOD: usize = 20;

/// Tuning for [`volume_scores`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VolumeScoreConfig {
    /// Candles in the volume baseline. A 20-bar baseline is the convention on
    /// every relative-volume readout; shorter chases the regime it measures.
    pub relative_period: usize,
    /// Candles below this relative volume score zero regardless of delta --
    /// the noise floor that stops a one-lot print in a dead market from
    /// reading as conviction.
    pub min_relative_volume: f64,
}

impl Default for VolumeScoreConfig {
    fn default() -> Self {
        Self {
            relative_period: DEFAULT_RELATIVE_PERIOD,
            min_relative_volume: 0.5,
        }
    }
}

/// One score per candle, oldest first.
///
/// A candle whose baseline is not yet warmed (`volume` history shorter than
/// `relative_period`) scores `None` rather than a number built from a
/// too-short window -- the same warm-up honesty `indicators::rsi` keeps, and
/// for the same reason: a partial average is a number that lies.
#[must_use]
pub fn volume_scores(candles: &[Candle], config: &VolumeScoreConfig) -> Vec<Option<f64>> {
    let baseline = volume_baseline(candles, config.relative_period);
    candles
        .iter()
        .enumerate()
        .map(|(index, candle)| {
            let mean = baseline[index]?;
            Some(score_of(candle, mean, config))
        })
        .collect()
}

/// The newest score, or `None` during warm-up.
#[must_use]
pub fn latest_volume_score(candles: &[Candle], config: &VolumeScoreConfig) -> Option<f64> {
    volume_scores(candles, config).pop().flatten()
}

/// The trailing mean volume of the `period` candles **before** each candle,
/// `None` until that many candles exist.
///
/// Excluding the candle being scored is the design, not an oversight: a 3x
/// spike inside its own baseline raises the baseline it is measured against
/// and reads 2.9x instead of 3x -- a small lie on exactly the candles the
/// score exists to flag.
///
/// One walk, oldest to newest, keeping a running sum: O(n) where the obvious
/// slice-average-per-candle loop is O(n * period) -- a week of 1m candles
/// with a 20-bar baseline is ten thousand candles either way, but the wasm
/// frame this runs in is shared with drawing.
fn volume_baseline(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    if period == 0 {
        return vec![None; candles.len()];
    }
    #[allow(clippy::cast_precision_loss)]
    let divisor = period as f64;

    let mut out = Vec::with_capacity(candles.len());
    let mut sum = 0.0;
    for (index, candle) in candles.iter().enumerate() {
        out.push(if index >= period {
            Some(sum / divisor)
        } else {
            None
        });
        sum += candle.volume;
        if index + 1 > period {
            sum -= candles[index - period].volume;
        }
    }
    out
}

/// The score of one candle against a known baseline.
fn score_of(candle: &Candle, baseline: f64, config: &VolumeScoreConfig) -> f64 {
    if !candle.volume.is_finite() || !baseline.is_finite() || baseline <= 0.0 {
        return 0.0;
    }
    let relative = candle.volume / baseline;
    if relative < config.min_relative_volume {
        return 0.0;
    }

    let delta_share = if candle.volume > 0.0 {
        candle.delta() / candle.volume
    } else {
        0.0
    };
    // `delta_share` is mathematically within [-1, 1]; a corrupt feed can break
    // that, and a score outside the range would poison every comparison built
    // on the documented range.
    let delta_share = delta_share.clamp(-1.0, 1.0);
    let strength = ((relative - 1.0) / 2.0).clamp(0.0, 1.0);
    delta_share * strength
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn candle(index: i64, volume: f64, buy: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time: index * Timeframe::M1.nanos(),
            open: 100.0,
            high: 100.5,
            low: 99.5,
            close: 100.0,
            volume,
            buy_volume: buy,
            sell_volume: volume - buy,
        }
    }

    #[test]
    fn an_average_volume_candle_is_not_pressure() {
        // 20 quiet candles, then one at the same volume entirely buy-aggressed.
        let mut candles: Vec<Candle> = (0..20).map(|i| candle(i64::from(i), 10.0, 5.0)).collect();
        candles.push(candle(20, 10.0, 10.0));
        let config = VolumeScoreConfig::default();
        let score = latest_volume_score(&candles, &config).expect("the baseline is warmed");
        // Relative volume is 1.0 -> strength 0; delta share is +1.0. The
        // multiplication is the design: average volume is not pressure.
        assert!((score - 0.0).abs() < 1e-9, "{score}");
    }

    #[test]
    fn a_three_times_volume_fully_one_sided_candle_scores_one() {
        let mut candles: Vec<Candle> = (0..20).map(|i| candle(i64::from(i), 10.0, 5.0)).collect();
        candles.push(candle(20, 30.0, 30.0));
        let config = VolumeScoreConfig::default();
        let score = latest_volume_score(&candles, &config).expect("the baseline is warmed");
        assert!((score - 1.0).abs() < 1e-9, "{score}");
    }

    #[test]
    fn heavy_selling_scores_negative_and_the_range_holds() {
        let mut candles: Vec<Candle> = (0..20).map(|i| candle(i64::from(i), 10.0, 5.0)).collect();
        candles.push(candle(20, 30.0, 0.0));
        let config = VolumeScoreConfig::default();
        let score = latest_volume_score(&candles, &config).expect("the baseline is warmed");
        assert!((score + 1.0).abs() < 1e-9, "{score}");

        // And a corrupt candle whose buy/sell split overflows its volume is
        // clamped, not propagated.
        candles.push(candle(21, 30.0, 40.0));
        let score = latest_volume_score(&candles, &config).expect("the baseline is warmed");
        assert!((-1.0..=1.0).contains(&score), "{score}");
    }

    #[test]
    fn the_noise_floor_silences_a_one_lot_in_a_dead_market() {
        let mut candles: Vec<Candle> = (0..20).map(|i| candle(i64::from(i), 1000.0, 500.0)).collect();
        // One-tenth of the baseline, entirely buy-aggressed.
        candles.push(candle(20, 100.0, 100.0));
        let config = VolumeScoreConfig::default();
        assert_eq!(
            latest_volume_score(&candles, &config),
            Some(0.0),
            "below the noise floor there is no signal"
        );
    }

    #[test]
    fn warm_up_is_none_not_a_number_from_a_short_window() {
        let candles = vec![candle(0, 10.0, 6.0), candle(1, 12.0, 8.0), candle(2, 8.0, 4.0)];
        let scores = volume_scores(&candles, &VolumeScoreConfig::default());
        assert_eq!(scores, vec![None, None, None]);
    }

    #[test]
    fn the_baseline_walk_matches_the_obvious_slice_average() {
        let candles: Vec<Candle> = (0..25)
            .map(|i| candle(i64::from(i), f64::from(i) + 1.0, f64::from(i)))
            .collect();
        let baseline = volume_baseline(&candles, 5);
        // Warmed from index 5: five candles must exist *before* the one being
        // measured. Volumes are 1, 2, 3... so the mean of the five before
        // index i is i - 2.
        for (index, value) in baseline.iter().enumerate().skip(5) {
            let expected = index as f64 - 2.0;
            assert!(
                (value.expect("warmed here") - expected).abs() < 1e-9,
                "index {index}: got {value:?}, expected {expected}"
            );
        }
        // And the first five are honest warm-up, not short averages.
        assert!(baseline[..5].iter().all(|v| v.is_none()));
    }

    #[test]
    fn a_zero_period_config_warms_never() {
        let candles = vec![candle(0, 10.0, 6.0)];
        let config = VolumeScoreConfig {
            relative_period: 0,
            ..VolumeScoreConfig::default()
        };
        assert_eq!(volume_scores(&candles, &config), vec![None]);
    }
}
