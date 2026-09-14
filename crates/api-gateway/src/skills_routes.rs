//! `/skills` (`docs/10`, `docs/12`).
//!
//! Reads are served from the on-disk skill library. Writes deliberately
//! return 501: the `skills` table in `docs/13` is scoped per `user_id`, and
//! authentication does not land until Phase 7. An unauthenticated endpoint
//! that writes documents to disk -- or that has to invent a user -- is worse
//! than an honest "not yet".
//!
//! The versioning rules themselves are implemented and tested in
//! `ai-agent::skills` (append-only, major-version-stable ids); only the
//! persistence backing them is deferred.

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use ai_agent::Skill;

use crate::error::ApiError;
use crate::AppState;

/// Summary of a skill, for list views.
#[derive(Debug, Serialize)]
pub struct SkillSummary {
    /// Stable id, e.g. `liquidity-sweep-absorption-v2`.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Version string, e.g. `2.1`.
    pub version: String,
    /// Category: `liquidity`, `footprint`, ...
    pub category: String,
    /// Markets the skill was written for.
    pub preferred_markets: Vec<String>,
}

impl From<&Skill> for SkillSummary {
    fn from(skill: &Skill) -> Self {
        Self {
            id: skill.id(),
            name: skill.name.clone(),
            version: skill.version.clone(),
            category: skill.category.clone(),
            preferred_markets: skill.preferred_markets.clone(),
        }
    }
}

/// `GET /skills` -- newest version of every skill.
pub async fn list(State(state): State<AppState>) -> Json<Vec<SkillSummary>> {
    let summaries: Vec<SkillSummary> = state
        .skills
        .latest_versions()
        .into_iter()
        .map(SkillSummary::from)
        .collect();
    Json(summaries)
}

/// `GET /skills/{id}` -- the newest version with that id.
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Skill>, ApiError> {
    state
        .skills
        .by_id(&id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("no skill with id `{id}`")))
        .map(Json)
}

/// Why writes are refused.
const NOT_PERSISTED: &str =
    "skill persistence lands with authentication in Phase 7: the `skills` table is \
     scoped per user and there is no user context yet";

/// `POST /skills` -- create the first version of a skill.
///
/// 501, not a silent no-op. The `skills` table in `docs/13` is scoped per
/// `user_id` and authentication does not land until Phase 7, so there is
/// nowhere to put this that belongs to anyone.
pub async fn create() -> ApiError {
    ApiError::not_implemented(NOT_PERSISTED)
}

/// `PUT /skills/{id}` -- create a new version, never mutating in place.
///
/// Same reason as [`create`]. The append-only versioning rule itself is
/// implemented and tested in `ai-agent::skills`; only persistence is missing.
pub async fn create_version(Path(id): Path<String>) -> ApiError {
    ApiError::not_implemented(format!("`{id}` was not written: {NOT_PERSISTED}"))
}
