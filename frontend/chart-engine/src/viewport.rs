//! What part of the series the chart is showing, and how a gesture moves it
//! (`docs/14-FRONTEND-CHART-ENGINE.md`).
//!
//! ## Why this arithmetic is here and not in the shell
//!
//! `docs/14` keeps market arithmetic out of JavaScript: a second implementation
//! of a trading calculation must never exist, because the two copies drift and
//! the chart starts lying about the data. Zoom and pan are exactly that kind of
//! arithmetic — they decide *which bars* and *which prices* are on screen — so
//! the transition lives here, where `cargo test -p chart-engine` reaches it
//! without a browser.
//!
//! The shell's whole job is to turn a pointer position into a **fraction of the
//! plot rectangle** and send a gesture. It never computes a bar index and never
//! computes a price. Those are the two numbers a chart gets wrong when two
//! implementations disagree, and keeping them in one place is the point.
//!
//! ## A gesture, not a new viewport
//!
//! The shell sends what the user *did* — "zoom by 1.1 about 40% across" — rather
//! than the viewport it thinks should result. So the clamping, the minimum bar
//! count and the anchor arithmetic have one implementation, and a wheel event
//! that arrives a hundred times a second cannot accumulate a rounding drift the
//! engine would have prevented.

use serde::{Deserialize, Serialize};

/// Fewest bars the chart will show.
///
/// Below this the view stops being a chart: one bar stretched across the plot
/// looks like a data outage, and a wheel tick has nowhere left to go. The limit
/// is what makes "zoomed all the way in" a state the chart reports rather than a
/// blank canvas.
pub const MIN_BARS: usize = 10;

/// How far the price axis may be squeezed relative to the range the visible bars
/// would have produced on their own.
///
/// A price axis with no floor is a division waiting to happen: enough wheel
/// ticks and the span reaches zero, `price_to_y` divides by it, and the canvas
/// silently ignores the `NaN` — a blank chart with no error anywhere.
///
/// "On their own" is the load-bearing part, and it means [`fitted`] and not the
/// current span. A floor relative to the current span shrinks with the thing it
/// is supposed to bound, so it never binds at all.
///
/// [`fitted`]: Viewport::apply
const MIN_PRICE_FRACTION: f64 = 1.0e-6;

/// Largest factor a single gesture may apply.
///
/// A client can send anything. This is the difference between a wheel tick and a
/// request that inverts the axis.
const MAX_FACTOR: f64 = 10.0;

/// Largest shift a single pan may apply, as a fraction of the visible span.
///
/// A drag is a pointer movement divided by the plot's size, so a real one is
/// well under one span and ten is already generous. The cap is here because
/// `fraction * span` overflows to infinity for a large enough fraction, and an
/// infinite price range has a `NaN` span -- an axis [`PriceRange::is_usable`]
/// rejects, which the chart draws as nothing at all.
const MAX_PAN_FRACTION: f64 = 10.0;

/// A price range on the axis.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PriceRange {
    /// Cheapest price shown.
    pub min: f64,
    /// Dearest price shown.
    pub max: f64,
}

impl PriceRange {
    /// The span, never negative and never `NaN`.
    ///
    /// A caller that inverted the pair gets the absolute distance rather than a
    /// negative span, because a negative span propagates into every y coordinate
    /// and produces a chart drawn upside down rather than an error.
    #[must_use]
    pub fn span(&self) -> f64 {
        let span = self.max - self.min;
        if span.is_nan() || span < 0.0 {
            0.0
        } else {
            span
        }
    }

    /// Whether this range can be drawn at all.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.min.is_finite() && self.max.is_finite() && self.span() > 0.0
    }
}

/// Which slice of the series is visible, and at what price range.
///
/// `count: None` and `price: None` are both "the engine decides", which is the
/// state a chart starts in and the state `Fit` returns it to. Keeping them
/// optional rather than pre-filled matters: a shell that had to send `count`
/// would have to know how many bars the series has, and knowing that is the
/// first step towards doing the arithmetic itself.
///
/// The scene reports one of these rather than the [`Window`] it sliced with, so
/// the shell can echo what it was handed instead of copying fields across. See
/// [`Window::as_viewport`] for why that distinction is load-bearing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    /// Index of the first visible bar. Clamped into range on resolve.
    #[serde(default)]
    pub from: usize,
    /// How many bars are visible. `None` means all of them.
    #[serde(default)]
    pub count: Option<usize>,
    /// Price range. `None` fits the visible bars.
    #[serde(default)]
    pub price: Option<PriceRange>,
}

/// A viewport resolved against a series, and therefore drawable.
///
/// Every field here is decided — `count` is a number, `from` is inside the
/// series — and that is what makes it drawable and also what makes it the wrong
/// thing to hand back to the shell. What the scene reports is a [`Viewport`],
/// derived with [`Window::as_viewport`]; this type is the engine's own working
/// form, used to slice the series, and it never reaches the client.
///
/// The distinction matters because the two are *almost* interchangeable. A
/// resolved window and the request that produced it are the same view right up
/// until the series grows, and then they diverge: the request keeps showing
/// everything, the window keeps showing what it was resolved against.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// First visible bar index.
    pub from: usize,
    /// Number of visible bars.
    pub count: usize,
    /// Bars available in total.
    ///
    /// The engine's own bookkeeping, and what [`Window::is_all`] compares
    /// against. Not reported to the client: how many bars exist is a fact about
    /// the series, not something a request gets to assert, and nothing in the
    /// shell needs it -- the scene draws what it is handed.
    pub total: usize,
    /// The price range the axis is on, or `None` for "fitted to the bars".
    ///
    /// Carried through [`Window::as_viewport`], because without it a price zoom
    /// would survive exactly one frame: the shell would send back `price: None`,
    /// the engine would re-fit, and the axis would spring back to the candles'
    /// own range the moment the next one arrived.
    pub price: Option<PriceRange>,
}

impl Window {
    /// One past the last visible bar.
    #[must_use]
    pub fn end(&self) -> usize {
        self.from + self.count
    }

    /// Whether the whole series is on screen.
    #[must_use]
    pub fn is_all(&self) -> bool {
        self.from == 0 && self.count == self.total
    }

    /// The same window, re-anchored to the series' end.
    ///
    /// A live chart has to keep the newest bar on screen: frames arrive while
    /// the user is reading, and a window resolved against the series it was
    /// resolved against keeps showing the same bars while new ones append off
    /// the right edge -- the "live candles only appear after a reload" failure.
    /// Recomputing `from` from the end (rather than shifting it by one) makes
    /// catching up after any number of missed frames the same operation as
    /// following one, and cannot walk `from` past zero on a short series.
    ///
    /// The count and the price range are untouched: following moves the window
    /// along the time axis only, and never rewrites a zoom the user chose.
    #[must_use]
    pub fn followed(self) -> Self {
        Self {
            from: self.total.saturating_sub(self.count),
            ..self
        }
    }

    /// This window as the request that would reproduce it.
    ///
    /// The scene reports *this* rather than the window itself, and the difference
    /// is the whole reason it exists. A window has every field decided, so
    /// echoing one back says "these five hundred bars" where the user had asked
    /// for "all of them". The two are the same view until a new candle arrives,
    /// at which point the first stops including it — and a live chart that
    /// quietly freezes on the candle it was loaded with is a worse failure than
    /// the missing zoom it was meant to fix.
    ///
    /// `is_all()` is exactly the fitted case and only that case: `resolve` clamps
    /// `from` to `total - count`, which is zero whenever the count fills the
    /// series, so a window with `count == total` always starts at bar zero.
    #[must_use]
    pub fn as_viewport(&self) -> Viewport {
        Viewport {
            from: self.from,
            count: if self.is_all() {
                None
            } else {
                Some(self.count)
            },
            price: self.price,
        }
    }
}

/// What the user did, reduced to numbers by the shell.
///
/// Every variant is expressed as a **fraction of the plot rectangle** or a
/// factor, never as a bar index and never as a price. `anchor` runs 0..1 from
/// the left (oldest bar) to the right; a pan's `time` and `price` are fractions
/// of the visible span. Both are things a pointer position gives you directly,
/// and neither requires knowing anything about the series.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Gesture {
    /// Wheel over the time axis. `factor` above 1 zooms in.
    ZoomTime {
        /// How much, above 1 to zoom in.
        factor: f64,
        /// Where to hold still, 0 at the left edge and 1 at the right.
        anchor: f64,
    },
    /// Wheel with the price modifier held. `factor` above 1 zooms in.
    ZoomPrice {
        /// How much, above 1 to zoom in.
        factor: f64,
        /// Where to hold still, 0 at the top of the plot and 1 at the bottom.
        anchor: f64,
    },
    /// Drag. Both axes in one gesture, because a pointer move has both.
    ///
    /// Positive `time` moves the view towards newer bars, positive `price`
    /// towards dearer prices; each is a fraction of the visible span, so a drag
    /// of one plot width is `1.0`.
    ///
    /// One variant rather than two because a drag is one event. Sending it as
    /// two would mean two engine calls and two full repaints per pointer move,
    /// and a rebuild is a wasm call plus a canvas clear -- at pointer rates that
    /// is the difference between a drag and a slideshow.
    Pan {
        /// Shift along time, as a fraction of the visible span.
        time: f64,
        /// Shift along price, as a fraction of the visible price span.
        price: f64,
    },
    /// Show everything: all the bars, and a price axis fitted to them.
    Fit,
}

impl Viewport {
    /// Resolve against a series of `total` bars.
    ///
    /// Total, and safe for `total == 0`: an empty series resolves to an empty
    /// window rather than to a window of [`MIN_BARS`] bars that do not exist.
    #[must_use]
    pub fn resolve(&self, total: usize) -> Window {
        if total == 0 {
            return Window {
                from: 0,
                count: 0,
                total: 0,
                price: self.price,
            };
        }
        // `MIN_BARS.min(total)`: a series shorter than the minimum is shown
        // whole rather than padded with bars that are not there.
        let floor = MIN_BARS.min(total);
        let count = self.count.unwrap_or(total).clamp(floor, total);
        Window {
            from: self.from.min(total - count),
            count,
            total,
            price: self.price,
        }
    }

    /// Move by one gesture.
    ///
    /// `fitted` is the price range the *current* window's bars would produce on
    /// their own, which is what a price gesture starts from when the price axis
    /// has never been moved. The engine knows it because it has already resolved
    /// the window; the shell does not, which is why this is not a method on
    /// [`Gesture`].
    #[must_use]
    pub fn apply(&self, gesture: Gesture, total: usize, fitted: PriceRange) -> Self {
        let window = self.resolve(total);
        if window.count == 0 {
            return *self;
        }

        match gesture {
            Gesture::Fit => Self::default(),

            Gesture::ZoomTime { factor, anchor } => {
                let factor = sane_factor(factor);
                let anchor = sane_anchor(anchor);
                let count = (window.count as f64 / factor).round().max(1.0) as usize;
                let count = count.clamp(MIN_BARS.min(total).max(1), total);

                // Hold the bar under the anchor still: the anchor sits at
                // `from + anchor * count`, and it must sit there afterwards too.
                let anchor_bar = window.from as f64 + anchor * window.count as f64;
                let from = (anchor_bar - anchor * count as f64).round().max(0.0) as usize;

                Self {
                    from,
                    count: Some(count),
                    price: self.price,
                }
            }

            Gesture::ZoomPrice { factor, anchor } => {
                let factor = sane_factor(factor);
                let anchor = sane_anchor(anchor);
                // The floor is measured against the **fitted** range -- what the
                // visible bars produce on their own -- and never against the
                // current span. That distinction is the entire guard, and the
                // first version of this got it wrong: a floor relative to the
                // current span shrinks along with it, so `base.span() / factor`
                // is always the larger of the two and the floor never binds. The
                // axis then halves its way to zero and `price_to_y` divides by
                // it, which a canvas answers by drawing nothing at all.
                if !fitted.is_usable() {
                    return *self;
                }
                let base = self.price.unwrap_or(fitted);
                if !base.is_usable() {
                    return *self;
                }
                let span = (base.span() / factor).max(fitted.span() * MIN_PRICE_FRACTION);
                // `anchor` runs downward, and price runs upward, so the price
                // under the anchor is `max - anchor * span`.
                let held = base.max - anchor * base.span();
                let max = held + anchor * span;
                Self {
                    from: self.from,
                    count: self.count,
                    price: Some(PriceRange {
                        min: max - span,
                        max,
                    }),
                }
            }

            Gesture::Pan { time, price } => {
                let Some(time) = sane_fraction(time) else {
                    return *self;
                };
                let Some(price) = sane_fraction(price) else {
                    return *self;
                };

                // `self.count` is preserved when the drag is purely vertical.
                // Pinning it unconditionally would turn a price drag into a time
                // zoom: a chart that was showing everything would suddenly be
                // showing "these five hundred bars", and would stop growing as
                // new ones arrived.
                let (from, count) = if time == 0.0 {
                    (window.from, self.count)
                } else {
                    let shift = (time * window.count as f64).round();
                    (
                        (window.from as f64 + shift).max(0.0) as usize,
                        Some(window.count),
                    )
                };

                let range = if price == 0.0 {
                    self.price
                } else {
                    match self.price.unwrap_or(fitted) {
                        base if base.is_usable() => {
                            let shift = price * base.span();
                            Some(PriceRange {
                                min: base.min + shift,
                                max: base.max + shift,
                            })
                        }
                        // A flat window has no price axis to move, so that half
                        // of the drag is dropped. The time half still happens:
                        // one unusable axis must not freeze the other, or
                        // dragging a quiet chart would stop scrolling sideways
                        // as well.
                        _ => self.price,
                    }
                };

                Self {
                    from,
                    count,
                    price: range,
                }
            }
        }
    }
}

/// A factor a client is allowed to send.
///
/// `is_nan() || <= 0.0` rather than `!(factor > 0.0)`, which clippy rejects for
/// the right reason: `NaN` fails both comparisons, and a `NaN` factor would
/// propagate into the bar count and blank the chart.
fn sane_factor(factor: f64) -> f64 {
    if factor.is_nan() || factor <= 0.0 {
        1.0
    } else {
        factor.min(MAX_FACTOR)
    }
}

/// An anchor a client is allowed to send.
///
/// `clamp` alone is not enough, and this is the subtle one: `f64::clamp` is
/// written as two comparisons, `NaN` fails both of them, and the value passes
/// straight through. A `NaN` anchor then multiplies into `held`, so the price
/// range comes out `NaN` -- not a wrong axis, an axis [`PriceRange::is_usable`]
/// rejects, which the chart draws as nothing at all.
///
/// The fallback is the middle of the plot rather than zero: zero is the top
/// edge, which is a real anchor a user can ask for, and quietly substituting it
/// would turn a malformed request into a plausible-looking zoom.
fn sane_anchor(anchor: f64) -> f64 {
    if anchor.is_nan() {
        0.5
    } else {
        anchor.clamp(0.0, 1.0)
    }
}

/// A pan fraction a client is allowed to send, or `None` for "ignore this".
///
/// `None` rather than `0.0` for `NaN`: a zero is a legitimate "do not move", and
/// collapsing the two would hide the malformed input inside a no-op that looks
/// like a decision. Everything finite is capped rather than rejected, because
/// `fraction * span` overflows to infinity long before `f64` runs out of range
/// and the resulting range has a `NaN` span.
fn sane_fraction(fraction: f64) -> Option<f64> {
    if fraction.is_nan() {
        None
    } else {
        Some(fraction.clamp(-MAX_PAN_FRACTION, MAX_PAN_FRACTION))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOTAL: usize = 500;

    fn fitted() -> PriceRange {
        PriceRange {
            min: 100.0,
            max: 200.0,
        }
    }

    #[test]
    fn an_unset_viewport_shows_everything() {
        let window = Viewport::default().resolve(TOTAL);
        assert_eq!(window.from, 0);
        assert_eq!(window.count, TOTAL);
        assert!(window.is_all());
    }

    #[test]
    fn an_empty_series_resolves_to_an_empty_window() {
        // Not `MIN_BARS` bars that do not exist -- the shell draws nothing and
        // says so, which is the note `build` already sets.
        let window = Viewport::default().resolve(0);
        assert_eq!(window.count, 0);
        assert_eq!(window.end(), 0);
    }

    #[test]
    fn a_series_shorter_than_the_minimum_is_shown_whole() {
        let window = Viewport {
            from: 0,
            count: None,
            price: None,
        }
        .resolve(4);
        assert_eq!(window.count, 4, "not padded up to MIN_BARS");
    }

    #[test]
    fn from_is_clamped_so_the_window_never_runs_past_the_end() {
        let viewport = Viewport {
            from: 9_999,
            count: Some(100),
            price: None,
        };
        let window = viewport.resolve(TOTAL);
        assert_eq!(window.from, TOTAL - 100);
        assert_eq!(window.end(), TOTAL);
    }

    #[test]
    fn zooming_in_narrows_the_window_and_keeps_the_anchor_bar_still() {
        let start = Viewport {
            from: 0,
            count: None,
            price: None,
        }
        .resolve(TOTAL);
        let anchor = 0.5;
        let anchor_bar = start.from as f64 + anchor * start.count as f64;

        let zoomed = Viewport::default()
            .apply(
                Gesture::ZoomTime {
                    factor: 2.0,
                    anchor,
                },
                TOTAL,
                fitted(),
            )
            .resolve(TOTAL);

        assert_eq!(zoomed.count, TOTAL / 2);
        let new_anchor_bar = zoomed.from as f64 + anchor * zoomed.count as f64;
        assert!(
            (new_anchor_bar - anchor_bar).abs() <= 1.0,
            "the bar under the cursor moved: {anchor_bar} -> {new_anchor_bar}"
        );
    }

    #[test]
    fn zooming_in_stops_at_the_minimum_bar_count() {
        let mut viewport = Viewport::default();
        for _ in 0..100 {
            viewport = viewport.apply(
                Gesture::ZoomTime {
                    factor: 2.0,
                    anchor: 0.5,
                },
                TOTAL,
                fitted(),
            );
        }
        let window = viewport.resolve(TOTAL);
        assert_eq!(
            window.count, MIN_BARS,
            "the floor holds no matter how many ticks"
        );
    }

    #[test]
    fn zooming_out_stops_at_the_whole_series() {
        let mut viewport = Viewport::default();
        for _ in 0..100 {
            viewport = viewport.apply(
                Gesture::ZoomTime {
                    factor: 0.5,
                    anchor: 0.5,
                },
                TOTAL,
                fitted(),
            );
        }
        let window = viewport.resolve(TOTAL);
        assert_eq!(window.count, TOTAL);
        assert!(window.is_all());
    }

    #[test]
    fn panning_moves_by_a_fraction_of_the_visible_span() {
        let viewport = Viewport {
            from: 100,
            count: Some(100),
            price: None,
        };
        let moved = viewport
            .apply(
                Gesture::Pan {
                    time: 0.5,
                    price: 0.0,
                },
                TOTAL,
                fitted(),
            )
            .resolve(TOTAL);
        assert_eq!(moved.from, 150, "half of a 100-bar window is 50 bars");
        assert_eq!(moved.count, 100, "panning must not change the zoom");
    }

    #[test]
    fn a_vertical_drag_does_not_pin_the_bar_count() {
        // The trap in folding two pans into one gesture: it would be easy to pin
        // `count` unconditionally, and then a purely vertical drag would turn a
        // chart showing everything into one showing "these five hundred bars" --
        // which stops growing as new candles arrive. Nothing on screen would say
        // so; the chart would just quietly stop following the market.
        let fitted_viewport = Viewport::default();
        let dragged = fitted_viewport.apply(
            Gesture::Pan {
                time: 0.0,
                price: 0.5,
            },
            TOTAL,
            fitted(),
        );
        assert!(
            dragged.count.is_none(),
            "a price-only drag must leave the time axis alone"
        );
        assert!(dragged.resolve(TOTAL).is_all());
    }

    #[test]
    fn panning_cannot_leave_the_series() {
        let viewport = Viewport {
            from: 0,
            count: Some(100),
            price: None,
        };
        let back = viewport
            .apply(
                Gesture::Pan {
                    time: -50.0,
                    price: 0.0,
                },
                TOTAL,
                fitted(),
            )
            .resolve(TOTAL);
        assert_eq!(back.from, 0, "dragging past the oldest bar stops there");

        let forward = viewport
            .apply(
                Gesture::Pan {
                    time: 50.0,
                    price: 0.0,
                },
                TOTAL,
                fitted(),
            )
            .resolve(TOTAL);
        assert_eq!(forward.end(), TOTAL, "and past the newest bar stops there");
    }

    #[test]
    fn fit_returns_to_everything() {
        let viewport = Viewport {
            from: 200,
            count: Some(20),
            price: Some(PriceRange {
                min: 150.0,
                max: 160.0,
            }),
        };
        let fitted_viewport = viewport.apply(Gesture::Fit, TOTAL, fitted());
        assert_eq!(fitted_viewport, Viewport::default());
        assert!(fitted_viewport.resolve(TOTAL).is_all());
        assert!(
            fitted_viewport.price.is_none(),
            "fit must hand the price axis back to the engine"
        );
    }

    #[test]
    fn a_price_zoom_holds_the_price_under_the_anchor() {
        let viewport = Viewport::default();
        let anchor = 0.25;
        let held = fitted().max - anchor * fitted().span();

        let zoomed = viewport.apply(
            Gesture::ZoomPrice {
                factor: 2.0,
                anchor,
            },
            TOTAL,
            fitted(),
        );
        let price = zoomed.price.expect("a price zoom sets an explicit range");

        assert!(
            (price.span() - fitted().span() / 2.0).abs() < 1e-9,
            "span should halve, got {}",
            price.span()
        );
        let still_held = price.max - anchor * price.span();
        assert!(
            (still_held - held).abs() < 1e-9,
            "the price under the cursor moved: {held} -> {still_held}"
        );
    }

    #[test]
    fn a_price_zoom_cannot_collapse_the_axis() {
        // The failure this prevents: enough ticks and the span reaches zero,
        // `price_to_y` divides by it, and the canvas ignores the NaN -- a blank
        // chart with nothing logged anywhere.
        //
        // Asserting the *floor* rather than merely "the span is positive", which
        // is what makes this test worth having: the first version of the code
        // measured the floor against the current span, so the floor shrank along
        // with the thing it was supposed to bound and could never bind. "Span is
        // positive" would have been satisfied by any arbitrary small number and
        // said nothing about whether the guard worked. `fitted` is a number the
        // shrinking span cannot fake.
        let mut viewport = Viewport::default();
        for _ in 0..200 {
            viewport = viewport.apply(
                Gesture::ZoomPrice {
                    factor: 2.0,
                    anchor: 0.5,
                },
                TOTAL,
                fitted(),
            );
        }
        let price = viewport.price.expect("an explicit range");
        assert!(price.is_usable(), "span collapsed to {}", price.span());

        let floor = fitted().span() * MIN_PRICE_FRACTION;
        assert!(
            (price.span() / floor - 1.0).abs() < 1e-6,
            "200 halvings must come to rest on the floor {floor}, not on {}",
            price.span()
        );
    }

    #[test]
    fn a_price_pan_moves_without_rescaling() {
        let base = Viewport::default()
            .apply(
                Gesture::ZoomPrice {
                    factor: 2.0,
                    anchor: 0.5,
                },
                TOTAL,
                fitted(),
            )
            .price
            .expect("explicit range");

        let panned = Viewport::default()
            .apply(
                Gesture::ZoomPrice {
                    factor: 2.0,
                    anchor: 0.5,
                },
                TOTAL,
                fitted(),
            )
            .apply(
                Gesture::Pan {
                    time: 0.0,
                    price: 0.5,
                },
                TOTAL,
                fitted(),
            )
            .price
            .expect("explicit range");

        assert!(
            (panned.span() - base.span()).abs() < 1e-9,
            "panning must not change the price scale"
        );
        let expected = base.span() * 0.5;
        assert!((panned.min - (base.min + expected)).abs() < 1e-9);
    }

    #[test]
    fn a_hostile_factor_is_bounded_rather_than_inverting_the_axis() {
        // A client can send anything. `NaN` fails both comparisons, so
        // `!(factor > 0.0)` would have let it through -- and a `NaN` factor
        // propagates into the bar count, which is a chart with no bars on it.
        let base = Viewport {
            from: 100,
            count: Some(100),
            price: None,
        };
        for factor in [f64::NAN, 0.0, -0.0, -2.0] {
            let moved = base
                .apply(
                    Gesture::ZoomTime {
                        factor,
                        anchor: 0.5,
                    },
                    TOTAL,
                    fitted(),
                )
                .resolve(TOTAL);
            assert_eq!(
                moved.count, 100,
                "factor {factor} should be a no-op, not a new window"
            );
        }

        // `INFINITY` is a different case and gets a different answer: the
        // gesture is legitimate, only its magnitude is not. So it is *capped*
        // rather than dropped -- which is what `MAX_FACTOR` is for -- and the
        // window that comes out is still a real one rather than one bar
        // stretched across the plot.
        let capped = Viewport::default()
            .apply(
                Gesture::ZoomTime {
                    factor: f64::INFINITY,
                    anchor: 0.5,
                },
                TOTAL,
                fitted(),
            )
            .resolve(TOTAL);
        assert_eq!(
            capped.count,
            TOTAL / 10,
            "an infinite zoom lands on the ceiling, not on the whole series"
        );
    }

    #[test]
    fn a_hostile_anchor_cannot_poison_the_price_axis() {
        // The subtle one, and the reason `sane_anchor` exists: `f64::clamp` is
        // two comparisons, `NaN` fails both, and the value passes through. A
        // `NaN` anchor then multiplies into `held`, so the range comes out `NaN`
        // -- an axis `is_usable` rejects, drawn as nothing at all.
        let viewport = Viewport::default().apply(
            Gesture::ZoomPrice {
                factor: 2.0,
                anchor: f64::NAN,
            },
            TOTAL,
            fitted(),
        );
        let price = viewport
            .price
            .expect("the gesture still has to produce a range");
        assert!(
            price.is_usable(),
            "a NaN anchor produced {price:?}, span {}",
            price.span()
        );
        assert!(price.min.is_finite() && price.max.is_finite());
    }

    #[test]
    fn no_hostile_input_can_produce_an_unusable_window() {
        // The property the tests above are instances of, stated once: for *any*
        // gesture a client can send, what comes out is something the chart can
        // draw -- at least MIN_BARS bars, never past the end of the series, and a
        // price range with a real span.
        //
        // Written as a sweep rather than as four more examples because the bugs
        // it found were in the *interaction*: `NaN` passed through `clamp`, and
        // `1e300 * span` overflowed to an infinite range whose span is `NaN`.
        let hostile = [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -1.0e300,
            1.0e300,
            0.0,
            -0.0,
        ];
        for factor in hostile {
            for anchor in hostile {
                for fraction in hostile {
                    for gesture in [
                        Gesture::ZoomTime { factor, anchor },
                        Gesture::ZoomPrice { factor, anchor },
                        Gesture::Pan {
                            time: fraction,
                            price: 0.0,
                        },
                        Gesture::Pan {
                            time: 0.0,
                            price: fraction,
                        },
                        Gesture::Pan {
                            time: fraction,
                            price: fraction,
                        },
                    ] {
                        let viewport = Viewport::default().apply(gesture, TOTAL, fitted());
                        let window = viewport.resolve(TOTAL);
                        assert!(
                            window.count >= MIN_BARS.min(TOTAL),
                            "{gesture:?} produced only {} bars",
                            window.count
                        );
                        assert!(
                            window.end() <= TOTAL,
                            "{gesture:?} ran past the series: {window:?}"
                        );
                        if let Some(price) = viewport.price {
                            assert!(
                                price.is_usable(),
                                "{gesture:?} collapsed the price axis to {}",
                                price.span()
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_hostile_pan_fraction_is_ignored_or_capped() {
        let viewport = Viewport {
            from: 100,
            count: Some(100),
            price: None,
        };
        // `NaN` is a no-op: there is no direction in it to move.
        let unmoved = viewport
            .apply(
                Gesture::Pan {
                    time: f64::NAN,
                    price: 0.0,
                },
                TOTAL,
                fitted(),
            )
            .resolve(TOTAL);
        assert_eq!(unmoved.from, 100, "NaN has no direction to move in");

        // An infinite drag *has* a direction, so it moves -- to the end of the
        // series and no further. Capping rather than dropping leaves `resolve`'s
        // clamp as the single thing that decides where the edge is.
        let forward = viewport
            .apply(
                Gesture::Pan {
                    time: f64::INFINITY,
                    price: 0.0,
                },
                TOTAL,
                fitted(),
            )
            .resolve(TOTAL);
        assert_eq!(forward.end(), TOTAL);

        let backward = viewport
            .apply(
                Gesture::Pan {
                    time: f64::NEG_INFINITY,
                    price: 0.0,
                },
                TOTAL,
                fitted(),
            )
            .resolve(TOTAL);
        assert_eq!(backward.from, 0);
    }

    #[test]
    fn one_unusable_axis_does_not_cancel_the_other() {
        // A flat window has no price axis to drag, so that half of the gesture is
        // dropped. The time half has to survive it: a quiet stretch of chart is
        // exactly when someone wants to scroll back to where the market moved,
        // and freezing both axes would look like the chart had locked up.
        let flat = PriceRange {
            min: 100.0,
            max: 100.0,
        };
        let viewport = Viewport {
            from: 100,
            count: Some(100),
            price: None,
        };
        let moved = viewport
            .apply(
                Gesture::Pan {
                    time: 0.5,
                    price: 0.25,
                },
                TOTAL,
                flat,
            )
            .resolve(TOTAL);
        assert_eq!(moved.from, 150, "the time half still moved");
        assert!(moved.count == 100);
    }

    #[test]
    fn a_price_gesture_on_a_flat_series_is_a_no_op() {
        // A flat series has no span, so there is no price axis to zoom. Returning
        // the viewport unchanged is the honest answer; inventing a range would
        // draw a chart of a market that did not move.
        let flat = PriceRange {
            min: 100.0,
            max: 100.0,
        };
        let viewport = Viewport::default().apply(
            Gesture::ZoomPrice {
                factor: 2.0,
                anchor: 0.5,
            },
            TOTAL,
            flat,
        );
        assert!(viewport.price.is_none());
    }

    // --- the shell's echo ---------------------------------------------------

    #[test]
    fn a_window_reports_the_request_that_would_reproduce_it() {
        // The shell holds the scene's `viewport` and sends it back unchanged, so
        // what the scene reports has to be a *request* rather than the resolved
        // window: the two are the same view until the series grows, and after
        // that only one of them still means what the user asked for.
        for requested in [
            Viewport {
                from: 120,
                count: Some(60),
                price: Some(PriceRange {
                    min: 150.0,
                    max: 160.0,
                }),
            },
            Viewport {
                from: 0,
                count: Some(1),
                price: None,
            },
            Viewport {
                from: 7,
                count: None,
                price: None,
            },
            Viewport::default(),
        ] {
            let reported = requested.resolve(TOTAL).as_viewport();
            // The wire round trip the shell actually performs.
            let echoed: Viewport =
                serde_json::from_value(serde_json::to_value(reported).expect("serializes"))
                    .expect("a reported viewport must deserialize as a request");
            assert_eq!(echoed, reported, "the echo must be faithful: {requested:?}");
            assert_eq!(
                echoed.resolve(TOTAL),
                requested.resolve(TOTAL),
                "and it must resolve to the same window: {requested:?}"
            );
        }
    }

    #[test]
    fn a_fitted_view_stays_fitted_across_a_new_candle() {
        // The regression this guards, as behaviour rather than as shape. A
        // `Window` has every field decided, so reporting one directly would say
        // "these five hundred bars" where the user asked for "all of them". The
        // two are identical on the frame they are sent -- and then the chart
        // stops including new candles, permanently, which is a worse failure than
        // the missing zoom it was meant to fix.
        let mut viewport = Viewport::default();
        for total in [TOTAL, TOTAL + 1, TOTAL + 2] {
            viewport = viewport.resolve(total).as_viewport();
            assert!(
                viewport.resolve(total).is_all(),
                "a fitted chart stopped following the market at {total} bars: {viewport:?}"
            );
        }

        // And the boundary: one bar short of the series is *not* fitted, or
        // zooming out by a single notch would silently opt the chart out of
        // following.
        let nearly = Viewport {
            from: 0,
            count: Some(TOTAL - 1),
            price: None,
        };
        assert_eq!(nearly.resolve(TOTAL).as_viewport().count, Some(TOTAL - 1));
    }

    #[test]
    fn a_fitted_axis_is_reported_as_an_explicit_null() {
        // `None` means "keep fitting", and it has to arrive as a `null` rather
        // than as a missing key: a shell that had to special-case absence is a
        // shell one refactor away from sending a range it invented.
        let json = serde_json::to_value(Viewport::default().resolve(TOTAL).as_viewport())
            .expect("serializes");
        assert!(
            json.get("price").is_some(),
            "`price` must be on the wire even when it is null: {json}"
        );
        assert!(json["price"].is_null());
        assert!(json["count"].is_null(), "and so must `count`: {json}");
        assert_eq!(json["from"], 0);
    }
}
