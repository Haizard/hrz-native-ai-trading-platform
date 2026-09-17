//! The shapes a user draws on the chart (`docs/14-FRONTEND-CHART-ENGINE.md`).
//!
//! ## Two anchors and a kind
//!
//! Every shape a trader draws is a pair of points and a rule for what goes
//! between them, so that is the whole vocabulary: a [`DrawingKind`] and up to two
//! [`Anchor`]s. A trendline is the segment, a rectangle is the area, a Fibonacci
//! is the levels, and a horizontal line is the degenerate case that needs one
//! anchor.
//!
//! The kinds are an enum rather than free text because a kind nobody can draw
//! must be **refused**, not stored: a drawing that saves and then never appears
//! is worse than one that will not save. `ray`, `channel` and a measured move are
//! the obvious next ones, and each is a variant plus an arm in the scene's
//! `drawing_parts` -- no new storage, because they are all two anchors.
//!
//! ## The shell says where on the screen, the engine says what that is
//!
//! An [`Anchor`] is either `absolute` -- a millisecond timestamp and a price --
//! or `fraction`, a position over the plot rectangle. The shell sends fractions
//! while a drawing is being placed or dragged, and the scene reports every
//! anchor back as absolute. So the shell never turns a pointer position into a
//! price or a time.
//!
//! That is the interaction model's rule generalised. It is also why a drag needs
//! no gesture type of its own: **a drag is a request whose anchor is a fraction**,
//! so it folds into the same one-rebuild-per-frame coalescing a wheel does.
//!
//! The two forms use different field names -- `x`/`y` against `time`/`price` --
//! and the difference is load-bearing. One pair of names for both would make "the
//! shell sent a fraction where the engine expected a timestamp" a mistake nothing
//! catches: the numbers are the same shape, the units are not, and the result is
//! a drawing somewhere in 1970.
//!
//! ## The time unit is milliseconds
//!
//! The platform stores nanoseconds, and everything else crossing to the browser
//! is nanoseconds. An anchor is the one value that has to make the round trip
//! *back* into a database, and JSON numbers are doubles: a nanosecond timestamp
//! is past 2^53, so one read into JavaScript and written back is a different
//! number. Milliseconds fit exactly. The conversion is in `db::drawings`, at the
//! boundary, and nowhere else.

use serde::{Deserialize, Serialize};

/// The kinds this engine can draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrawingKind {
    /// The segment between the two anchors.
    Trendline,
    /// A horizontal line across the plot, at the first anchor's price.
    Hline,
    /// The rectangle the two anchors span.
    Rect,
    /// Fibonacci retracement levels between the two anchors.
    Fib,
}

impl DrawingKind {
    /// Every kind, for a client that wants to build a toolbar.
    pub const ALL: [Self; 4] = [Self::Trendline, Self::Hline, Self::Rect, Self::Fib];

    /// The wire name, which is also the storage's value and the shell's colour
    /// key.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Trendline => "trendline",
            Self::Hline => "hline",
            Self::Rect => "rect",
            Self::Fib => "fib",
        }
    }

    /// Whether this kind needs its second anchor.
    ///
    /// The one rule with four cases. It lives beside the enum so it cannot drift
    /// from the list of kinds -- and `db::drawings::needs_second_anchor` is the
    /// same rule on the storage side, pinned against this one by a test.
    #[must_use]
    pub const fn needs_second_anchor(self) -> bool {
        !matches!(self, Self::Hline)
    }
}

/// Where one end of a drawing is, and in what unit.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum Anchor {
    /// A millisecond timestamp and a price: what gets stored.
    Absolute {
        /// Milliseconds since the epoch.
        time: f64,
        /// The price.
        price: f64,
    },
    /// A position on the plot as a fraction of it: `0.0` at the left or top
    /// edge, `1.0` at the right or bottom.
    ///
    /// The same number the viewport gestures carry, and for the same reason: it
    /// is what a pointer position gives you without knowing anything about the
    /// price scale.
    Fraction {
        /// Across the plot.
        x: f64,
        /// Down the plot.
        y: f64,
    },
}

impl Anchor {
    /// The millisecond time and the price, when this is an absolute anchor.
    ///
    /// `None` for a fraction, rather than a guess: a fraction's `x` is not a
    /// time, and the whole point of the two variants is that the difference
    /// cannot be papered over.
    #[must_use]
    pub const fn absolute(self) -> Option<(f64, f64)> {
        match self {
            Self::Absolute { time, price } => Some((time, price)),
            Self::Fraction { .. } => None,
        }
    }
}

/// One drawing, as a request carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drawing {
    /// The caller's id for it. Stable across frames, and the row id once saved.
    pub id: String,
    /// What to put between the anchors.
    pub kind: DrawingKind,
    /// The first anchor, which every kind has.
    pub a1: Anchor,
    /// The second, for the kinds that have one.
    #[serde(default)]
    pub a2: Option<Anchor>,
    /// What the user called it, if anything.
    #[serde(default)]
    pub label: Option<String>,
    /// Whether the caller has this one selected.
    ///
    /// Only the selected drawing gets handles. Every drawing's anchors at once
    /// is visual noise, and it invites a drag nobody can aim.
    #[serde(default)]
    pub selected: bool,
}

impl Drawing {
    /// Why this drawing's anchors cannot be drawn, if they cannot.
    ///
    /// Total, and it names the problem rather than answering a `bool`: the
    /// reason goes into the scene's note with the drawing's id, because a
    /// drawing that silently does not appear teaches whoever drew it nothing.
    ///
    /// Anchors only, and not the id. The id is the shell's and the scene's --
    /// storage mints its own -- so a caller that is validating a request body has
    /// no id to give and should not have to invent one. `place` checks it, which
    /// is the only place that needs it.
    ///
    /// # Errors
    /// Returns the reason as a `String`.
    pub fn validate_anchors(&self) -> Result<(), String> {
        if self.kind.needs_second_anchor() && self.a2.is_none() {
            return Err(format!(
                "a {} needs two anchors and has one",
                self.kind.name()
            ));
        }
        for (which, anchor) in [("first", Some(self.a1)), ("second", self.a2)] {
            let Some(anchor) = anchor else { continue };
            let ok = match anchor {
                // `is_finite` is false for both `NaN` and the infinities, which
                // is the whole check: a `NaN` price produces a `NaN` y and the
                // canvas silently ignores it, so the drawing would simply be
                // absent with nothing anywhere saying why.
                Anchor::Absolute { time, price } => time.is_finite() && price.is_finite(),
                Anchor::Fraction { x, y } => x.is_finite() && y.is_finite(),
            };
            if !ok {
                return Err(format!("the {which} anchor is not a usable position"));
            }
        }
        Ok(())
    }
}

/// The Fibonacci ratios drawn, and the label each gets.
///
/// The retracement set and nothing else: the extensions (1.272, 1.618) are a
/// different drawing with a different meaning, and shipping them under the same
/// name would be a chart that quietly says something the user did not ask for.
///
/// The labels are strings rather than numbers formatted at draw time, because
/// `61.8` is a name for a level and `0.618` is the arithmetic behind it, and
/// `docs/14` keeps arithmetic out of JavaScript.
pub const FIB_LEVELS: [(f64, &str); 7] = [
    (0.0, "0"),
    (0.236, "23.6"),
    (0.382, "38.2"),
    (0.5, "50"),
    (0.618, "61.8"),
    (0.786, "78.6"),
    (1.0, "100"),
];

/// One positioned shape.
///
/// Geometry, not instructions: every coordinate is already in canvas pixels and
/// every choice is already made, so the shell iterates and strokes. That is
/// `docs/14`'s arrangement for the whole scene, and drawings are not an
/// exception -- it is what keeps the price scale in Rust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum DrawingPart {
    /// A line between two canvas points.
    Segment {
        /// Start, across.
        x1: f64,
        /// Start, down.
        y1: f64,
        /// End, across.
        x2: f64,
        /// End, down.
        y2: f64,
        /// Whether to stroke it dashed.
        dashed: bool,
    },
    /// A rectangle.
    Rect {
        /// Left edge.
        x: f64,
        /// Top edge.
        y: f64,
        /// Width.
        w: f64,
        /// Height.
        h: f64,
        /// Whether to fill it as well as outline it.
        filled: bool,
    },
    /// A label at a canvas point.
    Text {
        /// Left of the text.
        x: f64,
        /// Baseline.
        y: f64,
        /// Ready to draw.
        text: String,
    },
    /// A grab point, at an anchor.
    ///
    /// Emitted so the shell's hit-test is a distance between two screen points
    /// rather than a re-derivation of where the anchors are. It is also why the
    /// shell cannot offer a grab point the engine would not honour.
    Handle {
        /// Across.
        x: f64,
        /// Down.
        y: f64,
        /// Which anchor: `0` or `1`.
        anchor: u8,
    },
}

/// A point in plot fractions: across the plot, and down it.
///
/// The output counterpart of [`Anchor::Fraction`], and a separate type on
/// purpose. An `Anchor` is a position the caller may give in *either* unit; this
/// is always a fraction, so a `SceneDrawing` field of this type cannot report a
/// timestamp where the shell expects a fraction. The two are the same two
/// numbers and would be one type if the units could not be confused, which is
/// exactly what they can be.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Fraction {
    /// Across the plot: 0 at the left edge, 1 at the right.
    pub x: f64,
    /// Down the plot: 0 at the top, 1 at the bottom.
    pub y: f64,
}

/// One drawing, positioned, with its anchors resolved.
///
/// Carries the anchors as well as the shapes, and that is the point: the shell
/// needs the absolute form to **save** what the user just dragged. A scene that
/// reported only pixels would leave the shell with no way to persist a change
/// except to invert the price mapping, which is arithmetic and is forbidden
/// there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneDrawing {
    /// The caller's id, echoed.
    pub id: String,
    /// What shape it is, which is also the shell's colour key.
    pub kind: DrawingKind,
    /// What the user called it.
    pub label: Option<String>,
    /// Whether it is selected, so the shell can highlight it.
    pub selected: bool,
    /// The first anchor, always absolute.
    pub a1: Anchor,
    /// The second, always absolute, for the kinds that have one.
    pub a2: Option<Anchor>,
    /// Where the first anchor sits in plot fractions.
    ///
    /// Reported so a *body* drag can be a screen-space delta. Moving a whole
    /// drawing means moving both anchors by the same amount, and the only unit
    /// the shell can add in is the fraction it already divides by for `pan`.
    /// Without this the shell has two ways to be wrong and no way to be right:
    /// invert the price mapping itself, or drag one anchor and distort the
    /// shape -- which is what it did before this field existed.
    pub a1_fraction: Fraction,
    /// The second, for the kinds that have one.
    pub a2_fraction: Option<Fraction>,
    /// The shapes to draw, in order.
    pub parts: Vec<DrawingPart>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawing(kind: DrawingKind, a2: Option<Anchor>) -> Drawing {
        Drawing {
            id: "d1".into(),
            kind,
            a1: Anchor::Absolute {
                time: 1_767_225_600_000.0,
                price: 45_000.0,
            },
            a2,
            label: None,
            selected: false,
        }
    }

    fn second() -> Option<Anchor> {
        Some(Anchor::Absolute {
            time: 1_767_229_200_000.0,
            price: 45_500.0,
        })
    }

    #[test]
    fn only_a_horizontal_line_has_one_anchor() {
        assert!(!DrawingKind::Hline.needs_second_anchor());
        for kind in [DrawingKind::Trendline, DrawingKind::Rect, DrawingKind::Fib] {
            assert!(kind.needs_second_anchor(), "{kind:?} needs two anchors");
        }
    }

    #[test]
    fn a_two_anchor_kind_with_one_anchor_is_refused_by_name() {
        // Refused rather than drawn as a dot. A `fib` with one anchor has no
        // levels to compute and a `rect` has no area, and either would appear on
        // the chart as nothing at all -- which reads as "the drawing was lost"
        // rather than as "this request was wrong".
        for kind in [DrawingKind::Trendline, DrawingKind::Rect, DrawingKind::Fib] {
            let error = drawing(kind, None)
                .validate_anchors()
                .expect_err("must refuse");
            assert!(error.contains(kind.name()), "{error}");
            assert!(error.contains("two anchors"), "{error}");
        }
    }

    #[test]
    fn every_kind_accepts_the_anchors_it_needs() {
        for kind in DrawingKind::ALL {
            let a2 = if kind.needs_second_anchor() {
                second()
            } else {
                None
            };
            assert_eq!(drawing(kind, a2).validate_anchors(), Ok(()), "{kind:?}");
        }
    }

    #[test]
    fn a_horizontal_line_may_also_carry_a_second_anchor() {
        // Permitted rather than refused, because the shell does not send one and
        // refusing a field nobody sets is a rule with no user. What matters is
        // that the *storage* rule is checked on the way in, which it is -- see
        // the route's validator.
        assert_eq!(
            drawing(DrawingKind::Hline, second()).validate_anchors(),
            Ok(())
        );
    }

    #[test]
    fn no_hostile_number_produces_a_drawable_drawing() {
        // A `NaN` price produces a `NaN` y, and a canvas silently ignores a
        // `NaN` -- so the drawing would be absent with nothing saying why. The
        // sweep is over both variants and both anchors because the guard has two
        // arms and only one of them was obvious.
        let hostile = [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            f64::MIN,
        ];
        let mut refused = 0;
        let mut accepted = 0;
        for bad in hostile {
            for anchor in [
                Anchor::Absolute {
                    time: bad,
                    price: 1.0,
                },
                Anchor::Absolute {
                    time: 1.0,
                    price: bad,
                },
                Anchor::Fraction { x: bad, y: 0.5 },
                Anchor::Fraction { x: 0.5, y: bad },
            ] {
                for a2 in [None, Some(anchor)] {
                    let candidate = Drawing {
                        a1: anchor,
                        ..drawing(DrawingKind::Hline, a2)
                    };
                    match candidate.validate_anchors() {
                        Ok(()) => accepted += 1,
                        Err(_) => refused += 1,
                    }
                }
            }
        }
        // `f64::MAX` and `f64::MIN` are *finite*, so they are legitimate
        // positions with an illegitimate magnitude -- they must be accepted
        // here and dealt with by the mapping, which is where a number too large
        // to draw is a drawing off the canvas rather than an invalid one.
        assert!(refused > 0, "nothing was refused, so this checks nothing");
        assert!(accepted > 0, "everything was refused, which is its own bug");
    }

    #[test]
    fn the_anchor_check_says_nothing_about_the_id() {
        // Deliberately not this function's business. The id is the shell's and
        // the scene's, and storage mints its own -- so a route validating a
        // request body has no id to give, and a check here would force it to
        // invent one. `scene::place` is where the id is required, and it is
        // asserted there.
        let mut candidate = drawing(DrawingKind::Hline, None);
        candidate.id = String::new();
        assert_eq!(candidate.validate_anchors(), Ok(()));
    }

    #[test]
    fn the_anchor_wire_is_pinned() {
        // Three consumers read this shape: the shell, the API that stores it,
        // and the database's millisecond boundary. A rename here is not a
        // compile error in the shell -- it is a drawing that lands at 1970.
        let json = serde_json::to_value(Anchor::Absolute {
            time: 1_767_225_600_000.0,
            price: 45_000.0,
        })
        .expect("serializes");
        assert_eq!(json["unit"], "absolute");
        assert_eq!(json["time"], 1_767_225_600_000.0);
        assert_eq!(json["price"], 45_000.0);

        let fraction =
            serde_json::to_value(Anchor::Fraction { x: 0.25, y: 0.75 }).expect("serializes");
        assert_eq!(fraction["unit"], "fraction");
        assert_eq!(fraction["x"], 0.25);
        assert_eq!(fraction["y"], 0.75);

        // And a fraction does not deserialize as an absolute one, which is the
        // property the two field-name sets exist for.
        assert!(serde_json::from_value::<Anchor>(serde_json::json!({
            "unit": "fraction", "time": 0.5, "price": 0.5
        }))
        .is_err());
    }

    #[test]
    fn a_fraction_is_not_a_time() {
        assert_eq!(
            Anchor::Fraction { x: 0.5, y: 0.5 }.absolute(),
            None,
            "a fraction's x is not a timestamp, and returning one would be the \
             bug the two variants exist to prevent"
        );
        assert_eq!(
            Anchor::Absolute {
                time: 12.0,
                price: 34.0
            }
            .absolute(),
            Some((12.0, 34.0))
        );
    }

    #[test]
    fn every_kind_name_is_a_snake_case_wire_value() {
        for kind in DrawingKind::ALL {
            let json = serde_json::to_value(kind).expect("serializes");
            assert_eq!(json, kind.name(), "{kind:?} renamed on the wire");
        }
    }

    #[test]
    fn the_fib_levels_are_the_retracement_set() {
        // Pinned because a level set is a claim about what the drawing means.
        // The extensions are deliberately absent: they are a different drawing,
        // and shipping them under this name would say something the user did
        // not ask for.
        let ratios: Vec<f64> = FIB_LEVELS.iter().map(|(ratio, _)| *ratio).collect();
        assert_eq!(ratios, vec![0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0]);
        // Ascending, so the labels read top to bottom in one direction and the
        // drawing cannot appear scrambled.
        assert!(ratios.windows(2).all(|pair| pair[0] < pair[1]));
        // The labels are the percentages, already formatted.
        assert_eq!(FIB_LEVELS[4].1, "61.8");
    }

    #[test]
    fn a_part_serializes_with_its_shape_tag() {
        // The shell switches on `shape`, so it has to be on the wire and it has
        // to be snake_case.
        let json = serde_json::to_value(DrawingPart::Handle {
            x: 10.0,
            y: 20.0,
            anchor: 1,
        })
        .expect("serializes");
        assert_eq!(json["shape"], "handle");
        assert_eq!(json["anchor"], 1);

        let segment = serde_json::to_value(DrawingPart::Segment {
            x1: 0.0,
            y1: 0.0,
            x2: 1.0,
            y2: 1.0,
            dashed: false,
        })
        .expect("serializes");
        assert_eq!(segment["shape"], "segment");
        assert_eq!(segment["dashed"], false);
    }

    #[test]
    fn a_fraction_is_a_plain_pair_and_carries_no_unit() {
        // `Anchor::Fraction` is tagged, because an anchor may also be absolute
        // and the tag is how the two are told apart. This is not: it is only
        // ever a fraction, so it is only ever two numbers. A `unit` here would
        // be a field the shell reads and can never find to be anything else.
        let json = serde_json::to_value(Fraction { x: 0.25, y: 0.75 }).expect("serializes");
        assert_eq!(json["x"], 0.25);
        assert_eq!(json["y"], 0.75);
        assert!(json["unit"].is_null(), "{json}");

        let back: Fraction = serde_json::from_value(json).expect("round trips");
        assert_eq!(back, Fraction { x: 0.25, y: 0.75 });
    }

    #[test]
    fn a_fraction_is_not_an_anchor() {
        // The two are the same two numbers and deliberately not one type. This
        // asserts the distinction is real on the wire as well as in the type
        // system: the tagged form and the plain one are different JSON, so a
        // fraction cannot be dropped where an anchor is expected and parse.
        let anchor = serde_json::to_value(Anchor::Fraction { x: 0.25, y: 0.75 }).expect("ok");
        let fraction = serde_json::to_value(Fraction { x: 0.25, y: 0.75 }).expect("ok");
        assert_eq!(anchor["unit"], "fraction");
        assert_ne!(anchor, fraction);
        assert!(
            serde_json::from_value::<Anchor>(fraction.clone()).is_err(),
            "a bare fraction must not parse as an anchor: {fraction}"
        );
    }
}
