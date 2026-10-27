# Community Indicators + Workspace Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the subscription matrix's community-indicator tiers real (publish, browse, install, update, report), then stop a page refresh from discarding the user's workspace (panes, series, indicators, active tab, layout).

**Architecture:** Community items are catalog rows plus immutable versions; install forks a pinned version into a new owned indicator workspace in one transaction. Workspace persistence is a per-user opaque JSONB snapshot (`workspace_state`) written by the shell and restored on boot, so no layout slot is consumed and the whole workspace is one read/write.

**Tech Stack:** Rust (Axum 0.8, sqlx 0.8/PostgreSQL), vanilla JS shell, existing strategy-DSL/Pine-lite validators and chart engine.

**Spec:** `docs/superpowers/specs/2026-10-27-community-indicators-design.md`

## Global Constraints

- `.env` is gitignored; never print, commit, or expose `DATABASE_URL`, JWT secrets, payment-provider keys, broker credentials, or other secrets.
- Do not run `cargo fmt --all`: the repo is not rustfmt-clean and it rewrites 100+ unrelated files.
- Do not run `tools/guard_check.py` while the user is testing; it temporarily mutates `frontend/app/app.js`.
- Do not restart the user's gateway on `:8080` during their session.
- `cargo test` skips when `DATABASE_URL` is unset; integration tests must assert their own setup statuses.
- `-1` is unlimited throughout the plan matrix; prices are cents.
- Source is untrusted even after publishing: publish, install, and update re-run the current validator.
- All new SQL uses runtime `sqlx::query` (not the compile-time macros), matching the DB crate.
- Every new public item in `crates/db` needs `#![deny(missing_docs)]`-compatible docs.
- The shell escapes every server string it renders (`escapeHtml`) and never injects source as HTML.

> **Amendment (2026-10-27, after execution):** the owner directed that community
> indicators be freely accessible like the libraries on TradingView, LuxAlgo and MT5.
> Every plan now carries `Included`; Desk's `Priority` is catalog placement only.
> Tasks below that describe `Feature::CommunityBrowse` / `Feature::CommunityPublish`
> or Core/Elite gating record the original plan, and were superseded by the
> un-gating change. `docs/52` and `docs/53` are authoritative.

---

## Phase 1 — Community Indicators

### Task 1: Split the billing feature and make the library available

**Files:**
- Modify: `crates/api-gateway/src/billing/plans.rs`
- Modify: `crates/api-gateway/src/billing/mod.rs`
- Modify: `crates/api-gateway/src/billing/service.rs`
- Modify: `crates/api-gateway/src/billing/routes.rs`
- Test: `crates/api-gateway/tests/billing_flow.rs`

**Interfaces:**
- Produces: `Feature::CommunityBrowse` (code `community_browse`, minimum `Core`) and `Feature::CommunityPublish` (code `community_publish`, minimum `Elite`).
- Produces: `Feature::allowed_by` reads `Features.community_indicators`:
  - `CommunityBrowse` → `Browse | Publish | Priority`
  - `CommunityPublish` → `Publish | Priority`
- Produces: `community_indicators_available: true` from `/billing/plans` and `/billing/subscription`.
- Removes: `COMMUNITY_INDICATORS_UNAVAILABLE` / 501 entirely.

- [ ] **Step 1: Replace the reserved-feature test with a ladder test in `plans.rs`**

Replace `community_indicators_are_reserved_but_not_available` with:

```rust
#[test]
fn community_tiers_unlock_browse_then_publish() {
    let free = plan(PlanId::Free).features;
    let core = plan(PlanId::Core).features;
    let pro = plan(PlanId::Pro).features;
    let elite = plan(PlanId::Elite).features;
    let desk = plan(PlanId::Desk).features;

    assert!(!Feature::CommunityBrowse.allowed_by(&free));
    assert!(Feature::CommunityBrowse.allowed_by(&core));
    assert!(Feature::CommunityBrowse.allowed_by(&pro));
    assert!(Feature::CommunityBrowse.allowed_by(&elite));
    assert!(Feature::CommunityBrowse.allowed_by(&desk));

    assert!(!Feature::CommunityPublish.allowed_by(&free));
    assert!(!Feature::CommunityPublish.allowed_by(&core));
    assert!(!Feature::CommunityPublish.allowed_by(&pro));
    assert!(Feature::CommunityPublish.allowed_by(&elite));
    assert!(Feature::CommunityPublish.allowed_by(&desk));

    assert_eq!(Feature::CommunityBrowse.minimum_plan(), PlanId::Core);
    assert_eq!(Feature::CommunityPublish.minimum_plan(), PlanId::Elite);
    assert_eq!(
        plan(PlanId::Desk).features.community_indicators,
        CommunityIndicators::Priority
    );
}
```

- [ ] **Step 2: Run the test and see it fail**

Run: `cargo test -p api-gateway --lib billing::plans::tests::community_tiers_unlock_browse_then_publish`
Expected: FAIL to compile — `CommunityBrowse` does not exist.

- [ ] **Step 3: Update the enum and its comments**

In `plans.rs`:

```rust
/// Access to the community indicator library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CommunityIndicators {
    /// No access.
    None,
    /// Browse and install public indicators.
    Browse,
    /// Browse, install and publish.
    Publish,
    /// Browse, publish, and priority placement in the catalog.
    Priority,
}
```

Change the `Features.community_indicators` doc to:

```rust
    /// What the community indicator library allows (`docs/53`).
    pub community_indicators: CommunityIndicators,
```

Change `Feature`:

```rust
    /// Browse, install, update and report community indicators.
    CommunityBrowse,
    /// Publish and maintain community indicators.
    CommunityPublish,
```

Change `code`, `minimum_plan`, and `allowed_by`:

```rust
    pub const fn code(self) -> &'static str {
        match self {
            Self::Orderflow => "orderflow",
            Self::AdvancedOrderflow => "orderflow_advanced",
            Self::ByoModelKey => "byo_model_key",
            Self::LiveBots => "live_bots",
            Self::CommunityBrowse => "community_browse",
            Self::CommunityPublish => "community_publish",
        }
    }

    pub const fn minimum_plan(self) -> PlanId {
        match self {
            Self::Orderflow => PlanId::Core,
            Self::AdvancedOrderflow | Self::ByoModelKey | Self::LiveBots => PlanId::Pro,
            Self::CommunityBrowse => PlanId::Core,
            Self::CommunityPublish => PlanId::Elite,
        }
    }

    pub const fn allowed_by(self, features: &Features) -> bool {
        match self {
            Self::Orderflow => features.orderflow,
            Self::AdvancedOrderflow => features.orderflow_advanced,
            Self::ByoModelKey => features.byo_model_key,
            Self::CommunityBrowse => matches!(
                features.community_indicators,
                CommunityIndicators::Browse | CommunityIndicators::Publish | CommunityIndicators::Priority
            ),
            Self::CommunityPublish => matches!(
                features.community_indicators,
                CommunityIndicators::Publish | CommunityIndicators::Priority
            ),
            Self::LiveBots => false,
        }
    }
```

Update the matrix rows: Core/Pro keep `Browse`, Elite keeps `Publish`, Desk keeps `Priority`; only the comments change.

- [ ] **Step 4: Remove the 501 special case in `billing/mod.rs`**

Replace the whole `BillingError::FeatureRequired` arm with:

```rust
            BillingError::FeatureRequired { feature, plan } => ApiError::coded(
                StatusCode::PAYMENT_REQUIRED,
                "PLAN_FEATURE_REQUIRED",
                format!(
                    "{} requires the {} plan; you are on {}",
                    feature.code(),
                    feature.minimum_plan(),
                    plan
                ),
            )
            .with_details(serde_json::json!({
                "feature": feature.code(),
                "plan": plan.as_str(),
                "required_plan": feature.minimum_plan().as_str(),
            })),
```

Replace `community_indicators_are_not_a_paywall_yet` with:

```rust
#[test]
fn community_publish_is_a_402_at_the_elite_boundary() {
    let error: ApiError = BillingError::FeatureRequired {
        feature: Feature::CommunityPublish,
        plan: PlanId::Core,
    }
    .into();
    assert_eq!(error.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(error.code(), "PLAN_FEATURE_REQUIRED");
    assert_eq!(
        error.details().and_then(|d| d.get("feature")),
        Some(&serde_json::json!("community_publish"))
    );
    assert_eq!(
        error.details().and_then(|d| d.get("required_plan")),
        Some(&serde_json::json!("elite"))
    );
}
```

Update the module doc paragraph that says the library is reserved.

- [ ] **Step 5: Make availability true in the service and the catalog route**

In `service.rs`, replace the `community_indicators_available` doc and value:

```rust
    /// Whether the community indicator library exists for this deployment.
    ///
    /// A `BillingService` only exists when a database does, and the library
    /// is stored in that database, so this is always true here.
    pub community_indicators_available: bool,
```

and `community_indicators_available: true,`.

In `routes.rs`, change the doc and value of `PlansResponse.community_indicators_available`:

```rust
    /// Whether this deployment has the community indicator library.
    pub community_indicators_available: bool,
```

```rust
        community_indicators_available: state.db.is_some(),
```

- [ ] **Step 6: Update `billing_flow.rs`**

Change lines asserting the flag:

```rust
    assert_eq!(catalog["community_indicators_available"], true);
...
    assert_eq!(entitlements["community_indicators_available"], true);
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p api-gateway --lib billing::`
Expected: PASS.

Run: `cargo test -p api-gateway --test billing_flow` (requires `DATABASE_URL`).
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/api-gateway/src/billing/plans.rs crates/api-gateway/src/billing/mod.rs \
        crates/api-gateway/src/billing/service.rs crates/api-gateway/src/billing/routes.rs \
        crates/api-gateway/tests/billing_flow.rs
git commit -m "feat: community tiers are real entitlements, not a reserved row"
```

---

### Task 2: Add the community schema and DB layer

**Files:**
- Create: `crates/db/migrations/0016_community_indicators.sql`
- Create: `crates/db/src/community.rs`
- Modify: `crates/db/src/lib.rs`

**Interfaces:**
- Produces types: `CommunityIndicatorRow`, `CommunityVersionRow`, `CommunityInstallRow`, `CommunityListItem`, `NewCommunityItem`, `NewCommunityVersion`, `InstallOutcome`, `UpdateOutcome`.
- Produces functions (all `pub async fn`):
  - `list_community_indicators(pool, q: Option<&str>, tag: Option<&str>, sort: &str, limit: i64, offset: i64) -> Result<(Vec<CommunityListItem>, i64), DbError>`
  - `list_author_indicators(pool, user_id) -> Result<Vec<CommunityListItem>, DbError>`
  - `list_community_installs(pool, user_id) -> Result<Vec<(CommunityIndicatorRow, CommunityInstallRow, i32)>, DbError>`
  - `get_community_indicator(pool, slug) -> Result<Option<CommunityIndicatorRow>, DbError>`
  - `get_community_indicator_by_id(pool, id) -> Result<Option<CommunityIndicatorRow>, DbError>`
  - `get_community_version(pool, id) -> Result<Option<CommunityVersionRow>, DbError>`
  - `list_community_versions(pool, indicator_id, limit) -> Result<Vec<CommunityVersionRow>, DbError>`
  - `get_community_install(pool, user_id, indicator_id) -> Result<Option<CommunityInstallRow>, DbError>`
  - `count_author_indicators(pool, user_id) -> Result<i64, DbError>`
  - `count_community_installs(pool, user_id) -> Result<i64, DbError>`
  - `create_community_indicator(pool, author_user_id, slug, new: &NewCommunityItem) -> Result<CommunityIndicatorRow, DbError>`
  - `create_community_version(pool, author_user_id, indicator_id, new: &NewCommunityVersion) -> Result<Option<CommunityVersionRow>, DbError>`
  - `update_community_indicator(pool, author_user_id, indicator_id, title, summary, tags, status) -> Result<Option<CommunityIndicatorRow>, DbError>`
  - `install_community_indicator(pool, installer, indicator_id, version: &CommunityVersionRow, workspace_name, provenance, memory) -> Result<InstallOutcome, DbError>`
  - `update_community_install(pool, installer, indicator_id, version: &CommunityVersionRow, provenance) -> Result<UpdateOutcome, DbError>`
  - `report_community_indicator(pool, reporter, indicator_id, reason, note: Option<&str>) -> Result<bool, DbError>`

- [ ] **Step 1: Write the migration**

`crates/db/migrations/0016_community_indicators.sql`:

```sql
-- Community indicators (`docs/53`): a catalog row per published indicator and
-- immutable versions beneath it. Install records pin one version to one owned
-- indicator workspace, so the existing workspace/revision read paths need no
-- community-specific branch.

CREATE TABLE IF NOT EXISTS community_indicators (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    author_user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    slug               TEXT NOT NULL UNIQUE,
    title              TEXT NOT NULL,
    summary            TEXT NOT NULL,
    tags               TEXT[] NOT NULL DEFAULT '{}',
    author_name        TEXT NOT NULL,
    author_tier        TEXT NOT NULL DEFAULT 'publish'
                       CHECK (author_tier IN ('publish', 'priority')),
    status             TEXT NOT NULL DEFAULT 'listed'
                       CHECK (status IN ('listed', 'unlisted', 'removed')),
    current_version_id UUID,
    install_count      BIGINT NOT NULL DEFAULT 0 CHECK (install_count >= 0),
    report_count       INTEGER NOT NULL DEFAULT 0 CHECK (report_count >= 0),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS community_indicator_versions (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    indicator_id   UUID NOT NULL REFERENCES community_indicators (id) ON DELETE CASCADE,
    version_number INTEGER NOT NULL,
    source         TEXT NOT NULL,
    summary        TEXT NOT NULL,
    symbol         TEXT NOT NULL,
    timeframe      TEXT NOT NULL,
    validation     JSONB NOT NULL,
    preview        JSONB NOT NULL,
    published_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (indicator_id, version_number)
);

ALTER TABLE community_indicators
    ADD CONSTRAINT community_indicators_current_version_fk
    FOREIGN KEY (current_version_id) REFERENCES community_indicator_versions (id);

CREATE TABLE IF NOT EXISTS community_indicator_installs (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    indicator_id UUID NOT NULL REFERENCES community_indicators (id) ON DELETE CASCADE,
    user_id      UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES indicator_workspaces (id) ON DELETE CASCADE,
    version_id   UUID NOT NULL REFERENCES community_indicator_versions (id),
    installed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, indicator_id)
);

CREATE TABLE IF NOT EXISTS community_indicator_reports (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    indicator_id     UUID NOT NULL REFERENCES community_indicators (id) ON DELETE CASCADE,
    reporter_user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    reason           TEXT NOT NULL
                     CHECK (reason IN ('spam', 'misleading', 'malicious', 'copyright', 'other')),
    note             TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (indicator_id, reporter_user_id)
);

CREATE INDEX IF NOT EXISTS community_indicators_featured_idx
    ON community_indicators (status, author_tier DESC, install_count DESC, created_at DESC);
CREATE INDEX IF NOT EXISTS community_indicators_new_idx
    ON community_indicators (status, created_at DESC);
CREATE INDEX IF NOT EXISTS community_indicators_popular_idx
    ON community_indicators (status, install_count DESC, created_at DESC);
CREATE INDEX IF NOT EXISTS community_indicators_tags_idx
    ON community_indicators USING GIN (tags);
CREATE INDEX IF NOT EXISTS community_indicators_author_idx
    ON community_indicators (author_user_id, created_at DESC);
```

- [ ] **Step 2: Write `crates/db/src/community.rs`**

Start with the types and row decoders:

```rust
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

use serde::{Deserialize, Serialize};
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
```

Row decoders (private):

```rust
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
```

List functions:

```rust
/// The public catalog, filtered and ordered.
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
```

Getters and counters:

```rust
/// One catalog row by slug, in any status.
pub async fn get_community_indicator(
    pool: &PgPool,
    slug: &str,
) -> Result<Option<CommunityIndicatorRow>, DbError> {
    let sql = format!("SELECT {INDICATOR_COLUMNS} FROM community_indicators i WHERE i.slug = $1");
    let row = sqlx::query(&sql).bind(slug).fetch_optional(pool).await?;
    row.as_ref().map(indicator_from).transpose().map_err(Into::into)
}

/// One catalog row by id.
pub async fn get_community_indicator_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<CommunityIndicatorRow>, DbError> {
    let sql = format!("SELECT {INDICATOR_COLUMNS} FROM community_indicators i WHERE i.id = $1");
    let row = sqlx::query(&sql).bind(id).fetch_optional(pool).await?;
    row.as_ref().map(indicator_from).transpose().map_err(Into::into)
}

/// One version by id.
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
    row.as_ref().map(version_from).transpose().map_err(Into::into)
}

/// Versions of one indicator, newest first.
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
    rows.iter().map(version_from).collect::<Result<_, _>>().map_err(Into::into)
}

/// The caller's pin on one item.
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
    row.as_ref().map(install_from).transpose().map_err(Into::into)
}

/// How many items this author has ever published.
pub async fn count_author_indicators(pool: &PgPool, user_id: Uuid) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM community_indicators WHERE author_user_id = $1",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?)
}

/// How many items this user has installed.
pub async fn count_community_installs(pool: &PgPool, user_id: Uuid) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM community_indicator_installs WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?)
}
```

Writers:

```rust
/// Insert a catalog row and its first version atomically.
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
             (author_user_id, slug, title, summary, tags, author_name, author_tier) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {columns}"
    ))
    .bind(author_user_id)
    .bind(slug)
    .bind(&new.title)
    .bind(&new.summary)
    .bind(&new.tags)
    .bind(&new.author_name)
    .bind(&new.author_tier)
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
    row.as_ref().map(indicator_from).transpose().map_err(Into::into)
}
```

Install/update/report:

```rust
/// Fork the pinned version into a new owned workspace.
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
        object.insert("revision_id".to_string(), Value::String(revision_id.to_string()));
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
        object.insert("revision_id".to_string(), Value::String(revision_id.to_string()));
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
    Ok(UpdateOutcome::Updated { revision_id })
}

/// Record a report once per reporter; return whether it was new.
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
```

- [ ] **Step 3: Export the module**

In `crates/db/src/lib.rs`:

```rust
pub mod community;
```

and after the `chart_snapshots` export:

```rust
pub use community::{
    count_author_indicators, count_community_installs, create_community_indicator,
    create_community_version, get_community_indicator, get_community_indicator_by_id,
    get_community_install, get_community_version, install_community_indicator,
    list_author_indicators, list_community_installs, list_community_indicators,
    list_community_versions, report_community_indicator, update_community_indicator,
    update_community_install, CommunityIndicatorRow, CommunityInstallRow, CommunityListItem,
    CommunityVersionRow, InstallOutcome, NewCommunityItem, NewCommunityVersion, UpdateOutcome,
};
```

- [ ] **Step 4: Compile**

Run: `cargo check -p db`
Expected: PASS. The embedded migration is read at compile time, so a typo in the filename fails here.

- [ ] **Step 5: Commit**

```bash
git add crates/db/migrations/0016_community_indicators.sql crates/db/src/community.rs crates/db/src/lib.rs
git commit -m "feat: community catalog, immutable versions, install pins and reports"
```

---

### Task 3: Community routes and integration tests

**Files:**
- Create: `crates/api-gateway/src/community_routes.rs`
- Modify: `crates/api-gateway/src/lib.rs`
- Create: `crates/api-gateway/tests/community_flow.rs`

**Interfaces:**
- Consumes everything from Task 2 and `Feature::CommunityBrowse` / `CommunityPublish` from Task 1.
- Produces routes mounted under `/community/indicators`.
- Produces `MAX_PUBLISHED_INDICATORS = 25`, `MAX_VERSIONS_PER_INDICATOR = 50`, `MAX_INSTALLS_PER_USER = 100`, `MAX_COMMUNITY_SOURCE_BYTES = 64 * 1024`, `MAX_COMMUNITY_TAGS = 5`.

- [ ] **Step 1: Write the failing integration tests**

`crates/api-gateway/tests/community_flow.rs`:

```rust
//! Community indicators (`docs/53`) against the real router and database.

mod common;

use axum::http::StatusCode;
use serde_json::{json, Value};

fn preview(name: &str) -> Value {
    json!({
        "revision_id": name,
        "evidence": [{
            "id": "sweep",
            "event": "liquidity_sweep",
            "time": 1_700_000_000_000_000_000_i64,
            "price": 100.0,
            "explanation": "Price swept the prior low."
        }],
        "zones": [],
        "markers": [{
            "id": "sweep-marker",
            "evidence_id": "sweep",
            "time": 1_700_000_000_000_000_000_i64,
            "price": 100.0,
            "label": "Sweep",
            "kind": "context"
        }],
        "links": []
    })
}

/// A workspace with one validated revision, owned by `token`.
async fn workspace_with_revision(h: &common::Harness, token: &str) -> (String, String) {
    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Community source", "symbol": "BTCUSDT", "timeframe": "5m" }),
            Some(token),
        )
        .await;
    let (status, body) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            json!({
                "source": "name: community\nversion: '1'\nkind: indicator\nmarket: BTCUSDT\ntimeframes:\n  entry: 5m",
                "summary": "A sweep followed by displacement.",
                "change_summary": "Initial",
                "preview": preview("community-rev"),
            }),
            Some(token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "revision create failed: {body}");
    (workspace_id, body["id"].as_str().unwrap().to_string())
}

async fn publish(h: &common::Harness, token: &str, title: &str) -> Value {
    let (workspace_id, revision_id) = workspace_with_revision(h, token).await;
    let (status, body) = h
        .post(
            "/community/indicators",
            json!({
                "workspace_id": workspace_id,
                "revision_id": revision_id,
                "title": title,
                "summary": "Sweep of a prior low, then displacement.",
                "tags": ["smc", "fvg"],
                "author_name": "Test Author",
            }),
            Some(token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "publish failed: {body}");
    body
}

#[tokio::test]
async fn free_cannot_browse_and_core_can() {
    let Some(h) = common::Harness::new().await else { return };
    let free = h.register().await;
    let (status, body) = h.get("/community/indicators", Some(&free.token)).await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["error"]["details"]["feature"], "community_browse");

    let core = h.register_with_plan("core").await;
    let (status, body) = h.get("/community/indicators", Some(&core.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 0);

    free.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn core_cannot_publish_and_elite_can() {
    let Some(h) = common::Harness::new().await else { return };
    let core = h.register_with_plan("core").await;
    let (workspace_id, revision_id) = workspace_with_revision(&h, &core.token).await;
    let (status, body) = h
        .post(
            "/community/indicators",
            json!({ "workspace_id": workspace_id, "revision_id": revision_id, "title": "Nope" }),
            Some(&core.token),
        )
        .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["error"]["details"]["feature"], "community_publish");

    let elite = h.register_with_plan("elite").await;
    let item = publish(&h, &elite.token, "Liquidity sweep + FVG").await;
    assert_eq!(item["author_tier"], "publish");
    assert_eq!(item["source"], "name: community\nversion: '1'\nkind: indicator\nmarket: BTCUSDT\ntimeframes:\n  entry: 5m");

    core.cleanup(&h.database).await;
    elite.cleanup(&h.database).await;
}

#[tokio::test]
async fn publishing_someone_elses_workspace_is_404() {
    let Some(h) = common::Harness::new().await else { return };
    let alice = h.register_with_plan("elite").await;
    let bob = h.register_with_plan("elite").await;
    let (workspace_id, revision_id) = workspace_with_revision(&h, &alice.token).await;
    let (status, _) = h
        .post(
            "/community/indicators",
            json!({ "workspace_id": workspace_id, "revision_id": revision_id, "title": "Stolen" }),
            Some(&bob.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    alice.cleanup(&h.database).await;
    bob.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_rejected_revision_cannot_be_published() {
    let Some(h) = common::Harness::new().await else { return };
    let elite = h.register_with_plan("elite").await;
    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Reject", "symbol": "BTCUSDT", "timeframe": "5m" }),
            Some(&elite.token),
        )
        .await;
    let (status, body) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            json!({
                "source": "name: bad\nversion: '1'\nkind: indicator\nmarket: BTCUSDT\ntimeframes:\n  entry: 5m",
                "summary": "Bad",
                "change_summary": "Bad",
                "preview": { "revision_id": "", "evidence": [], "zones": [], "markers": [], "links": [] },
            }),
            Some(&elite.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["status"], "rejected");
    let revision_id = body["id"].as_str().unwrap();

    let (status, body) = h
        .post(
            "/community/indicators",
            json!({ "workspace_id": workspace_id, "revision_id": revision_id, "title": "Bad item" }),
            Some(&elite.token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "COMMUNITY_REVISION_NOT_VALIDATED");

    elite.cleanup(&h.database).await;
}

#[tokio::test]
async fn install_forks_once_and_update_moves_the_pin() {
    let Some(h) = common::Harness::new().await else { return };
    let author = h.register_with_plan("elite").await;
    let core = h.register_with_plan("core").await;
    let item = publish(&h, &author.token, "Sweep fork").await;
    let slug = item["slug"].as_str().unwrap();

    let (status, body) = h
        .post(&format!("/community/indicators/{slug}/install"), json!({}), Some(&core.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["already_installed"], false);
    let workspace_id = body["workspace_id"].as_str().unwrap().to_string();

    let (status, again) = h
        .post(&format!("/community/indicators/{slug}/install"), json!({}), Some(&core.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["already_installed"], true);
    assert_eq!(again["workspace_id"], workspace_id);

    let installed = h.ok("/community/indicators/installed", Some(&core.token)).await;
    assert_eq!(installed["items"].as_array().unwrap().len(), 1);

    let detail = h.ok(&format!("/community/indicators/{slug}"), Some(&core.token)).await;
    assert_eq!(detail["installed"]["workspace_id"], workspace_id);
    assert_eq!(detail["update_available"], false);

    author.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_new_version_shows_update_available_and_update_appends() {
    let Some(h) = common::Harness::new().await else { return };
    let author = h.register_with_plan("elite").await;
    let core = h.register_with_plan("core").await;
    let item = publish(&h, &author.token, "Versioned").await;
    let slug = item["slug"].as_str().unwrap().to_string();
    let (_, installed) = h
        .post(&format!("/community/indicators/{slug}/install"), json!({}), Some(&core.token))
        .await;

    let (workspace_id, _) = workspace_with_revision(&h, &author.token).await;
    let (status, second) = h
        .post(
            &format!("/community/indicators/{slug}/versions"),
            json!({
                "workspace_id": workspace_id,
                "title": "Versioned",
                "summary": "Version two.",
                "tags": ["smc"],
            }),
            Some(&author.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");

    let detail = h.ok(&format!("/community/indicators/{slug}"), Some(&core.token)).await;
    assert_eq!(detail["update_available"], true);

    let (status, updated) = h
        .post(&format!("/community/indicators/{slug}/update"), json!({}), Some(&core.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let revision_id = updated["revision_id"].as_str().unwrap().to_string();

    let revisions = h
        .ok(
            &format!("/indicator-workspaces/{}/revisions", installed["workspace_id"].as_str().unwrap()),
            Some(&core.token),
        )
        .await;
    let numbers: Vec<i64> = revisions.as_array().unwrap().iter().map(|r| r["revision_number"].as_i64().unwrap()).collect();
    assert_eq!(numbers, vec![2, 1]);
    assert_eq!(revisions[0]["id"], revision_id);

    let (status, body) = h
        .post(&format!("/community/indicators/{slug}/update"), json!({}), Some(&core.token))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "COMMUNITY_ALREADY_CURRENT");

    author.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn unlisting_hides_the_item_but_keeps_installed_copies() {
    let Some(h) = common::Harness::new().await else { return };
    let author = h.register_with_plan("elite").await;
    let core = h.register_with_plan("core").await;
    let item = publish(&h, &author.token, "Unlisted").await;
    let slug = item["slug"].as_str().unwrap();
    h.post(&format!("/community/indicators/{slug}/install"), json!({}), Some(&core.token)).await;

    let (status, body) = h
        .put(&format!("/community/indicators/{slug}"), json!({ "status": "unlisted" }), Some(&author.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _) = h.get(&format!("/community/indicators/{slug}"), Some(&core.token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let installed = h.ok("/community/indicators/installed", Some(&core.token)).await;
    assert_eq!(installed["items"].as_array().unwrap().len(), 1);

    let (status, body) = h
        .post(&format!("/community/indicators/{slug}/update"), json!({}), Some(&core.token))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "COMMUNITY_NOT_LISTED");

    author.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn reports_are_idempotent() {
    let Some(h) = common::Harness::new().await else { return };
    let author = h.register_with_plan("elite").await;
    let core = h.register_with_plan("core").await;
    let item = publish(&h, &author.token, "Reported").await;
    let slug = item["slug"].as_str().unwrap();

    let (status, first) = h
        .post(&format!("/community/indicators/{slug}/report"), json!({ "reason": "spam", "note": "looks wrong" }), Some(&core.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["reported"], true);
    assert_eq!(first["already_reported"], false);

    let (status, second) = h
        .post(&format!("/community/indicators/{slug}/report"), json!({ "reason": "spam" }), Some(&core.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["already_reported"], true);

    author.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn desk_priority_sorts_first() {
    let Some(h) = common::Harness::new().await else { return };
    let elite = h.register_with_plan("elite").await;
    let desk = h.register_with_plan("desk").await;
    publish(&h, &elite.token, "Ordinary item").await;
    let priority = publish(&h, &desk.token, "Priority item").await;
    assert_eq!(priority["author_tier"], "priority");

    let core = h.register_with_plan("core").await;
    let listed = h.ok("/community/indicators?sort=featured", Some(&core.token)).await;
    assert_eq!(listed["items"][0]["author_tier"], "priority");

    elite.cleanup(&h.database).await;
    desk.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn free_cannot_install() {
    let Some(h) = common::Harness::new().await else { return };
    let author = h.register_with_plan("elite").await;
    let free = h.register().await;
    let item = publish(&h, &author.token, "Paid only").await;
    let slug = item["slug"].as_str().unwrap();
    let (status, body) = h
        .post(&format!("/community/indicators/{slug}/install"), json!({}), Some(&free.token))
        .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["error"]["details"]["feature"], "community_browse");

    author.cleanup(&h.database).await;
    free.cleanup(&h.database).await;
}

#[tokio::test]
async fn only_the_author_can_add_versions_or_patch_metadata() {
    let Some(h) = common::Harness::new().await else { return };
    let author = h.register_with_plan("elite").await;
    let other = h.register_with_plan("elite").await;
    let item = publish(&h, &author.token, "Mine").await;
    let slug = item["slug"].as_str().unwrap();
    // The other user's own workspace must not let them add a version to the
    // author's item: ownership of the item is checked first.
    let (other_workspace, _) = workspace_with_revision(&h, &other.token).await;

    let (status, _) = h
        .post(
            &format!("/community/indicators/{slug}/versions"),
            json!({ "workspace_id": other_workspace, "title": "Mine" }),
            Some(&other.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = h
        .put(&format!("/community/indicators/{slug}"), json!({ "title": "Stolen" }), Some(&other.token))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    author.cleanup(&h.database).await;
    other.cleanup(&h.database).await;
}

#[tokio::test]
async fn metadata_and_source_bounds_are_named() {
    let Some(h) = common::Harness::new().await else { return };
    let elite = h.register_with_plan("elite").await;
    let (workspace_id, revision_id) = workspace_with_revision(&h, &elite.token).await;

    let (status, body) = h
        .post(
            "/community/indicators",
            json!({ "workspace_id": workspace_id, "revision_id": revision_id, "title": "x" }),
            Some(&elite.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "COMMUNITY_METADATA_INVALID");

    let tags: Vec<String> = (0..6).map(|n| format!("tag{n}")).collect();
    let (status, body) = h
        .post(
            "/community/indicators",
            json!({ "workspace_id": workspace_id, "revision_id": revision_id, "title": "Valid title", "tags": tags }),
            Some(&elite.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "COMMUNITY_METADATA_INVALID");

    elite.cleanup(&h.database).await;
}
```

The harness already has `get`, `post`, `created`, `ok`, `put`, and `delete`; the tests above use those directly and need no new helpers.

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test -p api-gateway --test community_flow`
Expected: FAIL — `/community/indicators` routes 404.

- [ ] **Step 3: Write `community_routes.rs`**

Implement the module with these exact types and handlers:

```rust
//! `/community/indicators`: the community indicator library (`docs/53`).
//!
//! ## Browse is one level, publish is another
//!
//! Every route enforces `Feature::CommunityBrowse`; publish-family routes
//! additionally enforce `Feature::CommunityPublish`. A refusal is the same 402
//! `PLAN_FEATURE_REQUIRED` as every other feature.
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
use crate::billing::{enforce_feature, Feature};
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
```

Handlers: `list`, `installed`, `mine`, `get`, `publish`, `add_version`, `patch`, `install`, `update`, `report`.

Helpers:

```rust
fn database(state: &AppState) -> Result<Arc<db::Database>, ApiError> {
    state
        .db
        .clone()
        .ok_or_else(|| ApiError::unavailable("the community library needs the database; it is not configured"))
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
```

Validation helpers:

```rust
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
```

Re-vet:

```rust
fn revet(source: &str, validation: &serde_json::Value, preview: &serde_json::Value) -> Result<(), ApiError> {
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
    let output: chart_engine::IndicatorOutput = serde_json::from_value(preview.clone())
        .map_err(|error| source_invalid(vec![format!("the preview is not a chart output: {error}")]))?;
    output.validate().map_err(|error| source_invalid(vec![error]))?;
    Ok(())
}
```

`tier`:

```rust
fn tier_of(entitlements: &crate::billing::Entitlements) -> &'static str {
    match entitlements.features.community_indicators {
        crate::billing::CommunityIndicators::Priority => "priority",
        _ => "publish",
    }
}
```

Note: `CommunityIndicators` is exported at `crate::billing::CommunityIndicators`; import it.

Response shapes:

```rust
#[derive(Debug, Serialize)]
pub struct CommunityItem {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub author_name: String,
    pub author_tier: String,
    pub install_count: i64,
    pub version: i32,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize)]
pub struct CommunityList {
    pub items: Vec<CommunityItem>,
    pub total: i64,
}

#[derive(Debug, Serialize)]
pub struct VersionSummary {
    pub id: String,
    pub version_number: i32,
    pub summary: String,
    pub published_at: i64,
}

#[derive(Debug, Serialize)]
pub struct InstalledView {
    pub workspace_id: String,
    pub version: i32,
    pub revision_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommunityDetail {
    #[serde(flatten)]
    pub item: CommunityItem,
    pub source: String,
    pub validation: serde_json::Value,
    pub preview: serde_json::Value,
    pub versions: Vec<VersionSummary>,
    pub installed: Option<InstalledView>,
    pub update_available: bool,
    pub is_author: bool,
}
```

Mapping helpers:

```rust
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
```

Detail builder:

```rust
async fn detail(
    database: &db::Database,
    viewer: Uuid,
    item: &db::CommunityIndicatorRow,
) -> Result<CommunityDetail, ApiError> {
    let version_id = item.current_version_id.ok_or_else(unknown)?;
    let version = db::get_community_version(database.pool(), version_id)
        .await?
        .ok_or_else(unknown)?;
    let versions = db::list_community_versions(database.pool(), item.id, VERSION_LIST_LIMIT).await?;
    let install = db::get_community_install(database.pool(), viewer, item.id).await?;
    let revision_id = match &install {
        Some(install) => {
            let revisions = db::list_indicator_revisions(
                database.pool(), viewer, install.workspace_id, 50,
            ).await?;
            revisions.into_iter().max_by_key(|r| r.revision_number).map(|r| r.id.to_string())
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
            .map(|v| VersionSummary {
                id: v.id.to_string(),
                version_number: v.version_number,
                summary: v.summary.clone(),
                published_at: v.published_at / 1_000_000,
            })
            .collect(),
        installed: install.as_ref().map(|install| InstalledView {
            workspace_id: install.workspace_id.to_string(),
            version: versions
                .iter()
                .find(|v| v.id == install.version_id)
                .map_or(0, |v| v.version_number),
            revision_id,
        }),
        update_available: install
            .as_ref()
            .is_some_and(|install| install.version_id != version_id),
        is_author: item.is_author(viewer),
    })
}
```

List handler:

```rust
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

pub async fn list(
    State(state): State<AppState>,
    user: UserContext,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> Result<Json<CommunityList>, ApiError> {
    let database = database(&state)?;
    enforce_feature(&state, user.user_id, Feature::CommunityBrowse).await?;
    let limit = i64::from(query.limit.unwrap_or(24).clamp(1, 50));
    let offset = i64::from(query.offset.unwrap_or(0));
    let sort = query.sort.as_deref().unwrap_or("featured");
    let (rows, total) = db::list_community_indicators(
        database.pool(),
        query.q.as_deref().filter(|q| !q.trim().is_empty()),
        query.tag.as_deref().filter(|t| !t.trim().is_empty()),
        sort,
        limit,
        offset,
    )
    .await?;
    Ok(Json(CommunityList {
        items: rows.iter().map(|row| item_view(&row.indicator, row.version_number)).collect(),
        total,
    }))
}
```

`installed`:

```rust
pub async fn installed(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<CommunityList>, ApiError> {
    let database = database(&state)?;
    enforce_feature(&state, user.user_id, Feature::CommunityBrowse).await?;
    let rows = db::list_community_installs(database.pool(), user.user_id).await?;
    Ok(Json(CommunityList {
        items: rows.iter().map(|(row, _, version)| item_view(row, *version)).collect(),
        total: rows.len() as i64,
    }))
}
```

`mine`:

```rust
pub async fn mine(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<CommunityList>, ApiError> {
    let database = database(&state)?;
    enforce_feature(&state, user.user_id, Feature::CommunityPublish).await?;
    let rows = db::list_author_indicators(database.pool(), user.user_id).await?;
    Ok(Json(CommunityList {
        items: rows.iter().map(|row| item_view(&row.indicator, row.version_number)).collect(),
        total: rows.len() as i64,
    }))
}
```

`get`:

```rust
pub async fn get(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
) -> Result<Json<CommunityDetail>, ApiError> {
    let database = database(&state)?;
    enforce_feature(&state, user.user_id, Feature::CommunityBrowse).await?;
    let item = db::get_community_indicator(database.pool(), &slug).await?.ok_or_else(unknown)?;
    visibility(&item, user.user_id)?;
    Ok(Json(detail(&database, user.user_id, &item).await?))
}
```

Publish:

```rust
#[derive(Debug, Deserialize)]
pub struct PublishBody {
    pub workspace_id: Uuid,
    #[serde(default)]
    pub revision_id: Option<Uuid>,
    pub title: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub author_name: Option<String>,
}

pub async fn publish(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<PublishBody>,
) -> Result<(StatusCode, Json<CommunityDetail>), ApiError> {
    let database = database(&state)?;
    enforce_feature(&state, user.user_id, Feature::CommunityPublish).await?;
    let workspace = db::get_indicator_workspace(database.pool(), user.user_id, body.workspace_id)
        .await?
        .ok_or_else(|| ApiError::not_found("indicator workspace not found"))?;
    let revision_id = body
        .revision_id
        .or(workspace.active_revision_id)
        .ok_or_else(|| conflict("COMMUNITY_REVISION_NOT_VALIDATED", "the workspace has no revision to publish"))?;
    let revision = db::get_indicator_revision(database.pool(), user.user_id, workspace.id, revision_id)
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
    revet(&revision.source, &revision.validation, &revision.preview)?;

    let title = clean_title(&body.title)?;
    let summary = clean_summary(
        body.summary.as_deref().unwrap_or(revision.summary.as_str()),
    )?;
    let tags = clean_tags(&body.tags)?;
    let author_name = clean_author_name(body.author_name.as_deref(), user.user_id)?;

    let published = db::count_author_indicators(database.pool(), user.user_id).await?;
    if published >= MAX_PUBLISHED_INDICATORS {
        return Err(conflict(
            "COMMUNITY_PUBLISH_LIMIT_REACHED",
            format!("this account already has {MAX_PUBLISHED_INDICATORS} published indicators"),
        ));
    }
    let entitlements = enforce_feature(&state, user.user_id, Feature::CommunityPublish)
        .await?
        .ok_or_else(|| ApiError::unavailable("billing is not configured"))?;
    let item = db::create_community_indicator(
        database.pool(),
        user.user_id,
        &slug_for(&title),
        &db::NewCommunityItem {
            title,
            summary,
            tags,
            author_name,
            author_tier: tier_of(&entitlements).to_string(),
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
    Ok((StatusCode::CREATED, Json(detail(&database, user.user_id, &item).await?)))
}
```

Add version:

```rust
#[derive(Debug, Deserialize)]
pub struct VersionBody {
    pub workspace_id: Uuid,
    #[serde(default)]
    pub revision_id: Option<Uuid>,
    #[serde(default)]
    pub summary: Option<String>,
}

pub async fn add_version(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
    ApiJson(body): ApiJson<VersionBody>,
) -> Result<(StatusCode, Json<CommunityDetail>), ApiError> {
    let database = database(&state)?;
    enforce_feature(&state, user.user_id, Feature::CommunityPublish).await?;
    let item = db::get_community_indicator(database.pool(), &slug).await?.ok_or_else(unknown)?;
    if !item.is_author(user.user_id) {
        return Err(unknown());
    }
    if item.status == "removed" {
        return Err(conflict("COMMUNITY_NOT_LISTED", "a removed indicator cannot gain versions"));
    }
    let count = db::list_community_versions(database.pool(), item.id, MAX_VERSIONS_PER_INDICATOR + 1).await?.len() as i64;
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
        conflict("COMMUNITY_REVISION_NOT_VALIDATED", "the workspace has no revision to publish")
    })?;
    let revision = db::get_indicator_revision(database.pool(), user.user_id, workspace.id, revision_id)
        .await?
        .ok_or_else(|| ApiError::not_found("indicator revision not found"))?;
    if revision.status != "validated" {
        return Err(conflict("COMMUNITY_REVISION_NOT_VALIDATED", "only a validated revision can be published"));
    }
    if revision.source.len() > MAX_COMMUNITY_SOURCE_BYTES {
        return Err(source_invalid(vec![format!("the source exceeds {MAX_COMMUNITY_SOURCE_BYTES} bytes")]));
    }
    revet(&revision.source, &revision.validation, &revision.preview)?;
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
    let refreshed = db::get_community_indicator_by_id(database.pool(), item.id).await?.ok_or_else(unknown)?;
    Ok((StatusCode::CREATED, Json(detail(&database, user.user_id, &refreshed).await?)))
}
```

Patch:

```rust
#[derive(Debug, Deserialize)]
pub struct PatchBody {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub status: Option<String>,
}

pub async fn patch(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
    ApiJson(body): ApiJson<PatchBody>,
) -> Result<Json<CommunityDetail>, ApiError> {
    let database = database(&state)?;
    enforce_feature(&state, user.user_id, Feature::CommunityPublish).await?;
    let item = db::get_community_indicator(database.pool(), &slug).await?.ok_or_else(unknown)?;
    if !item.is_author(user.user_id) {
        return Err(unknown());
    }
    if item.status == "removed" {
        return Err(conflict("COMMUNITY_NOT_LISTED", "a removed indicator cannot be edited"));
    }
    let title = match body.title { Some(value) => clean_title(&value)?, None => item.title.clone() };
    let summary = match body.summary { Some(value) => clean_summary(&value)?, None => item.summary.clone() };
    let tags = match body.tags { Some(value) => clean_tags(&value)?, None => item.tags.clone() };
    let status = match body.status.as_deref() {
        None => item.status.clone(),
        Some("listed") => "listed".to_string(),
        Some("unlisted") => "unlisted".to_string(),
        Some(_) => return Err(metadata("status is `listed` or `unlisted`")),
    };
    let updated = db::update_community_indicator(
        database.pool(), user.user_id, item.id, &title, &summary, &tags, &status,
    )
    .await?
    .ok_or_else(unknown)?;
    Ok(Json(detail(&database, user.user_id, &updated).await?))
}
```

Install:

```rust
pub async fn install(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let database = database(&state)?;
    enforce_feature(&state, user.user_id, Feature::CommunityBrowse).await?;
    let item = db::get_community_indicator(database.pool(), &slug).await?.ok_or_else(unknown)?;
    visibility(&item, user.user_id)?;
    if !item.is_listed() && !item.is_author(user.user_id) {
        return Err(conflict("COMMUNITY_NOT_LISTED", "this indicator is not listed"));
    }
    let version_id = item.current_version_id.ok_or_else(unknown)?;
    let version = db::get_community_version(database.pool(), version_id).await?.ok_or_else(unknown)?;
    revet(&version.source, &version.validation, &version.preview)?;
    let installs = db::count_community_installs(database.pool(), user.user_id).await?;
    if installs >= MAX_INSTALLS_PER_USER {
        return Err(conflict(
            "COMMUNITY_INSTALL_LIMIT_REACHED",
            format!("this account keeps at most {MAX_INSTALLS_PER_USER} installed indicators"),
        ));
    }
    let provenance = format!("Installed {}'s {} v{}", item.author_name, item.title, version.version_number);
    let memory = serde_json::json!({
        "community": {
            "indicator_id": item.id,
            "slug": item.slug,
            "version": version.version_number,
            "author_name": item.author_name,
        }
    });
    match db::install_community_indicator(
        database.pool(), user.user_id, item.id, &version, &item.title, &provenance, &memory,
    )
    .await?
    {
        db::InstallOutcome::Installed { install, revision_id } => Ok(Json(serde_json::json!({
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
        db::InstallOutcome::NotListed => Err(conflict("COMMUNITY_NOT_LISTED", "this indicator is not listed")),
    }
}
```

Update:

```rust
pub async fn update(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let database = database(&state)?;
    enforce_feature(&state, user.user_id, Feature::CommunityBrowse).await?;
    let item = db::get_community_indicator(database.pool(), &slug).await?.ok_or_else(unknown)?;
    visibility(&item, user.user_id)?;
    let version_id = item.current_version_id.ok_or_else(unknown)?;
    let version = db::get_community_version(database.pool(), version_id).await?.ok_or_else(unknown)?;
    revet(&version.source, &version.validation, &version.preview)?;
    let provenance = format!("Updated to {}'s {} v{}", item.author_name, item.title, version.version_number);
    match db::update_community_install(database.pool(), user.user_id, item.id, &version, &provenance).await? {
        db::UpdateOutcome::Updated { revision_id } => Ok(Json(serde_json::json!({
            "workspace_id": item.id,
            "revision_id": revision_id,
            "version": version.version_number,
        }))),
        db::UpdateOutcome::NotInstalled => Err(ApiError::coded(
            StatusCode::CONFLICT, "COMMUNITY_NOT_INSTALLED",
            "install this indicator before updating it",
        )),
        db::UpdateOutcome::AlreadyCurrent => Err(conflict(
            "COMMUNITY_ALREADY_CURRENT", "this install already has the current version",
        )),
        db::UpdateOutcome::NotListed => Err(conflict(
            "COMMUNITY_NOT_LISTED", "this indicator is not listed",
        )),
    }
}
```

Report:

```rust
#[derive(Debug, Deserialize)]
pub struct ReportBody {
    pub reason: String,
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn report(
    State(state): State<AppState>,
    user: UserContext,
    Path(slug): Path<String>,
    ApiJson(body): ApiJson<ReportBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let database = database(&state)?;
    enforce_feature(&state, user.user_id, Feature::CommunityBrowse).await?;
    let item = db::get_community_indicator(database.pool(), &slug).await?.ok_or_else(unknown)?;
    visibility(&item, user.user_id)?;
    let reason = body.reason.trim().to_ascii_lowercase();
    if !matches!(reason.as_str(), "spam" | "misleading" | "malicious" | "copyright" | "other") {
        return Err(ApiError::bad_request(
            "COMMUNITY_METADATA_INVALID",
            "reason is spam, misleading, malicious, copyright or other",
        ));
    }
    let note = body.note.as_deref().map(str::trim).filter(|note| !note.is_empty());
    if note.is_some_and(|note| note.chars().count() > MAX_REPORT_NOTE_CHARS) {
        return Err(ApiError::bad_request(
            "COMMUNITY_METADATA_INVALID",
            format!("a report note is at most {MAX_REPORT_NOTE_CHARS} characters"),
        ));
    }
    let new = db::report_community_indicator(database.pool(), user.user_id, item.id, &reason, note).await?;
    Ok(Json(serde_json::json!({
        "reported": true,
        "already_reported": !new,
    })))
}
```

- [ ] **Step 4: Mount the routes**

In `crates/api-gateway/src/lib.rs`:

```rust
pub mod community_routes;
```

Import `patch` in the routing import:

```rust
use axum::routing::{delete, get, patch, post, put};
```

After the `/billing/*` routes:

```rust
        // Community indicators (`docs/53`).
        .route(
            "/community/indicators",
            get(community_routes::list).post(community_routes::publish),
        )
        .route("/community/indicators/installed", get(community_routes::installed))
        .route("/community/indicators/mine", get(community_routes::mine))
        .route(
            "/community/indicators/{slug}",
            get(community_routes::get).patch(community_routes::patch),
        )
        .route(
            "/community/indicators/{slug}/versions",
            post(community_routes::add_version),
        )
        .route(
            "/community/indicators/{slug}/install",
            post(community_routes::install),
        )
        .route(
            "/community/indicators/{slug}/update",
            post(community_routes::update),
        )
        .route(
            "/community/indicators/{slug}/report",
            post(community_routes::report),
        )
```

- [ ] **Step 5: Fix the test's PUT calls**

Replace every `h.request_put(` with `h.put(` (the harness already has `put`).

- [ ] **Step 6: Run the tests**

Run: `cargo test -p api-gateway --test community_flow`
Expected: PASS (11–12 tests, depending on database availability).

Run: `cargo check --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/api-gateway/src/community_routes.rs crates/api-gateway/src/lib.rs crates/api-gateway/tests/community_flow.rs
git commit -m "feat: community catalog routes with re-vetted publish and forked installs"
```

---

### Task 4: Community UI, publish action, and plan copy

**Files:**
- Modify: `frontend/app/index.html`
- Modify: `frontend/app/app.js`

**Interfaces:**
- Consumes `/community/indicators*` and the `community_browse` / `community_publish` feature codes.
- Produces `loadCommunity()`, `renderCommunity()`, `openCommunityDetail(slug)`, `installCommunity(slug)`, `updateCommunity(slug)`, `reportCommunity(slug)`, `saveCommunityMetadata(slug)`.
- Produces `openPublishForm(wsId, revId)` and `sendPublish()`.

- [ ] **Step 1: Add the tab and pane markup**

In `index.html`, after the Book/Brokers buttons and before Plan:

```html
      <button role="tab" data-pane="community" aria-selected="false">Community</button>
```

Add this pane before `#pane-billing`:

```html
    <div class="pane" id="pane-community" hidden>
      <h2>Community</h2>
      <p class="muted" id="communityLead">
        Indicators published by other users. Install a copy into your own
        workspace; it is yours to edit and will not change under you.
      </p>
      <div id="communityLocked" hidden>
        <p class="empty">Browsing the community library starts on Core.</p>
        <button type="button" id="communityUpgrade">See plans</button>
      </div>
      <div id="communityBody" hidden>
        <div class="row">
          <input id="communitySearch" type="search" placeholder="Search indicators…" />
          <select id="communitySort" title="Order">
            <option value="featured">Featured</option>
            <option value="new">Newest</option>
            <option value="popular">Most installed</option>
          </select>
        </div>
        <div class="row">
          <input id="communityTag" type="text" placeholder="tag" />
          <button type="button" data-community-view="catalog" class="primary">Catalog</button>
          <button type="button" data-community-view="installed">Installed</button>
          <button type="button" data-community-view="mine">Published by me</button>
        </div>
        <div id="communityList"><p class="empty">Open this tab to load the library.</p></div>
        <div id="communityDetail" hidden></div>
        <p id="communityMsg" class="muted"></p>
      </div>
    </div>
```

In the `#wsRevisions` details block, add below the revisions host:

```html
          <div id="wsPublishForm" hidden>
            <div class="row"><input id="wsPublishTitle" placeholder="Title" /></div>
            <div class="row"><input id="wsPublishSummary" placeholder="One-line summary" /></div>
            <div class="row"><input id="wsPublishTags" placeholder="tags, comma separated" /></div>
            <div class="row"><input id="wsPublishName" placeholder="Display name (optional)" /></div>
            <div class="row">
              <button type="button" id="wsPublishSend" class="primary">Publish</button>
              <button type="button" id="wsPublishCancel">Cancel</button>
            </div>
            <p id="wsPublishMsg" class="muted"></p>
          </div>
```

- [ ] **Step 2: Add Community state and loading**

In `app.js`, after the billing section (after `billingUpgradePrompt`), insert:

```js
// ---------------------------------------------------------------------------
// Community indicators (docs/53)
// ---------------------------------------------------------------------------

let communityView = "catalog";
let communityItems = [];
let communityDetailItem = null;
let communityLoading = false;

const communityLevel = () => {
  const features = billingFeatures();
  return features ? features.community_indicators : "none";
};
const canBrowseCommunity = () => ["browse", "publish", "priority"].includes(communityLevel());
const canPublishCommunity = () => ["publish", "priority"].includes(communityLevel());

function communityVisible() {
  const signedIn = Boolean(token());
  const browse = signedIn && canBrowseCommunity();
  el("communityLocked").hidden = browse;
  el("communityBody").hidden = !browse;
  if (!browse) {
    el("communityLocked").querySelector(".empty").textContent = signedIn
      ? "Browsing the community library starts on Core."
      : "Sign in to browse the community library.";
  }
  return browse;
}

async function loadCommunity() {
  const list = el("communityList");
  if (!communityVisible()) return;
  if (communityLoading) return;
  communityLoading = true;
  list.innerHTML = `<p class="empty">Loading…</p>`;
  el("communityMsg").textContent = "";
  try {
    const path =
      communityView === "installed"
        ? "/community/indicators/installed"
        : communityView === "mine"
          ? "/community/indicators/mine"
          : (() => {
              const params = new URLSearchParams();
              const q = el("communitySearch").value.trim();
              const tag = el("communityTag").value.trim();
              if (q) params.set("q", q);
              if (tag) params.set("tag", tag);
              params.set("sort", el("communitySort").value);
              return `/community/indicators?${params}`;
            })();
    const body = await api(path);
    communityItems = Array.isArray(body.items) ? body.items : [];
    if (!communityItems.length) {
      list.innerHTML = `<p class="empty">${
        communityView === "mine"
          ? "You have not published an indicator yet — open an indicator workspace and use Publish."
          : communityView === "installed"
            ? "Nothing installed yet."
            : "No indicators match that search."
      }</p>`;
      return;
    }
    list.innerHTML = communityItems.map(communityCard).join("");
  } catch (e) {
    if (!billingUpgradePrompt(e)) list.innerHTML = `<p class="error">${escapeHtml(e.message)}</p>`;
  } finally {
    communityLoading = false;
  }
}

function communityCard(item) {
  const installed = communityView === "installed";
  const badge = item.author_tier === "priority" ? `<span class="up">priority</span>` : "";
  const tags = (item.tags || []).map((t) => `#${escapeHtml(t)}`).join(" ");
  return `
    <div class="ws-revision">
      <div class="row">
        <strong>${escapeHtml(item.title)}</strong>
        ${badge}
        <span class="muted">v${item.version}</span>
        <span class="spacer"></span>
        <span class="muted">${item.install_count} install${item.install_count === 1 ? "" : "s"}</span>
      </div>
      <div class="muted">${escapeHtml(item.summary)}</div>
      <div class="muted" style="font-size:11px">${escapeHtml(item.author_name)} ${tags}</div>
      <div class="row" style="margin-top:4px">
        <button type="button" data-community-open="${escapeHtml(item.slug)}">Open</button>
        ${installed ? "" : `<button type="button" data-community-install="${escapeHtml(item.slug)}">Install</button>`}
      </div>
    </div>`;
}

async function openCommunityDetail(slug) {
  const host = el("communityDetail");
  try {
    communityDetailItem = await api(`/community/indicators/${encodeURIComponent(slug)}`);
  } catch (e) {
    if (!billingUpgradePrompt(e)) pageToast(e.message);
    return;
  }
  const item = communityDetailItem;
  const versions = (item.versions || [])
    .map((v) => `<li>v${v.version_number} — ${escapeHtml(v.summary)}</li>`)
    .join("");
  const installed = item.installed
    ? `<p class="muted">Installed v${item.installed.version}${
        item.update_available ? " — update available" : " — up to date"
      }</p>`
    : "";
  host.hidden = false;
  host.innerHTML = `
    <h3 class="section">${escapeHtml(item.title)}</h3>
    <p class="muted">${escapeHtml(item.summary)}</p>
    <p class="muted" style="font-size:11px">${escapeHtml(item.author_name)} · v${item.version} · ${
      item.install_count
    } installs</p>
    ${installed}
    <div class="row">
      ${
        item.is_author
          ? `<button type="button" data-community-status="${item.status === "listed" ? "unlisted" : "listed"}">
               ${item.status === "listed" ? "Unlist" : "Relist"}
             </button>`
          : `<button type="button" data-community-install="${escapeHtml(item.slug)}">Install</button>
             <button type="button" data-community-report="${escapeHtml(item.slug)}">Report</button>`
      }
      ${item.installed && item.update_available ? `<button type="button" data-community-update="${escapeHtml(item.slug)}">Update</button>` : ""}
      <button type="button" data-community-close="1">Close</button>
    </div>
    <details><summary>Source</summary><pre style="white-space:pre-wrap;font-size:11px">${escapeHtml(item.source)}</pre></details>
    <details><summary>Versions</summary><ul>${versions}</ul></details>`;
}

async function installCommunity(slug) {
  el("communityMsg").textContent = "installing…";
  try {
    const result = await api(`/community/indicators/${encodeURIComponent(slug)}/install`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    });
    el("communityMsg").textContent = result.already_installed
      ? "Already installed — opening the workspace."
      : "Installed. Opening the workspace.";
    communityView = "installed";
    await loadCommunity();
    openWorkspaceById(result.workspace_id);
  } catch (e) {
    if (!billingUpgradePrompt(e)) el("communityMsg").textContent = e.message;
  }
}

async function updateCommunity(slug) {
  try {
    await api(`/community/indicators/${encodeURIComponent(slug)}/update`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    });
    pageToast("Updated — a new revision is active in your workspace.");
    await openCommunityDetail(slug);
  } catch (e) {
    if (!billingUpgradePrompt(e)) pageToast(e.message);
  }
}

async function reportCommunity(slug) {
  const reason = prompt("Reason: spam, misleading, malicious, copyright, other", "spam");
  if (!reason) return;
  try {
    const result = await api(`/community/indicators/${encodeURIComponent(slug)}/report`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ reason }),
    });
    pageToast(result.already_reported ? "You already reported this." : "Reported.");
  } catch (e) {
    if (!billingUpgradePrompt(e)) pageToast(e.message);
  }
}

async function setCommunityStatus(slug, status) {
  try {
    await api(`/community/indicators/${encodeURIComponent(slug)}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ status }),
    });
    await openCommunityDetail(slug);
  } catch (e) {
    if (!billingUpgradePrompt(e)) pageToast(e.message);
  }
}
```

`openWorkspaceById` does not exist; add it next to `selectPane` usage:

```js
function openWorkspaceById(workspaceId) {
  pendingIndicatorWorkspaceId = workspaceId;
  selectPane("indicator");
  scheduleWorkspaceSave();
}
```

- [ ] **Step 3: Wire the tab, view buttons, and delegated clicks**

In `selectPane`, add:

```js
  activeTabName = name;
  if (name === "community") loadCommunity();
```

In `main()`, after the billing listeners:

```js
  for (const button of document.querySelectorAll("[data-community-view]")) {
    button.addEventListener("click", () => {
      communityView = button.dataset.communityView;
      for (const other of document.querySelectorAll("[data-community-view]")) {
        other.classList.toggle("primary", other === button);
      }
      el("communityDetail").hidden = true;
      loadCommunity();
    });
  }
  el("communitySearch").addEventListener("keydown", (event) => {
    if (event.key === "Enter") loadCommunity();
  });
  el("communityTag").addEventListener("change", loadCommunity);
  el("communitySort").addEventListener("change", loadCommunity);
  el("communityUpgrade").addEventListener("click", () => selectPane("billing"));
  el("pane-community").addEventListener("click", (event) => {
    const open = event.target.closest("[data-community-open]");
    if (open) return openCommunityDetail(open.dataset.communityOpen);
    const install = event.target.closest("[data-community-install]");
    if (install) return installCommunity(install.dataset.communityInstall);
    const update = event.target.closest("[data-community-update]");
    if (update) return updateCommunity(update.dataset.communityUpdate);
    const report = event.target.closest("[data-community-report]");
    if (report) return reportCommunity(report.dataset.communityReport);
    const status = event.target.closest("[data-community-status]");
    if (status) return setCommunityStatus(status.dataset.communityStatus, status.dataset.communityStatus);
    if (event.target.closest("[data-community-close]")) el("communityDetail").hidden = true;
  });
```

Note the status button already carries the target status in `data-community-status`, so `setCommunityStatus(status.dataset.communityStatus, status.dataset.communityStatus)` is wrong; fix the markup to use:

```html
<button type="button" data-community-status="${escapeHtml(item.slug)}" data-community-next="${item.status === "listed" ? "unlisted" : "listed"}">
```

and the handler:

```js
    const status = event.target.closest("[data-community-status]");
    if (status) return setCommunityStatus(status.dataset.communityStatus, status.dataset.communityNext);
```

- [ ] **Step 4: Publish from the revision row**

In `loadRevisions`, add a Publish button for each validated revision:

```js
          ${r.status === "validated" ? `<button onclick="openPublishForm('${wsId}','${r.id}')" title="Publish this revision to the community">Publish</button>` : ""}
```

Add the functions after `loadRevisions`:

```js
let wsPublishTarget = null;

function openPublishForm(wsId, revId) {
  if (!canPublishCommunity()) {
    pageToast("Publishing community indicators starts on Elite.");
    selectPane("billing");
    return;
  }
  const ws = wsWorkspaces.find((w) => w.id === wsId);
  wsPublishTarget = { wsId, revId };
  el("wsPublishForm").hidden = false;
  el("wsPublishTitle").value = (ws && ws.name) || "My indicator";
  el("wsPublishSummary").value = "";
  el("wsPublishTags").value = "";
  el("wsPublishName").value = "";
  el("wsPublishMsg").textContent = "";
}

async function sendPublish() {
  if (!wsPublishTarget) return;
  const title = el("wsPublishTitle").value.trim();
  const summary = el("wsPublishSummary").value.trim();
  const tags = el("wsPublishTags").value
    .split(",")
    .map((tag) => tag.trim())
    .filter(Boolean);
  const authorName = el("wsPublishName").value.trim();
  const msg = el("wsPublishMsg");
  msg.textContent = "publishing…";
  try {
    const body = await api("/community/indicators", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        workspace_id: wsPublishTarget.wsId,
        revision_id: wsPublishTarget.revId,
        title,
        summary: summary || undefined,
        tags,
        author_name: authorName || undefined,
      }),
    });
    msg.textContent = `Published as ${body.slug}.`;
    el("wsPublishForm").hidden = true;
    pageToast(`Published "${body.title}" to the community.`);
  } catch (e) {
    if (!billingUpgradePrompt(e)) msg.textContent = e.message;
  }
}
```

Wire in `main()`:

```js
  el("wsPublishSend").addEventListener("click", sendPublish);
  el("wsPublishCancel").addEventListener("click", () => { el("wsPublishForm").hidden = true; });
```

- [ ] **Step 5: Update the plan cards and prompt codes**

In `app.js`:

- Remove `"COMMUNITY_INDICATORS_UNAVAILABLE"` from `BILLING_PROMPT_CODES`.
- Delete the `if (error.code === "COMMUNITY_INDICATORS_UNAVAILABLE")` branch in `billingUpgradePrompt`.
- Update the billing section comment to say the community library is real.
- Replace the plan-card community text:

```js
      const community = plan.features.community_indicators;
      const communityText =
        community === "none"
          ? "not included"
          : community === "browse"
            ? "browse + install"
            : community === "publish"
              ? "browse + publish"
              : "browse + publish + priority placement";
```

and remove `communityAvailable`.

- [ ] **Step 6: Add `activeTabName` and persistence hooks**

Near `let activePane = null;`:

```js
let activeTabName = "chat";
```

In `selectPane`, set it and schedule a workspace save.

In `index.html`, update the billing pane's comment to remove "reserved".

- [ ] **Step 7: Syntax-check and manually verify**

Run: `node --check frontend/app/app.js`
Expected: PASS.

Manual (browser, signed in as Elite):
1. Open Plan → cards say "browse + publish".
2. Open an indicator workspace → Revisions → Publish on a validated revision → fill title → Publish.
3. Open Community → the item appears with the author name and tags.
4. Sign in as a Core account → Install the item → a new workspace opens with revision 1 and the same source.
5. Publish a second version as the author → Core's Community → Installed shows "update available" → Update → revision 2 is active.
6. Unlist as the author → Core no longer sees it in the catalog but keeps the workspace.

- [ ] **Step 8: Commit**

```bash
git add frontend/app/index.html frontend/app/app.js
git commit -m "feat: a community tab, publishing from the revision row, and real plan copy"
```

---

## Phase 2 — Workspace Persistence

### Task 5: `workspace_state` schema, DB layer, and routes

**Files:**
- Create: `crates/db/migrations/0017_workspace_state.sql`
- Create: `crates/db/src/workspace_state.rs`
- Modify: `crates/db/src/lib.rs`
- Create: `crates/api-gateway/src/workspace_routes.rs`
- Modify: `crates/api-gateway/src/lib.rs`
- Create: `crates/api-gateway/tests/workspace_flow.rs`

**Interfaces:**
- Produces `db::get_workspace_state(pool, user_id) -> Result<Option<Value>, DbError>`.
- Produces `db::put_workspace_state(pool, user_id, &Value) -> Result<i64, DbError>` returning `updated_at` in nanoseconds.
- Produces `GET /workspace` → `{ "state": Value | null }`.
- Produces `PUT /workspace` (body `{ "state": Value }`) → `{ "saved": true, "updated_at": i64 }`.

- [ ] **Step 1: Write the failing integration test**

`crates/api-gateway/tests/workspace_flow.rs`:

```rust
//! Workspace persistence (`docs/54`): one opaque, owner-scoped snapshot.

mod common;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn a_workspace_round_trips_and_is_owner_scoped() {
    let Some(h) = common::Harness::new().await else { return };
    let alice = h.register().await;
    let bob = h.register().await;

    let empty = h.ok("/workspace", Some(&alice.token)).await;
    assert!(empty["state"].is_null());

    let state = json!({
        "version": 1,
        "layout": { "mode": "grid", "columns": 3 },
        "activePane": 1,
        "activeTab": "community",
        "pans": [],
        "panes": [
            { "symbol": "BTCUSDT", "timeframe": "5m", "mode": "candles", "limit": "500", "indicator": null, "scripts": [] },
            { "symbol": "ETHUSDT", "timeframe": "15m", "mode": "line", "limit": "1000", "indicator": null, "scripts": [] }
        ]
    });
    let (status, body) = h.put("/workspace", json!({ "state": state }), Some(&alice.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let read = h.ok("/workspace", Some(&alice.token)).await;
    assert_eq!(read["state"], state);

    let other = h.ok("/workspace", Some(&bob.token)).await;
    assert!(other["state"].is_null(), "another user must not see Alice's state");

    alice.cleanup(&h.database).await;
    bob.cleanup(&h.database).await;
}

#[tokio::test]
async fn an_invalid_state_is_refused() {
    let Some(h) = common::Harness::new().await else { return };
    let user = h.register().await;

    for bad in [
        json!({ "state": "not an object" }),
        json!({ "state": { "version": 2, "panes": [] } }),
        json!({ "state": { "version": 1, "panes": [] } }),
        json!({ "state": { "version": 1, "panes": [{}; 101] } }),
    ] {
        let (status, body) = h.put("/workspace", bad, Some(&user.token)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "WORKSPACE_STATE_INVALID");
    }

    user.cleanup(&h.database).await;
}
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -p api-gateway --test workspace_flow`
Expected: FAIL — `/workspace` 404.

- [ ] **Step 3: Write the migration**

`crates/db/migrations/0017_workspace_state.sql`:

```sql
-- The shell's workspace, as one opaque snapshot (`docs/54`).
--
-- A pane is a view, not a saved layout: it must not consume the plan's layout
-- allowance, and restoring it is one read rather than N chart-session reads.
-- The server stores the blob the shell wrote; it only enforces ownership,
-- shape and size.

CREATE TABLE IF NOT EXISTS workspace_state (
    user_id    UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    state      JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

- [ ] **Step 4: Write the DB module**

`crates/db/src/workspace_state.rs`:

```rust
//! The workspace the shell restores on load (`docs/54`).

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbError;

/// The caller's stored workspace snapshot, if one has been saved.
pub async fn get_workspace_state(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<Value>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT state FROM workspace_state WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?)
}

/// Replace the caller's snapshot and return its `updated_at` in nanoseconds.
pub async fn put_workspace_state(
    pool: &PgPool,
    user_id: Uuid,
    state: &Value,
) -> Result<i64, DbError> {
    let updated: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "INSERT INTO workspace_state (user_id, state) VALUES ($1, $2) \
         ON CONFLICT (user_id) DO UPDATE SET state = EXCLUDED.state, updated_at = now() \
         RETURNING updated_at",
    )
    .bind(user_id)
    .bind(state)
    .fetch_one(pool)
    .await?;
    Ok(crate::repositories::dt_to_ns(updated))
}
```

Export in `lib.rs`:

```rust
pub mod workspace_state;
...
pub use workspace_state::{get_workspace_state, put_workspace_state};
```

- [ ] **Step 5: Write the routes**

`crates/api-gateway/src/workspace_routes.rs`:

```rust
//! `/workspace`: the shell's durable workspace (`docs/54`).
//!
//! The server stores one opaque JSON object and enforces three things: it is
//! an object, it fits the size bound, and it belongs to the caller. The shape
//! inside is the shell's, because the shell is the only writer and the only
//! reader -- a second interpretation here would drift from the page.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::AppState;

/// The largest snapshot stored, in bytes.
const MAX_STATE_BYTES: usize = 2 * 1024 * 1024;
/// The largest number of panes a snapshot may describe.
const MAX_PANES: usize = 100;
/// The only snapshot version this release writes.
const STATE_VERSION: i64 = 1;

/// `GET /workspace`
#[derive(Debug, Serialize)]
pub struct WorkspaceResponse {
    /// The snapshot, or null when nothing has been saved.
    pub state: Option<serde_json::Value>,
}

/// `PUT /workspace`'s body.
#[derive(Debug, Deserialize)]
pub struct SaveWorkspaceBody {
    /// The snapshot.
    pub state: serde_json::Value,
}

/// `PUT /workspace`'s response.
#[derive(Debug, Serialize)]
pub struct SavedResponse {
    /// Always true; a failed save is an error response.
    pub saved: bool,
    /// Unix milliseconds.
    pub updated_at: i64,
}

fn database(state: &AppState) -> Result<Arc<db::Database>, ApiError> {
    state.db.clone().ok_or_else(|| {
        ApiError::unavailable("the workspace store needs the database; it is not configured")
    })
}

fn validate(state: &serde_json::Value) -> Result<(), ApiError> {
    let Some(object) = state.as_object() else {
        return Err(ApiError::bad_request(
            "WORKSPACE_STATE_INVALID",
            "the workspace state is a JSON object",
        ));
    };
    if object.get("version").and_then(|v| v.as_i64()) != Some(STATE_VERSION) {
        return Err(ApiError::bad_request(
            "WORKSPACE_STATE_INVALID",
            format!("the workspace state is version {STATE_VERSION}"),
        ));
    }
    let panes = object.get("panes").and_then(|v| v.as_array()).ok_or_else(|| {
        ApiError::bad_request(
            "WORKSPACE_STATE_INVALID",
            "the workspace state carries a `panes` list",
        )
    })?;
    if panes.is_empty() || panes.len() > MAX_PANES {
        return Err(ApiError::bad_request(
            "WORKSPACE_STATE_INVALID",
            format!("a workspace has 1-{MAX_PANES} panes"),
        ));
    }
    let bytes = serde_json::to_vec(state)
        .map_err(|error| ApiError::bad_request("WORKSPACE_STATE_INVALID", error.to_string()))?
        .len();
    if bytes > MAX_STATE_BYTES {
        return Err(ApiError::bad_request(
            "WORKSPACE_STATE_INVALID",
            format!("the workspace state is {bytes} bytes; the limit is {MAX_STATE_BYTES}"),
        ));
    }
    Ok(())
}

/// `GET /workspace`
pub async fn get(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<WorkspaceResponse>, ApiError> {
    let database = database(&state)?;
    let saved = db::get_workspace_state(database.pool(), user.user_id).await?;
    Ok(Json(WorkspaceResponse { state: saved }))
}

/// `PUT /workspace`
pub async fn put(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<SaveWorkspaceBody>,
) -> Result<Json<SavedResponse>, ApiError> {
    let database = database(&state)?;
    validate(&body.state)?;
    let updated_at = db::put_workspace_state(database.pool(), user.user_id, &body.state).await?;
    Ok(Json(SavedResponse {
        saved: true,
        updated_at: updated_at / 1_000_000,
    }))
}
```

`GET` returns only `state`: the shell has no use for the timestamp, and a field that is always null is a lie. `PUT` keeps `updated_at`, which is the honest "this save landed" acknowledgement.

- [ ] **Step 6: Mount the routes**

In `lib.rs`:

```rust
pub mod workspace_routes;
```

and after the chart-session routes:

```rust
        .route(
            "/workspace",
            get(workspace_routes::get).put(workspace_routes::put),
        )
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p api-gateway --test workspace_flow`
Expected: PASS.

Run: `cargo check --workspace`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/db/migrations/0017_workspace_state.sql crates/db/src/workspace_state.rs \
        crates/db/src/lib.rs crates/api-gateway/src/workspace_routes.rs \
        crates/api-gateway/src/lib.rs crates/api-gateway/tests/workspace_flow.rs
git commit -m "feat: store the shell workspace as one owner-scoped snapshot"
```

---

### Task 6: Restore the workspace in the shell

**Files:**
- Modify: `frontend/app/app.js`

**Interfaces:**
- Produces `pane.snapshot()` and `pane.applyState(saved)`.
- Produces `workspaceSnapshot()`, `scheduleWorkspaceSave()`, `saveWorkspaceState()`, `loadWorkspaceState()`, `restoreWorkspace(state, entries)`.
- Consumes `GET/PUT /workspace`.

- [ ] **Step 1: Add the pane snapshot API**

Inside `createChartPane`'s `paneApi`, after `attachedScripts()`:

```js
    /// The durable half of this pane: what a refresh must bring back.
    snapshot() {
      return {
        symbol: el("symbol").value,
        timeframe: el("timeframe").value,
        mode: el("mode").value,
        limit: el("limit").value,
        indicator: indicator ? JSON.parse(JSON.stringify(indicator)) : null,
        scripts: attachedScripts.map((script) => ({
          source: script.source,
          inputs: script.inputs || {},
          name: script.name,
          inputsSpec: Array.isArray(script.inputsSpec) ? script.inputsSpec : [],
          visible: script.visible !== false,
        })),
      };
    },

    /// Re-apply a snapshot's series-independent half. The page fills the
    /// series first; this restores the mode, the bar limit and the layers.
    applyState(saved) {
      if (!saved) return;
      if (saved.mode) el("mode").value = saved.mode;
      if (saved.limit) el("limit").value = saved.limit;
      indicator = saved.indicator || null;
      attachedScripts = (Array.isArray(saved.scripts) ? saved.scripts : [])
        .filter((script) => script && typeof script.source === "string")
        .map((script) => ({
          source: script.source,
          inputs: script.inputs || {},
          name: script.name,
          inputsSpec: Array.isArray(script.inputsSpec) ? script.inputsSpec : [],
          visible: script.visible !== false,
        }));
      layerSettingsFor = null;
      renderNow();
      syncIndicatorChip();
      noteWorkspaceChange();
    },
```

- [ ] **Step 2: Make `addPane` able to build a restored pane**

At the top of `addPane`, add an optional `saved` argument:

```js
function addPane(saved = null) {
```

After `pane.reset();`, guard the default fill:

```js
  if (!saved) {
    const wanted = Number(node.querySelector(".limit").value) || 0;
    pane.fillSeries(
      coverage,
      activePane.symbol(),
      nextFrame(activePane.symbol(), activePane.timeframe(), wanted)
    );
  }
```

- [ ] **Step 3: Add the workspace state module**

Insert after `persistLayout()`:

```js
// ---------------------------------------------------------------------------
// The workspace snapshot (docs/54)
//
// The page is a set of panes, and a refresh used to lose all of them: the
// panes existed only in memory. The server keeps one opaque snapshot per
// user; the shell writes it on change (debounced) and restores it before the
// first fetch. Signed-out visitors keep the old localStorage-only behavior.
// ---------------------------------------------------------------------------

let workspaceReady = false;
let workspaceSaveTimer = 0;
let workspaceSavedJSON = "";
let pendingIndicatorWorkspaceId = null;

/// The snapshot the server stores. `version` is the schema the shell wrote;
/// the route refuses anything else, so an old tab cannot corrupt a new page.
function workspaceSnapshot() {
  return {
    version: 1,
    layout: { mode: layoutMode, columns: gridColumns },
    activePane: Math.max(0, panes.indexOf(activePane)),
    activeTab: activeTabName,
    indicatorWorkspaceId: wsActiveId,
    panes: panes.map((pane) => pane.snapshot()),
  };
}

/// Persist soon, not on every candle. A burst of attach/change events is one
/// write; the window is short enough that a refresh a second later still sees it.
function scheduleWorkspaceSave() {
  if (!workspaceReady || !token()) return;
  if (workspaceSaveTimer) return;
  workspaceSaveTimer = setTimeout(() => {
    workspaceSaveTimer = 0;
    saveWorkspaceState();
  }, 800);
}

/// Called from the pane API and the page's own mutation points.
function noteWorkspaceChange() {
  scheduleWorkspaceSave();
}

async function saveWorkspaceState() {
  if (!workspaceReady || !token()) return;
  const snapshot = workspaceSnapshot();
  const encoded = JSON.stringify(snapshot);
  if (encoded === workspaceSavedJSON) return;
  try {
    await api("/workspace", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ state: snapshot }),
    });
    workspaceSavedJSON = encoded;
  } catch {
    // A lost save is not worth a toast mid-drag; the next change retries.
  }
}

/// Read the snapshot, or null when signed out / nothing saved.
async function loadWorkspaceState() {
  if (!token()) return null;
  try {
    const body = await api("/workspace");
    return body && body.state ? body.state : null;
  } catch {
    return null;
  }
}
```

`wsActiveId` is declared later in the file with `let`; referencing it inside the function body is fine because the function runs after initialization. `activeTabName` is declared near panes.

- [ ] **Step 4: Restore the layout and panes**

Add after `openFirstPane()` / `closePane`:

```js
/// Apply the saved layout without losing the panes already built.
function applyWorkspaceLayout(saved) {
  const layout = saved && saved.layout;
  if (!layout) return;
  if (layout.mode === "grid" || layout.mode === "split") {
    if (layoutMode !== layout.mode) setLayoutMode(layout.mode);
  }
  const columns = Number(layout.columns);
  if (Number.isInteger(columns) && columns >= 1 && columns <= 10 && columns !== gridColumns) {
    setGridColumns(columns);
  }
}

/// Bring the page back to a saved snapshot. Called once after coverage loads.
async function restoreWorkspace(saved, entries) {
  if (!saved || !Array.isArray(saved.panes) || !saved.panes.length) {
    workspaceReady = true;
    scheduleWorkspaceSave();
    return false;
  }
  applyWorkspaceLayout(saved);

  // Trim or grow to the saved count. `closePane` refuses the last pane, and
  // the snapshot already guarantees at least one; the loop stops at one.
  while (panes.length > saved.panes.length && panes.length > 1) {
    closePane(panes[panes.length - 1]);
  }
  while (panes.length < saved.panes.length && panes.length < paneCap()) {
    if (!addPane({})) break;
  }

  for (let index = 0; index < panes.length; index += 1) {
    const pane = panes[index];
    const state = saved.panes[index] || {};
    pane.applyState(state);
    pane.fillSeries(entries, state.symbol || undefined, state.timeframe || undefined);
  }

  const active = Number(saved.activePane);
  if (Number.isInteger(active) && active >= 0 && active < panes.length) {
    setActive(panes[active]);
  }
  if (typeof saved.activeTab === "string") selectPane(saved.activeTab);
  if (saved.indicatorWorkspaceId) pendingIndicatorWorkspaceId = saved.indicatorWorkspaceId;

  workspaceReady = true;
  workspaceSavedJSON = JSON.stringify(workspaceSnapshot());
  return true;
}
```

- [ ] **Step 5: Wire restore into boot**

In the `boot()` function (the async one containing `loadCoverage`), replace:

```js
  const entries = await loadCoverage();
  for (const pane of panes) pane.fillSeries(entries, pane.symbol() || undefined);
```

with:

```js
  const entries = await loadCoverage();
  const savedWorkspace = await loadWorkspaceState();
  const restored = await restoreWorkspace(savedWorkspace, entries);
  if (!restored) {
    for (const pane of panes) pane.fillSeries(entries, pane.symbol() || undefined);
    workspaceReady = true;
    scheduleWorkspaceSave();
  }
```

and the first refresh becomes every pane's:

```js
  for (const pane of panes) {
    if (pane.symbol()) pane.refresh().then(() => pane.connectLive());
  }
```

- [ ] **Step 6: Save on every workspace mutation**

Add `scheduleWorkspaceSave()` at the end of:
- `setActive` (after `activePane = pane;` and the UI updates)
- `addPane` (before `return pane;`)
- `closePane` (after the splice and active fix)
- `setLayoutMode`
- `setGridColumns`
- `selectPane` (already added `activeTabName`; add the save)
- `pane.attachIndicator`
- `pane.attachScript`
- `pane.clearScripts`
- `pane.removeScriptAt`
- `pane.setScriptVisible`
- `pane.fillSeries` (after `paintTitle()`)
- the delegated `change` listener on `#charts`
- the mode select's own `change` listener
- `wsActiveId` change (where the workspace list selects a row)

If `scheduleWorkspaceSave` is declared later, function declarations hoist, so the calls are safe.

- [ ] **Step 7: Restore after sign-in**

In `authenticate`, after `loadBilling();`:

```js
    loadWorkspaceState().then((saved) => {
      if (saved) restoreWorkspace(saved, coverage);
    });
```

`restoreWorkspace` calls `pane.refresh()` only through the caller; add a refresh after restore in `authenticate`:

```js
    loadWorkspaceState().then(async (saved) => {
      if (!saved) return;
      await restoreWorkspace(saved, coverage);
      for (const pane of panes) {
        if (pane.symbol()) pane.refresh().then(() => pane.connectLive());
      }
    });
```

- [ ] **Step 8: Honor the saved indicator workspace**

In `loadWorkspaces`, when choosing the default active workspace, prefer `pendingIndicatorWorkspaceId`:

```js
  const preferred = pendingIndicatorWorkspaceId;
  pendingIndicatorWorkspaceId = null;
  const first = (preferred && wsWorkspaces.find((ws) => ws.id === preferred)) || wsWorkspaces[0];
```

Use `first` where the function currently selects `wsWorkspaces[0]`. When `wsActiveId` changes, call `scheduleWorkspaceSave()`.

- [ ] **Step 9: Clear the workspace on sign-out**

Where the sign-out path calls `setToken(null)` / removes `TOKEN_KEY`, set:

```js
  workspaceReady = false;
  workspaceSavedJSON = "";
```

so a signed-out page never writes over the account's snapshot.

- [ ] **Step 10: Syntax-check and manually verify**

Run: `node --check frontend/app/app.js`
Expected: PASS.

Manual:
1. Sign in, add a second pane, set it to ETHUSDT 15m, attach an indicator and a script layer, switch to the Book tab.
2. Refresh. Expect: two panes, same symbols and timeframes, same mode/limit, the indicator chip and script chips restored, Book selected.
3. Change layout to Grid 3 columns, add a third pane, refresh. Expect: grid with three panes.
4. Sign out, refresh. Expect: one pane and the default layout; sign back in and refresh. Expect the saved workspace returns.

- [ ] **Step 11: Commit**

```bash
git add frontend/app/app.js
git commit -m "fix: a refresh restores the workspace instead of discarding it"
```

---

## Task 7: Documentation and final verification

**Files:**
- Create: `docs/53-COMMUNITY-INDICATORS.md`
- Create: `docs/54-WORKSPACE-PERSISTENCE.md`
- Modify: `docs/52-SUBSCRIPTIONS.md`

- [ ] **Step 1: Write `docs/53-COMMUNITY-INDICATORS.md`**

Cover, in the repo's doc style:
- purpose and what is published (a validated revision, never a skill or private memory);
- the four tables and why versions are immutable;
- publish → re-vet → list; reports are an audit trail; author unlist; operator `removed`;
- install = fork in one transaction, one install per user, explicit update appends a revision;
- entitlement table (browse Core+, publish Elite+, priority Desk);
- every route and error code from the spec;
- abuse bounds as platform constants, not plan limits;
- what is deferred (moderation console, ratings, follows, payouts, community skills/strategies, automatic updates).

- [ ] **Step 2: Write `docs/54-WORKSPACE-PERSISTENCE.md`**

Cover:
- the bug (panes existed only in memory; drawings already survived because they are server-backed);
- the snapshot shape and that the server treats it as opaque;
- why a dedicated table instead of reusing `chart_sessions` (a pane is a view, not a saved layout; no plan slot; one read/write);
- restore order (layout → pane count → series → layers → active pane → tab);
- the debounce and the `workspaceReady` gate;
- what is not persisted (chart viewport/zoom, unsaved tool state, transient popovers) and why.

- [ ] **Step 3: Replace the reserved section in `docs/52-SUBSCRIPTIONS.md`**

- Change the "no paid feature is advertised before it exists" bullet: community indicators now exist and are enforced.
- Replace `COMMUNITY_INDICATORS_UNAVAILABLE (501)` in the error table with `PLAN_FEATURE_REQUIRED (402)` for `community_browse` / `community_publish`.
- Replace the "reserved" section with the real ladder table and a pointer to `docs/53`.
- Update the frontend-copy bullet to say the plan cards show browse/publish/priority.
- Remove the availability `false` wording; state it is true whenever a database is configured.

- [ ] **Step 4: Final verification**

Run, in order:

```bash
cargo check --workspace
cargo test -p api-gateway --lib
cargo test -p api-gateway --test community_flow
cargo test -p api-gateway --test workspace_flow
cargo test -p api-gateway --test billing_flow
node --check frontend/app/app.js
git diff --check
```

Expected: all pass; `git diff --check` silent (no whitespace errors).

- [ ] **Step 5: Commit**

```bash
git add docs/52-SUBSCRIPTIONS.md docs/53-COMMUNITY-INDICATORS.md docs/54-WORKSPACE-PERSISTENCE.md
git commit -m "docs: community indicators and workspace persistence"
```
