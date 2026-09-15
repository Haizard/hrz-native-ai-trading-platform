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

  **Built as of 2026-09-15: supply/demand zones, the first area the engine can draw.**
  A "Zones" toggle in the header. When it is on, the request carries `zones: true` and the
  scene comes back with a `regions` array — price bands with a time extent, drawn behind
  the candles.

  The interesting part is where the detection runs. It runs **in the wasm engine, on the
  candles the request already carries** — not in the shell, and not behind a route of its
  own. That is the arrangement the volume profile and VWAP already use: the engine calls
  `analytics-core` and reimplements none of it. So the browser never runs a detector,
  `docs/14`'s no-arithmetic rule holds by construction, and switching the toggle on costs
  no extra round trip.

  Two things worth keeping:

  - A zone is an **area**, and nothing else in the scene is. Every other detector in this
    workspace reports a point — `ImbalanceEvent.price_level`, `AbsorptionEvent.price_level`,
    `LiquidityLevel.price` — and `Scene` drew candles, horizontal `levels`, profile bars and
    footprint cells. `analytics_core::regions::Region` is the type that was missing, and
    `SceneRegion` is its positioned form.
  - The band carries `y_top` **and** `h`, not two y coordinates. `ProfileBar` already had
    that shape, and it is why the shell's fill is `fillRect(x, y_top, w, h)` with nothing to
    subtract — the rule about arithmetic is easiest to keep when the geometry has no gaps in
    it.

  A fresh zone is drawn solid and a mitigated one faded, because a zone price has already
  traded back through is not a level any more. Rendering the two the same is how a chart
  teaches someone to buy something that no longer exists.

- **DOM/order book panel**: live bid/ask ladder from `/ws/orderbook/{symbol}`.

  **Built as of 2026-09-15: the ladder, and why it is computed in Rust.** The panel is a
  "Book" tab in the aside: asks descending into the spread, then bids, each row showing
  price, size, cumulative size, and a depth bar.

  The bars are the interesting part, because "no arithmetic over market data in
  JavaScript" is the rule this frontend is built on, and a depth bar is a running total
  scaled against the deepest row on the book. So the channel does not send a bare
  snapshot: it sends a **`dom::Ladder`**, built in `crates/api-gateway/src/dom.rs`, which
  carries each level's `cumulative` and a `bar_pct` already worked out. The shell sets a
  width from `bar_pct` and formats numbers; it derives nothing.

  Two details worth keeping:

  - `bar_pct` is scaled against the deepest row on **either** side, not per side. Scaled
    per side, a book with 0.01 resting against 100 draws two full bars, and the ladder
    stops being a comparison — which is the only thing it is for.
  - The ladder is a **superset** of `OrderBookSnapshot`: same `symbol`, `timestamp`,
    `bids`, `asks`, with fields added per level. `OrderBookLevel` is what the database
    persists, so it was never going to grow a presentational field.

  When the channel has no book it says so (a `notice` naming the symbol and the likely
  cause), and the panel shows that message rather than an empty ladder — an empty ladder
  is indistinguishable from a market with no liquidity.
- **AI chat panel**: natural-language input, streaming responses via
  `/ws/agent/{session_id}`, action buttons (Analyze / Create Strategy / Backtest /
  Create Bot) matching the source research's UI sketch, and the ability to highlight the
  exact chart region(s) referenced in the AI's explanation (map thesis fields like
  `entry_price`/timestamps back to chart coordinates).

  **Built as of 2026-09-15: a transcript, with the agent's work shown while it runs.** The
  panel keeps every question and its answer instead of replacing its contents on each
  ask — the answer you were reading used to disappear the moment you asked the next one.
  It talks to `/ws/agent/{session_id}` rather than `POST /agent/ask`, so the socket can
  report what the run is doing.

  That distinction is the whole design. A question takes about a minute, and a panel that
  says nothing for a minute is indistinguishable from one that has hung. So the socket
  sends `progress` frames — which timeframe is being read, which turn of the loop is
  running, which tool was called and whether it worked — and the panel shows them as they
  arrive under the question.

  It is not token streaming, and that is a finding rather than a shortcut: the agent's
  answer is a `submit_thesis` **tool call**, and the answering phase refuses every other
  tool and tells the model not to answer in prose. There are no answer tokens to stream.
  Streaming narration instead would be streaming text this design discards.

  A finished turn folds its steps into a `<details>` summary; the turn in flight shows
  them live. Nothing stores them — the transcript is the only record the agent's steps
  ever have.

- **Backtest/bot dashboards**: performance report visualization, trade list, bot
  status/controls (pause/resume/kill).

  **Built as of 2026-09-15: the run list and the equity curve.** The Strategy tab's
  backtest section lists the strategy's stored runs (newest first, in a select) and draws
  the selected one: the metrics, then the curve. A run is *read back* through
  `GET /backtests/{id}` rather than re-run, and the panel that draws a fresh run is the
  same panel that draws an old one — so the two can never disagree. Running a backtest
  refreshes the list and selects the new run.

  The curve follows the same rule as the ladder, and for the same reason. Fitting a series
  into a box is arithmetic — a min, a max, and a division per point — so
  `crates/api-gateway/src/plot.rs` does it and `equity_plot` on the response carries the
  points as percentages of the box, each with its own value, plus `zero_y` for where flat
  sits. The shell writes a `polyline` and formats labels. It derives nothing.

  Two properties worth keeping:

  - The run **started flat**, so the plotted series starts at zero rather than at wherever
    the first trade left it. A curve that begins at the top-left corner makes a run whose
    first trade won look like it began in profit.
  - `zero_y` is placed in Rust. Above it the run is up, below it it is down, and finding
    that line is arithmetic like everything else — so the shell is told where to draw it,
    and told nothing when zero falls outside the box.

  A run with no curve says which of the two reasons it is: no trades in the window, or a
  run stored before the curve was kept. The report's own `total_trades` is what tells them
  apart.

  **Built as of 2026-09-15: the bot panel is live.** Each bot has a **Watch** button that
  opens `/ws/bots/{bot_id}` and streams that bot's activity into a log — decisions as the
  bot makes them, with the outcome in English, newest first. Launching a bot watches it
  automatically, because that is the one moment a user certainly wants to look.

  The socket is one per bot and filters server-side, so watching one bot does not subscribe
  you to every other. The panel does not poll: the summary still comes from `GET /bots`
  (a button changes a status, so the list is re-read), but a decision appears when the bot
  makes it.

  Both the list and the log are drawn by one renderer from `bots`, `botLog` and
  `watchedBot`, so a socket frame and a refresh land in the same place and cannot disagree
  about what a bot is doing. The log is capped at 60 events — about an hour of a 1m bot —
  so a forgotten tab does not grow forever.

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
