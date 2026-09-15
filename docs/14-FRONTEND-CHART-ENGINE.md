# 14 — Frontend & Chart Engine

## Purpose
The trading workstation UI: candlestick/footprint/volume-profile chart, order book/DOM,
AI chat panel with chart-region highlighting, strategy editor (three modes), and
account/settings screens.

## Decision gate: Rust/WASM-first vs. React+TypeScript

The source research strongly favors a Rust-first frontend (Leptos/Dioxus + WASM +
WebGPU/Canvas) so the chart engine shares `analytics-core` directly with the backend,
and so footprint/volume-profile rendering with potentially hundreds of thousands of
visual primitives doesn't bottleneck on per-cell DOM elements. That is the target
architecture for this roadmap.

**Pragmatic exception:** if agent/team velocity in Rust web frameworks proves
significantly slower than in React+TypeScript during early prototyping, it is
acceptable to build the **application shell** (panels, forms, settings, strategy
editor, AI chat) in React+TypeScript while keeping the **chart engine itself** as a
Rust/WASM module compiled from `analytics-core` and mounted into the React app via a
canvas/WebGPU element. What must never happen is a *second* implementation of the
trading math in TypeScript — the chart engine, whichever shell hosts it, calls into the
same Rust analytics code, compiled to WASM. Make this decision explicitly and document
it in this file once made; don't let it drift undecided.

## DECISION — 2026-09-14: Rust/WASM chart, vanilla-JS shell

**Chosen: the target architecture, not the exception.** The chart engine is a
Rust crate (`frontend/chart-engine`) compiled to `wasm32-unknown-unknown` and
driven from plain JavaScript. There is no Node toolchain, no bundler and no
framework.

### Why

1. **The deployment is Rust-only, and that is worth keeping.** The image builds
   two binaries in one stage and copies them into `debian:bookworm-slim`. Adding
   a React shell means adding a Node build stage, a package manager, a lockfile
   to audit, and a second dependency tree to keep current — for a UI that is
   mostly forms. That cost lands on every future deploy, not once.
2. **The alternative's advantage does not apply yet.** The exception exists for
   "if agent/team velocity in Rust web frameworks proves significantly slower".
   It has not been measured, because there is no UI to measure it on. Choosing
   the heavier stack before the measurement is the wrong order.
3. **Nothing about the chart gets easier with React.** The parts that matter —
   scales, price/time transforms, footprint aggregation, volume profile — are
   arithmetic over `analytics-core`, and `docs/14` already forbids doing them in
   TypeScript. React would host the canvas; it would not help draw it.
4. **The wasm target is already installed** and `analytics-core` already
   compiles for it, because `sandbox-guest` depends on that.

### What this commits us to

- **No JS arithmetic over market data.** The shell fetches `/candles`, hands the
  JSON to the wasm module, and draws the scene it returns. Any computation the
  UI wants that is not pure layout belongs in `analytics-core` or the chart
  engine, where it is unit-tested natively.
- **The scene is data, not drawing instructions.** The engine returns positioned
  rectangles, lines and labels; the shell decides nothing about what a candle
  looks like. That is what keeps the engine testable without a browser.
- **If a Node build is ever genuinely needed** for panels and forms, the
  exception above still applies and the chart module is already the right shape
  to mount into it. This decision is reversible at the shell, which is the
  cheapest place for it to be reversible.

### How the wasm is produced and served

`frontend/chart-engine` is a `cdylib` + `rlib`: the `cdylib` is the browser
artifact and the `rlib` exists so the scene builder can be unit-tested on the
host, where assertions and a debugger work. `xtask build-frontend` compiles it
and copies the `.wasm` next to the shell, which the gateway serves from
`frontend/app`.

There is no `wasm-bindgen`. The module exports four functions — `alloc`,
`dealloc`, `build_scene` and accessors for the result buffer — and the shell
copies a JSON request in and a JSON scene out. That is a few lines of glue on
each side instead of a code generator, a CLI tool and a matching version
requirement between them.

## Chart engine architecture (either hosting option)

```
Market data (via WebSocket)
        ↓
Rust/WASM chart engine
        ↓
   ┌────┴─────┐
   │          │
CPU calc   GPU-friendly buffers
   │          │
   └────┬─────┘
        ▼
   Canvas / WebGL / WebGPU
```

- Do not render each footprint cell or volume-profile bar as an individual DOM/UI
  component. Build a compact in-memory scene representation (arrays of positioned
  rectangles/bars with color) and draw it directly via Canvas 2D (simplest, ship first)
  or WebGL/WebGPU (if profiling shows Canvas 2D is insufficient at target data density).
- Reuse `analytics-core` compiled to `wasm32-unknown-unknown` for any client-side
  recomputation (e.g. adjusting a volume-profile bucket size interactively without a
  round-trip) — never reimplement the math in JS/TS.

## Required views
- **Chart**: candlesticks, footprint mode (bid/ask ladder per price level), volume
  profile overlay, VWAP/POC/VAH/VAL lines, delta/CVD sub-panel, drawing tools
  (trendlines, Fibonacci, rectangles — start minimal, expand later).
- **DOM/order book panel**: live bid/ask ladder from `/ws/orderbook/{symbol}`.

  **Built as of 2026-09-15: the feed, not the panel.** The channel is live — snapshots
  are maintained in `market-data` and arrive on the socket (see `docs/12`) — but nothing
  draws them yet. That half is a real view: a ladder with per-level size, and the
  cumulative-depth bars that make size readable at a glance.

  Those bars are the reason it is not a one-liner. "No arithmetic over market data in
  JavaScript" is the rule this whole frontend is built on, and a depth bar is a running
  total scaled against the largest one. So the snapshot has to arrive with what the panel
  needs to draw it, or the ladder ships without the bars. Decide that when the panel is
  built; do not quietly do the sums in `app.js`.
- **AI chat panel**: natural-language input, streaming responses via
  `/ws/agent/{session_id}`, action buttons (Analyze / Create Strategy / Backtest /
  Create Bot) matching the source research's UI sketch, and the ability to highlight the
  exact chart region(s) referenced in the AI's explanation (map thesis fields like
  `entry_price`/timestamps back to chart coordinates).
- **Strategy editor**: three modes sharing one underlying `StrategyDocument` — natural
  language (delegates to `/agent/generate-strategy`), visual builder (condition blocks
  composed via UI, serialized to the same schema), and raw DSL (YAML/JSON text editor
  with the validator's errors shown inline).

  **Built as of 2026-09-14: all three modes.** Every mode writes into the same text box,
  so the document a user saves is always the one they can read — a strategy is something
  a bot will execute. Applying the builder rewrites the box and marks the source
  `created_by: visual_builder`, the same way natural language marks it `agent`.

  The builder is deliberately a *thin* view over the document, and two rules keep it
  from becoming a second opinion about what a strategy means:

  - **It owns no vocabulary.** Dropdowns are filled from `GET /strategies/schema`, which
    is generated from `strategy-dsl` itself (`ALL_FIELDS`, `ALL_FUNCS`, `ALL_STOP_KINDS`,
    `TakeProfitKind::ALL`, `Timeframe::all`). A new field or stop rule in Rust appears in
    the builder with no JS change, and the builder cannot offer a condition the validator
    would reject.
  - **It does not parse YAML.** Opening an existing document goes through
    `POST /strategies/validate`, which now echoes the parsed `document`. There is one
    parser in this system and it is in Rust; the client never decides what a file says.

  What JS *does* parse is a single condition, enough to turn it into controls. That
  parser is a strict subset of `expr.rs`'s grammar, and anything outside the subset — a
  nested call such as `above(delta, threshold(5))` — becomes raw text rather than a
  dropdown that cannot show it. The pane says so out loud ("N condition(s) kept as
  text"), so degradation is visible instead of silent.

  The acceptance property is idempotence: emit → parse → emit must be byte-identical, or
  a user who opens a strategy in the builder and applies it without touching anything has
  rewritten it. `tools/check_builder.mjs` asserts that, writes its documents to
  `target/builder-check/`, and CI runs `strategy-cli validate` — the real parser — over
  each one. A hand-written YAML emitter in JavaScript is exactly the kind of thing that
  looks right and parses wrong.

- **Backtest/bot dashboards**: performance report visualization, trade list, bot
  status/controls (pause/resume/kill).

## Real-time data handling
- Subscribe to `/ws/market/{symbol}/{timeframe}` for the active chart; resubscribe on
  symbol/timeframe change; unsubscribe on unmount to avoid leaking server-side fan-out
  resources.
- Apply client-side interpolation/coalescing for very high-frequency updates if the
  render loop can't keep up — never let the UI thread block waiting on network data.

## Done criteria
- A user can load a symbol, switch to footprint mode, see live-updating footprint cells
  driven by real trade data, ask the AI a question, and see the response's referenced
  price levels highlighted on the chart — all without a full page reload.
- The chosen hosting architecture (pure Rust/WASM vs. React shell + WASM chart module)
  is documented here as an explicit decision with the date and rationale it was made.
