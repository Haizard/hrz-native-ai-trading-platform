//! Community indicators (`docs/53`): catalog rows, immutable versions,
//! install pins and abuse reports.
//!
//! ## Versions are immutable; metadata is not
//!
//! A version is the artifact an installer receives and never changes. Title,
//! summary, tags and listing status live on the catalog row. `current_version_id`
//! is the version an install receives today.
//!
//! ## Install is a fork, in one transaction
//!
//! Installing copies the pinned version into a new workspace owned by the
//! installer, then records the install. A failure must not leave an empty
//! workspace or a pin with no revision, so all of it shares one transaction.

use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::dt_to_ns;

/// One catalog row.
#[derive(Debug, Clone, PartialEq)]
pub struct CommunityIndicatorRow {
    /// Row id.
    pub id: Uuid,
    /// The publisher.
    pub author_user_id: Uuid,
    /// Immutable URL name.
    pub slug: String,
    /// Display title.
    pub title: String,
    /// Short description.
    pub summary: String,
    /// Normalized tags.
    pub tags: Vec<String>,
    /// Display name at publish time.
    pub author_name: String,
    /// `publish` or `priority`, a snapshot of the publisher's tier.
    pub author_tier: String,
    /// `listed`, `unlisted` or `removed`.
    pub status: String,
    /// The version installs receive today.
    pub current_version_id: Option<Uuid>,
    /// Cumulative installs.
    pub install_count: i64,
    /// Reports received.
    pub report_count: i32,
    /// Unix nanoseconds.
    pub created_at: i64,
    /// Unix nanoseconds.
    pub updated_at: i64,
}

impl CommunityIndicatorRow {
    /// Whether the public catalog may show this row.
    #[must_use]
    pub fn is_listed(&self) -> bool {
        self.status == "listed"
    }

    /// Whether this user may see the row even when unlisted.
    #[must_use]
    pub fn is_author(&self, user_id: Uuid) -> bool {
        self.author_user_id == user_id
    }
}

/// One immutable published version.
#[derive(Debug, Clone, PartialEq)]
pub struct CommunityVersionRow {
    /// Row id.
    pub id: Uuid,
    /// Owning catalog row.
    pub indicator_id: Uuid,
    /// 1-based, monotonic per indicator.
    pub version_number: i32,
    /// The validated source.
    pub source: String,
    /// Plain-language summary at publish time.
    pub summary: String,
    /// Workspace symbol at publish time.
    pub symbol: String,
    /// Workspace timeframe at publish time.
    pub timeframe: String,
    /// Validator report copied from the revision.
    pub validation: Value,
    /// Chart output preview copied from the revision.
    pub preview: Value,
    /// Unix nanoseconds.
    pub published_at: i64,
}

/// One install pin.
#[derive(Debug, Clone, PartialEq)]
pub struct CommunityInstallRow {
    /// Row id.
    pub id: Uuid,
    /// Catalog row.
    pub indicator_id: Uuid,
    /// Installer.
    pub user_id: Uuid,
    /// The owned workspace the fork created.
    pub workspace_id: Uuid,
    /// The published version currently applied.
    pub version_id: Uuid,
    /// Unix nanoseconds.
    pub installed_at: i64,
    /// Unix nanoseconds.
    pub updated_at: i64,
}

/// A catalog row plus its current version number, for list responses.
#[derive(Debug, Clone, PartialEq)]
pub struct CommunityListItem {
    /// The catalog row.
    pub indicator: CommunityIndicatorRow,
    /// `0` only if `current_version_id` is unexpectedly absent.
    pub version_number: i32,
}

/// Everything needed to publish a first version.
#[derive(Debug, Clone, PartialEq)]
pub struct NewCommunityItem {
    /// Display title, already validated.
    pub title: String,
    /// Short description.
    pub summary: String,
    /// Normalized tags.
    pub tags: Vec<String>,
    /// Display name.
    pub author_name: String,
    /// `publish` or `priority`.
    pub author_tier: String,
    /// `listed` (public) or `unlisted` (private), chosen at publish time.
    pub status: String,
    /// The version payload.
    pub version: NewCommunityVersion,
}

/// One version's immutable payload.
#[derive(Debug, Clone, PartialEq)]
pub struct NewCommunityVersion {
    /// The validated source.
    pub source: String,
    /// Plain-language summary.
    pub summary: String,
    /// Workspace symbol.
    pub symbol: String,
    /// Workspace timeframe.
    pub timeframe: String,
    /// Validator report.
    pub validation: Value,
    /// Chart output preview.
    pub preview: Value,
}

/// What an install attempt did.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallOutcome {
    /// A new workspace and revision were created.
    Installed {
        /// The install row.
        install: CommunityInstallRow,
        /// The workspace's first revision.
        revision_id: Uuid,
    },
    /// The caller already owns an install; nothing was created.
    AlreadyInstalled(CommunityInstallRow),
    /// The item is not listed and the caller is not its author.
    NotListed,
}

/// What an update attempt did.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateOutcome {
    /// A revision was appended and the pin moved.
    Updated {
        /// The install's workspace.
        workspace_id: Uuid,
        /// The new revision.
        revision_id: Uuid,
    },
    /// The caller has no install.
    NotInstalled,
    /// The pin already points at this version.
    AlreadyCurrent,
    /// The item is no longer listed.
    NotListed,
}

fn indicator_from(row: &sqlx::postgres::PgRow) -> Result<CommunityIndicatorRow, sqlx::Error> {
    Ok(CommunityIndicatorRow {
        id: row.try_get("id")?,
        author_user_id: row.try_get("author_user_id")?,
        slug: row.try_get("slug")?,
        title: row.try_get("title")?,
        summary: row.try_get("summary")?,
        tags: row.try_get("tags")?,
        author_name: row.try_get("author_name")?,
        author_tier: row.try_get("author_tier")?,
        status: row.try_get("status")?,
        current_version_id: row.try_get("current_version_id")?,
        install_count: row.try_get("install_count")?,
        report_count: row.try_get("report_count")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
        updated_at: dt_to_ns(row.try_get("updated_at")?),
    })
}

fn version_from(row: &sqlx::postgres::PgRow) -> Result<CommunityVersionRow, sqlx::Error> {
    Ok(CommunityVersionRow {
        id: row.try_get("id")?,
        indicator_id: row.try_get("indicator_id")?,
        version_number: row.try_get("version_number")?,
        source: row.try_get("source")?,
        summary: row.try_get("summary")?,
        symbol: row.try_get("symbol")?,
        timeframe: row.try_get("timeframe")?,
        validation: row.try_get("validation")?,
        preview: row.try_get("preview")?,
        published_at: dt_to_ns(row.try_get("published_at")?),
    })
}

fn install_from(row: &sqlx::postgres::PgRow) -> Result<CommunityInstallRow, sqlx::Error> {
    Ok(CommunityInstallRow {
        id: row.try_get("id")?,
        indicator_id: row.try_get("indicator_id")?,
        user_id: row.try_get("user_id")?,
        workspace_id: row.try_get("workspace_id")?,
        version_id: row.try_get("version_id")?,
        installed_at: dt_to_ns(row.try_get("installed_at")?),
        updated_at: dt_to_ns(row.try_get("updated_at")?),
    })
}

const INDICATOR_COLUMNS: &str = "i.id, i.author_user_id, i.slug, i.title, i.summary, i.tags, \
     i.author_name, i.author_tier, i.status, i.current_version_id, i.install_count, \
     i.report_count, i.created_at, i.updated_at";

/// The public catalog, filtered and ordered.
///
/// # Errors
/// Returns [`DbError::Pool`] when the catalog cannot be read.
pub async fn list_community_indicators(
    pool: &PgPool,
    q: Option<&str>,
    tag: Option<&str>,
    sort: &str,
    limit: i64,
    offset: i64,
) -> Result<(Vec<CommunityListItem>, i64), DbError> {
    let order = match sort {
        "new" => "i.created_at DESC",
        "popular" => "i.install_count DESC, i.created_at DESC",
        _ => "(i.author_tier = 'priority') DESC, i.install_count DESC, i.created_at DESC",
    };
    let sql = format!(
        "SELECT {INDICATOR_COLUMNS}, COALESCE(v.version_number, 0) AS version_number \
           FROM community_indicators i \
           LEFT JOIN community_indicator_versions v ON v.id = i.current_version_id \
          WHERE i.status = 'listed' \
            AND ($1::text IS NULL OR i.title ILIKE '%' || $1 || '%' OR i.summary ILIKE '%' || $1 || '%') \
            AND ($2::text IS NULL OR $2 = ANY(i.tags)) \
          ORDER BY {order} \
          LIMIT $3 OFFSET $4"
    );
    let rows = sqlx::query(&sql)
        .bind(q)
        .bind(tag)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    let items = rows
        .iter()
        .map(|row| {
            Ok(CommunityListItem {
                indicator: indicator_from(row)?,
                version_number: row.try_get("version_number")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM community_indicators i \
          WHERE i.status = 'listed' \
            AND ($1::text IS NULL OR i.title ILIKE '%' || $1 || '%' OR i.summary ILIKE '%' || $1 || '%') \
            AND ($2::text IS NULL OR $2 = ANY(i.tags))",
    )
    .bind(q)
    .bind(tag)
    .fetch_one(pool)
    .await?;
    Ok((items, total))
}

/// Everything this author has published, including unlisted and removed rows.
///
/// # Errors
/// Returns [`DbError::Pool`] when the rows cannot be read.
pub async fn list_author_indicators(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<CommunityListItem>, DbError> {
    let sql = format!(
        "SELECT {INDICATOR_COLUMNS}, COALESCE(v.version_number, 0) AS version_number \
           FROM community_indicators i \
           LEFT JOIN community_indicator_versions v ON v.id = i.current_version_id \
          WHERE i.author_user_id = $1 \
          ORDER BY i.created_at DESC"
    );
    let rows = sqlx::query(&sql).bind(user_id).fetch_all(pool).await?;
    rows.iter()
        .map(|row| {
            Ok(CommunityListItem {
                indicator: indicator_from(row)?,
                version_number: row.try_get("version_number")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

/// The caller's installs, with the catalog row and pinned version number.
///
/// # Errors
/// Returns [`DbError::Pool`] when the rows cannot be read.
pub async fn list_community_installs(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<(CommunityIndicatorRow, CommunityInstallRow, i32)>, DbError> {
    let sql = format!(
        "SELECT {INDICATOR_COLUMNS}, \
                x.id AS install_id, x.indicator_id AS install_indicator_id, \
                x.user_id AS install_user_id, x.workspace_id, x.version_id, \
                x.installed_at, x.updated_at, v.version_number AS install_version_number \
           FROM community_indicator_installs x \
           JOIN community_indicators i ON i.id = x.indicator_id \
           JOIN community_indicator_versions v ON v.id = x.version_id \
          WHERE x.user_id = $1 \
          ORDER BY x.updated_at DESC"
    );
    let rows = sqlx::query(&sql).bind(user_id).fetch_all(pool).await?;
    rows.iter()
        .map(|row| {
            let install = CommunityInstallRow {
                id: row.try_get("install_id")?,
                indicator_id: row.try_get("install_indicator_id")?,
                user_id: row.try_get("install_user_id")?,
                workspace_id: row.try_get("workspace_id")?,
                version_id: row.try_get("version_id")?,
                installed_at: dt_to_ns(row.try_get("installed_at")?),
                updated_at: dt_to_ns(row.try_get("updated_at")?),
            };
            Ok((
                indicator_from(row)?,
                install,
                row.try_get("install_version_number")?,
            ))
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

/// One catalog row by slug, in any status.
///
/// # Errors
/// Returns [`DbError::Pool`] when the row cannot be read.
pub async fn get_community_indicator(
    pool: &PgPool,
    slug: &str,
) -> Result<Option<CommunityIndicatorRow>, DbError> {
    let sql = format!("SELECT {INDICATOR_COLUMNS} FROM community_indicators i WHERE i.slug = $1");
    let row = sqlx::query(&sql).bind(slug).fetch_optional(pool).await?;
    row.as_ref()
        .map(indicator_from)
        .transpose()
        .map_err(Into::into)
}

/// One catalog row by id.
///
/// # Errors
/// Returns [`DbError::Pool`] when the row cannot be read.
pub async fn get_community_indicator_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<CommunityIndicatorRow>, DbError> {
    let sql = format!("SELECT {INDICATOR_COLUMNS} FROM community_indicators i WHERE i.id = $1");
    let row = sqlx::query(&sql).bind(id).fetch_optional(pool).await?;
    row.as_ref()
        .map(indicator_from)
        .transpose()
        .map_err(Into::into)
}

/// One version by id.
///
/// # Errors
/// Returns [`DbError::Pool`] when the row cannot be read.
pub async fn get_community_version(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<CommunityVersionRow>, DbError> {
    let row = sqlx::query(
        "SELECT id, indicator_id, version_number, source, summary, symbol, timeframe, \
                validation, preview, published_at \
           FROM community_indicator_versions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.as_ref()
        .map(version_from)
        .transpose()
        .map_err(Into::into)
}

/// Versions of one indicator, newest first.
///
/// # Errors
/// Returns [`DbError::Pool`] when the rows cannot be read.
pub async fn list_community_versions(
    pool: &PgPool,
    indicator_id: Uuid,
    limit: i64,
) -> Result<Vec<CommunityVersionRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, indicator_id, version_number, source, summary, symbol, timeframe, \
                validation, preview, published_at \
           FROM community_indicator_versions WHERE indicator_id = $1 \
          ORDER BY version_number DESC LIMIT $2",
    )
    .bind(indicator_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(version_from)
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

/// The caller's pin on one item.
///
/// # Errors
/// Returns [`DbError::Pool`] when the row cannot be read.
pub async fn get_community_install(
    pool: &PgPool,
    user_id: Uuid,
    indicator_id: Uuid,
) -> Result<Option<CommunityInstallRow>, DbError> {
    let row = sqlx::query(
        "SELECT id, indicator_id, user_id, workspace_id, version_id, installed_at, updated_at \
           FROM community_indicator_installs WHERE user_id = $1 AND indicator_id = $2",
    )
    .bind(user_id)
    .bind(indicator_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref()
        .map(install_from)
        .transpose()
        .map_err(Into::into)
}

/// How many items this author has ever published.
///
/// # Errors
/// Returns [`DbError::Pool`] when the count cannot be read.
pub async fn count_author_indicators(pool: &PgPool, user_id: Uuid) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM community_indicators WHERE author_user_id = $1",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?)
}

/// How many items this user has installed.
///
/// # Errors
/// Returns [`DbError::Pool`] when the count cannot be read.
pub async fn count_community_installs(pool: &PgPool, user_id: Uuid) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM community_indicator_installs WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?)
}

/// Insert a catalog row and its first version atomically.
///
/// # Errors
/// Returns [`DbError::Pool`] when either insert or the commit fails.
pub async fn create_community_indicator(
    pool: &PgPool,
    author_user_id: Uuid,
    slug: &str,
    new: &NewCommunityItem,
) -> Result<CommunityIndicatorRow, DbError> {
    let mut tx = pool.begin().await?;
    let columns = "id, author_user_id, slug, title, summary, tags, author_name, author_tier, \
                   status, current_version_id, install_count, report_count, created_at, updated_at";
    let row = sqlx::query(&format!(
        "INSERT INTO community_indicators \
             (author_user_id, slug, title, summary, tags, author_name, author_tier, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {columns}"
    ))
    .bind(author_user_id)
    .bind(slug)
    .bind(&new.title)
    .bind(&new.summary)
    .bind(&new.tags)
    .bind(&new.author_name)
    .bind(&new.author_tier)
    .bind(&new.status)
    .fetch_one(&mut *tx)
    .await?;
    let indicator = indicator_from(&row)?;
    let version = sqlx::query(
        "INSERT INTO community_indicator_versions \
             (indicator_id, version_number, source, summary, symbol, timeframe, validation, preview) \
         VALUES ($1, 1, $2, $3, $4, $5, $6, $7) \
         RETURNING id, indicator_id, version_number, source, summary, symbol, timeframe, \
                   validation, preview, published_at",
    )
    .bind(indicator.id)
    .bind(&new.version.source)
    .bind(&new.version.summary)
    .bind(&new.version.symbol)
    .bind(&new.version.timeframe)
    .bind(&new.version.validation)
    .bind(&new.version.preview)
    .fetch_one(&mut *tx)
    .await?;
    let version_id: Uuid = version.try_get("id")?;
    let row = sqlx::query(&format!(
        "UPDATE community_indicators SET current_version_id = $2, updated_at = now() \
          WHERE id = $1 RETURNING {columns}"
    ))
    .bind(indicator.id)
    .bind(version_id)
    .fetch_one(&mut *tx)
    .await?;
    let out = indicator_from(&row)?;
    tx.commit().await?;
    Ok(out)
}

/// Append a version when the caller owns the item and it is not removed.
///
/// # Errors
/// Returns [`DbError::Pool`] when the transaction fails.
pub async fn create_community_version(
    pool: &PgPool,
    author_user_id: Uuid,
    indicator_id: Uuid,
    new: &NewCommunityVersion,
) -> Result<Option<CommunityVersionRow>, DbError> {
    let mut tx = pool.begin().await?;
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM community_indicators \
          WHERE id = $1 AND author_user_id = $2 FOR UPDATE",
    )
    .bind(indicator_id)
    .bind(author_user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(status) = status else {
        tx.rollback().await?;
        return Ok(None);
    };
    if status == "removed" {
        tx.rollback().await?;
        return Ok(None);
    }
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version_number), 0) + 1 \
           FROM community_indicator_versions WHERE indicator_id = $1",
    )
    .bind(indicator_id)
    .fetch_one(&mut *tx)
    .await?;
    let row = sqlx::query(
        "INSERT INTO community_indicator_versions \
             (indicator_id, version_number, source, summary, symbol, timeframe, validation, preview) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id, indicator_id, version_number, source, summary, symbol, timeframe, \
                   validation, preview, published_at",
    )
    .bind(indicator_id)
    .bind(next)
    .bind(&new.source)
    .bind(&new.summary)
    .bind(&new.symbol)
    .bind(&new.timeframe)
    .bind(&new.validation)
    .bind(&new.preview)
    .fetch_one(&mut *tx)
    .await?;
    let version = version_from(&row)?;
    sqlx::query(
        "UPDATE community_indicators \
            SET current_version_id = $2, updated_at = now() WHERE id = $1",
    )
    .bind(indicator_id)
    .bind(version.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(version))
}

/// Update the metadata of an item the caller owns.
///
/// # Errors
/// Returns [`DbError::Pool`] when the row cannot be updated.
pub async fn update_community_indicator(
    pool: &PgPool,
    author_user_id: Uuid,
    indicator_id: Uuid,
    title: &str,
    summary: &str,
    tags: &[String],
    status: &str,
) -> Result<Option<CommunityIndicatorRow>, DbError> {
    let columns = "id, author_user_id, slug, title, summary, tags, author_name, author_tier, \
                   status, current_version_id, install_count, report_count, created_at, updated_at";
    let row = sqlx::query(&format!(
        "UPDATE community_indicators \
            SET title = $3, summary = $4, tags = $5, status = $6, updated_at = now() \
          WHERE id = $1 AND author_user_id = $2 RETURNING {columns}"
    ))
    .bind(indicator_id)
    .bind(author_user_id)
    .bind(title)
    .bind(summary)
    .bind(tags)
    .bind(status)
    .fetch_optional(pool)
    .await?;
    row.as_ref()
        .map(indicator_from)
        .transpose()
        .map_err(Into::into)
}

/// Fork the pinned version into a new owned workspace.
///
/// # Errors
/// Returns [`DbError::Pool`] when the transaction fails.
#[allow(clippy::too_many_arguments)] // the fork's inputs, in order; an argument struct would not change the SQL
pub async fn install_community_indicator(
    pool: &PgPool,
    installer: Uuid,
    indicator_id: Uuid,
    version: &CommunityVersionRow,
    workspace_name: &str,
    provenance: &str,
    memory: &Value,
) -> Result<InstallOutcome, DbError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT author_user_id, status FROM community_indicators WHERE id = $1 FOR UPDATE",
    )
    .bind(indicator_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Ok(InstallOutcome::NotListed);
    };
    let author: Uuid = row.try_get("author_user_id")?;
    let status: String = row.try_get("status")?;
    if status != "listed" && author != installer {
        tx.rollback().await?;
        return Ok(InstallOutcome::NotListed);
    }
    let existing = sqlx::query(
        "SELECT id, indicator_id, user_id, workspace_id, version_id, installed_at, updated_at \
           FROM community_indicator_installs \
          WHERE user_id = $1 AND indicator_id = $2 FOR UPDATE",
    )
    .bind(installer)
    .bind(indicator_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = existing {
        tx.rollback().await?;
        return Ok(InstallOutcome::AlreadyInstalled(install_from(&row)?));
    }

    let workspace_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO indicator_workspaces (id, user_id, name, symbol, timeframe, memory) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(workspace_id)
    .bind(installer)
    .bind(workspace_name)
    .bind(&version.symbol)
    .bind(&version.timeframe)
    .bind(memory)
    .execute(&mut *tx)
    .await?;

    let revision_id = Uuid::new_v4();
    let mut preview = version.preview.clone();
    if let Some(object) = preview.as_object_mut() {
        object.insert(
            "revision_id".to_string(),
            Value::String(revision_id.to_string()),
        );
    }
    sqlx::query(
        "INSERT INTO indicator_revisions \
             (id, workspace_id, parent_revision_id, revision_number, source, summary, \
              change_summary, validation, preview, status) \
         VALUES ($1, $2, NULL, 1, $3, $4, $5, $6, $7, 'validated')",
    )
    .bind(revision_id)
    .bind(workspace_id)
    .bind(&version.source)
    .bind(&version.summary)
    .bind(provenance)
    .bind(&version.validation)
    .bind(&preview)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE indicator_workspaces SET active_revision_id = $2, updated_at = now() WHERE id = $1",
    )
    .bind(workspace_id)
    .bind(revision_id)
    .execute(&mut *tx)
    .await?;

    let row = sqlx::query(
        "INSERT INTO community_indicator_installs \
             (indicator_id, user_id, workspace_id, version_id) VALUES ($1, $2, $3, $4) \
         RETURNING id, indicator_id, user_id, workspace_id, version_id, installed_at, updated_at",
    )
    .bind(indicator_id)
    .bind(installer)
    .bind(workspace_id)
    .bind(version.id)
    .fetch_one(&mut *tx)
    .await?;
    let install = install_from(&row)?;
    sqlx::query(
        "UPDATE community_indicators SET install_count = install_count + 1, updated_at = now() \
          WHERE id = $1",
    )
    .bind(indicator_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(InstallOutcome::Installed {
        install,
        revision_id,
    })
}

/// Append the current version to an installed workspace.
///
/// # Errors
/// Returns [`DbError::Pool`] when the transaction fails.
pub async fn update_community_install(
    pool: &PgPool,
    installer: Uuid,
    indicator_id: Uuid,
    version: &CommunityVersionRow,
    provenance: &str,
) -> Result<UpdateOutcome, DbError> {
    let mut tx = pool.begin().await?;
    let status: String =
        sqlx::query_scalar("SELECT status FROM community_indicators WHERE id = $1 FOR UPDATE")
            .bind(indicator_id)
            .fetch_one(&mut *tx)
            .await?;
    if status != "listed" {
        tx.rollback().await?;
        return Ok(UpdateOutcome::NotListed);
    }
    let row = sqlx::query(
        "SELECT id, indicator_id, user_id, workspace_id, version_id, installed_at, updated_at \
           FROM community_indicator_installs \
          WHERE user_id = $1 AND indicator_id = $2 FOR UPDATE",
    )
    .bind(installer)
    .bind(indicator_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Ok(UpdateOutcome::NotInstalled);
    };
    let install = install_from(&row)?;
    if install.version_id == version.id {
        tx.rollback().await?;
        return Ok(UpdateOutcome::AlreadyCurrent);
    }
    let active: Option<Uuid> = sqlx::query_scalar(
        "SELECT active_revision_id FROM indicator_workspaces \
          WHERE id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(install.workspace_id)
    .bind(installer)
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(revision_number), 0) + 1 \
           FROM indicator_revisions WHERE workspace_id = $1",
    )
    .bind(install.workspace_id)
    .fetch_one(&mut *tx)
    .await?;
    let revision_id = Uuid::new_v4();
    let mut preview = version.preview.clone();
    if let Some(object) = preview.as_object_mut() {
        object.insert(
            "revision_id".to_string(),
            Value::String(revision_id.to_string()),
        );
    }
    sqlx::query(
        "INSERT INTO indicator_revisions \
             (id, workspace_id, parent_revision_id, revision_number, source, summary, \
              change_summary, validation, preview, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'validated')",
    )
    .bind(revision_id)
    .bind(install.workspace_id)
    .bind(active)
    .bind(next)
    .bind(&version.source)
    .bind(&version.summary)
    .bind(provenance)
    .bind(&version.validation)
    .bind(&preview)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE indicator_workspaces SET active_revision_id = $2, updated_at = now() WHERE id = $1",
    )
    .bind(install.workspace_id)
    .bind(revision_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE community_indicator_installs SET version_id = $2, updated_at = now() \
          WHERE id = $1",
    )
    .bind(install.id)
    .bind(version.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE community_indicators SET updated_at = now() WHERE id = $1")
        .bind(indicator_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(UpdateOutcome::Updated {
        workspace_id: install.workspace_id,
        revision_id,
    })
}

/// Record a report once per reporter; return whether it was new.
///
/// # Errors
/// Returns [`DbError::Pool`] when the transaction fails.
pub async fn report_community_indicator(
    pool: &PgPool,
    reporter: Uuid,
    indicator_id: Uuid,
    reason: &str,
    note: Option<&str>,
) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO community_indicator_reports (indicator_id, reporter_user_id, reason, note) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (indicator_id, reporter_user_id) DO NOTHING \
         RETURNING id",
    )
    .bind(indicator_id)
    .bind(reporter)
    .bind(reason)
    .bind(note)
    .fetch_optional(&mut *tx)
    .await?;
    if inserted.is_some() {
        sqlx::query(
            "UPDATE community_indicators SET report_count = report_count + 1 WHERE id = $1",
        )
        .bind(indicator_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(inserted.is_some())
}
