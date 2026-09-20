# Implementation Plan: AI-Generated Indicator Workspaces

## Delivery slices

1. **Indicator artifact contract** — add a versioned, restricted generated-module
   format; structured visual/event/evidence outputs; deterministic validation and unit
   fixtures. Extend the chart engine to place the new output primitives without browser
   market arithmetic.
2. **Workspace persistence** — migrations and repository APIs for workspaces, immutable
   revisions, compact memory, preview results, and alert preferences. Every lookup is
   user-scoped.
3. **AI revision service** — ground prompts in workspace memory/current source/chart
   context; generate a revision; validate/sandbox/preview/diff; retain last known-good
   revision on failure.
4. **Gateway contract** — authenticated workspace/revision/preview/source/restore and
   alert endpoints, plus an explicit indicator-to-bot draft endpoint.
5. **Narrative chart UX** — workspace chat, auto-attachment, revision cards, read-only
   source/diff/history, evidence-chain hover/selection, and alert controls.
6. **Bot promotion safety** — source-revision pinning, immutable backtest linkage,
   explicit approval gate, and integration tests proving active bots ignore later edits.

## First implementation increment

Implement slice 1 as a standalone deterministic core: a typed indicator output model
with evidence-chain validation and chart-engine positioning tests. It creates the safe
contract every later database, AI, and UI layer consumes; it does not yet let generated
code bypass the existing DSL/WASM sandbox.

## Verification per slice

- Unit/property tests for output validation, finite coordinates, bounded output, stable
  ids, and deterministic replay.
- Native/WASM equivalence tests for the generated-module host once the sandbox adapter
  is introduced.
- API integration tests for ownership, failed-revision rollback, alert deduplication,
  and bot revision isolation.
- Browser-facing chart-engine tests for viewport anchoring, clipping, decluttering, and
  evidence selection.

## Implementation status — 2026-09-20

### Complete

- **Slice 1 — indicator contract and chart renderer.** `IndicatorOutput` has bounded,
  validated evidence, zones, markers, and links. The Rust/WASM chart engine positions
  market coordinates and the browser shell draws the resulting scene; JavaScript does
  not calculate price/time transforms.
- **Core workspace persistence.** User-scoped workspaces, immutable revisions, active
  revision tracking, previews, validation records, and revision history are migrated
  and exposed through authenticated routes.
- **Durable conversation foundation.** Workspace messages are append-only and the
  workspace stores a compact JSON memory summary after a successful generation turn.
- **Bedrock generation gate.** A workspace message invokes the existing Bedrock-backed
  Strategy DSL generator, stores the validated DSL as a strategy, verifies it through
  the existing WASM sandbox, creates an immutable indicator revision, and records the
  user and assistant messages. A provider or sandbox failure leaves the active revision
  unchanged.
- **Revision restore and alert preference storage.** Validated revisions can be restored
  server-side; event preferences are opt-in and user-scoped.
- **Revision-pinned bot drafts.** The schema and authenticated draft endpoint pin a
  draft to a validated workspace revision and a stored strategy, preventing later
  indicator edits from changing that draft.

### Partial

- **Generated visual output.** The current Bedrock path produces and validates the
  executable Strategy DSL and records an empty safe chart preview. Translating replayed
  strategy events into evidence-chain zones/markers/links remains to be implemented.
- **Gateway surface.** Workspace, revision, message, restore, alert-preference, and
  bot-draft routes exist. Dedicated single-revision source/preview reads and alert
  delivery/deduplication workers are not yet separate endpoints/services.
- **Chart attachment.** The shell has a safe `attachIndicator` hook and rendering support,
  but the workspace panel does not yet call it from the revision API.

### Not started

- Workspace chat UI, revision cards, source/diff/history views, restore controls, and
  alert controls in the application shell.
- Historical backtest linkage enforcement and the explicit approval endpoint that turns
  an indicator bot draft into a paper/live bot through the existing risk gates.
- Integration tests for workspace ownership, rollback, alert preference persistence,
  restore behavior, and bot revision isolation.
