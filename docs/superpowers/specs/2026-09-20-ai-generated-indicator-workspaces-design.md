# AI-Generated Indicator Workspaces — Design

## Purpose

Let a non-programming client describe a trading indicator or strategy in a persistent
AI chat, inspect the generated source read-only, iteratively request changes, and use
the resulting validated artifact on a chart. Indicators attach automatically after a
safe preview. Alerts require a user opt-in. Bots are separate, approval-gated artifacts.

The product must prove that an indicator detects and renders the rules it declares. It
cannot promise profitability or market outcomes.

## Goals

- Generate bespoke indicators from natural-language trading methodologies, including
  Smart Money Concepts such as liquidity sweeps, FVGs, order blocks, and BOS/CHoCH.
- Render polished, explanatory chart overlays that show the chain of evidence behind a
  signal, rather than only a final buy/sell marker.
- Preserve persistent project context so a follow-up request changes the existing
  implementation instead of starting a new one.
- Keep generated source transparent but read-only; every modification flows through
  chat, validation, preview, and revision history.
- Reuse one deterministic execution result for chart rendering, alerts, backtesting,
  paper trading, and promoted bots.

## Non-goals

- No claim that a generated indicator or bot is always profitable or correct about a
  future market move.
- No arbitrary native-code execution, filesystem/network/shell access, credentials, or
  direct order placement from generated indicator code.
- No manual editing of generated source in the first release.
- No silent modification of a running bot when its source indicator changes.

## Architecture

### Strategy workspace

A **Strategy Workspace** is the durable owner of a named client project (for example,
`BTC SMC FVG`). It owns:

- the persistent AI chat and a structured, compact memory summary;
- market context (symbol, venue, timeframe ladder, chart range);
- one or more indicator projects and bot projects;
- immutable source revisions, explanations, validation records, preview results, and
  backtest runs;
- links between an indicator revision and bots derived from it.

Each request to the AI is grounded in the current source, compact decision history,
active chart context, declared client rules, and the previous preview/backtest result.
The model must not rely only on an unbounded raw transcript.

### Generated module

The AI generates a module for a restricted Rust-like indicator SDK. The platform
compiles it to WASM and runs it in the existing sandbox. The module can read approved
market data and shared deterministic analytics, maintain bounded per-series state, and
emit normalized chart primitives and named events.

The SDK allows:

- candles, trades, order-flow, and approved higher-timeframe series;
- shared analytics such as pivots, ATR, VWAP, delta, CVD, volume profile, and market
  structure helpers;
- bounded state;
- zones, lines, rays, labels, arrows, markers, bands, lower-pane plots/histograms,
  events, and explanatory metadata.

The sandbox refuses filesystem, network, shell, secrets, unmanaged threads, unlimited
memory, unbounded computation, unsupported drawing commands, or trading actions.

### Revision lifecycle

Every chat-driven change creates an immutable revision containing source, a plain
language rule summary, a diff summary, generated examples/tests, compile/sandbox result,
detected events, chart primitives, and resource-use report.

1. The AI proposes a revision from the workspace context.
2. The platform compiles, validates, and sandbox-replays it on historical/current data.
3. It compares its signals and drawings with the currently attached revision.
4. A passing **indicator** revision automatically replaces the attached chart revision.
5. A failed revision never changes the chart, alerts, or bots; the prior valid revision
   remains active and the chat reports the precise failure.

The client can compare revisions and restore any prior valid revision. Source is shown
in a read-only advanced view together with the validation report and an explanation of
the logic.

## Chart rendering and explainability

The module emits primitives anchored to market time and price, never canvas pixels. The
existing Rust/WASM chart engine transforms them for zoom, pan, viewport changes, and
responsive layout.

The default visual language is **Narrative Intelligence**. Indicators present an
evidence chain, for example:

```text
liquidity sweep → displacement → BOS/CHoCH → FVG/order block → retest → signal
```

Each node is tied to the candle(s) and rule that produced it. The chart renders a
polished explanation through understated event markers, context-aware callouts,
non-overlapping labels, grouped annotations, status-aware zones, and optional connector
paths that communicate logical causality without misrepresenting price movement.

Zones have lifecycle states such as `created`, `active`, `tapped`, `mitigated`, and
`invalidated`. Selecting or hovering an item reveals the named rule, the inputs that
passed, source revision, and links to prior evidence in its chain. Auto-decluttering
compresses old evidence, maintains price visibility, and expands on selection.

## Alerts

Indicators emit named events (for example, `bullish_fvg_created`,
`fvg_retested`, and `long_setup`). Alerts are off by default. The client enables desired
events in the attached indicator panel and chooses available delivery channels.

Delivery is deduplicated by indicator revision, symbol, timeframe, closed bar, and event
identity. A revision failure cannot enable, disable, or alter existing alert settings.

## Bot promotion and safety gate

An indicator may emit signals but cannot trade. **Create bot from this indicator** makes
a separate draft pinned to an exact source indicator revision. The AI can propose entry,
exit, invalidation, sizing, venue, and timeframe rules based on its named signals.

A bot becomes active only after:

1. client review of the human-readable rules and risk/execution settings;
2. immutable historical backtest tied to indicator and bot revisions;
3. optional paper/forward validation;
4. explicit approval that names the exact bot revision, execution mode, and risk limits.

The active bot and its source indicator remain visually linked on the chart. Subsequent
indicator edits create new bot drafts; they never alter a running bot until independently
reviewed and approved.

## Failure handling

A revision must pass source/schema validation, SDK capability checks, WASM resource
limits, deterministic replay, visual-output validation, and chart/backtest parity. The
platform rejects invalid time/price anchors, unsupported styles, corrupt coordinates,
and excessive object counts.

Errors are actionable. Examples: a missing 4H data dependency, an unsupported SDK call,
or a per-bar computation-budget breach. The system keeps the last known-good indicator
active and records the failed attempt without affecting alerts or bots.

## Acceptance criteria

- A client can create, revise, compare, restore, and inspect a read-only generated
  indicator through one persistent workspace chat.
- A valid revision automatically attaches and renders on the active chart.
- FVG, liquidity-sweep, and BOS/CHoCH fixtures produce deterministic evidence-chain
  drawings at the expected candle/time/price coordinates.
- Zoom/pan/timeframe changes preserve correct anchors and readable decluttered output.
- User-enabled alerts fire once per event identity; disabled alerts never deliver.
- A failed revision leaves the active indicator, alert configuration, and bot state
  untouched.
- The same input data produces matching chart events, preview results, backtest events,
  and paper-bot decisions.
- Bot creation is a separate draft workflow, and no indicator revision can silently
  change an active bot.

## Approved implementation decisions

### Generation path

The first workspace release uses the existing Bedrock-backed agent and its validated
Strategy DSL generator. A workspace message is grounded in the owned workspace's
compact memory, current revision, declared chart context, and a bounded recent-message
window. It produces a candidate Strategy DSL document rather than arbitrary executable
indicator source. The platform validates and runs that document in the existing WASM
sandbox, replays it against the requested window, and converts the deterministic result
into the restricted `IndicatorOutput` preview contract.

This deliberately reuses the existing agent rate limit, DSL validator, sandbox, and bot
runtime. An indicator-specific SDK is deferred until the shared DSL cannot express a
required capability; it must not create a second execution path in the meantime.

### Durable workspace conversation

`indicator_workspace_messages` stores append-only user and assistant messages, with a
message kind and structured payload for revision outcomes. The workspace's `memory`
field is updated only by the service after a successful turn, with a compact summary
that bounds prompt growth. Revisions retain their source, preview, validation report,
and parent link independently of chat messages, so the history remains auditable even
when memory is compacted.

### API and attachment contract

Authenticated workspace endpoints expose message submission, revision source/preview,
revision restore, alert preferences, and revision-pinned bot drafts. The server, not the
browser, chooses an active revision. The browser receives only a server-validated preview
and attaches it to the Rust/WASM chart engine; it never evaluates generated source or
transforms market coordinates itself.

### Bot promotion

Creating a bot from an indicator creates an immutable draft tied to the source revision.
The draft stores its required historical backtest and execution/risk settings. Promotion
requires an explicit approval request naming that exact draft and revision. Approval then
delegates to the existing paper/live bot creation and live-risk gates. Editing or restoring
an indicator always creates a new candidate revision and never mutates a draft or an
active bot.

### Verification

Integration tests cover user ownership, Bedrock-unavailable handling, failed-revision
rollback, source/preview read-only behavior, alert ownership and preference persistence,
revision restore, and the invariant that a bot draft or active bot retains its original
indicator revision after later workspace edits.
