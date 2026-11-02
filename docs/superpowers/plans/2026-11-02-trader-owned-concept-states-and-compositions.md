# Trader-Owned Concept States and Indicator Composition

## Goal

Let a trader create a personal, reproducible variant of a registered concept,
describe it in their own trading terminology, combine variants from different
concept families, and save the result as a reusable indicator. A trader's
variant must not change the registry concept or silently change when the
registry is updated.

## User workflow

1. Browse the existing concept library and choose a concept, regardless of
   family.
2. Create an **adjusted state**: set only declared parameters, give the state a
   trader-facing name and optional explanation, and preview it on chart data.
   Keep the canonical concept id visible as its identity.
3. Add adjusted states from any families to an indicator composition. Choose
   how the states combine (`all`, `any`, `not`, or ordered `sequence`) and set
   each leaf's direction, timeframe, lookback, and delay where supported.
4. Preview and validate the composition, then save it as an indicator workspace
   revision. Reopening the revision restores the same aliases, parameters,
   composition, and detector versions.

## Semantics and boundaries

- A trader-facing name is an alias on a saved state, not a replacement for the
  canonical registry id. Events, strategy references, and provenance continue
  to use canonical ids.
- In the first release, editing a concept means changing its declared
  parameters and its trader-facing alias/description. It does not rewrite the
  detector algorithm. Arbitrary rule editing needs a separate constrained rule
  language and validator; it must not be implied by a free-text description.
- Combining families means composing catalog-backed concept leaves in one
  `market_data::composer::Composition`. It does not require merging the event
  and band DSLs or adding chart shapes.
- A composition is both an indicator artifact and a reproducible input to the
  existing strategy promotion path. Promotion must preserve the same pinned
  concept states.
- Saved variants are immutable once used in a revision. Editing creates a new
  state/revision; it never mutates a registry entry or a prior indicator
  revision.

## Existing building blocks

- `market_data::concepts::ConceptSpec` is the current 182-entry registry and
  declares concept parameters and versions.
- `market_data::composer::Composition` already supports cross-family leaves,
  condition trees, and per-canonical-id parameter overrides.
- `/concept-events/scan` already validates parameter overrides and applies
  them to event detection.
- `/indicator-series` and the pane/overlay concepts currently do not consume
  parameter overrides. The concept-state design identifies this as its first
  implementation increment.
- Indicator workspaces already store immutable revisions and render composition
  previews; the indicator panel and preview route are existing integration
  points.
- `docs/superpowers/specs/2026-10-31-concept-state-architecture-design.md`
  remains authoritative for definitions, detector provenance, and the staged
  route toward a single source of truth.

## Artifact shape

Use a versioned state record owned by the trader or indicator revision:

```json
{
  "state_id": "stable id within the artifact",
  "concept_id": "smc.bos",
  "concept_version": "registry version",
  "alias": "London continuation break",
  "description": "My continuation setup after the London sweep",
  "parameters": { "swing_lookback": 5 }
}
```

The composition references `state_id` for trader-authored identity and resolves
it to the pinned canonical `concept_id`, version, and parameters at validation
and runtime. For backward compatibility, existing documents that have no
state table continue to use their canonical ids and current defaults. The
resolved artifact must also capture any required data-input provenance from the
concept-state architecture before it can be treated as reproducible across
venues.

Do not store aliases by rewriting condition ids or by making aliases global:
two traders may use the same words for different variants, and one trader may
use different names for two states of the same registry concept.

## Implementation order

### 1. Make adjusted detections consistent

- Implement the `SeriesConfig`/validated `params` path for `/indicator-series`.
- Preserve the current output when parameters are omitted; make the series
  dispatcher read each declared period from the config.
- Surface adjustable parameters in the concept library UI and include them in
  pane and overlay fetches.
- Keep event scan and series validation sourced from the same parameter
  declarations.

### 2. Add pinned trader-owned concept states

- Add a serializable state model with stable local id, canonical concept id,
  pinned registry version, alias, optional description, and validated parameter
  values.
- Add server validation for existence, version availability, alias/description
  bounds, and parameter ranges. Never accept client-supplied detector code.
- Store states inside the immutable indicator revision source, so existing
  revision ownership and restore flows apply without a parallel persistence
  table.
- Add create/edit/duplicate controls to the indicator builder. Editing a used
  state produces a new draft/revision and keeps previous revisions intact.

### 3. Compose states across families

- Extend composition leaves to reference a state id while retaining a resolved
  canonical-id form for execution and older clients.
- Resolve each state independently into canonical id plus its own parameters;
  preserve current `DetectionPlan` grouping so one concept's settings cannot
  leak into another.
- Support `all`, `any`, `not`, and `sequence` across any registry families.
- Show each leaf's alias and canonical family/id in the builder and in event
  evidence. Validate that all referenced state ids exist and are unique.

### 4. Build and persist the indicator

- Compile the composition into the existing indicator workspace document and
  preview path; add no new visual shapes.
- Show aliases in the indicator legend/cards while retaining canonical ids in
  detail/evidence and exports.
- Save and reopen as immutable indicator revisions. Restore state definitions
  and composition exactly from the selected revision.
- Ensure indicator-to-strategy promotion carries the same resolved states and
  refuses a revision if a pinned detector version is unavailable.

### 5. Provenance and reproducibility gates

- Follow the concept-state architecture increments for event/series
  `DataProvenance`, resolved input source, and definition coverage.
- Include canonical concept id/version, effective parameters, and resolved input
  source in preview/event evidence and exported artifacts.
- Fail validation rather than silently substituting a new registry version or
  unsupported input source.

## Acceptance criteria

- A trader can adjust at least one event concept and one series concept, assign
  each a custom alias, and preview both; omitted parameters retain current
  behavior.
- One indicator can combine states from at least two different concept
  families with `all`, `any`, and `sequence` semantics.
- Two states of the same concept can coexist with different parameters and
  aliases without clobbering one another.
- The preview, saved revision, reopened revision, and promoted strategy resolve
  the same canonical versions and effective parameters.
- Unknown concepts, invalid parameter ranges, duplicate state ids, unresolved
  versions, and unsupported inputs fail with field-specific validation errors.
- Existing documents without trader-owned states retain their current meaning.
- Aliases affect presentation only; canonical ids remain stable in machine
  evidence and provenance.

## Not part of this release

- Arbitrary user-authored detector code or free-text-to-rule execution.
- Per-venue concept ids or chart-shape changes.
- Sharing/publishing trader states independently of the existing indicator
  workspace and community-indicator workflows.
- Confidence scoring or statistical claims about a trader's variant.

