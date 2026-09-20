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
