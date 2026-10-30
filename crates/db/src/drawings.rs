//! Drawings: the shapes a user has drawn on a symbol (`docs/14`).
//!
//! ## A list of points, and a kind that says how many
//!
//! Everything a trader draws is `(time, price)` points with a rule for what goes
//! between them: `kind`, and a list of points. A trendline is the segment, a
//! rectangle is the area, a Fibonacci is the levels, a horizontal line is the
//! one-point case, a parallel channel is two points for the line plus one for
//! its width, and a polyline or an Elliott impulse is a point per click however
//! many the user places.
//!
//! Storage holds the list and no rule about its length. This used to keep a
//! second copy of the engine's arity -- two `needs_*` functions whose case lists
//! had to be edited in step with an enum in another crate, and whose only real
//! test was a comparison of the two copies against each other. The route checks
//! the shape with `chart_engine::DrawingKind::anchors` before it gets here, so
//! what is in the column is a list the engine already said it can draw. The one
//! thing with no per-kind exception is a `CHECK` on the column: an empty list is
//! not a shape.
//!
//! ## This is the one user-authored table that is edited
//!
//! Everywhere else in this schema a user's record is append-only, because the
//! question a review asks is "when did this change?" -- see
//! [`record_venue_opt_in`](crate::live::record_venue_opt_in). A drawing is not a
//! consent record. It is a shape the user is still moving, and an append-only
//! table would collect one row per drag of the mouse. The history that matters
//! for a drawing is the drawing.
//!
//! ## Milliseconds, not nanoseconds, and only here
//!
//! The platform's internal clock is unix nanoseconds (`docs/13`) and
//! [`crate::repositories::dt_to_ns`] is how everything else crosses that
//! boundary. A point's time does not use it, deliberately: a point is the value
//! that makes the round trip *out* to a browser and *back* into the database, and
//! JSON numbers are doubles. A nanosecond timestamp is past 2^53, so one read
//! into JavaScript and written back is a different number -- a few hundred
//! nanoseconds, which is invisible on a chart and is still a write that changed
//! the data. Milliseconds fit exactly, so the times in this column are
//! milliseconds and nothing converts them.
//!
//! `points` is `JSONB`, which is what the paragraph above forced: a fixed number
//! of anchor columns cannot hold a drawing with five points, and keeping
//! `a1_time … a3_price` beside a `points` array is two vocabularies for one fact.
//! The case against JSONB was made in `0011` for a three-anchor vocabulary and
//! expired when the vocabulary outgrew it -- the note at the top of
//! `0018_drawing_points.sql` records which of its arguments still hold.
//!
//! `created_at` and `updated_at` are reported in nanoseconds like every other
//! timestamp in this crate, because nothing sends them back.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::dt_to_ns;

/// The kinds the engine knows how to draw.
///
/// A closed list, and it lives here rather than in the route because it is the
/// storage's vocabulary: `kind` is a `TEXT` column with no `CHECK`, so this
/// array and the engine's enum are the only two places the set is written down,
/// and `a_kind_the_engine_cannot_draw_is_refused` keeps them in step.
///
/// The 2026-09 parity extension added the TradingView staples: channel, angle,
/// arc, circle, triangle, long/short position boxes and the date/price range
/// -- see `docs/21`. Each is one variant plus a `draw/` arm in the engine.
/// `kline` is listed but is the parity oddball: it is a bar chart per day and
/// draws with the candle geometry, not with anchors, so it lives in the
/// registry while storage keeps its own vocabulary.
///
/// The 2026-10 drawing-parity pass adds the rest of the lines group (a level that
/// runs one way, a cross, an info line, a regression fit, arrows) and the two
/// families that draw a *band* rather than a line: the disjoint and flat-top/bottom
/// channels, and the four pitchfork templates.
pub const KINDS: [&str; 44] = [
    "trendline",
    "hline",
    "vline",
    "ray",
    "extended",
    "hray",
    "crossline",
    "infoline",
    "regression",
    "arrow",
    "arrow_mark_up",
    "arrow_mark_down",
    "rect",
    "fib",
    "fib_extension",
    "trend_fib_extension",
    "trend_fib_time",
    "fib_time_zone",
    "fib_channel",
    "fib_fan",
    "pitchfan",
    "fib_arcs",
    "fib_circles",
    "fib_spiral",
    "fib_wedge",
    "gann_box",
    "gann_fan",
    "gann_square",
    "gann_square_fixed",
    "measure",
    "channel",
    "disjoint_channel",
    "flat_top_bottom",
    "pitchfork",
    "schiff_pitchfork",
    "modified_schiff_pitchfork",
    "inside_pitchfork",
    "angle",
    "arc",
    "circle",
    "triangle",
    "position_long",
    "position_short",
    "dateprice_range",
];

/// One point of a drawing: its time in **milliseconds** and its price in the
/// instrument's own units.
///
/// The key names are part of the contract, not a detail of this struct:
/// `0018_drawing_points.sql` builds the backfilled array with
/// `jsonb_build_object('time', …, 'price', …)`, so renaming a field here would
/// make the rows that migration wrote unreadable -- and unreadable *quietly*,
/// because [`points_from_json`] answers an unknown shape with the empty list.
/// `stored_point_json_matches_what_the_migration_wrote` pins both keys.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StoredPoint {
    /// Milliseconds since the epoch.
    pub time: f64,
    /// The price at that time.
    pub price: f64,
}

/// A stored drawing, with its points in **milliseconds**.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawingRow {
    /// The row id.
    pub id: Uuid,
    /// The symbol it is drawn on, uppercase.
    pub symbol: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// The shape's points, in the order they were placed. Non-empty on anything
    /// this module wrote; [`points_from_json`] is the only way a read comes back
    /// empty.
    pub points: Vec<StoredPoint>,
    /// How to draw it: whether the line extends past its points, which way a
    /// Fibonacci swing runs, and in later tasks the level set or the row count.
    /// `{}` means "the engine's default for this kind", which is why the column
    /// defaults to an empty object rather than being nullable: a missing key and
    /// a null key would then be two spellings of the same answer.
    pub options: Value,
    /// What the user called it, if anything.
    pub label: Option<String>,
    /// Who is accountable for this row: `None` is a hand-drawn one, and
    /// `Some("ai")` says the agent created it (`0009_drawing_provenance.sql`).
    pub created_by: Option<String>,
    /// Which agent drew it, when [`DrawingRow::created_by`] says one did.
    pub agent: Option<String>,
    /// The model's own confidence, 0..1, when the agent stated one.
    pub confidence: Option<f64>,
    /// The agent's stated reason, when it gave one.
    pub reason: Option<String>,
    /// When it was first stored, unix nanoseconds.
    pub created_at: i64,
    /// When it was last moved, unix nanoseconds.
    pub updated_at: i64,
}

/// A drawing about to be written.
///
/// Separate from [`DrawingRow`] because a row has an id and timestamps the caller
/// does not supply. The points are a `Vec` rather than two or three pairs of
/// fields so that a tool with a variable count -- a polyline, an impulse, a brush
/// stroke -- is the ordinary case here rather than one the struct cannot spell.
///
/// The symbol is *not* a field: it is a parameter of
/// [`create_drawing`] and is deliberately absent from [`update_drawing`], which
/// cannot move a drawing to another instrument. A symbol on this struct would be
/// a field one of the two callers ignores, which is how a later reader comes to
/// believe it is used.
#[derive(Debug, Clone, PartialEq)]
pub struct NewDrawing {
    /// One of [`KINDS`].
    pub kind: String,
    /// The shape's points, in the order they were placed.
    pub points: Vec<StoredPoint>,
    /// How to draw it; see [`DrawingRow::options`]. Use [`no_options`] for the
    /// drawing that has no opinion.
    pub options: Value,
    /// What the user called it, if anything.
    pub label: Option<String>,
    /// Provenance (`0009_drawing_provenance.sql`): who is accountable, which
    /// agent, its stated confidence and reason. `None` provenance is a
    /// hand-drawn object, which is what every writer before the AI path wrote.
    pub provenance: Option<Provenance>,
}

/// Who is accountable for a drawing, and what the model said about it.
///
/// A small struct rather than four loose `Option`s on [`NewDrawing`] because
/// the four travel together or not at all: `agent`/`confidence`/`reason`
/// without `created_by = "ai"` would be provenance nobody owns, and the
/// construction sites are few enough to keep honest.
#[derive(Debug, Clone, PartialEq)]
pub struct Provenance {
    /// `"ai"` for agent-created rows. A vocabulary, not free text, so a filter
    /// can match it exactly.
    pub created_by: String,
    /// The agent's name, e.g. `market-structure`.
    pub agent: Option<String>,
    /// The model's stated confidence, 0..1.
    pub confidence: Option<f64>,
    /// The model's stated reason, in its own words.
    pub reason: Option<String>,
}

/// The options value meaning "draw it the way this kind is drawn by default".
///
/// A function rather than `Value::Object(Default::default())` at each call site
/// because `{}` is a claim about the engine's defaults, not an empty map the
/// caller happened not to fill in -- and a caller that spells it as `null` would
/// be a drawing whose options the engine reads as nothing, which is the same
/// answer by accident rather than by rule.
#[must_use]
pub fn no_options() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Read a point list out of the column's JSON.
///
/// A list that cannot be read becomes empty rather than an error, for the same
/// reason [`crate::repositories::levels_from_json`] is not fallible: this runs
/// inside a `SELECT` over every drawing the user owns, and one row written by a
/// future version of this module -- a key renamed, a point in a shape the current
/// code does not understand -- would otherwise be a 500 for the whole chart. The
/// empty list is then refused by the engine as a drawing with no points, so the
/// cost is that one shape, visibly, rather than no shapes at all.
///
/// The all-or-nothing reading is the deliberate half of that: a five-point
/// polyline whose third point is malformed comes back as nothing, because half a
/// list would be drawn as a different shape and then saved as that other shape
/// the next time the user drags it.
#[must_use]
pub fn points_from_json(value: &Value) -> Vec<StoredPoint> {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

fn row_to_drawing(row: &sqlx::postgres::PgRow) -> Result<DrawingRow, DbError> {
    let points: Value = row.try_get("points")?;
    let options: Value = row.try_get("options")?;
    Ok(DrawingRow {
        id: row.try_get("id")?,
        symbol: row.try_get("symbol")?,
        kind: row.try_get("kind")?,
        points: points_from_json(&points),
        options,
        label: row.try_get("label")?,
        created_by: row.try_get("created_by")?,
        agent: row.try_get("agent")?,
        confidence: row.try_get("confidence")?,
        reason: row.try_get("reason")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
        updated_at: dt_to_ns(row.try_get("updated_at")?),
    })
}

/// This user's drawings on one symbol, oldest first.
///
/// Oldest first rather than newest, because the order decides what covers what
/// when two shapes overlap, and a drawing that moved because a later one was
/// added is a drawing that appears to be unstable.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn list_drawings(
    pool: &PgPool,
    user_id: Uuid,
    symbol: &str,
) -> Result<Vec<DrawingRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, symbol, kind, points, options, label, \
         created_by, agent, confidence, reason, created_at, updated_at \
         FROM drawings WHERE user_id = $1 AND symbol = $2 ORDER BY created_at, id",
    )
    .bind(user_id)
    .bind(symbol)
    .fetch_all(pool)
    .await?;

    rows.iter().map(row_to_drawing).collect()
}

/// Store a new drawing, returning its id.
///
/// `symbol` is a parameter rather than a field of [`NewDrawing`] for the reason
/// the struct's own comment gives: it belongs to one of the two writers.
///
/// # Errors
/// Returns [`DbError::Pool`] if the insert fails -- which covers both ways a
/// write can be refused here: the `CHECK` on the array length, and a point that
/// will not encode, since a non-finite price is not a JSON number. The route
/// refuses both first, so reaching them is a caller that came in by another door.
pub async fn create_drawing(
    pool: &PgPool,
    user_id: Uuid,
    symbol: &str,
    drawing: &NewDrawing,
) -> Result<Uuid, DbError> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO drawings (user_id, symbol, kind, points, options, label, \
         created_by, agent, confidence, reason) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(user_id)
    .bind(symbol)
    .bind(drawing.kind.as_str())
    .bind(sqlx::types::Json(&drawing.points))
    .bind(&drawing.options)
    .bind(drawing.label.as_deref())
    .bind(drawing.provenance.as_ref().map(|p| p.created_by.as_str()))
    .bind(drawing.provenance.as_ref().and_then(|p| p.agent.as_deref()))
    .bind(drawing.provenance.as_ref().and_then(|p| p.confidence))
    .bind(
        drawing
            .provenance
            .as_ref()
            .and_then(|p| p.reason.as_deref()),
    )
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Move a drawing, reporting whether it was there to move.
///
/// `false` means the id does not exist **or belongs to somebody else**, and the
/// two are the same answer on purpose: a 403 would confirm that the id exists,
/// which is a fact a caller has no business learning from a guess. This is the
/// rule [`crate::strategies`] states for strategies, applied to a drawing.
///
/// `created_at` is deliberately not touched. A drawing's age is a fact about the
/// user's analysis, and a drag is not a reason to lose it.
///
/// The symbol is not updatable either. Moving a drawing to another instrument is
/// not an edit to it, it is a different drawing -- and a route that accepted one
/// would let a client rewrite an analysis of `BTCUSDT` into one of something
/// else while keeping its id.
///
/// Provenance is deliberately not updatable either. An edit does not change who
/// is accountable for the object: a drag on an AI-drawn level is still a level
/// the AI proposed and the user moved, and rewriting `created_by` on edit would
/// launder an AI object into a human one (or the reverse) by touching its
/// geometry.
///
/// The whole point list is replaced rather than patched. A drawing's points are
/// one object's geometry, and an update that could address a point by index would
/// let a client resize a trendline into a polyline without the kind changing --
/// which is the disagreement the engine's arity rule exists to prevent.
///
/// # Errors
/// Returns [`DbError::Pool`] if the update fails.
pub async fn update_drawing(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
    drawing: &NewDrawing,
) -> Result<bool, DbError> {
    let updated: Option<Uuid> = sqlx::query_scalar(
        "UPDATE drawings SET kind = $3, points = $4, options = $5, label = $6, updated_at = now() \
         WHERE id = $1 AND user_id = $2 RETURNING id",
    )
    .bind(id)
    .bind(user_id)
    .bind(drawing.kind.as_str())
    .bind(sqlx::types::Json(&drawing.points))
    .bind(&drawing.options)
    .bind(drawing.label.as_deref())
    .fetch_optional(pool)
    .await?;
    Ok(updated.is_some())
}

/// Delete a drawing, reporting whether there was one to delete.
///
/// `false` is not an error. Deleting something twice, or deleting something
/// that was never there, leaves the user in the state they asked for -- and a
/// 404 on the second attempt would make a UI's "remove" button report a failure
/// for having worked.
///
/// # Errors
/// Returns [`DbError::Pool`] if the delete fails.
pub async fn delete_drawing(pool: &PgPool, user_id: Uuid, id: Uuid) -> Result<bool, DbError> {
    let deleted: Option<Uuid> =
        sqlx::query_scalar("DELETE FROM drawings WHERE id = $1 AND user_id = $2 RETURNING id")
            .bind(id)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    Ok(deleted.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_kind_is_lowercase_ascii() {
        // `kind` crosses three boundaries -- a SQL column with no CHECK, a serde
        // enum tag, and a JS string comparison. A capital letter would break the
        // third silently: the drawing would be stored, listed, and never drawn.
        for kind in KINDS {
            assert!(
                kind.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{kind} is not a snake_case wire value"
            );
        }
    }

    #[test]
    fn stored_point_json_matches_what_the_migration_wrote() {
        // The keys a point is stored under, pinned against the SQL that built the
        // existing rows: `0018` writes jsonb_build_object('time', .., 'price', ..).
        let value = serde_json::to_value(StoredPoint {
            time: 1_767_225_600_000.0,
            price: 45_000.5,
        })
        .expect("a point serializes");
        assert_eq!(
            value,
            json!({"time": 1_767_225_600_000.0, "price": 45_000.5}),
            "a renamed field would make the backfilled drawings read as empty"
        );
    }

    #[test]
    fn a_point_list_is_the_array_the_column_checks() {
        // `drawings_points_is_a_non_empty_array` counts an array, so what reaches
        // the driver is one JSON object per point -- not a map keyed by index, and
        // not a flat list of numbers.
        let points = vec![
            StoredPoint {
                time: 1_767_225_600_000.0,
                price: 45_000.0,
            },
            StoredPoint {
                time: 1_767_229_200_000.0,
                price: 46_000.0,
            },
        ];
        let value = serde_json::to_value(&points).expect("a list serializes");
        assert_eq!(value.as_array().map(Vec::len), Some(2));
        assert_eq!(points_from_json(&value), points, "and reads back");
    }

    #[test]
    fn a_point_list_that_cannot_be_read_is_empty_rather_than_fatal() {
        // The non-fatal half of `points_from_json`'s argument. Each case below is
        // a payload a future version could plausibly have written, and none of
        // them may turn a chart into an error page.
        for unreadable in [
            json!("a trendline"),
            json!(null),
            json!({}),
            json!([{"t": 1.0, "p": 2.0}]),
            json!([{"time": 1.0, "price": "high"}]),
            json!([{"time": "1767225600000", "price": 2.0}]),
            json!([[]]),
            // The shape a row that predates the backfill carries: `DEFAULT '[]'`
            // is a legal array of length zero, and the CHECK refuses it on write,
            // not on read.
            json!([]),
        ] {
            assert!(
                points_from_json(&unreadable).is_empty(),
                "{unreadable} read as something"
            );
        }
        // One leniency worth knowing about, because it is the reader's and not a
        // choice this module made: serde also deserialises a struct from a JSON
        // array, so `[[1.0, 2.0]]` *is* a point. Nothing writes that shape -- the
        // migration builds objects, and `StoredPoint` serialises to one -- so the
        // only thing the leniency costs is a second spelling this reader would
        // accept. Asserting it here rather than pretending it is refused.
        assert_eq!(
            points_from_json(&json!([[1_767_225_600_000.0, 45_000.0]])),
            vec![StoredPoint {
                time: 1_767_225_600_000.0,
                price: 45_000.0,
            }],
            "a positional point parses, because serde does"
        );
        // And the all-or-nothing half: one bad point is not a shorter drawing.
        let broken = json!([
            {"time": 1_767_225_600_000.0, "price": 45_000.0},
            {"time": 1_767_229_200_000.0, "price": null},
        ]);
        assert!(points_from_json(&broken).is_empty(), "not one point");
    }

    #[test]
    fn milliseconds_survive_a_json_number_and_nanoseconds_do_not() {
        // The reason a point's time is milliseconds, measured against the format
        // it is really stored in. `float_roundtrip` is what makes this exact
        // rather than merely close: serde_json's default parser is best-effort and
        // can hand back a value one ULP away.
        for ms in [0.0, 1_672_515_782_136.0, 1_767_225_600_000.0, -86_400_000.0] {
            let point = StoredPoint {
                time: ms,
                price: 45_000.5,
            };
            let text = serde_json::to_string(&point).expect("a point prints");
            let back: StoredPoint = serde_json::from_str(&text).expect("a point parses");
            assert_eq!(back, point, "{ms} did not survive the column");
        }

        // The alternative would not have. A present-day nanosecond timestamp is
        // 1.7e18, past the 2^53 a double holds exactly, so at this magnitude the
        // spacing is 256 and a time that goes to JavaScript and comes back is a
        // different instant.
        let base = 1_767_225_600_000_000_000i64;
        assert_eq!(
            (base + 1) as f64,
            base as f64,
            "one nanosecond is not a distinguishable step at this magnitude"
        );
        assert_ne!(
            (base + 256) as f64,
            base as f64,
            "256 is the step that does move, so the collision above is about the \
             magnitude rather than about this number"
        );
    }

    #[test]
    fn empty_options_is_the_object_the_column_defaults_to() {
        // `{}` and `null` are not the same claim, and the column's own default is
        // the empty object. A writer that sends null is saying "no options", which
        // the engine happens to read the same way -- by accident, and an accident
        // is not a contract.
        assert_eq!(no_options(), json!({}));
        assert_ne!(no_options(), Value::Null);
    }
}
