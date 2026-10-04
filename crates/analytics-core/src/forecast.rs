//! Monte Carlo forecast cones (docs/35): where could price plausibly go?
//!
//! A stationary-bootstrap resampling of the window's **own** log returns:
//! each simulated path walks forward from the last close, multiplying by a
//! randomly redrawn historical return per step. No drift is fitted and no
//! distribution is assumed — the cone is the series' own recent behavior,
//! replayed in shuffled order, which is exactly what a bootstrap promises
//! and nothing more.
//!
//! Two properties are load-bearing:
//!
//! * **Determinism.** The sampler is a seeded xorshift, and the caller
//!   derives the seed from the data window. Same window, same cone, every
//!   frame — a forecast that reshuffled itself on every redraw would read
//!   as a malfunction, not a model.
//! * **Honesty at the boundary.** Fewer than [`MIN_FORECAST_RETURNS`]
//!   usable returns returns `None`: a cone fitted to three moves would be a
//!   costume, and the caller surfaces the refusal rather than drawing it.

/// The quantiles computed at each step: the cone's outer band, inner band,
/// and median.
pub const FORECAST_QUANTILES: [f64; 5] = [0.05, 0.25, 0.50, 0.75, 0.95];

/// Longest horizon a request may ask for, in bars.
pub const MAX_FORECAST_STEPS: usize = 120;

/// Most paths a request may ask for. The default is far below this; the cap
/// exists so a hostile or mistaken request cannot turn one frame into a
/// second of sorting.
pub const MAX_FORECAST_PATHS: usize = 2000;

/// Fewer usable returns than this and the forecast refuses: the resampling
/// pool would be a anecdote, not a distribution.
pub const MIN_FORECAST_RETURNS: usize = 8;

/// What to simulate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForecastSpec {
    /// Horizon in bars.
    pub steps: usize,
    /// Number of simulated paths.
    pub paths: usize,
    /// The caller's seed. The engine derives it from the data window so the
    /// cone is stable across frames of the same view.
    pub seed: u64,
    /// The what-if knob (docs/35's what-if slice): every sampled return is
    /// multiplied by this before a path takes it, so 2.0 asks "what if the
    /// window's moves were twice as big" without fitting anything. 1.0 is
    /// the honest default; the clamp lives in [`bootstrap_forecast`].
    pub vol_scale: f64,
}

/// One forecast cone: the starting price and, per step, the five quantile
/// values in [`FORECAST_QUANTILES`] order.
#[derive(Debug, Clone, PartialEq)]
pub struct Forecast {
    /// The price every path starts from (the window's last close).
    pub start: f64,
    /// The path count actually simulated (after clamping), echoed so the
    /// drawing's label states the sample size honestly.
    pub paths: usize,
    /// `steps[k][q]` is the q-th quantile of the simulated prices at step
    /// `k + 1` bars out.
    pub steps: Vec<[f64; 5]>,
}

/// A seeded xorshift64* — small, fast, and deterministic across every build
/// of the crate, which a system-global RNG would not be.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A uniform index into `0..len`, via multiply-high so the low-bit
    /// modulo bias never picks favorites over a small return pool.
    fn below(&mut self, len: usize) -> usize {
        ((self.next() as u128 * len as u128) >> 64) as usize
    }
}

/// Resample the closes' own log returns into a cone of `spec.paths` paths,
/// `spec.steps` bars out.
///
/// `closes` is the window's close series, oldest first. Returns `None` when
/// fewer than [`MIN_FORECAST_RETURNS`] usable returns exist — non-positive
/// or non-finite closes produce no return and simply do not join the pool.
pub fn bootstrap_forecast(closes: &[f64], spec: &ForecastSpec) -> Option<Forecast> {
    let returns: Vec<f64> = closes
        .windows(2)
        .filter(|pair| pair[0].is_finite() && pair[1].is_finite() && pair[0] > 0.0 && pair[1] > 0.0)
        .map(|pair| (pair[1] / pair[0]).ln())
        .collect();
    if returns.len() < MIN_FORECAST_RETURNS {
        return None;
    }
    let start = *closes.last().filter(|last| last.is_finite() && **last > 0.0)?;
    let steps = spec.steps.clamp(1, MAX_FORECAST_STEPS);
    let paths = spec.paths.clamp(16, MAX_FORECAST_PATHS);
    // The what-if scale: 0 freezes every path at the last price (the honest
    // "no movement" case), 3x is the clamp -- past that the cone stops being
    // about this window and starts being a fireworks show.
    let vol_scale = if spec.vol_scale.is_finite() {
        spec.vol_scale.clamp(0.0, 3.0)
    } else {
        1.0
    };

    // Walk every path first, then read quantiles across paths per step: the
    // quantile is a statement about the paths' spread at a horizon, not
    // about one path's journey. The seed is spread by the golden-ratio
    // constant so adjacent seeds draw genuinely different shuffles (a bare
    // `| 1` folds 2k and 2k+1 onto the same state); zero folds onto one,
    // the one degenerate state xorshift cannot start from.
    let mut rng = XorShift(spec.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).max(1));
    let mut at_step: Vec<Vec<f64>> = vec![Vec::with_capacity(paths); steps];
    for _ in 0..paths {
        let mut price = start;
        for (k, bucket) in at_step.iter_mut().enumerate() {
            price *= (returns[rng.below(returns.len())] * vol_scale).exp();
            let _ = k;
            bucket.push(price);
        }
    }

    let steps = at_step
        .into_iter()
        .map(|mut bucket| {
            bucket.sort_by(f64::total_cmp);
            let n = bucket.len();
            let mut quantiles = [0.0; 5];
            for (slot, q) in quantiles.iter_mut().zip(FORECAST_QUANTILES) {
                // Nearest-rank on the sorted bucket.
                let rank = (q * (n - 1) as f64).round() as usize;
                *slot = bucket[rank.min(n - 1)];
            }
            quantiles
        })
        .collect();
    Some(Forecast { start, paths, steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ForecastSpec {
        ForecastSpec { steps: 30, paths: 200, seed: 42, vol_scale: 1.0 }
    }

    /// A gently rising series with realistic wiggle.
    fn closes(n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| 100.0 + i as f64 * 0.3 + ((i * 7919) % 7) as f64 - 3.0)
            .collect()
    }

    #[test]
    fn the_same_window_forecasts_the_same_cone() {
        let a = bootstrap_forecast(&closes(120), &spec()).expect("a cone");
        let b = bootstrap_forecast(&closes(120), &spec()).expect("a cone");
        assert_eq!(a, b, "determinism is the contract");
    }

    #[test]
    fn a_different_seed_shuffles_the_cone_but_not_the_start() {
        let a = bootstrap_forecast(&closes(120), &spec()).expect("a cone");
        let b = bootstrap_forecast(
            &closes(120),
            &ForecastSpec { seed: 43, ..spec() },
        )
        .expect("a cone");
        assert_eq!(a.start, b.start);
        assert_ne!(a.steps, b.steps, "a different shuffle is a different draw");
    }

    #[test]
    fn quantiles_are_ordered_at_every_step() {
        let cone = bootstrap_forecast(&closes(120), &spec()).expect("a cone");
        assert_eq!(cone.steps.len(), 30);
        for (k, step) in cone.steps.iter().enumerate() {
            for pair in step.windows(2) {
                assert!(pair[0] <= pair[1], "step {k}: {step:?}");
            }
        }
    }

    #[test]
    fn a_flat_series_forecasts_a_flat_cone() {
        let flat: Vec<f64> = vec![100.0; 60];
        let cone = bootstrap_forecast(&flat, &spec()).expect("a cone");
        for step in &cone.steps {
            for value in step {
                assert_eq!(*value, 100.0, "zero returns cannot wander");
            }
        }
    }

    #[test]
    fn too_few_returns_refuses() {
        let thin = [100.0, 101.0, 100.5, 101.5];
        assert_eq!(bootstrap_forecast(&thin, &spec()), None);
    }

    #[test]
    fn the_caps_clamp_a_runaway_request() {
        let big = ForecastSpec { steps: 99_999, paths: 99_999, seed: 1, vol_scale: 1.0 };
        let cone = bootstrap_forecast(&closes(120), &big).expect("a cone");
        assert_eq!(cone.steps.len(), MAX_FORECAST_STEPS);
    }

    #[test]
    fn the_what_if_scale_widens_the_cone_without_reshuffling_it() {
        // docs/35's what-if slice: 2x vol is the SAME shuffled draws, each
        // twice as big -- wider cone, same seed, so the comparison the user
        // makes between the two settings is honest.
        let base = bootstrap_forecast(&closes(120), &spec()).expect("a cone");
        let doubled = bootstrap_forecast(&closes(120), &ForecastSpec { vol_scale: 2.0, ..spec() })
            .expect("a cone");
        assert_eq!(base.start, doubled.start);
        let last = base.steps.len() - 1;
        let base_spread = base.steps[last][4] - base.steps[last][0];
        let doubled_spread = doubled.steps[last][4] - doubled.steps[last][0];
        assert!(
            doubled_spread > base_spread,
            "2x vol widens the 5-95 spread: {base_spread} vs {doubled_spread}"
        );
        // And the same scale twice is still deterministic.
        let again = bootstrap_forecast(&closes(120), &ForecastSpec { vol_scale: 2.0, ..spec() })
            .expect("a cone");
        assert_eq!(doubled, again);
    }

    #[test]
    fn a_zero_scale_freezes_every_path_at_the_last_close() {
        let frozen = bootstrap_forecast(&closes(120), &ForecastSpec { vol_scale: 0.0, ..spec() })
            .expect("a cone");
        for step in &frozen.steps {
            for value in step {
                assert_eq!(*value, frozen.start, "no movement means no movement");
            }
        }
        // Non-finite input falls back to the honest default rather than NaN
        // into every quantile.
        let nan = bootstrap_forecast(
            &closes(120),
            &ForecastSpec { vol_scale: f64::NAN, ..spec() },
        )
        .expect("a cone");
        let base = bootstrap_forecast(&closes(120), &spec()).expect("a cone");
        assert_eq!(nan, base, "NaN scale behaves as 1.0");
    }
}
