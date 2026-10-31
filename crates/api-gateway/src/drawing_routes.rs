//! `/drawings` — the shapes a user has drawn on a symbol (`docs/14`).
//!
//! ## Why this is a resource rather than a field on the chart
//!
//! A drawing outlives the page. It is the user's own analysis of a symbol, and
//! the point of storing it is that it is there tomorrow, on another machine, and
//! after a reload — so it is a row with an owner, not a value the shell keeps in
//! memory. The shell loads them on a symbol change and saves them when a drag
//! ends.
//!
//! ## Per user and per symbol, and deliberately not per timeframe
//!
//! A trendline is two `(time, price)` points, and those two points mean the same
//! thing on a 1m chart as on a 1h one. Scoping to the timeframe would hide a
//! level from the chart a trader switched to in order to check it.
//!
//! ## The body is the engine's own vocabulary
//!
//! [`DrawingBody`] reuses [`DrawingKind`], [`Anchor`] and [`DrawingOptions`]
//! rather than declaring its own, because those types *are* the wire: the shell
//! builds them from a pointer position, the engine resolves them, and this route
//! stores them. A second declaration would be a second thing to keep in step, and
//! the disagreement would be silent — nothing checks the shape of a drawing
//! against anything.
//!
//! ## The points are an array, and the kind says how many -- in the engine's words
//!
//! A drawing is `kind` plus a list of `(time, price)` points, so the body carries
//! a list and this route never counts it itself. "Which kinds take one point and
//! which take six" is [`chart_engine::Drawing::validate_anchors`]'s rule, read off
//! `DrawingKind::anchors`, and storage keeps no copy of it. The route is the only
//! place the two meet, so it is where a disagreement would have to hide -- and it
//! cannot, because there is only one statement of the rule.
//!
//! ## A fraction is refused, and that is the interesting validation
//!
//! An [`Anchor::Fraction`] is a position on a plot, which is what the shell sends
//! while a drawing is being placed. It is not storable: a fraction's meaning
//! depends on the window it was measured in, so a stored one would put the
//! drawing somewhere different on every load. The engine always reports absolute
//! anchors, so a client sending a fraction is a client that has not been through
//! the engine — and refusing it here is what stops a window-dependent value
//! reaching a table.
//!
//! ## Deleting twice is not an error
//!
//! `DELETE` answers 200 whether or not there was a drawing to delete. The user
//! asked for it to be gone and it is gone; a 404 on the second attempt would make
//! a "remove" button report a failure for having worked.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use chart_engine::{Anchor, Drawing, DrawingKind, DrawingOptions};
use serde_json::Value;

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiQuery};
use crate::AppState;

/// `GET /drawings?symbol=...`
#[derive(Debug, Deserialize)]
pub struct DrawingsQuery {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
}

/// `POST /drawings` and `PUT /drawings/{id}`.
#[derive(Debug, Deserialize)]
pub struct DrawingBody {
    /// Which shape.
    pub kind: DrawingKind,
    /// Its points, in placement order.
    pub points: Vec<Anchor>,
    /// What the user called it.
    #[serde(default)]
    pub label: Option<String>,
    /// How to draw it beyond where its points are -- absent means the engine's
    /// default for this kind, which is what a drawing the user never configured
    /// sends.
    #[serde(default)]
    pub options: DrawingOptions,
}

/// `POST /drawings` — the body plus the instrument.
///
/// `flatten` rather than a `symbol` field on [`DrawingBody`], because an update
/// must *not* accept one: moving a drawing to another instrument is not an edit
/// to it, it is a different drawing, and a route that accepted the field would
/// let a client rewrite an analysis of one symbol into an analysis of another
/// while keeping its id.
#[derive(Debug, Deserialize)]
pub struct CreateBody {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// The rest of the drawing.
    #[serde(flatten)]
    pub drawing: DrawingBody,
}

/// One drawing, as a client reads it.
#[derive(Debug, Serialize)]
pub struct DrawingResponse {
    /// The row id.
    pub id: String,
    /// Which shape.
    pub kind: DrawingKind,
    /// The points, always absolute, in placement order.
    pub points: Vec<Anchor>,
    /// What the user called it.
    pub label: Option<String>,
    /// How it is drawn; see [`DrawingBody::options`]. Always present, because a
    /// client that has to guess whether an absent field meant "default" or
    /// "older server" would guess wrong at least once.
    pub options: DrawingOptions,
    /// Who drew it: `"ai"` for agent-created objects, absent for a human's.
    /// The shell's layer filter reads this — it is a *fact about the row*,
    /// reported verbatim, and the shell derives its own layer from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// The drawing agent's stated confidence, when it stated one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Why the agent drew it, in its own words. This is what "why is this
    /// here?" reads; the shell surfaces it as a badge, no re-ask needed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `GET /drawings`
#[derive(Debug, Serialize)]
pub struct DrawingsResponse {
    /// The symbol these belong to.
    ///
    /// Echoed rather than implied. The shell asks for a new symbol's drawings
    /// every time the symbol changes, and a response that arrived late is
    /// otherwise indistinguishable from one that arrived for the symbol now on
    /// screen — so it would be drawn on the wrong chart, at prices that look
    /// plausible.
    pub symbol: String,
    /// The drawings, oldest first.
    pub drawings: Vec<DrawingResponse>,
}

/// `DELETE /drawings/{id}`
#[derive(Debug, Serialize)]
pub struct DeletedResponse {
    /// The id that was asked for.
    pub id: String,
    /// Whether a row was actually removed.
    ///
    /// Reported rather than assumed, because "already gone" and "deleted" are
    /// the same answer to the user and different facts to a caller reconciling
    /// two tabs.
    pub deleted: bool,
}

/// `GET /drawings`
///
/// # Errors
/// 503 without a database.
pub async fn list(
    State(state): State<AppState>,
    user: UserContext,
    ApiQuery(query): ApiQuery<DrawingsQuery>,
) -> Result<Json<DrawingsResponse>, ApiError> {
    let database = database(&state)?;
    let symbol = query.symbol.to_uppercase();
    let rows = db::drawings::list_drawings(database.pool(), user.user_id, &symbol).await?;

    let mut drawings = Vec::with_capacity(rows.len());
    for row in &rows {
        match describe(row) {
            Some(drawing) => drawings.push(drawing),
            // A kind this build cannot draw. Skipped rather than fatal: one
            // unreadable drawing must not take the other nine off the chart, and
            // the warning names the row so the cause is findable. It is a row
            // written by a newer engine and then rolled back, or edited by hand.
            None => tracing::warn!(
                id = %row.id,
                kind = %row.kind,
                "a stored drawing has a kind this build cannot draw, so it is not listed"
            ),
        }
    }

    Ok(Json(DrawingsResponse { symbol, drawings }))
}

/// `POST /drawings`
///
/// # Errors
/// 422 for a drawing that cannot be stored; 503 without a database.
pub async fn create(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<CreateBody>,
) -> Result<(StatusCode, Json<DrawingResponse>), ApiError> {
    let database = database(&state)?;
    let symbol = body.symbol.to_uppercase();
    let new = prepare(&body.drawing)?;
    let id = db::drawings::create_drawing(database.pool(), user.user_id, &symbol, &new).await?;

    tracing::debug!(%id, %symbol, kind = %new.kind, "a drawing was stored");
    Ok((StatusCode::CREATED, Json(response(id, &body.drawing))))
}

/// `PUT /drawings/{id}`
///
/// # Errors
/// 404 when there is no such drawing for this user; 422 for a drawing that
/// cannot be stored; 503 without a database.
pub async fn update(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<DrawingBody>,
) -> Result<Json<DrawingResponse>, ApiError> {
    let database = database(&state)?;
    let id = row_id(&id)?;
    let new = prepare(&body)?;

    // `false` is "not there, or not yours", and the two are one answer on
    // purpose: a 403 would confirm that the id exists, which is a fact a caller
    // has no business learning from a guess. The same rule `strategies` states.
    if !db::drawings::update_drawing(database.pool(), user.user_id, id, &new).await? {
        return Err(unknown());
    }
    Ok(Json(response(id, &body)))
}

/// `DELETE /drawings/{id}`
///
/// Idempotent: deleting a drawing that is already gone answers 200 with
/// `deleted: false`, because the user's intent is satisfied either way.
///
/// # Errors
/// 503 without a database.
pub async fn remove(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<DeletedResponse>, ApiError> {
    let database = database(&state)?;
    let id = row_id(&id)?;
    let deleted = db::drawings::delete_drawing(database.pool(), user.user_id, id).await?;
    Ok(Json(DeletedResponse {
        id: id.to_string(),
        deleted,
    }))
}

/// Build the response for a body that has just been written.
///
/// From the body rather than by re-reading the row: `prepare` does not change the
/// points, so a second query would be a round trip to learn what the caller just
/// told us — and, on the managed instance, a second of latency. Fractions cannot
/// reach here, because `prepare` refuses them first.
fn response(id: Uuid, body: &DrawingBody) -> DrawingResponse {
    DrawingResponse {
        id: id.to_string(),
        kind: body.kind,
        points: body.points.clone(),
        label: body.label.clone(),
        options: body.options,
        // A write that arrived over the HTTP route is the user's own work, and
        // the provenance fields are facts about the *stored row*, which this
        // response has not read. Absent until the next list; the shell needs
        // them for the layer filter, not for the shape it just drew.
        created_by: None,
        confidence: None,
        reason: None,
    }
}

/// Check a body, or say why it cannot be stored.
///
/// The point rule is the **engine's own** [`Drawing::validate_anchors`], run on a
/// drawing that is not going to be drawn. A second copy of "which kinds take how
/// many points" here -- or in storage, which used to have one -- is exactly how
/// the route and the engine come to disagree, and the way they disagree is a
/// drawing the API accepts and the chart never shows.
///
/// # Errors
/// 422, naming the problem.
fn prepare(body: &DrawingBody) -> Result<db::NewDrawing, ApiError> {
    let check = Drawing {
        // Empty, and it does not matter: `validate_anchors` is points only, on
        // purpose, because a route validating a request body has no id to give
        // and should not have to invent one.
        id: String::new(),
        kind: body.kind,
        points: body.points.clone(),
        label: body.label.clone(),
        selected: false,
        options: body.options,
    };
    check.validate_anchors().map_err(invalid)?;

    // Every point, in the order it arrived. Sorting them would be the tempting
    // tidiness and is wrong: `points[0]` is where the drawing *started*, which is
    // what a ray's origin, a projection's anchor and a pattern's X all read.
    let mut points = Vec::with_capacity(body.points.len());
    for anchor in &body.points {
        let (time, price) = absolute(*anchor)?;
        points.push(db::StoredPoint { time, price });
    }

    Ok(db::NewDrawing {
        kind: body.kind.name().to_string(),
        points,
        options: options_to_json(body.options),
        label: body.label.clone(),
        // A drawing that arrives over the HTTP route is the user's own work,
        // however it was produced: provenance is the *agent's* door's stamp,
        // and this is not that door.
        provenance: None,
    })
}

/// The millisecond time and price of a point, or a 422.
///
/// The interesting refusal. A `fraction` is a position on a plot, which is what
/// the shell sends while a drawing is being placed — it is not storable, because
/// its meaning depends on the window it was measured in, so a stored one would
/// put the drawing somewhere different on every load.
///
/// # Errors
/// 422 for a fraction.
fn absolute(anchor: Anchor) -> Result<(f64, f64), ApiError> {
    anchor.absolute().ok_or_else(|| {
        invalid(
            "a drawing is stored with absolute anchors. A `fraction` is a position on a plot, \
             so its meaning depends on the window it was measured in -- send the drawing to the \
             chart engine and store the anchors it reports back."
                .into(),
        )
    })
}

/// The options a row carries, in the column's own JSON.
///
/// Every field is always written, including `"extend": null` for "no opinion".
/// That is deliberate: the engine reads an *absent* `extend` as "use this kind's
/// reference default" -- a ray extends forward, a trendline does not -- and an
/// explicit `"none"` as "stop at the points". Collapsing the two to save four
/// bytes would turn a user's explicit choice into the other answer. A
/// `"level_profile": null` is the same arrangement on the ladder: the kind's own
/// reference set, which is why the retracement, the extensions and the time tools
/// all draw different grids from one stored row.
fn options_to_json(options: DrawingOptions) -> Value {
    // Infallible in practice: three fields, two enums and one bool, and no float
    // that could be a `NaN`. A fallback here would be a silent rewrite of a
    // setting the user chose, so the impossible case says so instead.
    serde_json::to_value(options).expect("drawing options are JSON-safe")
}

/// The options a row's JSON means, or the defaults if this build cannot read it.
///
/// Tolerant for the reason [`db::points_from_json`] is: this runs over every
/// drawing the user owns, and one row written by a newer engine that added an
/// option must not cost the whole list.
fn options_from_json(value: &Value) -> DrawingOptions {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

/// Map a stored row to the response, if this build still knows the kind.
///
/// `None` for a kind it does not. The alternative — failing the whole request —
/// would take a user's other drawings off the chart because of one row, which is
/// a worse outcome than a drawing they cannot see and a warning in the log.
fn describe(row: &db::DrawingRow) -> Option<DrawingResponse> {
    let kind = kind_from_name(&row.kind)?;
    Some(DrawingResponse {
        id: row.id.to_string(),
        kind,
        points: row
            .points
            .iter()
            .map(|point| Anchor::Absolute {
                time: point.time,
                price: point.price,
            })
            .collect(),
        label: row.label.clone(),
        options: options_from_json(&row.options),
        created_by: row.created_by.clone(),
        confidence: row.confidence,
        reason: row.reason.clone(),
    })
}

/// The [`DrawingKind`] a stored name refers to.
///
/// By name rather than by `serde_json::from_value`, so the mapping is a list this
/// module can be read against `db::DRAWING_KINDS` — and so
/// `every_kind_the_engine_draws_is_a_kind_the_database_stores` can check that the
/// two agree rather than checking that serde agrees with itself.
fn kind_from_name(name: &str) -> Option<DrawingKind> {
    DrawingKind::ALL
        .into_iter()
        .find(|kind| kind.name() == name)
}

/// Parse the path's id, answering 404 for anything that is not one.
///
/// A malformed id is not a 400: the caller asked for a drawing by a name that
/// cannot exist, which is the same answer as a drawing that does not exist, and
/// splitting them would tell a caller something about the id format.
fn row_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| unknown())
}

/// 422 for a drawing that cannot be stored.
fn invalid(reason: String) -> ApiError {
    ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "DRAWING_INVALID", reason)
}

/// 404 for a drawing this user does not have.
fn unknown() -> ApiError {
    ApiError::coded(StatusCode::NOT_FOUND, "DRAWING_UNKNOWN", "no such drawing")
}

fn database(state: &AppState) -> Result<&std::sync::Arc<db::Database>, ApiError> {
    state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured; drawings cannot be stored"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chart_engine::{Extend, LevelProfile};
    use db::{DrawingRow, StoredPoint};
    use serde_json::json;

    const T1: f64 = 1_767_225_600_000.0;
    const T2: f64 = 1_767_229_200_000.0;
    const T3: f64 = 1_767_232_800_000.0;

    fn point(time: f64, price: f64) -> Anchor {
        Anchor::Absolute { time, price }
    }

    fn fraction() -> Anchor {
        Anchor::Fraction { x: 0.4, y: 0.6 }
    }

    fn body(kind: DrawingKind, points: Vec<Anchor>) -> DrawingBody {
        DrawingBody {
            kind,
            points,
            label: None,
            options: DrawingOptions::default(),
        }
    }

    fn with_options(kind: DrawingKind, points: Vec<Anchor>, options: DrawingOptions) -> DrawingBody {
        DrawingBody {
            kind,
            points,
            label: None,
            options,
        }
    }

    fn stored_with_options(
        kind: &str,
        points: Vec<StoredPoint>,
        options: serde_json::Value,
    ) -> DrawingRow {
        DrawingRow {
            options,
            ..stored(kind, points)
        }
    }

    fn stored(kind: &str, points: Vec<StoredPoint>) -> DrawingRow {
        DrawingRow {
            id: Uuid::nil(),
            symbol: "BTCUSDT".into(),
            kind: kind.into(),
            points,
            options: db::no_options(),
            label: Some("the range I keep watching".into()),
            // A hand-drawn fixture: no provenance, which is what `None` means.
            created_by: None,
            agent: None,
            confidence: None,
            reason: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    /// The two lists of kinds, kept in step.
    ///
    /// Two crates describe the same vocabulary and nothing but this test connects
    /// them: the engine's enum and the storage's `KINDS` array. A kind added to
    /// one and not the other is a drawing the API accepts and the chart never
    /// shows, or one the engine can draw that nothing can save — and neither is a
    /// compile error anywhere.
    ///
    /// What this test no longer compares is the *arity*. Storage used to carry a
    /// second copy of "which kinds take two points" as two `needs_*` functions, and
    /// the only thing that copy could ever prove was that the two lists had been
    /// edited together. `DrawingKind::anchors` is now the single statement, and the
    /// route validates against it, so there is nothing here to agree with.
    #[test]
    fn every_kind_the_engine_draws_is_a_kind_the_database_stores() {
        for kind in DrawingKind::ALL {
            assert!(
                db::DRAWING_KINDS.contains(&kind.name()),
                "`{}` is not in db::DRAWING_KINDS: {:?}",
                kind.name(),
                db::DRAWING_KINDS
            );
        }
        assert_eq!(
            DrawingKind::ALL.len(),
            db::DRAWING_KINDS.len(),
            "one side has a kind the other does not"
        );
    }

    #[test]
    fn a_body_with_absolute_points_becomes_a_row_in_milliseconds() {
        let prepared = prepare(&body(
            DrawingKind::Trendline,
            vec![point(T1, 45_000.0), point(T2, 45_500.0)],
        ))
        .expect("must accept");

        assert_eq!(prepared.kind, "trendline");
        assert_eq!(
            prepared.points,
            vec![
                StoredPoint {
                    time: T1,
                    price: 45_000.0
                },
                StoredPoint {
                    time: T2,
                    price: 45_500.0
                },
            ]
        );
    }

    #[test]
    fn a_point_list_keeps_the_order_it_was_sent_in() {
        // The tempting tidiness -- sort the times -- would be a bug. `points[0]` is
        // where the drawing *started*, and a ray, a Fibonacci projection and a
        // pattern's X all read the first point as the origin. Sorted, a hand-drawn
        // ray pointing left becomes one pointing right.
        let prepared = prepare(&body(
            DrawingKind::Channel,
            vec![point(T3, 46_000.0), point(T2, 45_500.0), point(T1, 45_000.0)],
        ))
        .expect("must accept");

        assert_eq!(
            prepared.points,
            vec![
                StoredPoint {
                    time: T3,
                    price: 46_000.0
                },
                StoredPoint {
                    time: T2,
                    price: 45_500.0
                },
                StoredPoint {
                    time: T1,
                    price: 45_000.0
                },
            ]
        );
    }

    #[test]
    fn a_three_point_kind_keeps_every_point_it_was_given() {
        // The parity round trip. A route that dropped the third point would store a
        // channel, arc or triangle that restores as a two-point shape -- and the
        // point list is exactly where that kind of loss used to happen, because a
        // field nobody remembered to copy is invisible in a struct literal.
        let prepared = prepare(&body(
            DrawingKind::Channel,
            vec![point(T1, 45_000.0), point(T2, 45_500.0), point(T3, 46_000.0)],
        ))
        .expect("must accept");
        assert_eq!(prepared.points.len(), 3);
        assert_eq!(prepared.points[2], StoredPoint { time: T3, price: 46_000.0 });
    }

    #[test]
    fn a_fraction_point_is_refused_because_it_cannot_be_stored() {
        // The interesting validation. A fraction means "this far across the plot I
        // happened to be looking at", so storing one would put the drawing
        // somewhere different on every load -- and the shell has an absolute
        // answer available, because the engine reported one.
        //
        // Every point is a fraction here: a list that mixed the two units is the
        // engine's own refusal, not this one's.
        let error = prepare(&body(
            DrawingKind::Trendline,
            vec![fraction(), fraction()],
        ))
        .expect_err("must refuse");

        assert_eq!(error.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.code(), "DRAWING_INVALID");
        // The reason, not only the code: the whole value of a 422 is that the
        // caller can fix it, and a client that only learns "invalid" cannot.
        assert!(error.message().contains("fraction"), "{}", error.message());
        assert!(error.message().contains("engine"), "{}", error.message());
    }

    #[test]
    fn a_mixed_unit_list_is_refused_by_the_engine_before_the_route_looks() {
        // The other half of the same rule, and the order matters: the engine
        // rejects a list that is half screen position and half chart point, and
        // its words -- "send every point in one unit" -- are what the caller
        // reads, not the storage door's.
        let error = prepare(&body(
            DrawingKind::Trendline,
            vec![fraction(), point(1.0, 2.0)],
        ))
        .expect_err("must refuse");

        assert_eq!(error.code(), "DRAWING_INVALID");
        assert!(
            error.message().contains("one unit"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_fraction_in_a_later_point_is_refused_too() {
        // Every point, not the first two. The version of this check that ran on
        // `a1` and `a2` was the one that reached the database, and a mixed list is
        // the shape that cannot be drawn at all.
        let error = prepare(&body(
            DrawingKind::Channel,
            vec![point(1.0, 2.0), point(3.0, 4.0), fraction()],
        ))
        .expect_err("must refuse");
        assert_eq!(error.code(), "DRAWING_INVALID");
    }

    #[test]
    fn a_two_point_kind_with_one_point_is_refused_in_the_engines_words() {
        // The reason is the engine's own, because the rule is: a second copy of it
        // here would be a second thing to keep in step, and the way the two drift
        // apart is a drawing the API accepts and the chart never shows.
        let error = prepare(&body(DrawingKind::Fib, vec![point(1.0, 2.0)]))
            .expect_err("must refuse");
        assert_eq!(error.code(), "DRAWING_INVALID");
        assert!(error.message().contains("fib"), "{}", error.message());
        assert!(
            error.message().contains("takes 2 points"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_three_point_kind_without_its_third_point_is_refused() {
        let error = prepare(&body(
            DrawingKind::Triangle,
            vec![point(1.0, 2.0), point(3.0, 4.0)],
        ))
        .expect_err("must refuse");
        assert_eq!(error.code(), "DRAWING_INVALID");
        assert!(
            error.message().contains("takes 3 points"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_one_point_kind_with_two_points_is_refused() {
        // Exact arity, which is the half the old model could not express: a
        // horizontal line used to *accept* a second anchor because the column had a
        // slot for it and nothing read it. Now one point is the whole rule, and a
        // level sent with a stray second point is refused with the engine's reason
        // rather than stored as a shape the chart will not draw.
        let error = prepare(&body(
            DrawingKind::Hline,
            vec![point(1.0, 2.0), point(3.0, 4.0)],
        ))
        .expect_err("must refuse");
        assert_eq!(error.code(), "DRAWING_INVALID");
        assert!(error.message().contains("hline"), "{}", error.message());
        assert!(
            error.message().contains("takes 1 point"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn an_empty_point_list_is_refused_rather_than_stored() {
        // Storage's `CHECK` would reject it too, but the route is where a message
        // can still be read. An empty list is the shape a shell bug produces -- a
        // click that ended placement before it placed anything -- and a 422 that
        // names the kind is the difference between a fixed bug and a drawing that
        // silently never appeared.
        let error = prepare(&body(DrawingKind::Trendline, vec![])).expect_err("must refuse");
        assert_eq!(error.code(), "DRAWING_INVALID");
        assert!(error.message().contains("this has 0"), "{}", error.message());
    }

    #[test]
    fn a_horizontal_line_is_accepted_with_one_point() {
        let prepared = prepare(&body(
            DrawingKind::Hline,
            vec![point(T1, 45_000.0)],
        ))
        .expect("must accept");
        assert_eq!(prepared.kind, "hline");
        assert_eq!(prepared.points.len(), 1);
    }

    #[test]
    fn a_hostile_point_is_refused_rather_than_stored() {
        // A `NaN` price produces a `NaN` y and a canvas silently ignores it, so the
        // drawing would be absent with nothing anywhere saying why. Refusing it at
        // the door is the only place the reason can be given.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = prepare(&body(DrawingKind::Hline, vec![point(bad, 1.0)]))
                .expect_err("must refuse");
            assert_eq!(error.code(), "DRAWING_INVALID", "{bad}");
        }
    }

    #[test]
    fn options_are_written_as_json_and_read_back() {
        // The user's setting, not only their geometry: a route that dropped
        // `options` would save a drawing that reverts to the default the moment the
        // page reloads.
        let prepared = prepare(&with_options(
            DrawingKind::Ray,
            vec![point(T1, 45_000.0), point(T2, 45_500.0)],
            DrawingOptions {
                extend: Some(Extend::Both),
                reverse: true,
                level_profile: Some(LevelProfile::Ote),
                closed: false,
            },
        ))
        .expect("must accept");
        assert_eq!(prepared.options["extend"], json!("both"));
        assert_eq!(prepared.options["reverse"], json!(true));
        assert_eq!(prepared.options["level_profile"], json!("ote"));

        let described = describe(&stored_with_options(
            "ray",
            vec![
                StoredPoint {
                    time: T1,
                    price: 45_000.0,
                },
                StoredPoint {
                    time: T2,
                    price: 45_500.0,
                },
            ],
            prepared.options,
        ))
        .expect("a known kind");
        assert_eq!(
            described.options,
            DrawingOptions {
                extend: Some(Extend::Both),
                reverse: true,
                level_profile: Some(LevelProfile::Ote),
                closed: false,
            }
        );
    }

    #[test]
    fn an_explicit_none_is_not_the_same_answer_as_no_option() {
        // The two are different claims and the wire has to keep them apart. An
        // absent `extend` means "draw this kind the way the reference does", so a
        // ray runs forward; `"extend": "none"` means "stop at the points", which is
        // a thing the user chose. Writing defaults out as `{}` would collapse the
        // second into the first.
        let unopinionated = prepare(&body(
            DrawingKind::Ray,
            vec![point(T1, 45_000.0), point(T2, 45_500.0)],
        ))
        .expect("must accept");
        let stopped = prepare(&with_options(
            DrawingKind::Ray,
            vec![point(T1, 45_000.0), point(T2, 45_500.0)],
            DrawingOptions {
                extend: Some(Extend::None),
                ..DrawingOptions::default()
            },
        ))
        .expect("must accept");

        assert!(unopinionated.options["extend"].is_null());
        assert_eq!(stopped.options["extend"], json!("none"));
        assert_ne!(unopinionated.options, stopped.options);
    }

    #[test]
    fn a_body_with_no_options_key_is_accepted() {
        // The shell sends options only when the user changed them, so an absent
        // field is the ordinary request rather than a malformed one.
        let parsed: DrawingBody = serde_json::from_str(
            r#"{"kind":"trendline","points":[
                 {"unit":"absolute","time":1767225600000,"price":45000},
                 {"unit":"absolute","time":1767229200000,"price":45500}
               ]}"#,
        )
        .expect("must deserialize");
        assert_eq!(parsed.options, DrawingOptions::default());
        assert_eq!(parsed.label, None);
        assert!(prepare(&parsed).is_ok());
    }

    #[test]
    fn options_this_build_cannot_read_are_the_defaults_rather_than_a_500() {
        // The forward-compat half, and the reason a later task can add a field
        // without a migration: a row written by an engine that knows a value-area
        // percentage reads, on this build, as a drawing with no opinion about it --
        // which is what the same drawing looked like before the option existed.
        assert_eq!(
            options_from_json(&json!({"value_area_pct": 70, "trend_anchor": "latest swing"})),
            DrawingOptions::default()
        );
        // Including a column that is not an object at all.
        assert_eq!(options_from_json(&Value::Null), DrawingOptions::default());
        // And the mirror of both: an option this build *does* know is read, so the
        // tolerance above is not the whole struct being quietly dropped.
        assert_eq!(
            options_from_json(&json!({"level_profile": "ote"})),
            DrawingOptions {
                level_profile: Some(LevelProfile::Ote),
                ..DrawingOptions::default()
            }
        );
    }

    #[test]
    fn a_row_becomes_the_response_the_shell_reads() {
        let described = describe(&stored(
            "fib",
            vec![
                StoredPoint {
                    time: T1,
                    price: 45_000.0,
                },
                StoredPoint {
                    time: T2,
                    price: 45_500.0,
                },
            ],
        ))
        .expect("a known kind");

        assert_eq!(described.kind, DrawingKind::Fib);
        assert_eq!(
            described.points,
            vec![point(T1, 45_000.0), point(T2, 45_500.0)],
            "every point comes back, in order, as an absolute anchor"
        );
        assert_eq!(described.label.as_deref(), Some("the range I keep watching"));
    }

    #[test]
    fn a_stored_kind_this_build_cannot_draw_is_skipped_rather_than_fatal() {
        // A row written by a newer engine and then rolled back, or edited by hand.
        // One unreadable drawing must not take the other nine off the chart, and the
        // route warns with the row's id so the cause is findable.
        let row = stored(
            "not_a_kind",
            vec![StoredPoint {
                time: 0.0,
                price: 1.0,
            }],
        );
        assert!(describe(&row).is_none());
        // And a kind the engine *does* know is not skipped, so the branch above is
        // about the unknown kind rather than about the fixture.
        let known = DrawingRow {
            kind: "hline".into(),
            ..row
        };
        assert!(describe(&known).is_some());
    }

    #[test]
    fn an_id_that_is_not_a_uuid_is_absent_rather_than_malformed() {
        let error = row_id("not-an-id").expect_err("must refuse");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.code(), "DRAWING_UNKNOWN");
        assert!(row_id(&Uuid::nil().to_string()).is_ok());
    }

    #[test]
    fn the_response_pins_the_keys_the_shell_reads() {
        // A rename here is not a compile error anywhere. It is a drawing that stops
        // appearing, or one whose points the shell cannot post back -- and the
        // second failure is a drag that silently does not save.
        let wire = serde_json::to_value(DrawingResponse {
            id: "8f3a".into(),
            kind: DrawingKind::Channel,
            points: vec![point(T1, 45_000.0), point(T2, 45_500.0), point(T3, 46_000.0)],
            label: Some("watch this".into()),
            options: DrawingOptions {
                extend: Some(Extend::Left),
                reverse: false,
                level_profile: Some(LevelProfile::Extension),
                closed: false,
            },
            created_by: Some("ai".into()),
            confidence: Some(0.82),
            reason: Some("three rejections".into()),
        })
        .expect("serializes");

        assert_eq!(wire["id"], "8f3a");
        assert_eq!(wire["kind"], "channel");
        assert_eq!(wire["label"], "watch this");
        // The points are an array, so a shell that counts them gets a number rather
        // than `undefined` -- which is the difference between restoring a three-point
        // channel and restoring nothing at all.
        let points = wire["points"].as_array().expect("an array of points");
        assert_eq!(points.len(), 3);
        for (index, anchor) in points.iter().enumerate() {
            assert_eq!(anchor["unit"], "absolute", "point {index}");
            assert!(anchor["time"].is_number(), "point {index}");
            assert!(anchor["price"].is_number(), "point {index}");
        }
        assert_eq!(points[2]["price"], 46_000.0);
        assert_eq!(wire["options"]["extend"], "left");
        assert_eq!(wire["options"]["level_profile"], "extension");
        // The provenance keys the shell's layer filter reads.
        assert_eq!(wire["created_by"], "ai");
        assert_eq!(wire["confidence"], 0.82);
        assert_eq!(wire["reason"], "three rejections");
    }

    #[test]
    fn a_one_point_response_still_carries_an_array() {
        // Not a bare anchor and not `null`: the shell reads `points` the same way
        // for every kind, and a level whose response is spelled differently is a
        // branch in the restore code that only runs on Tuesdays.
        let wire = serde_json::to_value(describe(&stored(
            "hline",
            vec![StoredPoint {
                time: T1,
                price: 45_000.0,
            }],
        ))
        .expect("a known kind"))
        .expect("serializes");
        assert_eq!(wire["points"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn the_list_response_pins_the_symbol_it_echoes() {
        // The shell compares this against the symbol it asked for, so a rename
        // turns that check into `undefined !== "BTCUSDT"` -- which is true, so
        // every response is discarded and the chart shows no drawings at all.
        let listed = serde_json::to_value(DrawingsResponse {
            symbol: "BTCUSDT".into(),
            drawings: vec![],
        })
        .expect("serializes");
        assert_eq!(listed["symbol"], "BTCUSDT");
        assert_eq!(listed["drawings"], json!([]));
    }

    #[test]
    fn the_create_body_accepts_the_shells_flattened_shape() {
        // The shell posts `{symbol, kind, points, label}` -- one object, because it
        // has one object to post. `flatten` is what lets the symbol live outside
        // `DrawingBody` while the body stays flat on the wire.
        let parsed: CreateBody = serde_json::from_str(
            r#"{"symbol": "btcusdt", "kind": "trendline",
                "points": [
                  {"unit": "absolute", "time": 1.0, "price": 2.0},
                  {"unit": "absolute", "time": 3.0, "price": 4.0}
                ]}"#,
        )
        .expect("must deserialize");
        assert_eq!(parsed.symbol, "btcusdt");
        assert_eq!(parsed.drawing.kind, DrawingKind::Trendline);
        assert_eq!(parsed.drawing.points.len(), 2);
        assert_eq!(parsed.drawing.label, None);
    }
}
