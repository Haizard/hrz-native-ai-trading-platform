//! `/skills` (`docs/10`, `docs/12`, `docs/13`).
//!
//! ## Two sources, one view
//!
//! A skill is either **shipped** -- a YAML file under `SKILLS_DIR`, loaded at
//! startup, the same for everyone -- or **owned** -- a row in `skills`, scoped
//! by `user_id`. `GET /skills` returns both. Keeping them separate in storage
//! but merged on read is what lets a user publish `2.1` of a skill that ships
//! as `2.0` without forking the file on disk, while an untouched deployment
//! still serves the shipped library exactly as before.
//!
//! When both answer to the same name, **the user's copy wins**: their version
//! is the one they meant to trade. A listing shows one entry per skill -- its
//! newest version -- because "which version do I trade" is a different question
//! from "what have I ever published".
//!
//! ## Writes are append-only
//!
//! `PUT /skills/{id}` does not update a row, it inserts the next version.
//! `docs/10` is the reason: a past thesis has to stay explainable against the
//! skill version that produced it, so publishing `2.1` twice is a 409 rather
//! than a silent overwrite.
//!
//! ## The client-facing id is a slug, not the row id
//!
//! `Skill::id()` is `{name-slug}-v{major}`, which is also what `/agent/ask`
//! pins with `skill_id`. The table's `id` is a UUID and stays internal: a route
//! that took both would have two kinds of "not found".

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use ai_agent::Skill;

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
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
    /// Whether this is the caller's own copy rather than a shipped one.
    pub owned: bool,
}

/// A skill as a client sees it, with the id the client needs back.
///
/// The id is `{name-slug}-v{major}` and is derived, not stored -- so a caller
/// that wants to publish the next version cannot be expected to recompute the
/// slug. It is echoed on every write and read for exactly that reason.
#[derive(Debug, Serialize)]
pub struct SkillResponse {
    /// Stable id, e.g. `liquidity-sweep-absorption-v2`.
    pub id: String,
    /// The document itself.
    #[serde(flatten)]
    pub skill: Skill,
}

impl SkillResponse {
    fn new(skill: Skill) -> Self {
        Self {
            id: skill.id(),
            skill,
        }
    }
}

impl SkillSummary {
    fn new(skill: &Skill, owned: bool) -> Self {
        Self {
            id: skill.id(),
            name: skill.name.clone(),
            version: skill.version.clone(),
            category: skill.category.clone(),
            preferred_markets: skill.preferred_markets.clone(),
            owned,
        }
    }
}

/// The caller's stored skills, newest first.
///
/// A row whose document no longer deserializes is skipped with a warning
/// rather than failing the whole list: a `Skill` that gained a field since this
/// row was written should not make every other skill unreachable. Silent it is
/// not -- the warning names the row, so the drift is findable.
async fn owned_skills(state: &AppState, user: &UserContext) -> Result<Vec<Skill>, ApiError> {
    let Some(database) = state.db.as_ref() else {
        return Ok(Vec::new());
    };
    let rows =
        db::skills::list_skills(database.pool(), user.user_id, db::skills::MAX_SKILLS).await?;

    let mut skills = Vec::with_capacity(rows.len());
    for row in rows {
        match serde_json::from_value::<Skill>(row.document) {
            Ok(skill) => skills.push(skill),
            Err(e) => tracing::warn!(
                skill_id = %row.id,
                name = %row.name,
                version = %row.version,
                error = %e,
                "a stored skill no longer parses and was left out of the listing"
            ),
        }
    }
    Ok(skills)
}

/// The newest version of each skill, keyed by name.
///
/// Keyed by name and not by id because a major-version bump changes the id
/// (`{name}-v{major}`) while the skill is still the same skill. Listing every
/// version the user ever published would answer a question nobody asked in a
/// list view.
fn newest_by_name(skills: Vec<Skill>) -> BTreeMap<String, Skill> {
    let mut newest: BTreeMap<String, Skill> = BTreeMap::new();
    for skill in skills {
        let replace = newest
            .get(&skill.name)
            .is_none_or(|current| skill.version_key() > current.version_key());
        if replace {
            newest.insert(skill.name.clone(), skill);
        }
    }
    newest
}

/// `GET /skills` -- the caller's own skills, then everything shipped.
///
/// # Errors
/// 401 without a token. A missing database is *not* an error here, because the
/// shipped library is still answerable -- a deployment with no database should
/// lose writes, not reads.
pub async fn list(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<Vec<SkillSummary>>, ApiError> {
    let newest = newest_by_name(owned_skills(&state, &user).await?);
    let shipped: BTreeSet<&str> = newest.keys().map(String::as_str).collect();

    let mut out: Vec<SkillSummary> = newest
        .values()
        .map(|skill| SkillSummary::new(skill, true))
        .collect();
    out.extend(
        state
            .skills
            .latest_versions()
            .into_iter()
            .filter(|skill| !shipped.contains(skill.name.as_str()))
            .map(|skill| SkillSummary::new(skill, false)),
    );
    Ok(Json(out))
}

/// `GET /skills/{id}` -- the caller's newest copy if they have one, else the
/// shipped one.
///
/// # Errors
/// 404 when neither source has it, 401 without a token.
pub async fn get(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<SkillResponse>, ApiError> {
    let owned = owned_skills(&state, &user)
        .await?
        .into_iter()
        .filter(|skill| skill.id() == id)
        .max_by_key(Skill::version_key);
    if let Some(skill) = owned {
        return Ok(Json(SkillResponse::new(skill)));
    }

    state
        .skills
        .by_id(&id)
        .cloned()
        .map(SkillResponse::new)
        .ok_or_else(|| ApiError::not_found(format!("no skill with id `{id}`")))
        .map(Json)
}

/// Why a skill document is refused.
///
/// `Skill` is `#[serde(default)]` throughout, so `{}` deserializes into an
/// empty skill that would sort last and teach the model nothing. The three
/// fields that have to be real are the ones the id, the ordering and retrieval
/// are derived from.
///
/// The schema-v2 references (docs/40) are validated against the vocabularies
/// they name: capability ids against the registry's catalog, `applies_to`
/// against the registered tools. An unknown capability id would otherwise be
/// refused for a *typo* at runtime, indistinguishable from a real data gap;
/// an unknown tool name is doctrine that could never attach to a result.
fn validate_skill(skill: &Skill, registry: &capabilities::Registry) -> Result<(), ApiError> {
    if skill.name.trim().is_empty() {
        return Err(ApiError::bad_request(
            "SKILL_NAME_REQUIRED",
            "a skill needs a `name`: it is what its id is derived from",
        ));
    }
    if skill.category.trim().is_empty() {
        return Err(ApiError::bad_request(
            "SKILL_CATEGORY_REQUIRED",
            "a skill needs a `category`: retrieval ranks by it",
        ));
    }
    let major = skill.version.split('.').next().unwrap_or_default();
    if major.parse::<u32>().is_err() {
        return Err(ApiError::bad_request(
            "SKILL_VERSION_INVALID",
            format!(
                "`version` must start with a number, e.g. `1.0` or `2.1`; got `{}`",
                skill.version
            ),
        ));
    }
    if skill.knowledge.trim().is_empty() && skill.rules.is_empty() {
        return Err(ApiError::bad_request(
            "SKILL_EMPTY",
            "a skill needs a `knowledge` body or at least one `rule`: with neither there is \
             nothing for the model to apply",
        ));
    }

    let known: Vec<&str> = registry.catalog().iter().map(|d| d.id).collect();
    let check_capability = |id: &str| -> Result<(), ApiError> {
        if known.contains(&id) {
            return Ok(());
        }
        Err(ApiError::bad_request(
            "SKILL_CAPABILITY_UNKNOWN",
            format!(
                "`{id}` is not a registered capability. The catalog holds: {}",
                known.join(", ")
            ),
        ))
    };
    for need in skill
        .capability_requirements
        .required
        .iter()
        .chain(&skill.capability_requirements.preferred)
    {
        check_capability(&need.capability)?;
    }
    for rule in &skill.capability_requirements.fallback {
        check_capability(&rule.needs)?;
    }

    // Tool names come from the real registry, so a rename in `tools.rs` turns
    // a stale skill into a 422 at write time rather than silent doctrine.
    let tools = ai_agent::ToolRegistry::market_analysis();
    for tool in &skill.applies_to.tools {
        if !tools.contains(tool) {
            return Err(ApiError::bad_request(
                "SKILL_TOOL_UNKNOWN",
                format!(
                    "`{tool}` is not a registered tool: a tool skill's doctrine attaches by \
                     tool name, and this one would never fire"
                ),
            ));
        }
    }
    Ok(())
}

/// Store a skill, refusing a version this user already has.
async fn insert_skill(state: &AppState, user: &UserContext, skill: &Skill) -> Result<(), ApiError> {
    let database = state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured; skills cannot be stored"))?;

    if db::skills::skill_version_exists(database.pool(), user.user_id, &skill.name, &skill.version)
        .await?
    {
        return Err(ApiError::coded(
            StatusCode::CONFLICT,
            "SKILL_VERSION_EXISTS",
            format!(
                "you already have `{}` version `{}`. Versions are never overwritten -- post a \
                 higher version instead.",
                skill.name, skill.version
            ),
        ));
    }

    let document = serde_json::to_value(skill)
        .map_err(|e| ApiError::internal(format!("could not serialize the skill: {e}")))?;
    db::skills::create_skill(
        database.pool(),
        user.user_id,
        &skill.name,
        &skill.version,
        &skill.category,
        &document,
    )
    .await?;
    Ok(())
}

/// `POST /skills` -- publish the first version of a skill.
///
/// # Errors
/// 400 when the document is not usable, 409 when this version already exists,
/// 503 without a database.
pub async fn create(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(skill): ApiJson<Skill>,
) -> Result<(StatusCode, Json<SkillResponse>), ApiError> {
    validate_skill(&skill, &state.capability_registry)?;
    insert_skill(&state, &user, &skill).await?;
    Ok((StatusCode::CREATED, Json(SkillResponse::new(skill))))
}

/// `PUT /skills/{id}` -- publish the next version, never mutating in place.
///
/// The document's own `id()` has to match the path. Without that check,
/// `PUT /skills/anything` would happily file a skill under a different name
/// and the id in the URL would mean nothing.
///
/// # Errors
/// 400 on a mismatched or unusable document, 409 when the version exists,
/// 503 without a database.
pub async fn create_version(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
    ApiJson(skill): ApiJson<Skill>,
) -> Result<(StatusCode, Json<SkillResponse>), ApiError> {
    validate_skill(&skill, &state.capability_registry)?;
    if skill.id() != id {
        return Err(ApiError::bad_request(
            "SKILL_ID_MISMATCH",
            format!(
                "the document is `{}` but the path is `{id}`. A version of a skill keeps its \
                 name, so only the version may differ.",
                skill.id()
            ),
        ));
    }
    insert_skill(&state, &user, &skill).await?;
    Ok((StatusCode::CREATED, Json(SkillResponse::new(skill))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real catalog over the real provider profiles — the point of the
    /// validation is that it checks against what the deployment knows.
    fn registry() -> capabilities::Registry {
        capabilities::Registry::new(
            capabilities::descriptor::STANDARD,
            market_data::exchanges::profile::standard(),
        )
    }

    fn valid_skill() -> Skill {
        Skill {
            name: "Test Skill".into(),
            category: "liquidity".into(),
            knowledge: "something real".into(),
            ..Skill::default()
        }
    }

    #[test]
    fn an_unknown_capability_id_is_refused_with_the_catalog_listed() {
        let mut skill = valid_skill();
        skill.capability_requirements.required = vec![ai_agent::CapabilityNeed {
            capability: "footprnt".into(),
        }];
        let err = validate_skill(&skill, &registry()).unwrap_err();
        let body = format!("{err:?}");
        assert!(body.contains("SKILL_CAPABILITY_UNKNOWN"), "{body}");
        assert!(
            body.contains("footprint"),
            "the refusal lists the valid ids so the typo is fixable: {body}"
        );
    }

    #[test]
    fn a_fallback_rule_also_validates_its_capability() {
        let mut skill = valid_skill();
        skill.capability_requirements.fallback = vec![ai_agent::FallbackRule {
            needs: "nonsense".into(),
            accept: ai_agent::FallbackAccept::Derived,
        }];
        let err = validate_skill(&skill, &registry()).unwrap_err();
        assert!(format!("{err:?}").contains("SKILL_CAPABILITY_UNKNOWN"));
    }

    #[test]
    fn an_unknown_tool_name_is_refused_because_doctrine_would_never_attach() {
        let mut skill = valid_skill();
        skill.applies_to.tools = vec!["get_footprnt".into()];
        let err = validate_skill(&skill, &registry()).unwrap_err();
        assert!(format!("{err:?}").contains("SKILL_TOOL_UNKNOWN"));
    }

    #[test]
    fn a_document_with_real_references_passes() {
        let mut skill = valid_skill();
        skill.kind = ai_agent::SkillKind::Tool;
        skill.applies_to.tools = vec!["get_footprint".into()];
        skill.capability_requirements.required = vec![ai_agent::CapabilityNeed {
            capability: "footprint".into(),
        }];
        validate_skill(&skill, &registry()).expect("real vocabulary validates");
    }
}
