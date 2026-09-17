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

  **Built as of 2026-09-15: supply/demand zones, and then any band a client can define.**
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

  ### The concept layer: a client's document, not a new detector

  The toggle above draws one measurement. The thing it generalises to is **a document the
  client writes**, carried in the same request as `concepts: Vec<Concept>` and measured by
  the same engine:

  ```json
  {"name": "bullish_gap", "label": "bullish gap", "side": "Buy", "window": 3,
   "lower": {"high": 0}, "upper": {"low": 2},
   "require": [{"left": {"high": 0}, "op": "below", "right": {"low": 2}}]}
  ```

  That is a fair value gap, completely defined. Nothing in this workspace knows what a fair
  value gap is: there is no detector, no enum variant, no field, no entry in a list. The
  same shape with different numbers is an order block, a breaker block, or a concept nobody
  has named yet.

  This is what keeps `docs/06` intact. Its rule — *"keep the condition grammar small and
  explicit rather than a general-purpose expression language; every operator supported must
  be enumerable and individually testable"* — constrains the **grammar**, not the set of
  *measurements*. So the operators stay closed (`below`, `above`, `below_or_equal`,
  `above_or_equal`) and the **vocabulary becomes definable**: a client names a concept,
  defines it from primitives, and it becomes a band the closed grammar can be asked about.
  You cannot enumerate "any concept"; you can make concepts *definable*. The document is
  data, never code — `crates/sandbox` compiles the interpreter and passes the document in,
  and that decision is what makes this safe to accept from a user at all.

  Four details worth keeping:

  - **The built-in detector is not privileged.** `region_rects` takes bands from both
    producers and does the same thing with them. `demand` and `supply` are simply the bands
    that ship; a client's band is not a special case, which is why a new concept needs no
    new drawing code.
  - **The name is the colour key and `side` is the fallback.** A concept the shell has never
    heard of still reads as a direction rather than as grey.
  - **A refused document is not drawn, and the note says why.** Half a pattern is worse than
    none, and a silent refusal teaches whoever wrote the document nothing. The refusal is
    reported with the document's name and the reason.
  - **The scene has its own vocabulary.** `SceneOrigin` mirrors `RegionOrigin` rather than
    carrying it, because `Region`'s wire is a typed analytics message where `BreakKind`
    travels as `"Bos"`, and the scene's wire is `snake_case` throughout because the shell
    switches on the strings. Carrying the analytics type would put a `"Bos"` beside a
    `"buy"` in the same object. `Side::name` and `BreakKind::name` are the same bridge, for
    the same reason.

  ### The interaction model: a gesture, not a viewport

  Added 2026-09-17. The paragraph *below* records that zoom and pan were never specified; this
  is the specification, and it is short because it has exactly one rule.

  **The shell sends what the user *did*, not the view it thinks should result.**

  ```json
  {"gesture": {"kind": "zoom_time", "factor": 1.1, "anchor": 0.42}}
  ```

  `anchor` is a fraction of the plot rectangle — 0 at the left edge, 1 at the right — which is a
  number a pointer position gives you directly. The engine answers with the *view* it resolved
  to, and that is the only thing the shell stores:

  ```json
  {"viewport": {"from": 318, "count": 140, "price": null}}
  ```

  Note what is **not** in that object: the total number of bars. The shell never learns it, and
  the request never asserts it.

  Three consequences, and they are the reason for the rule rather than decoration:

  - **There is one implementation of the clamping.** The minimum bar count, the ends of the
    series and the price floor are decided in Rust, where `cargo test -p chart-engine` reaches
    them without a browser.
  - **A wheel event cannot accumulate drift.** Every gesture is applied to the engine's own last
    answer rather than to a number the shell carried forward, so there is nothing for a rounding
    error to compound in. Had the shell computed the new viewport itself and sent that, a rounding
    error would compound with every tick and the chart would slowly disagree with its own axis.
    The shell also folds a burst of events into one gesture per frame, which is a performance
    measure rather than the correctness one — see below.
  - **The no-arithmetic rule is kept, with one known exception.** This document's hard rule is
    that the shell performs no arithmetic over market data. Deciding which bar is under the cursor
    and which price sits at the top of the plot is that kind of arithmetic, and the shell does
    neither — it divides a pixel offset by a rectangle's width, which is a display concern, and
    sends the fraction. The exception is `drawThesis`, which maps the thesis's stop and target
    prices to y itself; it is pre-existing, and `docs/19` row 18 tracks it precisely because
    zooming the price axis has just made it live.

  **Why a gesture and not just a viewport.** The alternative is for the shell to send the
  viewport it wants. That works, and it is what most chart libraries do, but it makes the
  *client* the authority on how many bars exist: the shell would have to know the series length
  to express "all of them", and once it knows that it is one step from computing a bar index. A
  gesture has no opinion about the series, so the request stays O(1) and the shell never needs
  the bar count at all.

  **What the scene reports is the request that would reproduce the view, not the window it drew.**
  The engine slices with a `Window` — every field decided, `count` a number — and then reports
  `Window::as_viewport()`: the same view in request form. The two are the same picture until the
  series grows, and then only one of them still means what the user asked for. A chart that was
  showing *everything* and echoed back "these five hundred bars" would keep those five hundred
  and stop including new candles, permanently, one candle at a time — a worse failure than the
  missing zoom it was meant to fix, and invisible on the frame it happens.
  `a_fitted_view_stays_fitted_across_a_new_candle` and `a_fitted_scene_keeps_following_new_candles`
  assert it at both levels.

  The shell echoes the object verbatim rather than copying fields across, which is why it is
  pinned: a field-by-field copy is where a rename becomes a chart that forgets where the user had
  scrolled to, once per frame — which reads as a flicker, not as a bug.
  `a_window_reports_the_request_that_would_reproduce_it` and `the_shell_reads_these_viewport_keys`
  hold the two ends of it.

  **What moves with the window, and what does not.** The candles, the volume profile, the VWAP
  and the price axis are all measured over the **visible slice**. A profile over the whole
  series beside candles showing a tenth of it is a chart that disagrees with itself, and VWAP is
  period-dependent: a line computed over everything marks a price nobody in the window traded
  at, and still looks like a VWAP. `the_profile_and_the_levels_follow_the_window` therefore
  compares the numbers rather than the shape.

  Zones are the deliberate exception. They keep the **whole** series and are clamped to the
  window at draw time, because a band that formed before the left edge is still a band — and
  clipping the detection would erase exactly the levels a trader scrolled back to look at.

  **Heikin-Ashi is transformed before the slice, never after.** The transform is recursive from
  the first candle, so slicing first would restart the averaging at the window's left edge and
  give the same candle a different open depending on how far the user had scrolled. The window
  must not change the data.

  The gestures are `zoom_time`, `zoom_price`, `pan`, and `fit`. A hostile factor or anchor is
  **bounded rather than rejected** — `NaN` is a no-op, an infinite factor is capped at ten —
  because the failure mode of trusting one is a chart with nothing on it and no error anywhere.
  `no_hostile_input_can_produce_an_unusable_window` sweeps 1,715 combinations to say so.

  **What a client can actually send, measured rather than assumed.** JSON has no `NaN` or
  `Infinity` literal, and `JSON.stringify(1e999)` emits `null` — so those two branches are
  *unreachable from the wire*, and the wasm ABI check now proves it rather than leaving it as a
  belief: `1e999` arrives as `invalid type: null, expected f64` and is refused before the engine
  sees it. The hostile input that does reach the engine is a large *finite* one, and `1e308` is
  capped to a ten-fold zoom exactly as intended.

  So the wire is defended by serde's types plus `MAX_FACTOR`, and the `NaN` branches are for Rust
  callers — where a factor can be *computed* rather than parsed, and a `NaN` from a future
  calculation is a real possibility. Both are worth having; neither is load-bearing for the other.
  That is worth writing down because "the guard would catch it" is a comfortable thing to believe
  about a path that cannot reach the guard.

  Two of those guards are worth naming, because both were written wrong first and both were
  caught by a test rather than by reading:

  - **The price floor is measured against `fitted`, not against the current span.** The first
    version used the current span, which shrinks along with the thing it was supposed to
    bound — so the floor could never bind, and the axis halved its way to zero. The test that
    caught it asserted the *floor's value*, not merely that the span was positive; "positive"
    would have been satisfied by any arbitrary small number.
  - **`f64::clamp` does not sanitise `NaN`.** It is two comparisons, `NaN` fails both, and the
    value passes straight through — so a `NaN` anchor multiplied into the price range and
    produced an axis `is_usable` rejects. The sweep found it; no single example would have.

  **A drag is one gesture with two components, not two gestures.** `pan` carries `time` and
  `price` together because a pointer move has both axes at once. Sending them separately would
  mean two engine calls and two full repaints per pointer move, and a rebuild is a wasm call
  plus a canvas clear — at pointer rates that is a drag that feels like a slideshow. The shell
  also coalesces through `requestAnimationFrame` and *folds* gestures rather than replacing
  them, so a wheel burst becomes one rebuild and a drag that outruns the frame rate does not
  lose the pixels it skipped.

  ### What this interaction does not do yet

  Recorded 2026-09-17, so the next reader does not have to discover them:

  - **No pinch-to-zoom.** A one-finger drag pans on a touchscreen (pointer events and
    `touch-action: none` give that for free), but two-finger zoom is not implemented. It is
    not written down as done because it has never been run on a touch device.
  - **The chart does not follow the right edge.** Once the user has zoomed, a new candle
    arriving extends the series and the view stays where it was — so a zoomed chart stops
    tracking the market until it is dragged back. Most platforms pin to the newest bar when
    the view is already at the edge; that needs the engine to know whether it is, which is a
    question the `Window` can already answer and nothing yet asks.
  - **The time axis labels only the two ends.** `drawAxis` prints `scene.from` and `scene.to`
    and nothing between, so a zoomed window gives no sense of the interval. Not new, but more
    visible now that the window can be small.

  ### What this view does not have yet

  Recorded 2026-09-17, from using it. Four things are missing, and they are **not the same
  kind of missing** — which is why they are written down here rather than kept as a list of
  wishes.

  **Specified above and not built: drawing tools.** The bullet above asks for "drawing tools
  (trendlines, Fibonacci, rectangles — start minimal, expand later)". None exist. The "Built
  as of" note covers zones and the concept layer, so this is a gap in a view this document
  claims — the one item here a phase can be held to.

  **Never specified until 2026-09-17: zoom and pan.** Nothing in this document mentioned zoom,
  pan, scroll or wheel, and the done criteria did not either — so there was no interaction model
  to implement, and the first deliverable was the specification rather than code. That is now
  written: see *The interaction model: a gesture, not a viewport* above, which is built and
  tested. It was never a shell-only change either: `frontend/chart-engine/src/scene.rs` derived
  `price_min`/`price_max` from **every** candle it was handed and the engine had no viewport
  type, so a zoom needed a bar range and a price range in the scene request before the shell had
  anything to bind a wheel event to.

  **Never specified: more than one chart.** The shell has a single `<canvas id="chart">`, and
  its panes (`thesis`, `strategy`, `bots`, `book`) are *side panels chosen by tab*, not chart
  panes. A second chart — the same symbol at another timeframe, or another symbol — needs a
  layout decision this document has not made.

  **Never specified, and half-built: responsive layout.** The canvas already scales
  correctly: the shell sizes it from the wrapper's `clientWidth`/`clientHeight` times
  `devicePixelRatio`, and re-renders on `resize`. The *page* does not adapt — `index.html`
  has a fixed `aside { width: 380px }` and **no `@media` query anywhere**, so a narrow
  viewport squeezes the chart instead of reflowing.

  **Why the exit criterion caught none of this.** It reads: log in, view the BTCUSDT
  footprint chart, ask the AI for a setup, review the thesis, run a backtest, launch a paper
  bot. It tests a *session*, not a *chart*. A chart that cannot zoom, cannot be drawn on and
  exists exactly once passes it — the same shape as the bounded-duration soaks (`docs/19`
  row 15), where the criterion is met and the thing a user actually wanted was never in it.

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
