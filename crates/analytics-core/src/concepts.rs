//! Concepts: measurements a client defines.
//!
//! ## Why this module exists
//!
//! `strategy_dsl` exposes a closed vocabulary -- 38 fields and 8 comparison
//! functions -- and **every field is a scalar**. So an idea that is not already
//! one of those 38 has nowhere to go: asked to implement "wait for the gap to be
//! 30% mitigated", a model reaches for the nearest scalar it can find, and what
//! comes out is a proxy for the idea rather than an implementation of it. That is
//! not a model failure. It is the vocabulary.
//!
//! This module is where the vocabulary opens -- without opening the grammar.
//! `docs/06` requires the condition grammar to stay small and enumerable, and it
//! does: a concept is a **pattern**, built from a closed set of selectors and
//! comparisons, each individually testable. What becomes definable is the
//! *measurement*, not the logic built on top of it.
//!
//! ## The shape
//!
//! A pattern is a window of N candles plus a band:
//!
//! ```text
//!   window: 3                 candles 0, 1, 2 -- 0 is the oldest, 2 is the trigger
//!   lower:  high(0)           the band's cheaper edge
//!   upper:  low(2)            the band's dearer edge
//!   require:
//!     - high(0) below low(2)  the pattern fires only while this holds
//! ```
//!
//! That is a fair value gap -- written by whoever wants it, not by us. Nothing in
//! this file knows what a fair value gap is, and that is the point: an order
//! block, a displacement, an inside-bar break and a three-candle rejection are
//! the same document with different numbers. The client brings the idea.
//!
//! ## What a concept cannot say
//!
//! Arithmetic. There is no `+`, no ratio between two arbitrary prices, and no
//! free-form expression -- because that is what turns a closed grammar into a
//! general-purpose language, and `docs/06` forbids it. The two numeric knobs a
//! pattern gets are `window` and `min_band_ratio`, both named, both finite.
//!
//! Also absent: structure. A pattern sees candles, not swings, so "the origin of
//! the move that broke the last swing high" is not expressible here. That is the
//! built-in supply/demand detector's job, and letting patterns see swings is the
//! next axis rather than a gap to paper over.
//!
//! ## Two entry points, deliberately
//!
//! [`validate`] is for whoever *authored* the document -- a client, or a model on
//! their behalf -- and produces a message naming what is wrong. [`detect`] is for
//! whoever *runs* it, and is total: an unvalidated concept produces whatever it
//! can and never panics, because the chart engine calls it inside wasm where a
//! panic is a trap and a dead canvas.

use serde::{Deserialize, Serialize};

use crate::error::AnalyticsError;
use crate::regions::{mitigation, Region, RegionOrigin};
use crate::types::{Candle, Side};

/// The fewest candles a pattern can span.
pub const MIN_WINDOW: usize = 2;

/// The most candles a pattern can span.
///
/// Eight, and that is a boundary rather than a round number: past roughly eight
/// candles a "pattern" is a claim about structure -- swings, legs, ranges -- and
/// structure is not visible in a bare candle window. Widening this without adding
/// structure selectors would let someone write a concept that cannot mean what it
/// looks like it means.
pub const MAX_WINDOW: usize = 8;

/// The longest a concept name may be.
pub const MAX_NAME: usize = 48;

/// One price or volume, read from one candle of a pattern.
///
/// The index counts from the **oldest** candle of the window, so a pattern reads
/// left to right and the last index is the candle that triggers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    /// The candle's open.
    Open(usize),
    /// The candle's high.
    High(usize),
    /// The candle's low.
    Low(usize),
    /// The candle's close.
    Close(usize),
    /// The candle's midpoint, `(high + low) / 2`.
    Mid(usize),
    /// The candle's total volume.
    Volume(usize),
}

impl Selector {
    /// Which candle of the window this reads, oldest first.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Open(i)
            | Self::High(i)
            | Self::Low(i)
            | Self::Close(i)
            | Self::Mid(i)
            | Self::Volume(i) => i,
        }
    }

    /// Whether this reads a price or a volume.
    ///
    /// The validator uses it to refuse a comparison between the two, which is the
    /// kind of thing a model writes when it is reaching -- `volume(1) above
    /// close(0)` is not a mistake anyone makes on purpose, and it is not a
    /// mistake worth silently accepting either.
    #[must_use]
    pub const fn kind(self) -> SelectorKind {
        match self {
            Self::Volume(_) => SelectorKind::Volume,
            _ => SelectorKind::Price,
        }
    }

    /// Read it out of a window.
    ///
    /// `None` when the window is too short -- which cannot happen for a validated
    /// concept, and must not panic for an unvalidated one.
    #[must_use]
    pub fn value(self, window: &[Candle]) -> Option<f64> {
        let candle = window.get(self.index())?;
        Some(match self {
            Self::Open(_) => candle.open,
            Self::High(_) => candle.high,
            Self::Low(_) => candle.low,
            Self::Close(_) => candle.close,
            Self::Mid(_) => (candle.high + candle.low) / 2.0,
            Self::Volume(_) => candle.volume,
        })
    }
}

impl std::fmt::Display for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Open(_) => "open",
            Self::High(_) => "high",
            Self::Low(_) => "low",
            Self::Close(_) => "close",
            Self::Mid(_) => "mid",
            Self::Volume(_) => "volume",
        };
        write!(f, "{name}({})", self.index())
    }
}

/// What a [`Selector`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorKind {
    /// A price.
    Price,
    /// A size.
    Volume,
}

/// A comparison between two selectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    /// Strictly below.
    Below,
    /// Strictly above.
    Above,
    /// Below or equal.
    BelowOrEqual,
    /// Above or equal.
    AboveOrEqual,
}

impl Compare {
    /// Every comparison, for a client's vocabulary and exhaustive tests.
    pub const ALL: [Self; 4] = [
        Self::Below,
        Self::Above,
        Self::BelowOrEqual,
        Self::AboveOrEqual,
    ];

    /// Canonical name, as written in a document.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Below => "below",
            Self::Above => "above",
            Self::BelowOrEqual => "below_or_equal",
            Self::AboveOrEqual => "above_or_equal",
        }
    }

    /// Whether the comparison holds.
    ///
    /// A non-finite operand makes every comparison false, including the
    /// "or equal" ones. That is the safe direction: a pattern fires on evidence,
    /// and `NaN` is the absence of evidence.
    #[must_use]
    pub fn holds(self, left: f64, right: f64) -> bool {
        if !left.is_finite() || !right.is_finite() {
            return false;
        }
        match self {
            Self::Below => left < right,
            Self::Above => left > right,
            Self::BelowOrEqual => left <= right,
            Self::AboveOrEqual => left >= right,
        }
    }
}

/// One condition a pattern must satisfy to fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// The left operand.
    pub left: Selector,
    /// How the two are compared.
    pub op: Compare,
    /// The right operand.
    pub right: Selector,
}

impl Requirement {
    /// Whether it holds over a window.
    ///
    /// A window too short for either operand is `false`, not a panic.
    #[must_use]
    pub fn holds(self, window: &[Candle]) -> bool {
        match (self.left.value(window), self.right.value(window)) {
            (Some(left), Some(right)) => self.op.holds(left, right),
            _ => false,
        }
    }
}

/// A measurement a client defined.
///
/// See the module docs for the shape and for what it deliberately cannot say.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Concept {
    /// The name. Also the colour key, so it is an identifier rather than prose.
    pub name: String,
    /// How it should read on a chart. Defaults to the name with underscores
    /// opened out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Which side is expected to react from a band this finds.
    pub side: Side,
    /// How many candles the pattern spans.
    pub window: usize,
    /// The band's cheaper edge.
    pub lower: Selector,
    /// The band's dearer edge.
    pub upper: Selector,
    /// Conditions that must hold for the pattern to fire.
    #[serde(default)]
    pub require: Vec<Requirement>,
    /// The band must be at least this share of the window's own range.
    ///
    /// The one size knob. `None` draws every match -- which is the definition,
    /// not a default -- and a client who wants only meaningful bands sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_band_ratio: Option<f64>,
}

impl Concept {
    /// How the concept reads on a chart.
    ///
    /// The name is an identifier because it is also the colour key; a label with
    /// underscores in it is a key that leaked into a drawing.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.label {
            Some(label) => label.clone(),
            None => self.name.replace('_', " "),
        }
    }
}

/// Check a concept before anything tries to run it.
///
/// Every failure names the thing that is wrong, because the caller is usually
/// showing this to whoever wrote the document.
pub fn validate(concept: &Concept) -> Result<(), AnalyticsError> {
    let bad_name = |reason: String| AnalyticsError::BadConceptName {
        name: concept.name.clone(),
        reason,
    };

    if concept.name.is_empty() {
        return Err(bad_name("a name is required".into()));
    }
    if concept.name.len() > MAX_NAME {
        return Err(bad_name(format!(
            "a name must be at most {MAX_NAME} characters, got {}",
            concept.name.len()
        )));
    }
    // An identifier, because this becomes a colour key now and a field a
    // condition can reference later. Spaces and capitals would work for the
    // first and break the second.
    let mut characters = concept.name.chars();
    let first = characters.next().expect("checked non-empty");
    if !first.is_ascii_lowercase() {
        return Err(bad_name(
            "it must start with a lowercase letter, so it can also be a field a condition references"
                .into(),
        ));
    }
    if let Some(bad) =
        characters.find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_'))
    {
        return Err(bad_name(format!(
            "`{bad}` is not allowed; use lowercase letters, digits and underscores"
        )));
    }

    if concept.window < MIN_WINDOW || concept.window > MAX_WINDOW {
        return Err(AnalyticsError::ConceptWindowOutOfRange {
            window: concept.window,
            min: MIN_WINDOW,
            max: MAX_WINDOW,
        });
    }

    for (selector, role) in [
        (concept.lower, "the band's cheaper edge"),
        (concept.upper, "the band's dearer edge"),
    ] {
        check_selector(selector, role, concept.window)?;
        if selector.kind() != SelectorKind::Price {
            return Err(AnalyticsError::BadConceptSelector {
                selector: selector.to_string(),
                reason: format!("{role} has to be a price"),
            });
        }
    }
    if concept.lower == concept.upper {
        return Err(AnalyticsError::BadConceptSelector {
            selector: concept.lower.to_string(),
            reason: "both edges are the same selector, so the band would have no height".into(),
        });
    }

    for requirement in &concept.require {
        check_selector(requirement.left, "an operand", concept.window)?;
        check_selector(requirement.right, "an operand", concept.window)?;
        if requirement.left.kind() != requirement.right.kind() {
            return Err(AnalyticsError::MismatchedConceptComparison {
                left: requirement.left.to_string(),
                right: requirement.right.to_string(),
            });
        }
    }

    if let Some(ratio) = concept.min_band_ratio {
        if !ratio.is_finite() || ratio <= 0.0 {
            return Err(AnalyticsError::BadConceptRatio {
                ratio: ratio.to_string(),
            });
        }
    }

    Ok(())
}

/// Refuse a selector that reads a candle the window does not have.
fn check_selector(selector: Selector, role: &str, window: usize) -> Result<(), AnalyticsError> {
    if selector.index() >= window {
        return Err(AnalyticsError::BadConceptSelector {
            selector: selector.to_string(),
            reason: format!(
                "{role}, but candle {} is outside a window of {window}",
                selector.index()
            ),
        });
    }
    Ok(())
}

/// Find every band a concept describes, oldest first.
///
/// Total: a concept that has not been validated produces whatever it can and
/// never panics, because the chart engine calls this inside wasm. Call
/// [`validate`] first when you want to know *why* a document is wrong.
///
/// Every match is reported. A pattern loose enough to match most windows will
/// draw most windows -- that is the pattern's shape showing through, and
/// `require` and `min_band_ratio` are how a client narrows it, not a limit here.
#[must_use]
pub fn detect(candles: &[Candle], concept: &Concept) -> Vec<Region> {
    if concept.window < MIN_WINDOW || candles.len() < concept.window {
        return Vec::new();
    }

    // The right edge of every band: the last candle's close time. A band that
    // stopped where it formed would be a historical annotation; the point of the
    // drawing is that it is a level still in play.
    let last = &candles[candles.len() - 1];
    let to = last.open_time + last.timeframe.nanos();

    let mut out = Vec::new();
    for end in concept.window - 1..candles.len() {
        let window = &candles[end + 1 - concept.window..=end];
        if !concept.require.iter().all(|r| r.holds(window)) {
            continue;
        }

        let (Some(price_low), Some(price_high)) =
            (concept.lower.value(window), concept.upper.value(window))
        else {
            continue;
        };
        let height = price_high - price_low;
        if height.is_nan() || height <= 0.0 {
            // The band is flat or inverted *in this data*, which a document
            // cannot be refused for: the same pattern may be a real band three
            // candles later.
            continue;
        }

        if let Some(minimum) = concept.min_band_ratio {
            let own_low = window.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
            let own_high = window
                .iter()
                .map(|c| c.high)
                .fold(f64::NEG_INFINITY, f64::max);
            let own = own_high - own_low;
            if own.is_nan() || own <= 0.0 || height / own < minimum {
                continue;
            }
        }

        out.push(Region {
            side: concept.side,
            name: concept.label(),
            price_low,
            price_high,
            // A band opens at the first candle of the pattern -- the candle price
            // left from. Drawing it from the trigger would clip off the very edge
            // the level is measured from.
            formed_at: window[0].open_time,
            from: window[0].open_time,
            to,
            mitigated: mitigation(candles, end, concept.side, price_low, price_high),
            origin: RegionOrigin::Pattern,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    const TF: Timeframe = Timeframe::M5;

    fn candle(index: i64, open: f64, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: TF,
            open_time: index * TF.nanos(),
            open,
            high,
            low,
            close,
            volume: 10.0,
            buy_volume: 6.0,
            sell_volume: 4.0,
        }
    }

    /// A series with one bullish displacement, and nothing after it that returns.
    ///
    /// ```text
    ///   [0..1]   flat
    ///   [2]      A: tops at 101.0
    ///   [3]      B: the displacement, 100.5 -> 104.0
    ///   [4]      C: bottoms at 105.0   =>  the band 101.0 .. 105.0
    ///   [5..7]   price stays above 105, so the band is never touched
    /// ```
    ///
    /// `B`'s high is deliberately 105.5 rather than just above its own close.
    /// With a lower high, the window `[3, 4, 5]` would also satisfy the rule --
    /// `high(3) < low(5)` -- and a test asserting "one band" would be asserting
    /// something about the fixture rather than about the detector. Worth knowing:
    /// the rule is *any* three-candle separation, so a series that keeps
    /// separating will keep matching, and that is the rule working.
    fn bullish_series() -> Vec<Candle> {
        vec![
            candle(0, 100.0, 100.5, 99.5, 100.0),
            candle(1, 100.0, 100.5, 99.5, 100.2),
            candle(2, 100.2, 101.0, 100.0, 100.5),
            candle(3, 100.5, 105.5, 100.5, 104.0),
            candle(4, 105.0, 105.6, 105.0, 105.2),
            candle(5, 105.2, 105.6, 105.0, 105.4),
            candle(6, 105.4, 105.8, 105.1, 105.6),
            candle(7, 105.6, 106.0, 105.2, 105.7),
        ]
    }

    /// The mirror: one bearish displacement, and nothing after it that returns.
    ///
    /// ```text
    ///   [2]      A: bottoms at 99.0
    ///   [3]      B: the displacement down
    ///   [4]      C: tops at 95.0       =>  the band 95.0 .. 99.0
    ///   [5..7]   price stays below 95
    /// ```
    fn bearish_series() -> Vec<Candle> {
        vec![
            candle(0, 100.0, 100.5, 99.5, 100.0),
            candle(1, 100.0, 100.5, 99.5, 100.2),
            candle(2, 100.2, 100.6, 99.0, 99.5),
            candle(3, 99.5, 99.6, 94.5, 96.0),
            candle(4, 95.0, 95.0, 94.4, 94.6),
            candle(5, 94.8, 95.0, 94.4, 94.6),
            candle(6, 94.6, 94.8, 94.2, 94.4),
            candle(7, 94.4, 94.6, 94.0, 94.2),
        ]
    }

    /// The document a client writes for a fair value gap, upward.
    ///
    /// Note what is *not* here: nothing says "fair value gap". It is a window, a
    /// band and one condition.
    fn bullish_gap() -> Concept {
        Concept {
            name: "bullish_gap".into(),
            label: Some("bullish gap".into()),
            side: Side::Buy,
            window: 3,
            lower: Selector::High(0),
            upper: Selector::Low(2),
            require: vec![Requirement {
                left: Selector::High(0),
                op: Compare::Below,
                right: Selector::Low(2),
            }],
            min_band_ratio: None,
        }
    }

    fn bearish_gap() -> Concept {
        Concept {
            name: "bearish_gap".into(),
            label: None,
            side: Side::Sell,
            window: 3,
            lower: Selector::High(2),
            upper: Selector::Low(0),
            require: vec![Requirement {
                left: Selector::Low(0),
                op: Compare::Above,
                right: Selector::High(2),
            }],
            min_band_ratio: None,
        }
    }

    /// The last down candle before an impulse that closes above its high.
    ///
    /// The same shape as an order block, and written without anyone adding a
    /// detector for it.
    fn order_block() -> Concept {
        Concept {
            name: "bullish_order_block".into(),
            label: None,
            side: Side::Buy,
            window: 3,
            lower: Selector::Low(0),
            upper: Selector::High(0),
            require: vec![
                Requirement {
                    left: Selector::Close(0),
                    op: Compare::Below,
                    right: Selector::Open(0),
                },
                Requirement {
                    left: Selector::Close(2),
                    op: Compare::Above,
                    right: Selector::High(0),
                },
            ],
            min_band_ratio: None,
        }
    }

    // --- the language, not a menu ------------------------------------------

    #[test]
    fn a_pattern_a_client_writes_finds_its_bands() {
        let bands = detect(&bullish_series(), &bullish_gap());
        assert_eq!(bands.len(), 1, "one gap upward: {bands:#?}");
        let band = &bands[0];
        // Candle 2's high to candle 4's low. Not candle 3's range, which is wider.
        assert_eq!(band.price_low, 101.0, "{band:#?}");
        assert_eq!(band.price_high, 105.0, "{band:#?}");
        assert_eq!(band.formed_at, 2 * TF.nanos(), "the band opens at candle 2");
    }

    #[test]
    fn an_order_block_is_the_same_document_with_different_numbers() {
        // This is the whole claim of the module: the vocabulary is a language,
        // not a menu. An order block is expressible without anyone adding a
        // detector for it -- and this is the *same* `Concept` type, validated by
        // the same function, drawn by the same chart code.
        validate(&order_block()).expect("a client wrote this, so it must validate");

        let candles = vec![
            candle(0, 100.0, 100.5, 98.0, 98.5), // the down candle
            candle(1, 98.5, 99.0, 98.0, 98.8),
            candle(2, 98.8, 102.0, 98.5, 101.5), // the impulse through its high
        ];
        let bands = detect(&candles, &order_block());
        assert_eq!(bands.len(), 1, "{bands:#?}");
        assert_eq!(bands[0].price_low, 98.0, "{:#?}", bands[0]);
        assert_eq!(bands[0].price_high, 100.5, "{:#?}", bands[0]);
        assert_eq!(bands[0].origin, RegionOrigin::Pattern);

        // And on the gap series the same document finds nothing. Two concepts
        // are two patterns, not one pattern under two names.
        assert!(
            detect(&bullish_series(), &order_block()).is_empty(),
            "the gap series has no down candle followed by an impulse"
        );
        assert!(
            detect(&bullish_series(), &bearish_gap()).is_empty(),
            "and a bullish series has no downward gap"
        );
    }

    #[test]
    fn the_band_is_named_after_the_concept_and_knows_where_it_came_from() {
        let bands = detect(&bullish_series(), &bullish_gap());
        assert_eq!(
            bands[0].name, "bullish gap",
            "the label, not the identifier"
        );
        assert_eq!(bands[0].side, Side::Buy);
        assert_eq!(bands[0].origin, RegionOrigin::Pattern);
        assert_eq!(bands[0].origin.broken_level(), None);

        // And a concept with no label reads as its name, opened out.
        let bands = detect(&bearish_series(), &bearish_gap());
        assert_eq!(bands[0].name, "bearish gap");
    }

    #[test]
    fn both_directions_are_one_document_apart() {
        let up = detect(&bullish_series(), &bullish_gap());
        let down = detect(&bearish_series(), &bearish_gap());
        assert_eq!(up.len(), 1, "{up:#?}");
        assert_eq!(down.len(), 1, "{down:#?}");

        assert_eq!(up[0].side, Side::Buy);
        assert_eq!(down[0].side, Side::Sell);
        // The bearish band spans candle 4's high to candle 2's low, low-first.
        assert_eq!(down[0].price_low, 95.0, "{:#?}", down[0]);
        assert_eq!(down[0].price_high, 99.0, "{:#?}", down[0]);
    }

    #[test]
    fn a_band_price_never_came_back_to_is_fresh() {
        // Nothing after candle 4 comes back down to 105, so the band is untouched.
        let bands = detect(&bullish_series(), &bullish_gap());
        assert!(bands[0].is_fresh(), "{:#?}", bands[0]);
        assert_eq!(bands[0].mitigated, 0.0);
    }

    #[test]
    fn mitigation_is_measured_the_same_way_as_a_zone() {
        // The rule is shared, not copied: a concept means the same thing by
        // "mitigated" as the built-in detector does.
        let mut candles = bullish_series();
        // Halfway back into the 101..105 band.
        candles.push(candle(8, 104.8, 104.9, 103.0, 103.2));
        let bands = detect(&candles, &bullish_gap());
        let band = bands
            .iter()
            .find(|b| b.price_low == 101.0 && b.price_high == 105.0)
            .expect("the gap band");
        assert!(
            (band.mitigated - 0.5).abs() < 1e-9,
            "half the band, got {}",
            band.mitigated
        );
        assert!(!band.is_fresh(), "{band:#?}");
    }

    #[test]
    fn the_size_floor_is_the_one_knob_that_narrows_a_pattern() {
        // The same document with and without a floor. A pattern that matches
        // everything is not wrong, it is just not useful -- and the client is
        // the one who knows which is which.
        let mut concept = bullish_gap();
        assert_eq!(detect(&bullish_series(), &concept).len(), 1);
        // The band is 4 wide (101..105); the window's own range is 100..105.6.
        concept.min_band_ratio = Some(0.5);
        assert_eq!(
            detect(&bullish_series(), &concept).len(),
            1,
            "4 of 5.6 clears a 50% floor"
        );
        concept.min_band_ratio = Some(0.9);
        assert_eq!(
            detect(&bullish_series(), &concept).len(),
            0,
            "4 of 5.6 does not clear a 90% floor"
        );
    }

    // --- validation --------------------------------------------------------

    #[test]
    fn a_document_a_client_writes_validates() {
        validate(&bullish_gap()).expect("the example must be valid");
        validate(&bearish_gap()).expect("the example must be valid");
    }

    #[test]
    fn a_name_that_could_not_be_a_field_is_refused() {
        // The name is a colour key now and a field a condition references later,
        // so it is an identifier rather than prose.
        for (name, expected) in [
            ("", "a name is required"),
            ("Bullish Gap", "start with a lowercase letter"),
            ("1gap", "start with a lowercase letter"),
            ("bullish-gap", "not allowed"),
            ("bullish gap", "not allowed"),
        ] {
            let concept = Concept {
                name: name.into(),
                ..bullish_gap()
            };
            let error = validate(&concept).expect_err("{name} must be refused");
            assert!(
                error.to_string().contains(expected),
                "`{name}` gave `{error}`, expected something about `{expected}`"
            );
        }
        let long = Concept {
            name: "g".repeat(MAX_NAME + 1),
            ..bullish_gap()
        };
        assert!(
            validate(&long).is_err(),
            "a name over the limit must be refused"
        );
    }

    #[test]
    fn a_window_outside_the_supported_range_is_refused() {
        for window in [0, 1, MAX_WINDOW + 1] {
            let concept = Concept {
                window,
                ..bullish_gap()
            };
            let error = validate(&concept).expect_err("must be refused");
            assert!(
                matches!(error, AnalyticsError::ConceptWindowOutOfRange { .. }),
                "{window} gave {error}"
            );
        }
    }

    #[test]
    fn a_selector_outside_the_window_is_refused() {
        // `high(3)` in a three-candle window reads a candle that is not there.
        let concept = Concept {
            upper: Selector::Low(3),
            ..bullish_gap()
        };
        let error = validate(&concept).expect_err("must be refused");
        assert!(
            error.to_string().contains("outside a window of 3"),
            "{error}"
        );
    }

    #[test]
    fn a_band_edge_that_is_a_volume_is_refused() {
        let concept = Concept {
            upper: Selector::Volume(2),
            ..bullish_gap()
        };
        let error = validate(&concept).expect_err("must be refused");
        assert!(error.to_string().contains("has to be a price"), "{error}");
    }

    #[test]
    fn a_band_with_two_identical_edges_is_refused() {
        // It would have no height in any data, so it is refused statically
        // rather than skipped at runtime.
        let concept = Concept {
            upper: Selector::High(0),
            ..bullish_gap()
        };
        let error = validate(&concept).expect_err("must be refused");
        assert!(error.to_string().contains("no height"), "{error}");
    }

    #[test]
    fn a_comparison_between_a_price_and_a_volume_is_refused() {
        // The kind of thing a model writes when it is reaching.
        let concept = Concept {
            require: vec![Requirement {
                left: Selector::Volume(1),
                op: Compare::Above,
                right: Selector::Close(0),
            }],
            ..bullish_gap()
        };
        let error = validate(&concept).expect_err("must be refused");
        assert!(
            matches!(error, AnalyticsError::MismatchedConceptComparison { .. }),
            "{error}"
        );
    }

    #[test]
    fn a_ratio_that_is_not_a_positive_number_is_refused() {
        for ratio in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let concept = Concept {
                min_band_ratio: Some(ratio),
                ..bullish_gap()
            };
            let error = validate(&concept).expect_err("must be refused");
            assert!(
                matches!(error, AnalyticsError::BadConceptRatio { .. }),
                "{ratio} gave {error}"
            );
        }
    }

    // --- totality ----------------------------------------------------------

    #[test]
    fn detecting_with_an_unvalidated_concept_is_total() {
        // The chart engine calls this inside wasm, where a panic is a trap and a
        // dead canvas. So every way of being wrong has to be a skipped window
        // rather than an index out of bounds.
        let broken = [
            Concept {
                window: 0,
                ..bullish_gap()
            },
            Concept {
                window: 99,
                ..bullish_gap()
            },
            Concept {
                window: 1,
                ..bullish_gap()
            },
            Concept {
                upper: Selector::Low(7),
                ..bullish_gap()
            },
            Concept {
                min_band_ratio: Some(f64::NAN),
                ..bullish_gap()
            },
        ];
        for concept in &broken {
            let bands = detect(&bullish_series(), concept);
            assert!(
                bands.iter().all(|b| b.price_high.is_finite()),
                "{concept:#?} produced a non-finite band"
            );
        }
    }

    #[test]
    fn a_series_shorter_than_the_window_has_no_bands() {
        let short = vec![candle(0, 100.0, 101.0, 99.0, 100.5)];
        assert!(detect(&short, &bullish_gap()).is_empty());
        assert!(detect(&[], &bullish_gap()).is_empty());
    }

    #[test]
    fn a_comparison_against_a_non_finite_price_is_false() {
        // A candle with a NaN high -- which a corrupt feed can produce -- must
        // not fire a pattern, and must not poison `below_or_equal` either.
        for op in Compare::ALL {
            assert!(!op.holds(f64::NAN, 1.0), "{op:?}");
            assert!(!op.holds(1.0, f64::NAN), "{op:?}");
        }
    }

    #[test]
    fn the_selectors_name_themselves_the_way_a_document_writes_them() {
        // The validator's messages are read by whoever wrote the document, so
        // they have to use the document's own spelling.
        assert_eq!(Selector::High(0).to_string(), "high(0)");
        assert_eq!(Selector::Volume(3).to_string(), "volume(3)");
        assert_eq!(Compare::BelowOrEqual.name(), "below_or_equal");
        assert_eq!(Selector::Volume(0).kind(), SelectorKind::Volume);
        assert_eq!(Selector::Mid(0).kind(), SelectorKind::Price);
    }

    #[test]
    fn a_concept_survives_the_wire() {
        // This document is authored elsewhere -- a client, or a model on their
        // behalf -- so the JSON is the interface.
        let json = serde_json::to_string(&bullish_gap()).expect("serializes");
        let back: Concept = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(back, bullish_gap());
        assert_eq!(
            serde_json::to_value(bullish_gap()).unwrap()["lower"],
            serde_json::json!({"high": 0}),
            "a selector travels as its name and index"
        );
    }

    #[test]
    fn a_concept_may_omit_the_optional_fields() {
        // A model writing a document should not have to emit empty arrays and
        // nulls to be understood.
        let minimal: Concept = serde_json::from_str(
            r#"{"name":"gap","side":"Buy","window":3,
                "lower":{"high":0},"upper":{"low":2}}"#,
        )
        .expect("the optional fields are optional");
        assert!(minimal.require.is_empty());
        assert!(minimal.min_band_ratio.is_none());
        assert_eq!(minimal.label(), "gap");
        validate(&minimal).expect("and it is still valid");
    }
}
