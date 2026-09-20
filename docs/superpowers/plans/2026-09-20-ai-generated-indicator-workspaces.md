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
- **Generated visual output.** A workspace turn now replays the generated document
  (sandboxed) over a recent window and translates the signals it actually emitted into
  the chart's evidence chain: each fired condition is an evidence node linked into the
  entry marker, the entry-to-stop risk band is a zone whose lifecycle follows the exit,
  and the exit is its own linked node. The builder lives in `chart-engine`
  (`IndicatorOutput::from_replay`) and emits only output that passes `validate`, keeping
  the most recent setups when the primitive cap would otherwise be exceeded. A replay
  that cannot run (no stored candles, a declared timeframe with no data) records the
  reason and keeps an honest empty preview rather than failing the revision.

### Complete

- All slices (1–6) are now implemented:
  - **Slice 1:** Indicator artifact contract and chart renderer.
  - **Slice 2:** Workspace persistence with user-scoped workspaces, immutable
    revisions, active revision tracking, previews, validation records, and
    revision history.
  - **Slice 3:** AI revision service with Bedrock-backed generation, sandbox
    validation, replay-based preview generation, and evidence-chain translation.
  - **Slice 4:** Complete gateway surface: CRUD workspaces, revisions, messages,
    restore, alert preferences, bot drafts, and bot promotion approval.
  - **Slice 5:** Workspace chat UI with revision cards, source/preview views,
    chat panel, and alert controls in the application shell.
  - **Slice 6:** Bot promotion safety with revision-pinned drafts, backtest
    linkage validation, and the approval endpoint that creates a sandboxed bot
    through the existing risk gates.

- **Integration tests.** `indicator_workspace_flow.rs` covers cross-user denial,
  revision restore, rejected-revision restore refusal, alert upsert
  deduplication, workspace delete cascade, and single-revision source/preview
  reads.
- **Alert delivery worker.** Background task monitors bot decisions, matches
  events against enabled workspace alert preferences, and delivers via webhook
  with per-(workspace, revision, event) deduplication and cooldown.
