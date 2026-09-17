//! Drawings: the shapes a user has drawn on a symbol (`docs/14`).
//!
//! ## Two anchors and a kind
//!
//! Everything a trader draws is a pair of `(time, price)` points with a rule for
//! what goes between them, so that is the whole schema: `kind`, an anchor, and
//! an optional second anchor. A trendline is the segment, a rectangle is the
//! area, a Fibonacci is the levels, and a horizontal line is the degenerate case
//! that needs one anchor.
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
//! boundary. An anchor does not use it, deliberately: an anchor is the one value
//! that makes the round trip *out* to a browser and *back* into the database,
//! and JSON numbers are doubles. A nanosecond timestamp is past 2^53, so one
//! read into JavaScript and written back is a different number -- a few hundred
//! nanoseconds, which is invisible on a chart and is still a write that changed
//! the data. Milliseconds fit exactly, so the wire is milliseconds and this
//! module is where they become `TIMESTAMPTZ`.
//!
//! `created_at` and `updated_at` are reported in nanoseconds like every other
//! timestamp in this crate, because nothing sends them back.

use chrono::{DateTime, Utc};
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
/// Adding one is a variant in `chart_engine::drawing::DrawingKind` plus an arm
/// in its `parts` -- no migration, because every kind is two anchors.
pub const KINDS: [&str; 4] = ["trendline", "hline", "rect", "fib"];

/// Whether a kind needs its second anchor.
///
/// The one rule with four cases. It is here rather than in SQL because the
/// database can express "the pair is whole" (`drawings_anchor_pair_is_whole`)
/// but not "this kind needs one and that kind needs two", and it is here rather
/// than in the route because the route's job is the status code, not the
/// vocabulary.
///
/// An unknown kind answers `true`: it is refused before this is reached, and
/// demanding more rather than less of something unrecognised is the safe way to
/// be wrong.
#[must_use]
pub fn needs_second_anchor(kind: &str) -> bool {
    kind != "hline"
}

/// A stored drawing, with its anchors in **milliseconds**.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawingRow {
    /// The row id.
    pub id: Uuid,
    /// The symbol it is drawn on, uppercase.
    pub symbol: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// First anchor, milliseconds since the epoch.
    pub a1_time_ms: f64,
    /// First anchor's price.
    pub a1_price: f64,
    /// Second anchor, for the kinds that have one.
    pub a2_time_ms: Option<f64>,
    /// Second anchor's price.
    pub a2_price: Option<f64>,
    /// What the user called it, if anything.
    pub label: Option<String>,
    /// When it was first stored, unix nanoseconds.
    pub created_at: i64,
    /// When it was last moved, unix nanoseconds.
    pub updated_at: i64,
}

/// A drawing about to be written.
///
/// Separate from [`DrawingRow`] because a row has an id and timestamps the
/// caller does not supply, and because a struct with two optional fields that
/// must agree is a thing to construct deliberately rather than to spread.
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
    /// First anchor, milliseconds since the epoch.
    pub a1_time_ms: f64,
    /// First anchor's price.
    pub a1_price: f64,
    /// Second anchor, milliseconds since the epoch.
    pub a2_time_ms: Option<f64>,
    /// Second anchor's price.
    pub a2_price: Option<f64>,
    /// What the user called it, if anything.
    pub label: Option<String>,
}

/// Convert milliseconds to a UTC timestamp.
///
/// Total, because a `NaN` or an out-of-range millisecond value reaching the
/// driver is a 500 for a request that should have been refused earlier -- and a
/// drawing at the epoch is visibly wrong, which is better than a drawing that
/// silently is not there.
fn ms_to_dt(ms: f64) -> DateTime<Utc> {
    let rounded = if ms.is_finite() { ms.round() } else { 0.0 };
    DateTime::from_timestamp_millis(rounded as i64).unwrap_or(DateTime::UNIX_EPOCH)
}

/// Convert a UTC timestamp to milliseconds.
///
/// `timestamp_millis` rather than `dt_to_ns(..) / 1e6`: the second one converts
/// to a nanosecond `i64` first, which for a present-day timestamp is 1.7e18 --
/// past the range a `f64` represents exactly, so the division would be done on a
/// number that had already lost the low bits. This one is exact.
fn dt_to_ms(dt: DateTime<Utc>) -> f64 {
    dt.timestamp_millis() as f64
}

fn row_to_drawing(row: &sqlx::postgres::PgRow) -> Result<DrawingRow, DbError> {
    let a2_time: Option<DateTime<Utc>> = row.try_get("a2_time")?;
    let a2_price: Option<f64> = row.try_get("a2_price")?;
    Ok(DrawingRow {
        id: row.try_get("id")?,
        symbol: row.try_get("symbol")?,
        kind: row.try_get("kind")?,
        a1_time_ms: dt_to_ms(row.try_get("a1_time")?),
        a1_price: row.try_get("a1_price")?,
        // Both columns move together -- `drawings_anchor_pair_is_whole` says so
        // -- so a half-populated pair is a database that has been edited by
        // hand, and reporting `None` is the honest reading of it.
        a2_time_ms: a2_time.map(dt_to_ms),
        a2_price,
        label: row.try_get("label")?,
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
        "SELECT id, symbol, kind, a1_time, a1_price, a2_time, a2_price, label, \
         created_at, updated_at \
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
/// Returns [`DbError::Pool`] if the insert fails, or if the anchor pair is half
/// populated -- which the `CHECK` refuses, and which the route refuses first.
pub async fn create_drawing(
    pool: &PgPool,
    user_id: Uuid,
    symbol: &str,
    drawing: &NewDrawing,
) -> Result<Uuid, DbError> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO drawings (user_id, symbol, kind, a1_time, a1_price, a2_time, a2_price, label) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(user_id)
    .bind(symbol)
    .bind(drawing.kind.as_str())
    .bind(ms_to_dt(drawing.a1_time_ms))
    .bind(drawing.a1_price)
    .bind(drawing.a2_time_ms.map(ms_to_dt))
    .bind(drawing.a2_price)
    .bind(drawing.label.as_deref())
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
/// # Errors
/// Returns [`DbError::Pool`] if the update fails.
pub async fn update_drawing(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
    drawing: &NewDrawing,
) -> Result<bool, DbError> {
    let updated: Option<Uuid> = sqlx::query_scalar(
        "UPDATE drawings SET kind = $3, a1_time = $4, a1_price = $5, a2_time = $6, \
         a2_price = $7, label = $8, updated_at = now() \
         WHERE id = $1 AND user_id = $2 RETURNING id",
    )
    .bind(id)
    .bind(user_id)
    .bind(drawing.kind.as_str())
    .bind(ms_to_dt(drawing.a1_time_ms))
    .bind(drawing.a1_price)
    .bind(drawing.a2_time_ms.map(ms_to_dt))
    .bind(drawing.a2_price)
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

    #[test]
    fn only_a_horizontal_line_has_one_anchor() {
        assert!(!needs_second_anchor("hline"));
        for kind in ["trendline", "rect", "fib"] {
            assert!(needs_second_anchor(kind), "{kind} needs two anchors");
        }
    }

    #[test]
    fn an_unknown_kind_demands_more_rather_than_less() {
        // The safe way to be wrong about something unrecognised. A `false` here
        // would mean a kind nobody has heard of is allowed through with one
        // anchor, which is the shape that reaches the engine and cannot be drawn.
        assert!(needs_second_anchor("channel"));
        assert!(needs_second_anchor(""));
    }

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
    fn milliseconds_survive_the_round_trip_exactly() {
        // The reason the wire is milliseconds at all. A present-day nanosecond
        // timestamp is 1.7e18, past the 2^53 a JSON number can hold, so this
        // round trip is the one that has to be exact.
        for ms in [0.0, 1_672_515_782_136.0, 1_767_225_600_000.0, -86_400_000.0] {
            let dt = ms_to_dt(ms);
            assert_eq!(dt_to_ms(dt), ms, "{ms} did not survive");
        }
    }

    #[test]
    fn a_nonsense_millisecond_value_lands_at_the_epoch_rather_than_panicking() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300] {
            let dt = ms_to_dt(bad);
            // What matters is that it produced *a* timestamp and not a panic in
            // the driver. `1e300` saturates rather than wrapping to a negative.
            assert!(dt.timestamp_millis() >= 0, "{bad} produced {dt}");
        }
    }

    #[test]
    fn a_nanosecond_timestamp_would_not_have_survived_the_same_trip() {
        // The test above says milliseconds work; this one says the alternative
        // would not have, so the exception in this module's header is a measured
        // claim rather than a belief.
        //
        // The first version of this used `1_767_225_600_000_000_000` and
        // *failed*, because that value happens to be a multiple of 256 and is
        // therefore exactly representable. Which is the point worth keeping: at
        // this magnitude a `f64`'s spacing is 256, so a timestamp survives only by
        // luck -- and the ones that matter, anything carrying real
        // sub-millisecond precision, do not.
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

        let nanos = base + 1;
        assert_ne!(
            nanos as f64 as i64, nanos,
            "a nanosecond timestamp is not representable past 2^53, so this one \
             would come back from a browser as a different instant"
        );
    }
}
