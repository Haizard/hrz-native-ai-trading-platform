//! What the user is *looking at*, handed to the agent alongside their question.
//!
//! ## The gap this closes
//!
//! Every existing entry point asks the agent about a **symbol**. None asks about
//! a **view**. A user who has scrolled to a swing high on the 1h, drawn a level,
//! and typed "what is this?" has given the agent a symbol and a sentence -- so
//! the agent reads the default 1D/4H/1H/5M ladder from the newest bars and
//! answers a question nobody asked. It is not wrong; it is answering about a
//! different part of the chart than the one on screen.
//!
//! The fix is to send the viewport: timeframe, visible time range, the price
//! axis in view, whatever the user has drawn, and -- when the shell can produce
//! one -- a screenshot of the canvas. The agent then reasons about the window
//! that is actually in front of the user.
//!
//! ## The rule this module is built around
//!
//! **A viewport is a hint, never an instruction.** It says where to *look*; it
//! never says what is *there*. Every number in the packet is echoed verbatim
//! from the shell and is subject to the same grounding rule as any tool result:
//! if the agent wants to cite a price, it cites a tool or the ladder digest, not
//! "the user's screen looked like it was around 60k". A screenshot is
//! *illustration*, not evidence.
//!
//! That distinction is why [`ChartContext::render`] states the provenance of
//! each part explicitly, and why a screenshot is rendered as a description of
//! its subject ("a screenshot of the BTCUSDT 1h chart") rather than as anything
//! the model is invited to read numbers from. Letting a vision model transcribe
//! prices off a rendered canvas would reintroduce exactly the arithmetic-by-eye
//! failure principle #2 forbids -- and a canvas pixel is not a tick.
//!
//! ## Why `Option` everywhere
//!
//! The shell may not know any of this. An old client sends only a symbol; a test
//! harness sends nothing. Every field is optional and the renderer simply omits
//! what is absent -- there is no placeholder for "unknown timeframe", because a
//! placeholder is a fact the agent would then reason from.

use serde::{Deserialize, Serialize};

use analytics_core::Timeframe;

/// Most drawings carried into one prompt.
///
/// A user can have hundreds of levels on a chart. Past a couple of dozen the
/// list stops being context and starts being noise that pushes the actual
/// question out of the model's attention -- and a plan has a token cost.
pub const MAX_DRAWINGS: usize = 24;

/// Most screenshot bytes accepted.
///
/// The shell downscales before sending; this is the backstop for a client that
/// does not. Roughly 4 MB is a 1500x900 PNG or a generous JPEG -- enough for a
/// legible chart, and small enough that a request cannot be used to make the
/// gateway buffer something unbounded.
pub const MAX_SCREENSHOT_BYTES: usize = 4 * 1024 * 1024;

/// The medias the packet will carry.
///
/// An allowlist rather than a denylist: a `content-type` the vision model cannot
/// decode must be refused at the edge, not forwarded and silently dropped
/// upstream, which would leave the user believing the agent saw their chart.
pub const SCREENSHOT_MEDIA_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// A price level or marker the user has drawn on the chart.
///
/// Deliberately a flat, shell-shaped record rather than `chart_engine::Drawing`:
/// the agent does not need the anchor geometry, and depending on the chart
/// engine's full type would make every drawing-kind change a change to the AI
/// crate. What the agent needs is "the user marked this price, and called it
/// that".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawnLevel {
    /// What kind of mark it is, in the user's vocabulary.
    ///
    /// Free text from the shell (`"horizontal"`, `"trendline"`, `"zone"`) rather
    /// than an enum, because a chart engine that adds a tool should not have to
    /// change this crate to say so. It is rendered as a label, never branched on.
    pub kind: String,
    /// The price, when the mark has one. A trendline has two; the packet carries
    /// the more recent anchor, which is the one the user is pointing at.
    pub price: Option<f64>,
    /// The user's own label, if they typed one.
    pub label: Option<String>,
}

/// A screenshot of the chart canvas, as uploaded by the shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartScreenshot {
    /// Media type, e.g. `image/png`. Must be in [`SCREENSHOT_MEDIA_TYPES`].
    pub media_type: String,
    /// Base64-encoded image data.
    pub data: String,
}

impl ChartScreenshot {
    /// Validate a screenshot, returning it or the reason to refuse it.
    ///
    /// This runs at the **edge**, before the request is queued behind a paid
    /// model call. A screenshot the provider cannot decode would otherwise cost
    /// a full turn and come back as an opaque upstream error.
    ///
    /// # Errors
    /// A sentence naming what is wrong, suitable for showing the user.
    pub fn validate(self) -> Result<Self, String> {
        if !SCREENSHOT_MEDIA_TYPES.contains(&self.media_type.as_str()) {
            return Err(format!(
                "the chart screenshot is `{}`, which the model cannot read. Send one of: {}.",
                self.media_type,
                SCREENSHOT_MEDIA_TYPES.join(", ")
            ));
        }
        if self.data.is_empty() {
            return Err("the chart screenshot carries no data.".to_string());
        }
        // Base64 is 4 bytes per 3, so the encoded size overstates the image by
        // ~33%. Comparing the encoded string directly to the byte budget would
        // reject a legal 3 MB image, so the budget is scaled up to match.
        let encoded_budget = MAX_SCREENSHOT_BYTES / 3 * 4;
        if self.data.len() > encoded_budget {
            return Err(format!(
                "the chart screenshot is about {} KB, over the {} KB limit. Capture a smaller \
                 region or let the shell downscale it.",
                self.data.len() / 1024 * 3 / 4,
                MAX_SCREENSHOT_BYTES / 1024
            ));
        }
        Ok(self)
    }

    /// A one-line description for the prompt and the trace.
    ///
    /// Exists so the trace records *that* a screenshot was attached and how big
    /// it was, without the trace itself becoming a megabyte of image data.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} screenshot, about {} KB",
            self.media_type,
            self.data.len() / 1024 * 3 / 4
        )
    }
}

/// The chart the user is looking at when they ask.
///
/// Every field is optional: a client that knows only the symbol sends only the
/// symbol, and the agent behaves exactly as it did before this type existed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartContext {
    /// The resolution on screen. Overrides the skill/default ladder anchor when
    /// present, because the user is pointing at *this* one.
    pub timeframe: Option<Timeframe>,
    /// Open time of the oldest visible bar, unix nanoseconds.
    pub visible_from_ns: Option<i64>,
    /// Open time of the newest visible bar, unix nanoseconds.
    pub visible_to_ns: Option<i64>,
    /// Lowest price on the visible axis.
    ///
    /// Sent so the agent can tell "the user is zoomed into a 200-point range"
    /// from "the user is looking at the whole year" -- which changes what
    /// "this level" plausibly refers to.
    pub price_low: Option<f64>,
    /// Highest price on the visible axis.
    pub price_high: Option<f64>,
    /// Marks the user has drawn, in screen order.
    #[serde(default)]
    pub drawings: Vec<DrawnLevel>,
    /// A capture of the canvas, when the shell produced one.
    pub screenshot: Option<ChartScreenshot>,
}

impl ChartContext {
    /// Whether the packet carries nothing worth rendering.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.timeframe.is_none()
            && self.visible_from_ns.is_none()
            && self.visible_to_ns.is_none()
            && self.price_low.is_none()
            && self.price_high.is_none()
            && self.drawings.is_empty()
            && self.screenshot.is_none()
    }

    /// A one-line summary for the trace and the logs.
    ///
    /// Names the screenshot by description rather than by data, so a trace can
    /// be logged and stored without carrying the image.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "no chart context".to_string();
        }
        let mut parts: Vec<String> = Vec::new();
        if let Some(timeframe) = self.timeframe {
            parts.push(format!("{timeframe} chart"));
        }
        if let (Some(from), Some(to)) = (self.visible_from_ns, self.visible_to_ns) {
            parts.push(format!("{} visible bars", visible_bars(from, to, self.timeframe)));
        }
        if !self.drawings.is_empty() {
            parts.push(format!("{} drawings", self.drawings.len()));
        }
        if let Some(screenshot) = &self.screenshot {
            parts.push(screenshot.describe());
        }
        parts.join(", ")
    }

    /// The number of bars the viewport spans, when it is knowable.
    #[must_use]
    pub fn visible_bars(&self) -> Option<i64> {
        match (self.visible_from_ns, self.visible_to_ns, self.timeframe) {
            (Some(from), Some(to), Some(timeframe)) => Some(visible_bars(from, to, Some(timeframe))),
            _ => None,
        }
    }

    /// Drop the drawings past [`MAX_DRAWINGS`], keeping the most recent.
    ///
    /// The shell sends them oldest-first, so the tail is the newest -- and the
    /// newest marks are the ones a user is most likely to be asking about. This
    /// is a clamp rather than a rejection because a busy chart is not an error.
    pub fn clamp_drawings(&mut self) {
        if self.drawings.len() > MAX_DRAWINGS {
            let drop = self.drawings.len() - MAX_DRAWINGS;
            self.drawings.drain(..drop);
        }
    }

    /// Render the packet as a prompt section.
    ///
    /// Returns `None` when there is nothing to say, so callers append
    /// unconditionally without an empty heading in the prompt -- a heading with
    /// nothing under it reads to a model as "this was omitted on purpose".
    #[must_use]
    pub fn render(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }

        let mut out = String::new();
        out.push_str("## What the user is looking at\n");
        out.push_str(
            "The user sent this with their question. It says where they are on the \
             chart, never what the market did -- every price below is one they \
             supplied, and none of it is a substitute for a tool result.\n\n",
        );

        if let Some(timeframe) = self.timeframe {
            out.push_str(&format!("- Resolution on screen: `{timeframe}`\n"));
        }
        if let (Some(from), Some(to)) = (self.visible_from_ns, self.visible_to_ns) {
            out.push_str(&format!(
                "- Visible range: {} to {} ({} bars)\n",
                format_ns(from),
                format_ns(to),
                visible_bars(from, to, self.timeframe)
            ));
        }
        if let (Some(low), Some(high)) = (self.price_low, self.price_high) {
            out.push_str(&format!(
                "- Visible price axis: {low:.4} to {high:.4}\n"
            ));
        }

        if !self.drawings.is_empty() {
            out.push_str("\n- The user has drawn:\n");
            for level in &self.drawings {
                out.push_str("  - ");
                if let Some(price) = level.price {
                    out.push_str(&format!("{price:.4}"));
                } else {
                    out.push_str("(no single price)");
                }
                out.push_str(&format!(" -- {}", level.kind));
                if let Some(label) = &level.label {
                    out.push_str(&format!(", labelled \"{label}\""));
                }
                out.push('\n');
            }
        }

        if let Some(screenshot) = &self.screenshot {
            out.push_str(&format!(
                "\nA {} of the canvas is attached. Use it to see *where* the user is \
                 looking and what they have marked -- the shape of the setup, which \
                 bars they mean, which level they drew. Do not read prices off it, \
                 and do not treat anything you see in it as a measurement: it is an \
                 illustration of a viewport, and every number you cite still has to \
                 come from a tool.\n",
                screenshot.describe()
            ));
        }

        out.push_str(
            "\n### Referring to what they see\n\
             When the question uses a deictic -- \"this\", \"here\", \"that level\" -- \
             it means this viewport, not the market's latest bar. Read the window \
             with a tool if you need its contents.\n",
        );

        Some(out)
    }

    /// The timeframe the ladder should be anchored on.
    ///
    /// The visible resolution wins over the skill's ladder because the user is
    /// pointing at it, but only when they actually sent one -- otherwise the
    /// existing skill-then-default resolution stands.
    #[must_use]
    pub fn ladder_anchor(&self) -> Option<Timeframe> {
        self.timeframe
    }
}

/// Bars between two timestamps at a resolution, inclusive of both ends.
///
/// Three outcomes, and the difference matters:
///
/// * **one end time only** -- the resolution was not sent, so the span cannot be
///   divided into bars. `-1` says "not knowable" rather than guessing a
///   resolution, which would produce a confident wrong count.
/// * **to before from** -- an inverted viewport, which a zoomed-flipped chart can
///   genuinely produce. `-1` rather than a negative bar count.
/// * otherwise the count, which is `span / width + 1` because both endpoints are
///   bars in the range.
fn visible_bars(from_ns: i64, to_ns: i64, timeframe: Option<Timeframe>) -> i64 {
    let Some(timeframe) = timeframe else {
        return -1;
    };
    let width = timeframe.nanos().max(1);
    if to_ns < from_ns {
        return -1;
    }
    (to_ns - from_ns) / width + 1
}

/// Render a unix-nanosecond timestamp as an ISO-8601 UTC instant.
///
/// Hand-rolled rather than pulled from `chrono`, which `ai-agent` does not
/// otherwise depend on: a single formatting call is not worth a dependency edge,
/// and every consumer already treats these as plain integers on the wire.
///
/// A timestamp that will not fit a civil date is rendered as its raw value
/// rather than as an epoch, so a corrupt field is visible instead of looking
/// like 1970.
fn format_ns(ns: i64) -> String {
    const NS_PER_SEC: i64 = 1_000_000_000;
    let seconds = ns.div_euclid(NS_PER_SEC);
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    let (hour, minute, second) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );

    // Days since 1970-01-01 to a civil date, by the standard era decomposition
    // (Howard Hinnant's `civil_from_days`). Written out because the alternative
    // is a date library, and because a leap-year bug here would silently
    // misdate every level a user drew.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    // A field that produced a nonsense year is corrupt, and saying so beats
    // rendering "1970-01-01" for a value that was simply wrong.
    if !(1..=9999).contains(&y) {
        return format!("{ns}ns");
    }
    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(price: f64, label: &str) -> DrawnLevel {
        DrawnLevel {
            kind: "horizontal".into(),
            price: Some(price),
            label: Some(label.into()),
        }
    }

    #[test]
    fn an_empty_packet_renders_nothing_at_all() {
        // The point is not brevity: an empty heading in a prompt reads to a
        // model as "something was withheld", which invites it to speculate.
        let context = ChartContext::default();
        assert!(context.is_empty());
        assert!(context.render().is_none());
        assert_eq!(context.summary(), "no chart context");
    }

    #[test]
    fn a_viewport_renders_its_resolution_range_and_axis() {
        // 1_788_739_200 = 2026-09-07T00:00:00Z, a Monday -- verified against the
        // calendar, not computed in my head. Getting this wrong is how a test
        // ends up asserting the bug rather than the fix.
        let monday = 1_788_739_200i64;
        let context = ChartContext {
            timeframe: Some(Timeframe::H1),
            visible_from_ns: Some(monday * 1_000_000_000),
            visible_to_ns: Some((monday + 11 * 3600) * 1_000_000_000),
            price_low: Some(110_000.0),
            price_high: Some(118_500.0),
            ..Default::default()
        };

        let rendered = context.render().expect("a viewport is worth rendering");
        assert!(rendered.contains("`1h`"), "{rendered}");
        assert!(rendered.contains("2026-09-07T00:00:00Z"), "{rendered}");
        assert!(rendered.contains("2026-09-07T11:00:00Z"), "{rendered}");
        assert!(rendered.contains("(12 bars)"), "{rendered}");
        assert!(rendered.contains("110000.0000 to 118500.0000"), "{rendered}");
        assert_eq!(context.visible_bars(), Some(12));
    }

    #[test]
    fn drawings_are_rendered_with_their_prices_and_labels() {
        let mut context = ChartContext {
            timeframe: Some(Timeframe::M15),
            ..Default::default()
        };
        context.drawings.push(level(112_500.0, "weekly open"));
        context.drawings.push(DrawnLevel {
            kind: "trendline".into(),
            price: Some(111_800.0),
            label: None,
        });
        context.drawings.push(DrawnLevel {
            kind: "zone".into(),
            price: None,
            label: Some("supply".into()),
        });

        let rendered = context.render().expect("drawings are worth rendering");
        assert!(rendered.contains("112500.0000"), "{rendered}");
        assert!(rendered.contains("labelled \"weekly open\""), "{rendered}");
        assert!(rendered.contains("trendline"), "{rendered}");
        // A mark with no single price must say so rather than render a zero,
        // which the model would then treat as a level at 0. The assertion is
        // anchored to the line's start, because a level at 112500.0 also
        // contains the substring "0.0000" and a bare `contains` would pass on
        // exactly the output this test exists to reject.
        assert!(
            rendered.contains("  - (no single price) -- zone"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("  - 0.0000 -- zone"),
            "a pricelist entry of 0 reads to a model as a real level at zero: {rendered}"
        );
    }

    #[test]
    fn a_screenshot_is_described_and_explicitly_not_read_for_numbers() {
        let context = ChartContext {
            timeframe: Some(Timeframe::H4),
            screenshot: Some(ChartScreenshot {
                media_type: "image/png".into(),
                data: "A".repeat(4_000),
            }),
            ..Default::default()
        };

        let rendered = context.render().expect("a screenshot is worth rendering");
        assert!(rendered.contains("image/png screenshot"), "{rendered}");
        assert!(
            rendered.contains("Do not read prices off it"),
            "the grounding rule has to be stated where the screenshot is introduced, \
             because that is the only place the model is told it exists: {rendered}"
        );
        // And the trace line must not carry the image itself.
        assert!(context.summary().contains("image/png screenshot"));
        assert!(
            !context.summary().contains("AAAA"),
            "a summary that embeds the payload makes every log entry a megabyte"
        );
    }

    #[test]
    fn the_screenshot_allowlist_refuses_what_the_model_cannot_read() {
        let bad = ChartScreenshot {
            media_type: "image/svg+xml".into(),
            data: "AAAA".into(),
        };
        let error = bad.validate().expect_err("svg is not a raster the model reads");
        assert!(error.contains("cannot read"), "{error}");
        assert!(error.contains("image/png"), "the error lists what *is* accepted: {error}");

        let empty = ChartScreenshot {
            media_type: "image/png".into(),
            data: String::new(),
        };
        assert!(empty.validate().is_err(), "an empty payload is not a screenshot");

        let good = ChartScreenshot {
            media_type: "image/jpeg".into(),
            data: "A".repeat(1000),
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn the_size_limit_is_measured_in_image_bytes_not_base64_characters() {
        // A legal image just under the budget, base64-encoded, is ~33% longer
        // than the budget in characters. Comparing the encoded length directly
        // would reject an image the user is entitled to send.
        let image_bytes = MAX_SCREENSHOT_BYTES - 1024;
        let encoded_len = image_bytes / 3 * 4 + 8;
        let screenshot = ChartScreenshot {
            media_type: "image/png".into(),
            data: "A".repeat(encoded_len),
        };
        assert!(
            screenshot.validate().is_ok(),
            "an image under the byte budget must be accepted"
        );

        // And an image genuinely over the budget is still refused.
        let over = ChartScreenshot {
            media_type: "image/png".into(),
            data: "A".repeat(MAX_SCREENSHOT_BYTES / 3 * 4 + 4096),
        };
        assert!(over.validate().is_err());
    }

    #[test]
    fn drawings_are_clamped_to_the_newest_rather_than_rejected() {
        let mut context = ChartContext::default();
        for i in 0..(MAX_DRAWINGS + 10) {
            context.drawings.push(level(100.0 + i as f64, &format!("L{i}")));
        }
        context.clamp_drawings();

        assert_eq!(context.drawings.len(), MAX_DRAWINGS);
        // The tail is what survives: a user asking about a chart is asking about
        // what they drew most recently.
        assert_eq!(
            context.drawings.last().expect("a last drawing").label.as_deref(),
            Some("L33")
        );
        assert_eq!(
            context.drawings.first().expect("a first drawing").label.as_deref(),
            Some("L10")
        );
    }

    #[test]
    fn a_viewport_without_a_resolution_reports_no_bar_count() {
        // `-1` rather than a guess: dividing an hour span by an assumed
        // resolution is how "12 bars" becomes a fact the model cites.
        let context = ChartContext {
            visible_from_ns: Some(1_789_056_000_000_000_000),
            visible_to_ns: Some(1_789_059_600_000_000_000),
            ..Default::default()
        };
        assert_eq!(context.visible_bars(), None);
        let rendered = context.render().expect("a range is still worth rendering");
        assert!(rendered.contains("(-1 bars)"), "{rendered}");
        assert!(!rendered.contains("(12 bars)"), "{rendered}");
    }

    #[test]
    fn an_inverted_viewport_does_not_produce_a_negative_bar_count() {
        let context = ChartContext {
            timeframe: Some(Timeframe::M5),
            visible_from_ns: Some(1_789_059_600_000_000_000),
            visible_to_ns: Some(1_789_056_000_000_000_000),
            ..Default::default()
        };
        assert_eq!(context.visible_bars(), Some(-1));
    }

    #[test]
    fn a_known_instant_renders_as_its_civil_date() {
        // Anchored to a value checked against the calendar, not to arithmetic I
        // did in my head -- that is how the `1w` bucket bug survived six rounds
        // of testing.
        // 1_788_739_200 = 2026-09-07T00:00:00Z (a Monday).
        assert_eq!(
            format_ns(1_788_739_200 * 1_000_000_000),
            "2026-09-07T00:00:00Z"
        );
        // 0 is the epoch, and leap years have to survive the decomposition.
        assert_eq!(format_ns(0), "1970-01-01T00:00:00Z");
        // 2000-02-29T12:34:56Z, a leap day in a century leap year.
        assert_eq!(
            format_ns(951_827_696 * 1_000_000_000),
            "2000-02-29T12:34:56Z"
        );
        // Sub-second precision is dropped, not rounded into the next second.
        assert_eq!(format_ns(999_999_999), "1970-01-01T00:00:00Z");
        // Before the epoch still works, via `div_euclid` rather than `/`.
        assert_eq!(format_ns(-1) , "1969-12-31T23:59:59Z");
    }

    #[test]
    fn the_visible_resolution_anchors_the_ladder_when_one_is_sent() {
        let context = ChartContext {
            timeframe: Some(Timeframe::M5),
            ..Default::default()
        };
        assert_eq!(context.ladder_anchor(), Some(Timeframe::M5));

        // No timeframe: the existing skill-then-default resolution stands, and
        // the packet must not force a choice.
        assert_eq!(ChartContext::default().ladder_anchor(), None);
    }

    #[test]
    fn the_wire_shape_survives_a_round_trip() {
        // The shell is JavaScript, so these names are a contract no compiler
        // checks -- the same trap that silently emptied every instrument's base
        // asset in `market-data::symbols`. Pin the exact spelling.
        let context = ChartContext {
            timeframe: Some(Timeframe::M15),
            visible_from_ns: Some(1_789_056_000_000_000_000),
            visible_to_ns: Some(1_789_059_600_000_000_000),
            price_low: Some(1.0),
            price_high: Some(2.0),
            drawings: vec![level(1.5, "marked")],
            screenshot: Some(ChartScreenshot {
                media_type: "image/png".into(),
                data: "AAAA".into(),
            }),
        };

        let json = serde_json::to_value(&context).expect("serializes");
        assert_eq!(json["timeframe"], "15m", "the wire format is lowercase: {json}");
        assert_eq!(json["visible_from_ns"], 1_789_056_000_000_000_000i64);
        assert_eq!(json["visible_to_ns"], 1_789_059_600_000_000_000i64);
        assert_eq!(json["price_low"], 1.0);
        assert_eq!(json["price_high"], 2.0);
        assert_eq!(json["drawings"][0]["kind"], "horizontal");
        assert_eq!(json["drawings"][0]["price"], 1.5);
        assert_eq!(json["drawings"][0]["label"], "marked");
        assert_eq!(json["screenshot"]["media_type"], "image/png");
        assert_eq!(json["screenshot"]["data"], "AAAA");

        let back: ChartContext = serde_json::from_value(json).expect("deserializes");
        assert_eq!(back, context);
    }

    #[test]
    fn a_screenshot_and_drawings_may_both_be_absent() {
        // The shape an older client sends: camelCase-free, everything optional.
        let json = serde_json::json!({ "timeframe": "4h" });
        let context: ChartContext = serde_json::from_value(json).expect("deserializes");
        assert_eq!(context.timeframe, Some(Timeframe::H4));
        assert!(context.drawings.is_empty());
        assert!(context.screenshot.is_none());
        assert!(!context.is_empty());
    }
}
