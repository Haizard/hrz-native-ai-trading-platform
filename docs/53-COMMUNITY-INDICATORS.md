# 53 — Community Indicators

## The idea

A user builds an indicator in the studio, the validator accepts it, and a chart
shows it working. The community library is the step after that: **publish that
validated revision so someone else can run it on their own charts.**

What is published is a revision, not a workspace. It carries source, summary,
symbol, timeframe, the validator report and the chart preview — the artifact
the platform already knows how to run. It never carries chat messages,
workspace memory, alerts, strategies, bot drafts, or anything else private to
the author. A recipient is not granted a share of someone's studio; they are
handed a copy of one finished revision.

The library is open to every plan, the way a script library works on
TradingView, LuxAlgo and MT5: an account is the only requirement. There is no
browse or publish gate on the server, and the shell has no locked state to
explain. Desk's extra is placement, not access (`docs/52`): `author_tier =
priority` sorts an author's items first in the featured list.

## The four tables

`crates/db/migrations/0016_community_indicators.sql`:

| Table | What it holds |
|---|---|
| `community_indicators` | one catalog row per published indicator: slug, title, summary, tags, display name, author tier, status, current version, counters |
| `community_indicator_versions` | immutable published artifacts, one row per version number |
| `community_indicator_installs` | one row per `(user, indicator)`: the owned workspace and the pinned version |
| `community_indicator_reports` | one row per `(indicator, reporter)` and the reason |

Versions are immutable because an install pins one. If editing a published item
could rewrite version 3 in place, every installer's pinned copy would silently
change under them, and "update" would lose its meaning. Editing metadata is a
`PATCH` on the catalog row; editing the artifact is a new version, appended with
the next number, while every earlier version stays readable in `versions`.

The catalog's `status` is one of `listed`, `unlisted`, `removed`. The author
sees and controls the first two as **public** and **private**:

- **`listed` (public)** — visible in the catalog and installable.
- **`unlisted` (private)** — hidden from strangers; the author keeps it in
  "published by me" and can make it public again. Existing installers keep
  their forked workspace, but the catalog detail is the author's alone while
  the item is private, and updates are refused until it is public again. The
  author chooses public or private at publish time and can switch at any time
  without republishing.
- **`removed`** — terminal for the author. There is no route that moves an item
  into or out of `removed`; until a moderation console exists, a deployment
  operator sets it with a direct database update. The status and the report
  trail are what that console will read.

`author_tier` is copied from the author's entitlement at publish time
(`publish` or `priority`). It is display/sort data, not a live gate: a
`priority` item does not stop being priority-ordered when its author's plan
lapses.

## Publish, re-vet, list

```
validated revision ──► publish (re-vet) ──► immutable version 1 ──► public
                                      │                    └────► private
                                      └─► 422 COMMUNITY_SOURCE_INVALID
```

The workspace already stored a `validation` report when the revision was
accepted, but that report is not proof. Validators change; a revision that
passed last month must pass **now**. `publish`, `add_version` and `install`
all re-run the current validator (Pine-lite or the strategy DSL, depending on
the stored representation) and re-validate the chart preview. A failure is
`422 COMMUNITY_SOURCE_INVALID` with the validator's issues in
`details.issues`, not a `409` — the request was well-formed and the artifact
is unfit.

After the vets pass, the server trims and bounds the metadata, derives a slug
(`title-slug-abcdef`, six hex characters for uniqueness), normalizes tags to
lowercase, and writes the catalog row and version 1 in one insert. The
response is `201` with the full detail shape.

Only a revision with `status = 'validated'` can be published. A draft, a
running revision, or a rejected one is `409 COMMUNITY_REVISION_NOT_VALIDATED`.

`POST /community/indicators/{slug}/versions` is the same flow for version N+1:
the body names a workspace and optional revision (default: the workspace's
active revision), the author must own the item, and the version limit applies.
The item's status must not be `removed`.

## Install is a fork

```
install:
  BEGIN
    new indicator_workspace (caller, item title, version symbol/timeframe)
    new indicator_revision (source/summary/validation/preview copied,
                            preview.revision_id rewritten, parent = NULL,
                            change_summary = "Installed <author>'s <title> v<n>")
    new community_indicator_installs (caller, item, workspace, version)
    UPDATE community_indicators SET install_count = install_count + 1
  COMMIT
```

One transaction, because a failure between the workspace and the install row
would leave an empty workspace or a pin pointing at nothing. The new revision
is a normal owned revision, so every existing workspace read path works
unchanged — there is no "community" branch in the studio.

There is at most one install per `(user, indicator)`:

- A repeat install returns `200` with the existing workspace and
  `already_installed: true`; it does not create a second copy or move the pin.
- The install pins the **current** version. A newer version shows as
  `update_available: true` on the detail; nothing is applied automatically.
- `update` re-vets the current version, appends revision `MAX+1` to the owned
  workspace with `parent = active`, makes it active, and moves the pin in one
  transaction. The installer's own edits and earlier revisions remain.
- An install of an item that later becomes `unlisted` keeps working; only new
  installs and updates of non-listed items are refused. The author can always
  reach their own item.

The fork records provenance in the ownership memory the studio already shows:

```json
{ "community": { "indicator_id": "…", "slug": "…", "version": 3, "author_name": "…" } }
```

and a change summary `"Installed <author>'s <title> v<n>"` on the revision.

## Entitlements

There is no entitlement gate. Every plan carries `CommunityIndicators`, either
`Included` or `Priority`:

| Plan | Level | What it means |
|---|---|---|
| Free | `included` | browse, install, publish, manage own items |
| Core | `included` | same |
| Pro | `included` | same |
| Elite | `included` | same |
| Desk | `priority` | same, plus priority placement in `featured` |

Publishing copies the author's level into `author_tier` so the sort can favor
Desk without a live billing read per row. No route calls `enforce_feature`, so
a community request never answers `402 PLAN_FEATURE_REQUIRED`. A signed-in
account is the only requirement; a request without a token is `401`.

`GET /billing/plans` reports `community_indicators_available: true` whenever a
database is configured; the service (which only exists with a database) always
reports it true. There is no reserved/unavailable state.

A deployment **without** a database cannot serve `/community/*` at all: every
route reads or writes Postgres, so they answer `503`. With a database and no
billing service (the degraded mode), publishing records `author_tier =
"publish"` and every route proceeds, matching every other feature.

## Routes

All routes are under `/community`, require a bearer token, and return the
standard error envelope. Static paths are registered before `{slug}`.

| Method | Path | Access | Purpose |
|---|---|---|---|
| GET | `/community/indicators` | any account | Search/list the catalog |
| GET | `/community/indicators/installed` | any account | The caller's installs |
| GET | `/community/indicators/mine` | any account | The caller's published items |
| GET | `/community/indicators/{slug}` | any account | Detail: source, versions, pin state |
| POST | `/community/indicators` | any account | Publish a validated revision |
| POST | `/community/indicators/{slug}/versions` | author | Append version N+1 |
| PATCH | `/community/indicators/{slug}` | author | Title/summary/tags/visibility |
| POST | `/community/indicators/{slug}/install` | any account | Fork the current version |
| POST | `/community/indicators/{slug}/update` | installer | Apply the current version |
| POST | `/community/indicators/{slug}/report` | any account | Report the item |

### List

`GET /community/indicators?q=&tag=&sort=featured|new|popular&limit=&offset=`

- `q` matches title or summary case-insensitively; `tag` is exact; `limit` is
  1–50 (default 24) and `offset` defaults to 0.
- Response: `{ items, total }`, where `total` ignores paging.
- A summary carries `id`, `slug`, `title`, `summary`, `tags`, `author_name`,
  `author_tier`, `install_count`, `version`, `status`, `created_at`,
  `updated_at` — never source.
- `featured` puts `author_tier = 'priority'` first, then install count, then
  newest. `new` is newest-first. `popular` is install count then newest.
- Only `listed` items appear; `installed` and `mine` are the caller's own views
  and include unlisted rows.

### Detail

The detail adds `source`, `validation`, `preview` (all from the current
version), `versions` (newest first, bounded), `installed`
(`{workspace_id, version, revision_id} | null`), `update_available`, and
`is_author`.

A non-listed (private) item is `404` for anyone but its author. A `removed` item
is `404` for everyone but its author, who still sees it in `mine`.

### Publish body

```json
{
  "workspace_id": "uuid",
  "revision_id": "uuid (optional; default: the workspace's active revision)",
  "title": "Liquidity sweep + FVG",
  "summary": "1-280 characters (default: the revision's summary)",
  "tags": ["smc", "fvg"],
  "author_name": "optional display name",
  "visibility": "public (default) | private"
}
```

The workspace must belong to the caller, or `404`. An omitted author name
becomes `Trader-<six hex>`. Publish limit, version limit, install limit and
metadata bounds are below.

### Install / update responses

```json
// install
{ "workspace_id": "uuid", "revision_id": "uuid", "version": 3, "already_installed": false }

// update
{ "workspace_id": "uuid", "revision_id": "uuid", "version": 4 }
```

### Report

```json
{ "reason": "spam|misleading|malicious|copyright|other", "note": "optional, <=500 chars" }
```

The first report returns `{ reported: true, already_reported: false }` and
increments `report_count`; a repeat returns `already_reported: true` with the
count unchanged. Reports are an audit trail: they never hide, unlist or delete
an item by themselves. `report_count` is not exposed in the public catalog.

## Error vocabulary

| Status | Code | When |
|---|---|---|
| 401 | (unauthenticated) | no bearer token |
| 404 | `NOT_FOUND` | unknown, unlisted, or not the caller's row |
| 409 | `COMMUNITY_REVISION_NOT_VALIDATED` | publishing an unvalidated/rejected revision |
| 409 | `COMMUNITY_ALREADY_CURRENT` | updating with no newer version |
| 409 | `COMMUNITY_NOT_LISTED` | install/update/edit of a non-listed item |
| 409 | `COMMUNITY_NOT_INSTALLED` | updating without an install |
| 409 | `COMMUNITY_INSTALL_LIMIT_REACHED` | 100 installs |
| 409 | `COMMUNITY_PUBLISH_LIMIT_REACHED` | 25 published items |
| 409 | `COMMUNITY_VERSION_LIMIT_REACHED` | 50 versions on one item |
| 422 | `COMMUNITY_SOURCE_INVALID` | re-vetting failed; `details.issues` |
| 400 | `COMMUNITY_METADATA_INVALID` | title/summary/tags/name/status/reason bounds |
| 503 | (unavailable) | no database configured |

## Abuse bounds are platform constants

These are not plan limits and are not configurable per deployment; they are in
`community_routes.rs` next to the code that enforces them:

| Bound | Value |
|---|---|
| Published indicators per account | 25 |
| Versions per indicator | 50 |
| Installs per account | 100 |
| Source size | 64 KiB |
| Tags per item | 5, each 1–24 chars |
| Title | 3–80 chars |
| Summary | 1–280 chars |
| Author name | at most 40 chars (generated when empty) |
| Report note | at most 500 chars |

An account decides *whether* it can publish — every account can. The bounds
decide how much one account can add to a shared list; making them plan tiers
would sell more spam.

## Security

- **Source is untrusted input even after publishing.** Publish, version,
  install and update re-run the current validator and re-validate the preview;
  a stored `validation` report is never trusted as proof.
- **No arbitrary code.** The artifact is Pine-lite or the strategy DSL, run by
  the existing bounded interpreter. The community path adds no compiler, no
  WASM upload, no native execution.
- **No private data crosses the boundary.** A published version contains only
  source, summary, symbol, timeframe, validation and preview.
- **Ownership on every write.** Author actions are `author_user_id = caller`;
  installs are `user_id = caller`; the forked workspace uses the caller's id.
- **Escaped display data.** Titles, summaries, tags, author names and source
  are rendered through the shell's existing escaping helpers; source is shown
  in a read-only view, never injected as HTML.
- **Atomic counters.** `install_count` and `report_count` move in the same
  transaction as the row that justifies them.

## Frontend

The aside's **Community** tab (`#pane-community`) is loaded lazily, like the
Plan tab. It needs an account, not a rung: signed out, the body is hidden;
signed in, every view is available.

- Search, tag filter, sort (`featured`/`new`/`popular`) and three views —
  catalog, installed, published by me. Each card shows title, author, priority
  badge, summary, tags, install count, and an Install / Installed / Update
  button. Installed rows link to the owned workspace (the Studio tab opens it).
- Detail: read-only source, validation summary, version list, Report (or, for
  the author, the current visibility — public or private — with a **Make
  private** / **Make public** button, plus Edit metadata).
- Publishing lives where the artifact is: a **Publish** button on each
  validated revision in the Studio tab, and a small inline form for title,
  summary, tags, display name and visibility (public by default, or private).
  Success shows the slug. The author can switch an item between public and
  private later from its detail without publishing a new version.
- Private items carry a `private` badge in "published by me" and "installed".
- The Plan tab's cards read `included` on every plan and
  `included + priority placement` on Desk.

## Tests

`crates/api-gateway/tests/community_flow.rs` drives the real router and
database. The browser tests register on Free unless a case needs another rung:

- Free can list, read the detail, publish a validated revision and install.
- Publishing a workspace the caller does not own is 404; a rejected revision is 409.
- An install forks one owned workspace with one revision, identical source,
  install count 1; a second install is idempotent.
- A new version marks `update_available`; update appends revision 2 and moves the
  pin; a second update is 409.
- Unlisting hides the item from strangers, keeps installers' copies, and refuses
  installs; relisting makes it findable again.
- The author chooses private at publish time, strangers get 404, the item still
  appears in "published by me", and switching back to public restores access.
- Report is idempotent and counts once.
- Desk publishes with `author_tier = "priority"` and sorts first in `featured`.
- A non-author cannot add a version or patch metadata.
- Metadata and source bounds return the named 400/422.

Unit tests in `plans.rs` cover price ordering and assert that every plan
carries `Included` and only Desk carries `Priority`.

## What is deferred

- An operator moderation console. The `removed` status and the report trail are
  ready for it; until then, removal is a database update.
- Ratings, reviews, follows, author profiles, notifications and payouts.
- Community-published **skills** (methodology documents) and **strategies**.
- Automatic updates for installed indicators. Updates stay explicit.
- Paid or gated community items. Everything listed is installable by any
  account.
- Search ranking beyond `ILIKE` plus tag filter.
