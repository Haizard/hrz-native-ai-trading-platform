# Community Indicators — Design

## Purpose

Let a user publish a validated indicator they built in the AI indicator studio, let
another user discover it, install a pinned copy into their own workspace, and attach it
to a chart. The library is open to every plan, the way a script library works on
TradingView, LuxAlgo and MT5: an account is the only requirement. Desk's extra is
priority placement, not access.

This is infrastructure, not a marketplace. It creates the durable objects, the install
path and the UI. Ratings, follows, payouts and an operator moderation console are out of
scope for this release.

## Decisions taken

These were the three open design questions. Each is settled here so the
implementation has one answer.

### 1. What is published: an indicator revision, not a skill

A **skill** is a private methodology document that steers the AI; publishing one would
be publishing a prompt, not a usable chart artifact. An **indicator workspace
revision** is the runnable artifact: validated source, a plain-language summary, a
preview and the market context it was built against. It is what a chart attaches and
what an alert or bot pins.

So a community item is a **snapshot of one validated revision** plus catalog metadata.
The workspace itself stays private. Publishing does not grant anyone access to the
author's chat, memory, alerts or bots.

`skills` remains a private library. A future community-skills feature can reuse this
shape, but it is not this feature.

### 2. Moderation: list after revalidation, report and unlist

The platform has no moderator role or console, and inventing one now would ship a
review queue nobody can work. The source is also not arbitrary native code: it is
Pine-lite or the strategy DSL, interpreted with the existing bounded interpreter, and
it has already passed the same validator the workspace uses.

Publication therefore:

1. requires a `validated` revision;
2. **re-validates the source against the current engine** (a stored validation can be
   stale after a validator change);
3. rejects anything that no longer parses or whose preview no longer passes
   `IndicatorOutput::validate`;
4. lists the item immediately.

Abuse handling is deliberately small and honest:

- any browsing user may **report** an item once, with a reason and optional note;
- reports are stored and counted, and do **not** auto-hide the item (auto-hiding is a
  brigade target);
- the author may **unlist** their own item at any time;
- a deployment operator can mark an item `removed` with a direct database update until
  a moderation console exists. The status vocabulary and the author-side refusal are
  in place for that console; the console is not.

Desk's `priority` level is implemented as **priority placement**: items published by a
Desk subscriber carry an `author_tier = 'priority'` badge and sort first in the default
catalog order. The matrix comment changes from "review priority" to "priority
placement" to match what is actually delivered.

### 3. Install semantics: fork a pinned version, update explicitly

Installing **copies** the published version into a new workspace owned by the
installer. It does not reference the author's row live.

Why: revisions are immutable and ownership-scoped everywhere else. A live reference
would make the installer's chart change when the author publishes, would break if the
author unlists, and would give the installer a workspace it cannot own or edit.
A fork keeps every existing invariant.

- The new workspace starts with one revision, `revision_number = 1`, the published
  source, summary, validation and preview, and a `change_summary` naming the author and
  version.
- Provenance is also written into the workspace's `memory` so the indicator AI knows
  where the code came from.
- One install per user per indicator.
- The author's later versions do not auto-update the installer. The catalog shows
  "update available"; the installer's explicit **Update** appends the new version as a
  new revision (`parent = active`) and makes it active. Earlier revisions, including the
  installer's own edits, stay in history and can be restored.
- Unlisting stops new installs and updates but does not touch copies already installed.

The installed workspace's symbol and timeframe come from a snapshot stored on the
published version, so the catalog does not need access to the author's workspace row.

## Architecture

```text
community_indicators          one catalog row per published item
  └── community_indicator_versions   immutable snapshots, one per publish/update
        └── community_indicator_installs   one row per (installer, item) → owned workspace
community_indicator_reports   one row per (reporter, item)
```

The published item is a **catalog projection** plus **immutable versions**. Metadata
(title, summary, tags, listing status) may change; a version never does. `current_version_id`
is the one an installer receives.

Install and update both write into the existing `indicator_workspaces` /
`indicator_revisions` tables, so charts, alerts, bot drafts and the indicator studio
need no new read path. They already resolve an owned workspace to its active revision.

## Data model (migration `0016_community_indicators.sql`)

### `community_indicators`

```sql
id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
author_user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
slug              TEXT NOT NULL UNIQUE,
title             TEXT NOT NULL,
summary           TEXT NOT NULL,
tags              TEXT[] NOT NULL DEFAULT '{}',
author_name       TEXT NOT NULL,
author_tier       TEXT NOT NULL DEFAULT 'publish'
                  CHECK (author_tier IN ('publish', 'priority')),
status            TEXT NOT NULL DEFAULT 'listed'
                  CHECK (status IN ('listed', 'unlisted', 'removed')),
current_version_id UUID,
install_count     BIGINT NOT NULL DEFAULT 0 CHECK (install_count >= 0),
report_count      INTEGER NOT NULL DEFAULT 0 CHECK (report_count >= 0),
created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
```

- `slug` is immutable. It is derived from the title plus a short random suffix, so a
  later title edit does not break links.
- `author_name` is a display name stored **per item**, not on `users`. The email in
  `users` is never exposed. If the caller omits a name, the server stores
  `Trader-{6 hex from the user id}`.
- `author_tier` is the publisher's entitlement at publish time. It is a snapshot, not a
  join: a Desk subscriber who later downgrades keeps the badge on what they already
  published, and a re-publish does not silently change it.
- `install_count` is **cumulative**. Deleting the installed workspace removes the
  install row but does not decrement the count: the metric answers "how many times was
  this installed", not "how many copies exist now".
- `current_version_id` gets its foreign key in an `ALTER TABLE` after the versions table
  exists, the same shape `indicator_workspaces.active_revision_id` uses.
- `status` is soft. Nothing in the feature hard-deletes a catalog row; deleting the
  author cascades, which is the account-deletion rule everywhere else.

Indexes:

```sql
(status, author_tier DESC, install_count DESC, created_at DESC)  -- default/featured
(status, created_at DESC)                                          -- newest
(status, install_count DESC, created_at DESC)                      -- popular
USING GIN (tags)
(author_user_id, created_at DESC)                                  -- author's shelf
```

### `community_indicator_versions`

```sql
id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
indicator_id    UUID NOT NULL REFERENCES community_indicators (id) ON DELETE CASCADE,
version_number  INTEGER NOT NULL,
source          TEXT NOT NULL,
summary         TEXT NOT NULL,
symbol          TEXT NOT NULL,
timeframe       TEXT NOT NULL,
validation      JSONB NOT NULL,
preview         JSONB NOT NULL,
published_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
UNIQUE (indicator_id, version_number)
```

`symbol` and `timeframe` are the workspace context at publish time. They are needed to
create the installer's workspace without reading the author's private workspace.

`validation` and `preview` are the same shapes the workspace already stores, so the
install path can copy them and the existing chart/revision UI can read them unchanged.

### `community_indicator_installs`

```sql
id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
indicator_id  UUID NOT NULL REFERENCES community_indicators (id) ON DELETE CASCADE,
user_id       UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
workspace_id  UUID NOT NULL REFERENCES indicator_workspaces (id) ON DELETE CASCADE,
version_id    UUID NOT NULL REFERENCES community_indicator_versions (id),
installed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
UNIQUE (user_id, indicator_id)
```

- The unique key is the idempotency key for Install: a double-click returns the existing
  install rather than creating a second workspace.
- `ON DELETE CASCADE` on `workspace_id` means deleting the workspace also ends the
  install, so the item can be installed again.
- `version_id` is the version currently applied. It is not the revision id (which lives
  in the installer's workspace).

### `community_indicator_reports`

```sql
id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
indicator_id      UUID NOT NULL REFERENCES community_indicators (id) ON DELETE CASCADE,
reporter_user_id  UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
reason            TEXT NOT NULL
                  CHECK (reason IN ('spam', 'misleading', 'malicious', 'copyright', 'other')),
note              TEXT,
created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
UNIQUE (indicator_id, reporter_user_id)
```

The unique key makes a report idempotent and prevents one account from inflating
`report_count`.

### Abuse bounds (constants, not plan limits)

The subscription matrix does not gain columns for these; they are platform bounds, not
priced allowances:

- `MAX_PUBLISHED_INDICATORS = 25` per author;
- `MAX_VERSIONS_PER_INDICATOR = 50`;
- `MAX_INSTALLS_PER_USER = 100`;
- `MAX_COMMUNITY_SOURCE_BYTES = 64 * 1024`;
- `MAX_COMMUNITY_TAGS = 5`;
- title 3–80 characters, summary 1–280, tag 1–24, author name 1–40, report note ≤ 500.

They are enforced server-side and named in the error details so the UI can explain the
refusal.

## Lifecycle

```text
publish (validated revision, re-vetted, public or private)
  → listed (public) or unlisted (private)
      → author publishes a new version (listed item's version N+1)
      → author unlists → unlisted → author relists → listed
      → operator removes → removed (terminal for the author)
      → many reports → still listed; reports are an audit trail, not a verdict
```

- A `removed` item cannot be relisted or gain versions by its author.
- An `unlisted` (private) item's catalog detail is visible to its author only; it does
  not appear in the public catalog, and installers keep the forked workspace they
  already own but cannot update it until the item is public again.
- Versions are append-only. There is no edit-in-place and no delete-version route.

## Entitlements

There is no access gate. Community indicators are included on every plan; the
level exists only to order the catalog.

| Action | Plans |
|---|---|
| List and read the catalog, install, update, report | every plan |
| Publish, add a version, edit metadata, unlist | every plan (own items only) |
| Priority badge and default-sort placement | Desk |

Billing changes:

- `Feature::CommunityBrowse` and `Feature::CommunityPublish` are removed.
  `CommunityIndicators` becomes two levels, `Included` and `Priority`, and every plan
  carries `Included` except Desk (`Priority`). No route calls `enforce_feature` on a
  community request, so the path never answers `402 PLAN_FEATURE_REQUIRED`.
- Publishing copies the author's level into `author_tier` (`publish` or `priority`) so
  the featured sort can favor Desk without a billing read per catalog row.
- `GET /billing/plans` and `GET /billing/subscription` report
  `community_indicators_available: true` whenever the library can serve requests
  (`state.db.is_some()` for the public catalog; true inside `BillingService`, which only
  exists with a database).
- Update `docs/52` and the frontend plan cards: every plan says `included`, Desk says
  `included + priority placement`.

A deployment without a database cannot serve community routes at all, because every one
of them reads or writes the database. With a database and no billing service, publishing
records `author_tier = "publish"` and every route proceeds, matching every other feature.

## API

All routes are under `/community`, require a bearer token, and return the standard
error envelope. `{slug}` is the catalog slug.

| Method | Path | Access | Purpose |
|---|---|---|---|
| GET | `/community/indicators` | any account | Search/list the catalog |
| GET | `/community/indicators/installed` | any account | The caller's installs |
| GET | `/community/indicators/mine` | any account | The caller's published items |
| GET | `/community/indicators/{slug}` | any account | Detail, source, versions, install state |
| POST | `/community/indicators` | any account | Publish a validated revision |
| POST | `/community/indicators/{slug}/versions` | author | Publish version N+1 |
| PATCH | `/community/indicators/{slug}` | author | Title/summary/tags/status |
| POST | `/community/indicators/{slug}/install` | any account | Fork the current version |
| POST | `/community/indicators/{slug}/update` | installer | Apply the current version |
| POST | `/community/indicators/{slug}/report` | any account | Report the item |

Static paths (`installed`, `mine`) take precedence over `{slug}` in Axum 0.8.

### List

`GET /community/indicators?q=&tag=&sort=featured|new|popular&limit=&offset=`

- `q` is a case-insensitive match on title or summary.
- `tag` is exact.
- `limit` default 24, max 50; `offset` default 0.
- Response: `{ "items": [...], "total": n }`.
- An item summary carries `id`, `slug`, `title`, `summary`, `tags`, `author_name`,
  `author_tier`, `install_count`, `version` (current number), `created_at`,
  `updated_at`. It does **not** carry source; source is a detail read.
- `featured` order: `author_tier = 'priority'` first, then `install_count`, then newest.

### Detail

`GET /community/indicators/{slug}` returns the summary plus:

- `source`, `validation`, `preview` for the current version;
- `versions`: `[{ id, version_number, summary, published_at }]`, newest first, bounded;
- `installed`: `{ workspace_id, version, revision_id } | null`;
- `update_available`: whether the install's `version_id` differs from the current one;
- `is_author`: whether the caller owns the item.

A non-listed item is `404` for anyone but its author. A `removed` item is `404` for
everyone but its author (who sees the status in `mine`).

### Publish

```json
{
  "workspace_id": "uuid",
  "revision_id": "uuid",
  "title": "Liquidity sweep + FVG",
  "summary": "Sweep of a prior low, displacement, then an unmitigated FVG.",
  "tags": ["smc", "fvg"],
  "author_name": "Optional display name",
  "visibility": "public (default) | private"
}
```

- `visibility` maps to `status`: `public` → `listed`, `private` → `unlisted`. The author
  can switch later through `PATCH`, without publishing a new version. Anything else is
  `400 COMMUNITY_METADATA_INVALID`.

- `revision_id` is optional; absent means the workspace's `active_revision_id`.
- The workspace and revision must belong to the caller, or `404`.
- The revision must be `status = 'validated'`, or `409 COMMUNITY_REVISION_NOT_VALIDATED`.
- The source is re-vetted and the preview re-validated, or `422 COMMUNITY_SOURCE_INVALID`
  with the validator's issues.
- `summary` defaults to the revision's summary (truncated to 280).
- The server derives a slug, normalizes tags, and stores `author_tier` from the caller's
  current entitlement.
- Response `201` with the detail shape.

Adding a version uses the same body minus `workspace_id`; the item's own source store is
the pin, not a live workspace. The author must still own the item.

### Install / update

Install response:

```json
{
  "workspace_id": "uuid",
  "revision_id": "uuid",
  "version": 3,
  "already_installed": false
}
```

- A repeat install returns `200` with the existing workspace and
  `already_installed: true`; it does not create a second workspace.
- Install requires the item to be `listed` (the author may install their own item in any
  non-removed status).
- The new workspace gets the version's symbol/timeframe, the item title as its name, and
  a memory block:
  `{"community": {"indicator_id", "slug", "version", "author_name"}}`.
- The new revision copies source/summary/validation/preview; `preview.revision_id` is
  rewritten to the new revision id; `change_summary` is
  `"Installed <author_name>'s <title> v<version>"`; `parent_revision_id` is `NULL`.
- Installation and revision creation are one database transaction, so a failure cannot
  leave an empty workspace or a half-installed row.

Update:

- `404 COMMUNITY_NOT_INSTALLED` when there is no install.
- `409 COMMUNITY_NOT_LISTED` when the item is unlisted/removed.
- `409 COMMUNITY_ALREADY_CURRENT` when the install already points at the current
  version.
- Re-vets the version source; on success appends revision `MAX+1` with
  `parent = active`, sets it active, and moves the install's `version_id`.
- The installer's earlier revisions remain and can be restored through the existing
  workspace restore route.

### Report

```json
{ "reason": "spam", "note": "optional" }
```

- First report: `200 { "reported": true, "already_reported": false }` and
  `report_count + 1`.
- Repeat: `200 { "reported": true, "already_reported": true }`, count unchanged.
- The item stays listed. `report_count` is not exposed in the public catalog.

### Error vocabulary

| Status | Code | When |
|---|---|---|
| 402 | (never on this path) | community access is included on every plan |
| 404 | `NOT_FOUND` | unknown, unlisted, or not the caller's row |
| 409 | `COMMUNITY_REVISION_NOT_VALIDATED` | publish an unvalidated/rejected revision |
| 409 | `COMMUNITY_ALREADY_CURRENT` | update with no newer version |
| 409 | `COMMUNITY_NOT_LISTED` | install/update a non-listed item |
| 409 | `COMMUNITY_NOT_INSTALLED` | update without an install |
| 409 | `COMMUNITY_INSTALL_LIMIT_REACHED` | 100 installs |
| 409 | `COMMUNITY_PUBLISH_LIMIT_REACHED` | 25 published items |
| 409 | `COMMUNITY_VERSION_LIMIT_REACHED` | 50 versions on one item |
| 422 | `COMMUNITY_SOURCE_INVALID` | re-vetting failed; `details.issues` |
| 400 | `COMMUNITY_METADATA_INVALID` | title/summary/tags/name/visibility bounds |
| 503 | database or billing unavailable | deployment problem |

## Security

- **Source is untrusted input even after publishing.** Publish, install and update all
  re-run the current validator; the stored `validation` is never trusted as proof.
- **No arbitrary code.** The artifact is Pine-lite or the strategy DSL, executed by the
  existing bounded interpreter. The community path adds no compiler, no WASM upload and
  no native execution.
- **No private data crosses the boundary.** A published version contains only source,
  summary, symbol, timeframe, validation and preview. It never contains workspace
  memory, chat messages, bot drafts, alerts, strategies or the author's email.
- **Ownership on every write.** Author actions are `author_user_id = caller`; install
  actions are `user_id = caller`; the workspace creation uses the caller's id.
- **Display data is escaped.** Titles, summaries, tags, author names and source are
  rendered with the shell's existing escaping helpers; source is shown in a read-only
  view, never injected as HTML.
- **Atomic counters.** `install_count` and `report_count` move in the same transaction
  as the row that justifies them.
- **Bounds.** Source size, tag count, name lengths, publish count, version count and
  install count are enforced before the write.

## Frontend

A new **Community** tab in the aside, next to Chat.

- Signed out: the tab's body is hidden; signed in, every view is available.
- Browsing: search box, tag filter, sort select, and a grid of cards. Each card
  shows title, author + priority badge, summary, tags, install count, and an
  Install / Installed / Update button.
- Detail: source (collapsible, read-only), validation summary, versions, and a Report
  action. The author sees the visibility state with **Make private** / **Make public**
  instead of Report.
- **Published by me** and **Installed** toggles reuse the list; Installed rows link to
  the owned workspace (switching to the Indicator tab and opening it).
- Publishing lives where the artifact is: a **Publish** action in the indicator
  workspace's revision area, enabled when the active revision is `validated`. A small
  inline form collects title, summary, tags, display name and visibility (public by
  default). Success shows the slug and a link to the Community tab.
- The plan pane replaces "reserved, not built yet" with the actual wording: `included`
  on every plan, `included + priority placement` on Desk.
- All network state is loaded lazily when the tab opens, like the billing pane, and
  refreshed after sign-in.

## Tests

New `crates/api-gateway/tests/community_flow.rs`, using the real router and database:

1. Free lists the catalog and reads a detail.
2. Free publishes a validated revision; the detail's source equals the revision's source.
3. Publishing a workspace the caller does not own is 404.
4. A rejected revision cannot be published (409).
5. Free installs an item: a new owned workspace with one revision, identical
   source, install count 1. A second install is idempotent and does not duplicate.
6. Author adds version 2; installer sees `update_available`; update appends revision 2
   and moves the pin; a second update is 409.
7. Unlisting hides the item from others, keeps the installed copy, refuses updates, and
   relisting makes it findable again.
8. Report is idempotent and counts once.
9. Desk publishes with `author_tier = "priority"` and sorts first in `featured`.
10. An author publishes private, strangers get 404, the item is in `mine`, and switching
    back to public restores access.
11. A non-author cannot add a version or patch metadata (404).
12. Metadata and source bounds return the named 400/422.

Unit tests:

- `plans.rs`: every plan carries `Included`, Desk carries `Priority`, and price ordering
  still holds.
- Slug and metadata normalization helpers.

Verification: `cargo test -p api-gateway --lib`, the new `community_flow` test,
`cargo test -p api-gateway --test billing_flow`, `cargo check --workspace`,
`node --check frontend/app/app.js`, `git diff --check`.

## Files

- `crates/db/migrations/0016_community_indicators.sql` (new)
- `crates/db/src/community.rs` (new) and `crates/db/src/lib.rs` (export)
- `crates/api-gateway/src/community_routes.rs` (new)
- `crates/api-gateway/src/lib.rs` (mount `/community/*`)
- `crates/api-gateway/src/billing/plans.rs` (`Included`/`Priority` levels, comments)
- `crates/api-gateway/src/billing/mod.rs` (remove the 501 special case)
- `crates/api-gateway/src/billing/routes.rs`, `service.rs` (availability true)
- `frontend/app/index.html` (Community tab/pane, plan copy)
- `frontend/app/app.js` (catalog, detail, publish, install/update, report)
- `crates/api-gateway/tests/community_flow.rs` (new)
- `crates/api-gateway/tests/billing_flow.rs` (availability assertions)
- `docs/53-COMMUNITY-INDICATORS.md` (new)
- `docs/52-SUBSCRIPTIONS.md` (replace the reserved section)

## Deferred

- Operator moderation console (the `removed` status and the report trail are ready for
  it).
- Ratings, reviews, follows, author profiles, notifications and payouts.
- Community-published **skills** (methodology documents) and **strategies**.
- Automatic updates for installed indicators. Updates stay explicit.
- Paid or gated community items. Everything listed is installable by any account.
- Search ranking beyond `ILIKE` plus tag filter.
