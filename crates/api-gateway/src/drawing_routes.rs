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
//! [`DrawingBody`] reuses [`DrawingKind`] and [`Anchor`] rather than declaring its
//! own, because those two types *are* the wire: the shell builds them from a
//! pointer position, the engine resolves them, and this route stores them. A
//! second declaration would be a second thing to keep in step, and the
//! disagreement would be silent — nothing checks the shape of a drawing against
//! anything.
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

use chart_engine::{Anchor, Drawing, DrawingKind};

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
    /// The first anchor, which every kind has.
    pub a1: Anchor,
    /// The second, for the kinds that have one.
    #[serde(default)]
    pub a2: Option<Anchor>,
    /// What the user called it.
    #[serde(default)]
    pub label: Option<String>,
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
    /// The first anchor, always absolute.
    pub a1: Anchor,
    /// The second, always absolute, for the kinds that have one.
    pub a2: Option<Anchor>,
    /// What the user called it.
    pub label: Option<String>,
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
/// From the body rather than by re-reading the row: `prepare` does not change
/// the anchors, so a second query would be a round trip to learn what the caller
/// just told us — and, on the managed instance, a second of latency.
fn response(id: Uuid, body: &DrawingBody) -> DrawingResponse {
    DrawingResponse {
        id: id.to_string(),
        kind: body.kind,
        a1: body.a1,
        a2: body.a2,
        label: body.label.clone(),
    }
}

/// Check a body, or say why it cannot be stored.
///
/// The anchor rule is the **engine's own** [`Drawing::validate_anchors`], run on
/// a drawing that is not going to be drawn. A second copy of "which kinds need
/// two anchors" here is exactly how the route and the engine come to disagree,
/// and the disagreement would be a drawing the API accepts and the chart never
/// shows.
///
/// # Errors
/// 422, naming the problem.
fn prepare(body: &DrawingBody) -> Result<db::NewDrawing, ApiError> {
    let check = Drawing {
        // Empty, and it does not matter: `validate_anchors` is anchors only, on
        // purpose, because a route validating a request body has no id to give
        // and should not have to invent one.
        id: String::new(),
        kind: body.kind,
        a1: body.a1,
        a2: body.a2,
        label: body.label.clone(),
        selected: false,
    };
    check.validate_anchors().map_err(invalid)?;

    let (a1_time_ms, a1_price) = absolute(body.a1)?;
    let second = match body.a2 {
        Some(anchor) => Some(absolute(anchor)?),
        None => None,
    };

    Ok(db::NewDrawing {
        kind: body.kind.name().to_string(),
        a1_time_ms,
        a1_price,
        a2_time_ms: second.map(|(time, _)| time),
        a2_price: second.map(|(_, price)| price),
        label: body.label.clone(),
        // A drawing that arrives over the HTTP route is the user's own work,
        // however it was produced: provenance is the *agent's* door's stamp,
        // and this is not that door.
        provenance: None,
    })
}

/// The millisecond time and price of an anchor, or a 422.
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
        a1: Anchor::Absolute {
            time: row.a1_time_ms,
            price: row.a1_price,
        },
        // The `CHECK` keeps the pair whole, so a half-populated one means the
        // table has been edited by hand. Reported as absent, which is the honest
        // reading of it, rather than paired with a zero.
        a2: match (row.a2_time_ms, row.a2_price) {
            (Some(time), Some(price)) => Some(Anchor::Absolute { time, price }),
            _ => None,
        },
        label: row.label.clone(),
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

    fn absolute(time: f64, price: f64) -> Anchor {
        Anchor::Absolute { time, price }
    }

    fn body(kind: DrawingKind, a1: Anchor, a2: Option<Anchor>) -> DrawingBody {
        DrawingBody {
            kind,
            a1,
            a2,
            label: None,
        }
    }

    /// The two lists of kinds, and the rule about anchors, kept in step.
    ///
    /// Two crates describe the same vocabulary and nothing but this test connects
    /// them: the engine's enum and the storage's `KINDS` array. A kind added to
    /// one and not the other is a drawing the API accepts and the chart never
    /// shows, or one the engine can draw that nothing can save — and neither is a
    /// compile error anywhere.
    #[test]
    fn every_kind_the_engine_draws_is_a_kind_the_database_stores() {
        for kind in DrawingKind::ALL {
            assert!(
                db::DRAWING_KINDS.contains(&kind.name()),
                "`{}` is not in db::DRAWING_KINDS: {:?}",
                kind.name(),
                db::DRAWING_KINDS
            );
            // And the anchor rule agrees, which is the half a second copy would
            // get wrong: a `hline` the engine thinks needs two anchors is a
            // drawing nobody can create.
            assert_eq!(
                kind.needs_second_anchor(),
                db::drawings::needs_second_anchor(kind.name()),
                "the engine and the storage disagree about `{}`",
                kind.name()
            );
        }
        assert_eq!(
            DrawingKind::ALL.len(),
            db::DRAWING_KINDS.len(),
            "one side has a kind the other does not"
        );
    }

    #[test]
    fn a_body_with_absolute_anchors_becomes_a_row_in_milliseconds() {
        let prepared = prepare(&body(
            DrawingKind::Trendline,
            absolute(1_767_225_600_000.0, 45_000.0),
            Some(absolute(1_767_229_200_000.0, 45_500.0)),
        ))
        .expect("must accept");

        assert_eq!(prepared.kind, "trendline");
        assert_eq!(prepared.a1_time_ms, 1_767_225_600_000.0);
        assert_eq!(prepared.a1_price, 45_000.0);
        assert_eq!(prepared.a2_time_ms, Some(1_767_229_200_000.0));
        assert_eq!(prepared.a2_price, Some(45_500.0));
    }

    #[test]
    fn a_fraction_anchor_is_refused_because_it_cannot_be_stored() {
        // The interesting validation. A fraction means "this far across the plot
        // I happened to be looking at", so storing one would put the drawing
        // somewhere different on every load -- and the shell has an absolute
        // answer available, because the engine reported one.
        let error = prepare(&body(
            DrawingKind::Trendline,
            Anchor::Fraction { x: 0.4, y: 0.6 },
            Some(absolute(1.0, 2.0)),
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
    fn a_second_anchor_that_is_a_fraction_is_refused_too() {
        // Both anchors, because the first version of the check ran on `a1` and
        // the `a2` path was the one that reached the database.
        let error = prepare(&body(
            DrawingKind::Trendline,
            absolute(1.0, 2.0),
            Some(Anchor::Fraction { x: 0.4, y: 0.6 }),
        ))
        .expect_err("must refuse");
        assert_eq!(error.code(), "DRAWING_INVALID");
    }

    #[test]
    fn a_two_anchor_kind_with_one_anchor_is_refused_in_the_engines_words() {
        // The reason is the engine's own, because the rule is: a second copy of
        // it here would be a second thing to keep in step, and the way the two
        // drift apart is a drawing the API accepts and the chart never shows.
        let error =
            prepare(&body(DrawingKind::Fib, absolute(1.0, 2.0), None)).expect_err("must refuse");
        assert_eq!(error.code(), "DRAWING_INVALID");
        assert!(error.message().contains("fib"), "{}", error.message());
        assert!(
            error.message().contains("two anchors"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_horizontal_line_is_accepted_with_one_anchor() {
        let prepared = prepare(&body(
            DrawingKind::Hline,
            absolute(1_767_225_600_000.0, 45_000.0),
            None,
        ))
        .expect("must accept");
        assert_eq!(prepared.kind, "hline");
        assert_eq!(prepared.a2_time_ms, None);
        assert_eq!(prepared.a2_price, None);
    }

    #[test]
    fn a_hostile_anchor_is_refused_rather_than_stored() {
        // A `NaN` price produces a `NaN` y and a canvas silently ignores it, so
        // the drawing would be absent with nothing anywhere saying why. Refusing
        // it at the door is the only place the reason can be given.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = prepare(&body(DrawingKind::Hline, absolute(bad, 1.0), None))
                .expect_err("must refuse");
            assert_eq!(error.code(), "DRAWING_INVALID", "{bad}");
        }
    }

    #[test]
    fn a_row_becomes_the_response_the_shell_reads() {
        let row = db::DrawingRow {
            id: Uuid::nil(),
            symbol: "BTCUSDT".into(),
            kind: "fib".into(),
            a1_time_ms: 1_767_225_600_000.0,
            a1_price: 45_000.0,
            a2_time_ms: Some(1_767_229_200_000.0),
            a2_price: Some(45_500.0),
            label: Some("the range I keep watching".into()),
            // A hand-drawn fixture: no provenance, which is what `None` means.
            created_by: None,
            agent: None,
            confidence: None,
            reason: None,
            created_at: 0,
            updated_at: 0,
        };
        let described = describe(&row).expect("a known kind");

        assert_eq!(described.kind, DrawingKind::Fib);
        assert_eq!(
            described.a1,
            Anchor::Absolute {
                time: 1_767_225_600_000.0,
                price: 45_000.0
            }
        );
        assert_eq!(
            described.a2.map(Anchor::absolute),
            Some(Some((1_767_229_200_000.0, 45_500.0)))
        );
    }

    #[test]
    fn a_stored_kind_this_build_cannot_draw_is_skipped_rather_than_fatal() {
        // A row written by a newer engine and then rolled back, or edited by
        // hand. One unreadable drawing must not take the other nine off the
        // chart, and the route warns with the row's id so the cause is findable.
        let row = db::DrawingRow {
            id: Uuid::nil(),
            symbol: "BTCUSDT".into(),
            kind: "channel".into(),
            a1_time_ms: 0.0,
            a1_price: 1.0,
            a2_time_ms: None,
            a2_price: None,
            label: None,
            created_by: None,
            agent: None,
            confidence: None,
            reason: None,
            created_at: 0,
            updated_at: 0,
        };
        assert!(describe(&row).is_none());
        // And a kind the engine *does* know is not skipped, so the branch above
        // is about the unknown kind rather than about the fixture.
        let known = db::DrawingRow {
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
        // A rename here is not a compile error anywhere. It is a drawing that
        // stops appearing, or one whose anchors the shell cannot post back -- and
        // the second failure is a drag that silently does not save.
        let body = serde_json::to_value(DrawingResponse {
            id: "8f3a".into(),
            kind: DrawingKind::Trendline,
            a1: absolute(1.0, 2.0),
            a2: Some(absolute(3.0, 4.0)),
            label: Some("watch this".into()),
        })
        .expect("serializes");

        assert_eq!(body["id"], "8f3a");
        assert_eq!(body["kind"], "trendline");
        assert_eq!(body["label"], "watch this");
        for anchor in ["a1", "a2"] {
            assert_eq!(body[anchor]["unit"], "absolute", "{anchor}");
            assert!(body[anchor]["time"].is_number(), "{anchor}");
            assert!(body[anchor]["price"].is_number(), "{anchor}");
        }
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
        assert_eq!(listed["drawings"], serde_json::json!([]));
    }

    #[test]
    fn the_create_body_accepts_the_shells_flattened_shape() {
        // The shell posts `{symbol, kind, a1, a2, label}` -- one object, because
        // it has one object to post. `flatten` is what lets the symbol live
        // outside `DrawingBody` while the body stays flat on the wire.
        let parsed: CreateBody = serde_json::from_str(
            r#"{"symbol": "btcusdt", "kind": "trendline",
                "a1": {"unit": "absolute", "time": 1.0, "price": 2.0},
                "a2": {"unit": "absolute", "time": 3.0, "price": 4.0}}"#,
        )
        .expect("must deserialize");
        assert_eq!(parsed.symbol, "btcusdt");
        assert_eq!(parsed.drawing.kind, DrawingKind::Trendline);
        assert_eq!(parsed.drawing.label, None);
    }
}
