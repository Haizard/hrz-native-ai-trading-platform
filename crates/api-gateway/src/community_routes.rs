//! `/community/indicators`: the community indicator library (`docs/53`).
//!
//! ## Open to every plan
//!
//! Browsing, installing and publishing are not paid gates: the library is the
//! platform's public script shelf, and a Free account can use all of it. A
//! plan only affects catalog placement -- Desk authors sort first in
//! `featured`. Abuse is bounded by the platform constants below, not by plan
//! limits.
//!
//! ## Re-vetting is the gate, not the stored report
//!
//! The workspace already validated the revision, but a validator can change.
//! Publish, install and update re-run the current validator and refuse with
//! `422 COMMUNITY_SOURCE_INVALID` rather than trusting a stored `valid`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::UserContext;
use crate::billing::{CommunityIndicators, Entitlements};
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiQuery};
use crate::AppState;

const MAX_PUBLISHED_INDICATORS: i64 = 25;
const MAX_VERSIONS_PER_INDICATOR: i64 = 50;
const MAX_INSTALLS_PER_USER: i64 = 100;
const MAX_COMMUNITY_SOURCE_BYTES: usize = 64 * 1024;
const MAX_COMMUNITY_TAGS: usize = 5;
const MAX_TITLE_CHARS: usize = 80;
const MIN_TITLE_CHARS: usize = 3;
const MAX_SUMMARY_CHARS: usize = 280;
const MAX_TAG_CHARS: usize = 24;
const MAX_AUTHOR_CHARS: usize = 40;
const MAX_REPORT_NOTE_CHARS: usize = 500;
const VERSION_LIST_LIMIT: i64 = 50;

fn database(state: &AppState) -> Result<Arc<db::Database>, ApiError> {
    state.db.clone().ok_or_else(|| {
        ApiError::unavailable("the community library needs the database; it is not configured")
    })
}

fn visibility(item: &db::CommunityIndicatorRow, viewer: Uuid) -> Result<(), ApiError> {
    if item.is_listed() || item.is_author(viewer) {
        Ok(())
    } else {
        Err(unknown())
    }
}

fn unknown() -> ApiError {
    ApiError::not_found("community indicator not found")
}

fn conflict(code: &str, message: impl Into<String>) -> ApiError {
    ApiError::coded(StatusCode::CONFLICT, code, message)
}

fn metadata(message: impl Into<String>) -> ApiError {
    ApiError::bad_request("COMMUNITY_METADATA_INVALID", message)
}

fn source_invalid(issues: Vec<String>) -> ApiError {
    ApiError::coded(
        StatusCode::UNPROCESSABLE_ENTITY,
        "COMMUNITY_SOURCE_INVALID",
        "the published source no longer passes the current validator",
    )
    .with_details(serde_json::json!({ "issues": issues }))
}

fn clean_title(value: &str) -> Result<String, ApiError> {
    let title = value.trim();
    let count = title.chars().count();
    if !(MIN_TITLE_CHARS..=MAX_TITLE_CHARS).contains(&count) {
        return Err(metadata(format!(
            "a title is {MIN_TITLE_CHARS}-{MAX_TITLE_CHARS} characters; this one is {count}"
        )));
    }
    Ok(title.to_string())
}

fn clean_summary(value: &str) -> Result<String, ApiError> {
    let summary = value.trim();
    if summary.is_empty() || summary.chars().count() > MAX_SUMMARY_CHARS {
        return Err(metadata(format!(
            "a summary is 1-{MAX_SUMMARY_CHARS} characters"
        )));
    }
    Ok(summary.to_string())
}

fn clean_tags(tags: &[String]) -> Result<Vec<String>, ApiError> {
    if tags.len() > MAX_COMMUNITY_TAGS {
        return Err(metadata(format!(
            "at most {MAX_COMMUNITY_TAGS} tags; {} were sent",
            tags.len()
        )));
    }
    let mut out = Vec::new();
    for tag in tags {
        let tag = tag.trim().to_ascii_lowercase();
        if tag.is_empty() || tag.chars().count() > MAX_TAG_CHARS {
            return Err(metadata(format!(
                "a tag is 1-{MAX_TAG_CHARS} characters"
            )));
        }
        if !out.contains(&tag) {
            out.push(tag);
        }
    }
    Ok(out)
}

fn clean_author_name(value: Option<&str>, user_id: Uuid) -> Result<String, ApiError> {
    let value = value.unwrap_or("").trim();
    if value.is_empty() {
        return Ok(format!("Trader-{}", &user_id.simple().to_string()[..6]));
    }
    if value.chars().count() > MAX_AUTHOR_CHARS {
        return Err(metadata(format!(
            "an author name is at most {MAX_AUTHOR_CHARS} characters"
        )));
    }
    Ok(value.to_string())
}

fn slug_for(title: &str) -> String {
    let mut base = String::new();
    let mut last_dash = false;
    for ch in title.to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            base.push(ch);
            last_dash = false;
        } else if !last_dash {
            base.push('-');
            last_dash = true;
        }
    }
    let base = base.trim_matches('-');
    let base = if base.is_empty() { "indicator" } else { base };
    let base: String = base.chars().take(48).collect();
    let suffix = &Uuid::new_v4().simple().to_string()[..6];
    format!("{base}-{suffix}")
}

fn revet(
    source: &str,
    validation: &serde_json::Value,
    preview: &serde_json::Value,
) -> Result<(), ApiError> {
    let pine = validation
        .get("representation")
        .and_then(|value| value.as_str())
        == Some("code");
    if pine {
        pine_lite::vet(source).map_err(|errors| {
            source_invalid(
                errors
                    .iter()
                    .map(|error| {
                        format!(
                            "line {} col {} [{}]: {}",
                            error.span.line,
                            error.span.col,
                            pine_lite::kind_name(error.kind),
                            error.message
                        )
                    })
                    .collect(),
            )
        })?;
    } else {
        strategy_dsl::parse_and_validate(source)
            .map_err(|error| source_invalid(vec![error.to_string()]))?;
    }
    let output: chart_engine::IndicatorOutput = serde_json::from_value(preview.clone()).map_err(
        |error| source_invalid(vec![format!("the preview is not a chart output: {error}")]),
    )?;
    output
        .validate()
        .map_err(|error| source_invalid(vec![error]))?;
    Ok(())
}

fn tier_of(entitlements: &Entitlements) -> &'static str {
    match entitlements.features.community_indicators {
        CommunityIndicators::Priority => "priority",
        CommunityIndicators::Included => "publish",
    }
}

/// The tier recorded for an author at publish time.
///
/// Publishing is open to every plan, so this is placement, not permission:
/// Desk authors sort first in `featured`, everyone else is an ordinary
/// published author. A deployment with no billing service records the
/// ordinary tier.
async fn publish_tier(state: &AppState, user_id: Uuid) -> Result<&'static str, ApiError> {
    let Some(billing) = state.billing.as_ref() else {
        return Ok("publish");
    };
    Ok(tier_of(&billing.entitlements(user_id).await?))
}

/// One catalog row as the catalog and author views render it.
#[derive(Debug, Serialize)]
pub struct CommunityItem {
    /// Row id.
    pub id: String,
    /// URL name.
    pub slug: String,
    /// Display title.
    pub title: String,
    /// Short description.
    pub summary: String,
    /// Normalized tags.
    pub tags: Vec<String>,
    /// Display name at publish time.
    pub author_name: String,
    /// `publish` or `priority`.
    pub author_tier: String,
    /// Cumulative installs.
    pub install_count: i64,
    /// Current version number.
    pub version: i32,
    /// `listed`, `unlisted` or `removed`.
    pub status: String,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Unix milliseconds.
    pub updated_at: i64,
}

/// A catalog page.
#[derive(Debug, Serialize)]
pub struct CommunityList {
    /// The rows on this page.
    pub items: Vec<CommunityItem>,
    /// Every row matching the filter, ignoring paging.
    pub total: i64,
}

/// One version in a detail response.
#[derive(Debug, Serialize)]
pub struct VersionSummary {
    /// Version id.
    pub id: String,
    /// 1-based sequence.
    pub version_number: i32,
    /// Plain-language summary.
    pub summary: String,
    /// Unix milliseconds.
    pub published_at: i64,
}

/// The caller's pin on an item.
#[derive(Debug, Serialize)]
pub struct InstalledView {
    /// The owned workspace.
    pub workspace_id: String,
    /// The pinned version number.
    pub version: i32,
    /// The workspace's active revision, when it has one.
    pub revision_id: Option<String>,
}

/// Everything one item's page needs.
#[derive(Debug, Serialize)]
pub struct CommunityDetail {
    /// The catalog fields.
    #[serde(flatten)]
    pub item: CommunityItem,
    /// The current version's validated source.
    pub source: String,
    /// The current version's validator report.
    pub validation: serde_json::Value,
    /// The current version's chart preview.
    pub preview: serde_json::Value,
    /// Every version, newest first.
    pub versions: Vec<VersionSummary>,
    /// The caller's pin, when installed.
    pub installed: Option<InstalledView>,
    /// Whether the pin is behind the current version.
    pub update_available: bool,
    /// Whether the caller published this item.
    pub is_author: bool,
}

fn item_view(row: &db::CommunityIndicatorRow, version: i32) -> CommunityItem {
    CommunityItem {
        id: row.id.to_string(),
        slug: row.slug.clone(),
        title: row.title.clone(),
        summary: row.summary.clone(),
        tags: row.tags.clone(),
        author_name: row.author_name.clone(),
        author_tier: row.author_tier.clone(),
        install_count: row.install_count,
        version,
        status: row.status.clone(),
        created_at: row.created_at / 1_000_000,
        updated_at: row.updated_at / 1_000_000,
    }
}

async fn detail(
    database: &db::Database,
    viewer: Uuid,
    item: &db::CommunityIndicatorRow,
) -> Result<CommunityDetail, ApiError> {
    let version_id = item.current_version_id.ok_or_else(unknown)?;
    let version = db::get_community_version(database.pool(), version_id)
        .await?
        .ok_or_else(unknown)?;
    let versions =
        db::list_community_versions(database.pool(), item.id, VERSION_LIST_LIMIT).await?;
    let install = db::get_community_install(database.pool(), viewer, item.id).await?;
    let revision_id = match &install {
        Some(install) => {
            let revisions =
                db::list_indicator_revisions(database.pool(), viewer, install.workspace_id, 50)
                    .await?;
            revisions
                .into_iter()
                .max_by_key(|revision| revision.revision_number)
                .map(|revision| revision.id.to_string())
        }
        None => None,
    };
    Ok(CommunityDetail {
        item: item_view(item, version.version_number),
        source: version.source.clone(),
        validation: version.validation.clone(),
        preview: version.preview.clone(),
        versions: versions
            .iter()
            .map(|version| VersionSummary {
                id: version.id.to_string(),
                version_number: version.version_number,
                summary: version.summary.clone(),
                published_at: version.published_at / 1_000_000,
            })
            .collect(),
        installed: install.as_ref().map(|install| InstalledView {
            workspace_id: install.workspace_id.to_string(),
            version: versions
                .iter()
                .find(|version| version.id == install.version_id)
                .map_or(0, |version| version.version_number),
            revision_id,
        }),
        update_available: install
            .as_ref()
            .is_some_and(|install| install.version_id != version_id),
        is_author: item.is_author(viewer),
    })
}

/// `GET /community/indicators`
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Free-text filter over title and summary.
    #[serde(default)]
    pub q: Option<String>,
    /// One exact tag.
    #[serde(default)]
    pub tag: Option<String>,
    /// `featured`, `new` or `popular`.
    #[serde(default)]
    pub sort: Option<String>,
    /// Page size, 1-50.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Rows to skip.
    #[serde(default)]
    pub offset: Option<u32>,
}

/// `GET /community/indicators`
pub async fn list(
    State(state): State<AppState>,
    user: UserContext,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> Result<Json<CommunityList>, ApiError> {
    let database = database(&state)?;
    let limit = i64::from(query.limit.unwrap_or(24).clamp(1, 50));
    let offset = i64::from(query.offset.unwrap_or(0));
    let sort = query.sort.as_deref().unwrap_or("featured");
    let (rows, total) = db::list_community_indicators(
        database.pool(),
        query.q.as_deref().filter(|q| !q.trim().is_empty()),
        query.tag.as_deref().filter(|tag| !tag.trim().is_empty()),
        sort,
        limit,
        offset,
    )
    .await?;
    Ok(Json(CommunityList {
        items: rows
            .iter()
            .map(|row| item_view(&row.indicator, row.version_number))
            .collect(),
        total,
    }))
}

/// `GET /community/indicators/installed`
pub async fn installed(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<CommunityList>, ApiError> {
    let database = database(&state)?;
    let rows = db::list_community_installs(database.pool(), user.user_id).await?;
    Ok(Json(CommunityList {
        items: rows
            .iter()
            .map(|(row, _, version)| item_view(row, *version))
            .collect(),
        total: rows.len() as i64,
    }))
}

/// `GET /community/indicators/mine`
pub async fn mine(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<CommunityList>, ApiError> {
    let database = database(&state)?;
    let rows = db::list_author_indicators(database.pool(), user.user_id).await?;
    Ok(Json(CommunityList {
        items: rows
            .iter()
            .map(|row| item_view(&row.indicator, row.version_number))
            .collect(),
        total: rows.len() as i64,
    }))
}

/// `GET /community/indicators/{slug}`
pub async fn get(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
) -> Result<Json<CommunityDetail>, ApiError> {
    let database = database(&state)?;
    let item = db::get_community_indicator(database.pool(), &slug)
        .await?
        .ok_or_else(unknown)?;
    visibility(&item, user.user_id)?;
    Ok(Json(detail(&database, user.user_id, &item).await?))
}

/// `POST /community/indicators`'s body.
#[derive(Debug, Deserialize)]
pub struct PublishBody {
    /// The workspace that owns the revision.
    pub workspace_id: Uuid,
    /// The revision to publish; the active one when omitted.
    #[serde(default)]
    pub revision_id: Option<Uuid>,
    /// Display title.
    pub title: String,
    /// Short description; the revision's summary when omitted.
    #[serde(default)]
    pub summary: Option<String>,
    /// Tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Display name; a generated handle when omitted.
    #[serde(default)]
    pub author_name: Option<String>,
    /// `public` (default) or `private`. Private hides the item from the
    /// catalog and from strangers; the author, and anyone who already
    /// installed a copy, keep access.
    #[serde(default)]
    pub visibility: Option<String>,
}

/// `POST /community/indicators`
pub async fn publish(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<PublishBody>,
) -> Result<(StatusCode, Json<CommunityDetail>), ApiError> {
    let database = database(&state)?;
    let tier = publish_tier(&state, user.user_id).await?;
    let workspace = db::get_indicator_workspace(database.pool(), user.user_id, body.workspace_id)
        .await?
        .ok_or_else(|| ApiError::not_found("indicator workspace not found"))?;
    let revision_id = body
        .revision_id
        .or(workspace.active_revision_id)
        .ok_or_else(|| {
            conflict(
                "COMMUNITY_REVISION_NOT_VALIDATED",
                "the workspace has no revision to publish",
            )
        })?;
    let revision =
        db::get_indicator_revision(database.pool(), user.user_id, workspace.id, revision_id)
            .await?
            .ok_or_else(|| ApiError::not_found("indicator revision not found"))?;
    if revision.status != "validated" {
        return Err(conflict(
            "COMMUNITY_REVISION_NOT_VALIDATED",
            "only a validated revision can be published",
        ));
    }
    if revision.source.len() > MAX_COMMUNITY_SOURCE_BYTES {
        return Err(source_invalid(vec![format!(
            "the source is {} bytes; the community limit is {MAX_COMMUNITY_SOURCE_BYTES}",
            revision.source.len()
        )]));
    }
    revet(
        &revision.source,
        &revision.validation,
        &revision.preview,
    )?;

    let title = clean_title(&body.title)?;
    let summary = clean_summary(body.summary.as_deref().unwrap_or(revision.summary.as_str()))?;
    let tags = clean_tags(&body.tags)?;
    let author_name = clean_author_name(body.author_name.as_deref(), user.user_id)?;
    let status = match body.visibility.as_deref() {
        None | Some("public") => "listed",
        Some("private") => "unlisted",
        Some(_) => return Err(metadata("visibility is `public` or `private`")),
    };

    let published = db::count_author_indicators(database.pool(), user.user_id).await?;
    if published >= MAX_PUBLISHED_INDICATORS {
        return Err(conflict(
            "COMMUNITY_PUBLISH_LIMIT_REACHED",
            format!("this account already has {MAX_PUBLISHED_INDICATORS} published indicators"),
        ));
    }
    let item = db::create_community_indicator(
        database.pool(),
        user.user_id,
        &slug_for(&title),
        &db::NewCommunityItem {
            title,
            summary,
            tags,
            author_name,
            author_tier: tier.to_string(),
            status: status.to_string(),
            version: db::NewCommunityVersion {
                source: revision.source.clone(),
                summary: revision.summary.clone(),
                symbol: workspace.symbol.clone(),
                timeframe: workspace.timeframe.clone(),
                validation: revision.validation.clone(),
                preview: revision.preview.clone(),
            },
        },
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(detail(&database, user.user_id, &item).await?),
    ))
}

/// `POST /community/indicators/{slug}/versions`'s body.
#[derive(Debug, Deserialize)]
pub struct VersionBody {
    /// The workspace that owns the revision.
    pub workspace_id: Uuid,
    /// The revision to publish; the active one when omitted.
    #[serde(default)]
    pub revision_id: Option<Uuid>,
    /// Short description; the revision's summary when omitted.
    #[serde(default)]
    pub summary: Option<String>,
}

/// `POST /community/indicators/{slug}/versions`
pub async fn add_version(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
    ApiJson(body): ApiJson<VersionBody>,
) -> Result<(StatusCode, Json<CommunityDetail>), ApiError> {
    let database = database(&state)?;
    let item = db::get_community_indicator(database.pool(), &slug)
        .await?
        .ok_or_else(unknown)?;
    if !item.is_author(user.user_id) {
        return Err(unknown());
    }
    if item.status == "removed" {
        return Err(conflict(
            "COMMUNITY_NOT_LISTED",
            "a removed indicator cannot gain versions",
        ));
    }
    let count = db::list_community_versions(
        database.pool(),
        item.id,
        MAX_VERSIONS_PER_INDICATOR + 1,
    )
    .await?
    .len() as i64;
    if count >= MAX_VERSIONS_PER_INDICATOR {
        return Err(conflict(
            "COMMUNITY_VERSION_LIMIT_REACHED",
            format!("an indicator keeps at most {MAX_VERSIONS_PER_INDICATOR} versions"),
        ));
    }
    let workspace = db::get_indicator_workspace(database.pool(), user.user_id, body.workspace_id)
        .await?
        .ok_or_else(|| ApiError::not_found("indicator workspace not found"))?;
    let revision_id = body.revision_id.or(workspace.active_revision_id).ok_or_else(|| {
        conflict(
            "COMMUNITY_REVISION_NOT_VALIDATED",
            "the workspace has no revision to publish",
        )
    })?;
    let revision =
        db::get_indicator_revision(database.pool(), user.user_id, workspace.id, revision_id)
            .await?
            .ok_or_else(|| ApiError::not_found("indicator revision not found"))?;
    if revision.status != "validated" {
        return Err(conflict(
            "COMMUNITY_REVISION_NOT_VALIDATED",
            "only a validated revision can be published",
        ));
    }
    if revision.source.len() > MAX_COMMUNITY_SOURCE_BYTES {
        return Err(source_invalid(vec![format!(
            "the source exceeds {MAX_COMMUNITY_SOURCE_BYTES} bytes"
        )]));
    }
    revet(
        &revision.source,
        &revision.validation,
        &revision.preview,
    )?;
    let summary = clean_summary(body.summary.as_deref().unwrap_or(revision.summary.as_str()))?;
    db::create_community_version(
        database.pool(),
        user.user_id,
        item.id,
        &db::NewCommunityVersion {
            source: revision.source.clone(),
            summary,
            symbol: workspace.symbol.clone(),
            timeframe: workspace.timeframe.clone(),
            validation: revision.validation.clone(),
            preview: revision.preview.clone(),
        },
    )
    .await?
    .ok_or_else(unknown)?;
    let refreshed = db::get_community_indicator_by_id(database.pool(), item.id)
        .await?
        .ok_or_else(unknown)?;
    Ok((
        StatusCode::CREATED,
        Json(detail(&database, user.user_id, &refreshed).await?),
    ))
}

/// `PATCH /community/indicators/{slug}`'s body.
#[derive(Debug, Deserialize)]
pub struct PatchBody {
    /// New title.
    #[serde(default)]
    pub title: Option<String>,
    /// New summary.
    #[serde(default)]
    pub summary: Option<String>,
    /// New tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// `listed` or `unlisted`.
    #[serde(default)]
    pub status: Option<String>,
}

/// `PATCH /community/indicators/{slug}`
pub async fn patch(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
    ApiJson(body): ApiJson<PatchBody>,
) -> Result<Json<CommunityDetail>, ApiError> {
    let database = database(&state)?;
    let item = db::get_community_indicator(database.pool(), &slug)
        .await?
        .ok_or_else(unknown)?;
    if !item.is_author(user.user_id) {
        return Err(unknown());
    }
    if item.status == "removed" {
        return Err(conflict(
            "COMMUNITY_NOT_LISTED",
            "a removed indicator cannot be edited",
        ));
    }
    let title = match body.title {
        Some(value) => clean_title(&value)?,
        None => item.title.clone(),
    };
    let summary = match body.summary {
        Some(value) => clean_summary(&value)?,
        None => item.summary.clone(),
    };
    let tags = match body.tags {
        Some(value) => clean_tags(&value)?,
        None => item.tags.clone(),
    };
    let status = match body.status.as_deref() {
        None => item.status.clone(),
        Some("listed") => "listed".to_string(),
        Some("unlisted") => "unlisted".to_string(),
        Some(_) => return Err(metadata("status is `listed` or `unlisted`")),
    };
    let updated = db::update_community_indicator(
        database.pool(),
        user.user_id,
        item.id,
        &title,
        &summary,
        &tags,
        &status,
    )
    .await?
    .ok_or_else(unknown)?;
    Ok(Json(detail(&database, user.user_id, &updated).await?))
}

/// `POST /community/indicators/{slug}/install`
pub async fn install(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let database = database(&state)?;
    let item = db::get_community_indicator(database.pool(), &slug)
        .await?
        .ok_or_else(unknown)?;
    visibility(&item, user.user_id)?;
    let version_id = item.current_version_id.ok_or_else(unknown)?;
    let version = db::get_community_version(database.pool(), version_id)
        .await?
        .ok_or_else(unknown)?;
    revet(&version.source, &version.validation, &version.preview)?;
    let installs = db::count_community_installs(database.pool(), user.user_id).await?;
    if installs >= MAX_INSTALLS_PER_USER {
        return Err(conflict(
            "COMMUNITY_INSTALL_LIMIT_REACHED",
            format!("this account keeps at most {MAX_INSTALLS_PER_USER} installed indicators"),
        ));
    }
    let provenance = format!(
        "Installed {}'s {} v{}",
        item.author_name, item.title, version.version_number
    );
    let memory = serde_json::json!({
        "community": {
            "indicator_id": item.id,
            "slug": item.slug,
            "version": version.version_number,
            "author_name": item.author_name,
        }
    });
    match db::install_community_indicator(
        database.pool(),
        user.user_id,
        item.id,
        &version,
        &item.title,
        &provenance,
        &memory,
    )
    .await?
    {
        db::InstallOutcome::Installed {
            install,
            revision_id,
        } => Ok(Json(serde_json::json!({
            "workspace_id": install.workspace_id,
            "revision_id": revision_id,
            "version": version.version_number,
            "already_installed": false,
        }))),
        db::InstallOutcome::AlreadyInstalled(install) => Ok(Json(serde_json::json!({
            "workspace_id": install.workspace_id,
            "revision_id": serde_json::Value::Null,
            "version": version.version_number,
            "already_installed": true,
        }))),
        db::InstallOutcome::NotListed => Err(conflict(
            "COMMUNITY_NOT_LISTED",
            "this indicator is not listed",
        )),
    }
}

/// `POST /community/indicators/{slug}/update`
pub async fn update(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let database = database(&state)?;
    let item = db::get_community_indicator(database.pool(), &slug)
        .await?
        .ok_or_else(unknown)?;
    // An unlisted item stays reachable for the people who already have a
    // stake in it -- its author and its installers -- so they can relist or
    // update; everybody else is told it does not exist.
    let install = db::get_community_install(database.pool(), user.user_id, item.id).await?;
    if !item.is_listed() && install.is_none() && !item.is_author(user.user_id) {
        return Err(unknown());
    }
    let version_id = item.current_version_id.ok_or_else(unknown)?;
    let version = db::get_community_version(database.pool(), version_id)
        .await?
        .ok_or_else(unknown)?;
    revet(&version.source, &version.validation, &version.preview)?;
    let provenance = format!(
        "Updated to {}'s {} v{}",
        item.author_name, item.title, version.version_number
    );
    match db::update_community_install(database.pool(), user.user_id, item.id, &version, &provenance)
        .await?
    {
        db::UpdateOutcome::Updated {
            workspace_id,
            revision_id,
        } => Ok(Json(serde_json::json!({
            "workspace_id": workspace_id,
            "revision_id": revision_id,
            "version": version.version_number,
        }))),
        db::UpdateOutcome::NotInstalled => Err(ApiError::coded(
            StatusCode::CONFLICT,
            "COMMUNITY_NOT_INSTALLED",
            "install this indicator before updating it",
        )),
        db::UpdateOutcome::AlreadyCurrent => Err(conflict(
            "COMMUNITY_ALREADY_CURRENT",
            "this install already has the current version",
        )),
        db::UpdateOutcome::NotListed => Err(conflict(
            "COMMUNITY_NOT_LISTED",
            "this indicator is not listed",
        )),
    }
}

/// `POST /community/indicators/{slug}/report`'s body.
#[derive(Debug, Deserialize)]
pub struct ReportBody {
    /// `spam`, `misleading`, `malicious`, `copyright` or `other`.
    pub reason: String,
    /// Optional free-text detail.
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /community/indicators/{slug}/report`
pub async fn report(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
    ApiJson(body): ApiJson<ReportBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let database = database(&state)?;
    let item = db::get_community_indicator(database.pool(), &slug)
        .await?
        .ok_or_else(unknown)?;
    visibility(&item, user.user_id)?;
    let reason = body.reason.trim().to_ascii_lowercase();
    if !matches!(
        reason.as_str(),
        "spam" | "misleading" | "malicious" | "copyright" | "other"
    ) {
        return Err(ApiError::bad_request(
            "COMMUNITY_METADATA_INVALID",
            "reason is spam, misleading, malicious, copyright or other",
        ));
    }
    let note = body
        .note
        .as_deref()
        .map(str::trim)
        .filter(|note| !note.is_empty());
    if note.is_some_and(|note| note.chars().count() > MAX_REPORT_NOTE_CHARS) {
        return Err(ApiError::bad_request(
            "COMMUNITY_METADATA_INVALID",
            format!("a report note is at most {MAX_REPORT_NOTE_CHARS} characters"),
        ));
    }
    let new =
        db::report_community_indicator(database.pool(), user.user_id, item.id, &reason, note)
            .await?;
    Ok(Json(serde_json::json!({
        "reported": true,
        "already_reported": !new,
    })))
}
