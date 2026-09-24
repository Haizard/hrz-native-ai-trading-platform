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

  ### The footprint: a ladder, and the font that decides whether it is one

  Added 2026-09-17, from using it. Footprint mode existed and drew *something*, and the report
  was that it "is not well designed and presented": every cell was a colour, not one cell had a
  number, and the value area was a single band across the whole plot.

  **The defect that mattered was not the drawing, it was the font — and the decision was being
  made in two places.** The engine sized `font_px` from the row height alone
  (`row_height * 0.62`, clamped 6.0–11.0). The shell then dropped every number below
  `font_px >= 7`. `footprint_routes` sizes its bucket for `TARGET_ROWS = 45`; 45 rows on a 500px
  plot is an 11.1px row, so `11.1 * 0.62 = 6.9` — a tenth of a pixel under the shell's
  threshold. **A real window drew every cell and not one number.** A ladder with no numbers is
  not a footprint, and nothing in either half looked wrong on its own.

  The two halves of one decision have to be made in one place, and that place is the engine,
  because that is where the layout is:

  | number | what it is for |
  |---|---|
  | `legible_rows(plot_height)` | the row cap, derived from the plot rather than a constant |
  | `font_for_width(cell_width, pair_chars)` | the font the *widest* `bid x ask` needs |
  | `cell_for_font(font_px, pair_chars)` | the cell width that font needs — the same relation, read backwards |
  | `Grid::font_px` | the smaller of the two, clamped to a floor |
  | `Grid::show_text` | whether the shell should draw numbers at all |
  | `Grid::min_cell_px` | the narrowest a cell can be and still show *this window's* widest pair |

  **The cap and the font are two ends of one decision.** Capping at a constant `MAX_ROWS = 80`
  and *then* clamping the font meant the font fell under legibility and the text silently
  vanished — a cap that does not do what it claims. `legible_rows` derives the cap from the plot
  height so the font lands at or above `MIN_FONT_PX = 7.0`, and a truncation is **announced**
  (`Grid::note`, which `scene.rs` copies to `scene.note`) rather than being a silent middle
  slice: a chart that quietly drops levels is a chart that lies about the window.

  **A font that fits the row but not the cell is not legible either.** `1.40 x 2.85` is eleven
  characters; at 8.6px that is 59px of a 54px column. Both dimensions have to fit, so both are
  decided in the engine and the shell only reads `grid.show_text`.

  **And the constant that replaced it was the same defect one level up** (closed 2026-09-17, the
  day after it was written down). `MIN_COLUMN_PX` went 54 → 64 to keep the font above the floor —
  correct, and chosen by hand arithmetic against the engine's `MIN_FONT_PX = 7.0` and a 0.62 glyph
  ratio, **in another language, with nothing failing when the two drifted apart**. The number the
  shell needs is the one the engine just computed, so the engine now computes it: `cell_for_font`
  is `font_for_width` read backwards, and `Grid::min_cell_px` is it at `MIN_FONT_PX` for the widest
  pair in the window. The shell holds no width constant at all — it asks.

  **It also decides how many candles are on screen, which is why it was worth doing properly.**
  The shell has to choose a **column count** before any layout exists, and the column count is what
  sets how many candles fit. Sizing it from a constant that is safely *under* the engine's real
  requirement is a permanently narrower window than the chart can actually draw — 12 columns where
  14 fit. So the count is derived, not chosen:

  ```
  columns = clamp(floor(plot.w / (footCellPx || SEED_CELL_PX)), 8, 60)
  ```

  and `footCellPx` is learned from the last scene the engine produced. That is a fixed point — the
  count decides the window, the window decides the numbers, the numbers decide the count — and it
  is reached in **one** step, because the first load is seeded and every load after it is sized by
  what the last one actually needed. Two details are load-bearing and both were wrong first:

  - **`plot.w`, not the element's width.** The first version used
    `el("chart").parentElement.clientWidth`, which is the *element*; the plot is narrower by the
    price axis. A 54px element column was a 53px cell — one hundredth of a pixel under the point
    where no numbers draw. Before the first scene there is nothing to ask, so the estimate
    subtracts the axis the way `drawAxis` lays it out, which is an estimate of the right rectangle
    rather than of the one that contains it.
  - **`min_cell_px` follows the numbers, not the symbol.** It is computed from the widest pair *in
    that window*, so a symbol whose volumes print `1.21 K` gets wider cells than one printing
    `0.44` — which is the property the request was actually asking for.

  `tools/shell_check.mjs` measures the outcome rather than the arithmetic: it reads
  `min_cell_px` out of the last scene, computes what `floor(plot.w / min_cell_px)` says fits, and
  asserts that (a) it differs from the seed — i.e. the engine really does want a narrower cell
  than the constant it replaced — and (b) the next load *asks for that many*. Both are run against
  their own bug in `tools/guard_check.py`: fixing the cell width back to the seed, and never
  learning it, each break the check that names them. The engine-side guard is shown to fail by
  substituting `cell_for_font(6.0, …)` for `cell_for_font(MIN_FONT_PX, …)`, which reports
  *"23 columns is what `floor(1080 / 46.92)` says fits, and it came out at 6.00535509371414px"*.
  `tools/footprint_preview.mjs` derives its own column count the same way, so the preview shows
  what the shell draws rather than what it drew before.

  **The value area is per column.** It used to be `grid.rows.some(|row| row.in_value_area)`, so
  one candle's value area striped every other candle's ladder at prices those candles never
  traded. `ColumnScene` cells carry `in_value_area` and the shell bands each column separately.

  Three things the drawing had to gain to read as a ladder at all, judged against a professional
  footprint from another platform: every cell with volume **filled** and tinted by which side won
  (`0.3 + oneSided * 0.42`, so an unbalanced cell is more saturated), the pair drawn as one
  centred string `bid x ask` rather than two numbers in two corners, and a faint column
  background plus a frame so a column reads as a column even where it has no volume.

  **It was verified by looking at it, which is the only way it could have been.** jsdom lays
  nothing out, so `tools/shell_check.mjs` can only assert *what was painted*, never where.
  `tools/footprint_preview.mjs` renders a real `Mode::Footprint` scene to a self-contained page
  and headless Chrome screenshots it (`chrome.exe --headless=new --disable-gpu --hide-scrollbars
  --virtual-time-budget=4000 --window-size=W,H --screenshot=...`). The ladder was judged from
  that image. A presentation defect is not findable by asserting on a paint log, and a preview
  tool is cheaper than the alternative.

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

  ### Drawing tools: a document the user owns

  Added 2026-09-17. The bullet above has asked for "drawing tools (trendlines, Fibonacci,
  rectangles — start minimal, expand later)" since this document was written, and none were
  built. This is the specification.

  **A drawing is two anchors and a kind.** Nothing else is stored, because nothing else is
  needed: every shape a trader draws is a pair of `(time, price)` points and a rule for what
  to put between them.

  | kind | anchors | drawn as |
  |---|---|---|
  | `trendline` | 2 | the segment between them |
  | `hline` | 1 | a horizontal line across the plot at the first anchor's price |
  | `rect` | 2 | the rectangle they span |
  | `fib` | 2 | the retracement levels between them |

  Four kinds, and the kind is a `serde` enum rather than free text: a kind the engine does not
  know is **refused**, because a drawing that saves and then never appears is worse than one
  that will not save. `ray`, `channel` and a measured move are the obvious next ones, and each
  is a variant plus an arm — no new storage, because they are all two anchors.

  **An anchor is either absolute or a fraction of the plot, and the wire says which.**

  ```json
  {"kind": "trendline",
   "a1": {"unit": "fraction", "x": 0.31, "y": 0.62},
   "a2": {"unit": "fraction", "x": 0.74, "y": 0.20}}
  ```

  A `fraction` is a pointer position over the plot rectangle — the same number the viewport
  gestures already use — and the engine resolves it. An `absolute` is a millisecond timestamp
  and a price, which is what gets stored.

  The shell sends fractions while a drawing is being placed or dragged, and the scene reports
  every anchor back as `absolute`. So the shell never turns a pointer position into a price or
  a time: it hands over the fraction and reads back the answer. This is the interaction model's
  rule generalised — **the shell says where on the screen, the engine says what that is** —
  and it is why a drag needs no gesture type of its own. A drag *is* a request whose anchor is
  a fraction, so it folds into the same one-rebuild-per-frame coalescing a wheel does.

  The two forms use different field names — `x`/`y` against `time`/`price` — and the difference
  is load-bearing. One pair of names for both would make "the shell sent a fraction where the
  engine expected a timestamp" a mistake nothing catches: the numbers are the same shape, the
  units are not, and the result is a drawing somewhere in 1970.

  **The time unit is milliseconds, and that is a deliberate exception.** The platform stores
  nanoseconds (`docs/13`) and everything else crossing to the browser is nanoseconds —
  `Scene::from`, `Scene::to`. A drawing anchor is the first value that has to make the round
  trip *back* into the database, and JSON numbers are doubles: 1.7e18 is past 2^53, so a
  nanosecond timestamp read into JavaScript and written back is a different number. It is a
  few hundred nanoseconds, which is invisible on a chart and is still a write that changes the
  data. Milliseconds fit, so the drawing wire is milliseconds and the conversion happens once,
  in Rust, where a test can see it. `Scene::from` stays in nanoseconds precisely because
  nothing ever sends it back.

  **They are positioned in the engine and hit-tested in the shell.** The engine emits, per
  drawing, the shapes to draw *and* a `handle` part at each anchor:

  ```json
  {"shape": "segment", "x1": 12.0, "y1": 40.0, "x2": 300.0, "y2": 90.0}
  {"shape": "handle",  "x": 300.0, "y": 90.0, "anchor": 1}
  ```

  That is what makes a drag possible without putting the price scale in JavaScript. The shell's
  hit-test is a distance between two screen points — display geometry, the same class as the
  division in `plotFraction` — and the *meaning* of the point, which bar and which price, is
  never derived there. It also means the engine decides which handles exist, so the shell
  cannot offer a grab point the engine would not honour.

  Handles are emitted only for the **selected** drawing. Every drawing's anchors at once is
  visual noise, and it invites a drag nobody can aim. `anchor` is the engine's own numbering —
  `0` for the first anchor and `1` for the second — because the engine emits the handles and so
  it decides what they are called; a kind that reads only its first anchor emits only `0`.

  **A handle drag moves one anchor; a body drag moves the drawing.** Grabbing a handle sends the
  pointer's own position into that one anchor. Grabbing the *body* — a segment, a rectangle's
  interior — has to move both anchors by the same amount, or a trendline dragged by its middle
  changes slope and a rectangle collapses to a corner. That needs a *delta*, and the shell
  cannot add a delta to a timestamp: it has no way to turn a price into a fraction, which is the
  same prohibition that keeps the price scale in Rust.

  So the scene also reports where each anchor sits **in plot fractions**:

  ```json
  {"a1": {"unit": "absolute", "time": 1767225600000.0, "price": 45000.0},
   "a1_fraction": {"x": 0.31, "y": 0.62},
   "a2": {"unit": "absolute", "time": 1767250800000.0, "price": 45250.0},
   "a2_fraction": {"x": 0.74, "y": 0.20}}
  ```

  and a body drag is: read both fractions, add the pointer's offset over the plot's size — the
  same division `pan` already does — and send them back. The engine resolves them exactly as it
  resolves a placement, so there is still one implementation of "which price is under the
  pointer", and it is in Rust.

  Those two fields are a plain `{x, y}` and **not** an `Anchor`, which is a deliberate
  asymmetry. An `Anchor` is tagged because it may be either unit; a reported fraction is only
  ever a fraction, so a `unit` field there would be a branch the shell has to read and can never
  find to be anything but `"fraction"` — a decision point that cannot be decided differently.
  Two field names for two units is the rule from above; a tag on a value that has only one
  possible unit is the same rule applied too far.

  They are reported for **every** drawing, including one whose anchors arrived as timestamps.
  That matters more than it looks: everything loaded from the API arrives absolute, because that
  is the only form storage keeps. Reporting fractions only for anchors that arrived as fractions
  would leave a saved drawing undraggable until it had been dragged once — a feature that works
  the second time and not the first.

  A drawing that cannot be resolved — an unknown kind, a two-anchor kind with one anchor, two
  anchors at the *same* point, a `NaN` price — is **not drawn**, and the reason goes into
  [`Scene::note`] with the drawing's id, exactly as a refused concept document does. Half a shape
  is worse than none, and a silent refusal teaches whoever drew it nothing.

  The same-point case is the one the shell cannot pre-empt and should not have to. Every tool
  places its first anchor on pointer-down, so a *click* with the trendline tool selected puts
  both anchors at one point: a shape with no extent, invisible on the chart, and still there on
  the next reload. It is exactly "the pointer did not move between down and up", so the test is
  equality rather than a tolerance — a one-pixel drag is a different pair of numbers and is
  drawn. The whole anchor is compared rather than one coordinate, because a vertical trendline
  and a flat rectangle are both real things to draw, and the kind check keeps `hline` — the one
  tool that *is* a click — out of the rule entirely.

  **They are stored per user and per symbol, and they are mutable.** This is the one
  user-authored record in the schema that is *edited* rather than appended to.
  `venue_opt_ins` is append-only because "who turned this on, when, and why" is a question an
  incident review asks and a boolean column would destroy the answer. A trendline is not a
  consent record: it is a shape the user is still moving, and an append-only table would
  collect a row per drag of the mouse. The history that matters for a drawing is the drawing.

  Per **symbol** rather than per symbol-and-timeframe, and that is worth naming because the
  opposite is defensible. A trendline is two `(time, price)` points, and those two points mean
  the same thing on a 1m chart as on a 1h one; scoping to the timeframe would hide a level
  from the chart a trader switched to in order to check it.

  **Two destructive controls, and neither of them guesses.** Added 2026-09-17, from the report
  "when I click the clear button it removes all the attached tools on the chart instead of the
  one I have selected". The button said `Clear`, sat in a row of *drawing tools*, and deleted
  every drawing on the symbol on the first click — and there was no way to delete just the
  selection at all. Both halves of that are the same defect: **a control whose label does not
  say what it does is worse than a control that is missing**, because the user has already
  spent the click learning the wrong lesson.

  So there are now two, each named for its job:

  | control | deletes | guard |
  |---|---|---|
  | `Delete` | the selected drawing, and only it | **disabled** when nothing is selected |
  | `Clear all` | every drawing on this symbol | two clicks: the first asks (`Sure?`) for 4s |

  The `Delete` control is disabled rather than inert, because a button that looks pressable and
  does nothing is the same lie in a smaller size. The `Clear all` confirm is *in the button*
  rather than a `confirm()` dialog: a modal blocks the chart, and the chart is exactly what the
  user needs to look at to decide. Four seconds because that is long enough to read `Sure?` and
  short enough that a stray second click is not the thing that wipes the document. The label
  reverts on a timer, and the timer is disarmed when the click *does* clear, so the control
  cannot be left armed by a path that already fired.

  **What a drawing does not do yet**, recorded rather than left to be discovered:

  - **Nothing snaps.** An anchor lands where the pointer was, not on an open, high, low or
    close. Most platforms offer both; this one has the harder half.
  - **No text, no measurement, no undo.** A mis-placed drawing is deleted and redrawn.
  - **No multi-select and no group move.** One drawing is selected at a time, and the keyboard
    is that wide too: `Delete` removes the selected drawing, `Esc` abandons the shape being
    placed and, pressed again, returns to the cursor.
  - **The AI agent cannot see them.** ~~That is a permission decision rather than a technical
    one, and it is tracked in `docs/19` rather than guessed at here. The storage and the route
    are already scoped by user, so turning it on is a context change, not a schema change.~~
    **Resolved 2026-09-22** — `docs/19` row 19 closed by building it: the agent now has a
    read-only `get_user_drawings(symbol)` tool (`docs/09`), fed by drawings the gateway
    attaches to the request after authentication. The chart itself still does not push the
    current viewport's drawings into the agent's context packet — the user can ask the agent
    to look at their levels rather than the agent seeing them unprompted, which remains the
    honest boundary until it is a problem.
  - **A drawing is not confined to the window.** Its anchors are mapped absolutely and the
    canvas clips, which is the same arrangement zones use: a trendline drawn last week is
    still a trendline when the window has moved past it.

  ### The layout: what gives way when the window narrows

  Added 2026-09-17. The canvas has always scaled — it is sized from the wrapper's
  `clientWidth`/`clientHeight` times `devicePixelRatio`, and a `resize` re-renders — but the
  *page* did not adapt. `aside` was a fixed 380px with **no `@media` query anywhere**, so at
  800px the chart was already under half the width and at 500px it was nothing at all.

  **The chart is the surface the page exists for, so the panel is what gives way.** Two
  breakpoints, and they are two different problems rather than one problem at two sizes:

  | width | what changes |
  |---|---|
  | ≤ 1100px | `aside` narrows to 300px. Still side by side: 300px still holds a table and a paragraph, and the chart gains 80px. |
  | ≤ 900px | `main` becomes a column. The panel moves below the chart and the page scrolls. |

  **The second breakpoint is why the body stops being `height: 100%`.** A column that shares one
  height between a chart and a scrolling panel hands the chart whatever the panel's content
  leaves — the same failure as the first breakpoint, with the axes swapped. So the chart gets a
  height of its own (`60vh`, floor `260px`) and the page scrolls instead. That height is not
  cosmetic: `draw()` measures `wrap.clientHeight`, so it is also the number the engine is told
  the plot is.

  Three smaller rules, each because something becomes **unreachable** rather than merely ugly:

  - **The drawing toolbar wraps.** It is an overlay, so a control past the right edge is not
    clipped, it is gone.
  - **The gesture hint is given a right edge** so it wraps rather than running off the page. It
    is kept rather than hidden: it is what makes the wheel and the drag discoverable at all, and
    a narrow *window* is still a mouse.
  - **The sign-in row wraps**, and its two fields may shrink. Two default-width inputs and a
    button do not fit a phone.

  **The order of the two breakpoints is load-bearing.** Both match at 800px and both set
  `aside { width }`, so the stacking block has to come second or the panel ends up a 300px
  column under a full-width chart. `tools/shell_check.mjs` asserts the order, because swapping
  them looks like tidying.

  **A resize is a gesture, and it coalesces like one.** The listener used to call `render()`
  directly, so dragging a window edge — dozens of events a second, each a wasm rebuild plus a
  full repaint — ran one rebuild per event. It goes through `scheduleRender()` now, the same
  once-per-frame path a wheel and a pan use. The harness asserts a *count*, because that is the
  only way to tell coalescing from a shell that merely happened to be fast.

  **What the layout does not do yet.** There is no touch-specific hint: the copy names a wheel
  and `Shift`, neither of which exists on the device the 900px breakpoint is most likely
  serving. And the breakpoints are unverified in a real browser — jsdom lays nothing out, so the
  harness reads the stylesheet and asserts the declarations that stop the squeeze, the same
  trade `api-gateway/tests/packaging.rs` makes for the Docker image.

  ### More than one chart

  Added 2026-09-17. The shell had a single `<canvas id="chart">`, and its *panes* (`thesis`,
  `strategy`, `bots`, `book`) are side panels chosen by tab, not chart panes — which is the
  naming collision that made this look already done. A trader reading a 5m entry against a 4h
  trend wants both at once, and switching tabs is not that.

  **A pane is a chart, and it owns everything a chart is.** The list is long because the
  alternative is worse: a pane that shares any of it with its neighbour is a pane that can be
  made to disagree with itself.

  | the pane owns | why it cannot be shared |
  |---|---|
  | its series — symbol, timeframe, bar limit, chart type, zones | two panes exist in order to differ here |
  | its window — the `Viewport` the engine resolved | zooming one chart must not move the other; a shared window is one chart drawn twice |
  | the drawings it has *loaded*, its tool, its selection, the shape being placed | the store is per instrument, but "which chart am I drawing on" still has to be answerable, and a shared selection would draw handles on both |
  | its candles, its footprint, its live channel | they follow the series |
  | its note strip and its footprint stats | a refusal on one chart must not read as a refusal on both |
  | its canvas | the hit test is in canvas coordinates, so two charts sharing one would each grab the other's shapes |

  **The drawings store is per user and instrument, not per timeframe.** That was a deliberate
  choice — a trendline drawn at 5m is a claim about the market, not about the bar size — and it
  has a consequence worth stating: two panes on one instrument at two timeframes show the *same*
  shapes. A reader who takes "the panes are independent" as the whole rule will read that as a
  bug. The pane owns its loaded copy so that changing one pane's instrument does not empty the
  other's chart; it does not own the store.

  **What stays page-level is what is not a chart.** The session and the sign-in form; the aside
  and its four panels; the AI conversation; the bot list; the venue list. Those describe the
  *account*, and there is one account.

  **The aside follows the active pane.** A pane becomes active when it is interacted with — a
  pointer down on its canvas, or a change to one of its own controls — and the active pane is
  the one with a visible border. Anything in the aside that is about a chart follows it. There
  is exactly one such thing today and it needs the rule: the thesis overlay is drawn in
  JavaScript against a pane's price axis, so on a pane showing another instrument it would put
  one market's levels on another market's chart.

  **One wasm instance, many panes.** `build_scene` is a function of its request and the engine
  holds no viewport between calls — that is what makes the interaction model above work at all
  — so the panes share the module and each request carries its own window. Loading it once is
  also what stops the first pane being a different engine from the second.

  **A pane's options come from the data, not from the markup.** `GET /symbols` reports every
  instrument with candles and, per timeframe, how many there are and how many are missing. The
  page shipped a hardcoded `<option>BTCUSDT</option>` and a timeframe list that omitted `15m`,
  which the database has — so the page could not offer an instrument it was able to chart, and
  could not say why a timeframe was thin. Both selects are filled from that response, and a
  timeframe carries its bar count, because "3 bars" is the difference between a chart that is
  broken and a chart that is telling you the truth.

  **But the response is not in a usable order, and taking it as given was a regression.** The
  server lists its timeframes alphabetically — `15m, 1h, 1m, 4h, 5m` here — and the first option
  is therefore `15m`, which holds **three** bars against 53,182 for `5m`. The markup this replaced
  had `5m selected`, so the page started opening on the thinnest series on the deployment: the
  thin-chart case the bar count was added to *explain* arrived as the default instead. Two rules
  fix it, and neither names a timeframe:

  | rule | why |
  |---|---|
  | the options are ordered by **length**, parsed from the value (`1m, 5m, 15m, 1h, 4h`) | the server's order is not a ladder, and "the next timeframe up" means nothing without one |
  | a chart opens on the series with the **most bars** | derived, so it stays right when a different timeframe becomes the deepest; naming `5m` here would be the hardcoded option again, one line lower |
  | a new pane opens on the next one up **that can fill the window it asks for** | the threshold is the bar-limit select's own value rather than a figure invented for it; a timeframe that cannot fill the chart is not a chart yet, and if none can, the next one up is still the answer |

  The harness could not have caught this: its `/symbols` fixture had been tidied into
  `5m, 15m, 1h`, which is both the right order and the right default, so the check that now
  asserts the ladder passed against a fixture that made the question disappear. **A fixture that
  is tidier than production hides production's defects** — the order and the counts in
  `tools/shell_check.mjs` are now copied from a live `GET /symbols`.

  **There is always at least one pane.** Closing the last one is refused rather than leaving an
  empty page with no way back, and the control that would do it is not drawn. Adding stops at
  four: past that nothing is readable, and an uncapped button is a way to make the page
  unusable by accident.

  **How any of this is checked.** `tools/shell_check.mjs` loads the page into a DOM and drives
  it — `#split`, a wheel, a symbol change, a drawing gesture, a close — and asserts the two
  panes stay two. The checks that matter are the negatives: a wheel in one chart must rebuild
  that chart and leave the other's scene *the same object it was*, a symbol change in one must
  not rebuild the other, a shape drawn in the second must be stored against the second's
  instrument. Counting panes would pass for a shell where the second pane is a view of the
  first, which is the only way this feature can be wrong quietly. The channels carry the
  positive half: `/ws/market/SYMBOL/TIMEFRAME` is a per-pane claim and `/ws/orderbook/SYMBOL` is
  a page-level one, so which socket is open says which chart the page thinks it is showing.

  Every one of those checks was then run against the bug it names — the pane boundary replaced
  by a document-wide lookup, the clone replaced by the node itself, the cap removed, the last
  close button unguarded, `destroy` not closing its channel — by `tools/guard_check.py`, which
  patches one thing, runs the harness, and requires the named check to fail. Two of them did
  not, which is how the harness came to be sending a non-bubbling `change` event: no browser
  sends one, the page listens on the container, and the check that needed it had been passing
  because the pane it was about happened to be active already.

  **What a pane does not do yet.** It cannot be reordered or resized against its neighbour, and
  there is no layout beyond the row — a stack and a grid are the obvious next shapes. Two panes
  on one symbol also fetch that symbol's candles twice, which is correct and wasteful in equal
  measure.

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

  **Specified above and not built, until 2026-09-17: drawing tools.** The bullet above asks for
  "drawing tools (trendlines, Fibonacci, rectangles — start minimal, expand later)". All four
  kinds now exist, and the specification is *Drawing tools: a document the user owns* above. The
  four defects that building it exposed were all in the shell and all invisible from outside —
  the toolbar rendered, the buttons pressed, and nothing happened — which is why
  `tools/shell_check.mjs` exists: `chart-engine` had tests and `app.js` had none.

  **Never specified until 2026-09-17: zoom and pan.** Nothing in this document mentioned zoom,
  pan, scroll or wheel, and the done criteria did not either — so there was no interaction model
  to implement, and the first deliverable was the specification rather than code. That is now
  written: see *The interaction model: a gesture, not a viewport* above, which is built and
  tested. It was never a shell-only change either: `frontend/chart-engine/src/scene.rs` derived
  `price_min`/`price_max` from **every** candle it was handed and the engine had no viewport
  type, so a zoom needed a bar range and a price range in the scene request before the shell had
  anything to bind a wheel event to.

  **Never specified until 2026-09-17: more than one chart.** The shell had a single
  `<canvas id="chart">`, and its panes (`thesis`, `strategy`, `bots`, `book`) are *side panels
  chosen by tab*, not chart panes — a naming collision that made this look already done. A
  second chart needed a layout decision this document had not made: what a pane owns, what stays
  page-level, and what happens when there are four of them and the window is narrow. All three
  are now written down under *More than one chart* above, and built. The layout half is the same
  question as *The layout: what gives way when the window narrows*, because four panes in a row
  and one pane in a row are the same flex container.

  The one thing that made it cheap was already true and is worth naming: `build_scene` is a
  pure function of its request, so a pane is a *view* of a stateless engine and the second pane
  needed no engine work at all. Had the engine held a viewport between calls, this would have
  been a rewrite rather than a refactor.

  **Never specified, and half-built until 2026-09-17: responsive layout.** The canvas always
  scaled correctly — the shell sizes it from the wrapper's `clientWidth`/`clientHeight` times
  `devicePixelRatio`, and a `resize` re-renders — but the *page* did not adapt: `index.html` had
  a fixed `aside { width: 380px }` and no `@media` query anywhere, so a narrow viewport squeezed
  the chart instead of reflowing. Now specified and built: see *The layout: what gives way when
  the window narrows* above. What remains of it is the part that cannot be checked here — the
  breakpoints have never been seen in a browser.

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

  **Added 2026-09-22 (closing `docs/19` row 7): concepts, in the same three rules.** The
  concept editor is part of this builder rather than a fourth mode — the concepts block is
  one more section of the same document, driven by the same schema (`concepts.parts`,
  `concepts.selectors`, `concepts.ops`, `concepts.window`, `concepts.max_concepts`,
  `concepts.sides` off `GET /strategies/schema`). A selector it cannot model is kept as raw
  text the same way a condition is, `emit → parse → emit` idempotence covers concepts with
  the same fixture discipline, and applying the form writes the `concepts:` block through
  `documentFromConceptForm` — one emitter, one parser, no second execution path. The
  YAML emitter was generalized for this: a mapping nested inside a list item (a concept's
  `lower: {high: 0}`, a requirement's operands) is now emitted as a nested block mapping
  rather than refused, which is what the grammar's hand-written `Selector` serde impls
  expect.

  **Added 2026-09-22, from "it looks like a rocket remote control — a beginner cannot
  tell what is going on".** The indicator tab was a stack of headings and raw rows, with the
  conversation buried under a name field, a revision list, and an alerts list. The chat is
  the product of that tab, so it now leads: the pane opens on a list of *chats* — a name,
  a timeframe, and one row per conversation — and picking one opens a single conversation
  whose message stream scrolls between a pinned header and a pinned composer. Each message
  is a bubble (yours on the right in the accent, the AI's on the left in the panel tone),
  the generated source folds into a `<details>` inside its bubble, and revisions and alerts
  collapse to two thin bars above the stream so the composer keeps the bottom of the pane.
  One rule is load-bearing and easy to get wrong: an author `display: flex` outranks the
  `hidden` attribute's UA rule, so every view that is a flex column must also declare
  `[hidden] { display: none }` or both views render at once.

## Real-time data handling
- Subscribe to `/ws/market/{symbol}/{timeframe}` for the active chart; resubscribe on
  symbol/timeframe change; unsubscribe on unmount to avoid leaking server-side fan-out
  resources.
- Apply client-side interpolation/coalescing for very high-frequency updates if the
  render loop can't keep up — never let the UI thread block waiting on network data.

**Added 2026-09-17, from "why am I not seeing live data on the chart — it has been over seven
hours and I am seeing the same candles on each timeframe".** The chart was not broken. There was
no feed: `MARKET_FEED` defaults to `off`, so `ensure_feed` returned early with a `warn!` in the
log and nothing at all on the wire. The chart drew whatever `GET /candles` returned and sat
still. The silence was the defect — **a channel that is quiet for a reason it does not state is
indistinguishable from a channel that is broken**, and the user had no way to tell which one
they were looking at.

So the market channel now says why, the way the order-book channel already did:

- **`/ws/market/{symbol}/{timeframe}` sends a `Notice` naming `MARKET_FEED`** when `FeedMode` is
  not `Binance`, instead of sending `Subscribed` and then nothing. It fires immediately rather than
  after a grace period, unlike the order book's `DEPTH_GRACE`: the order book has to wait to see
  whether depth arrives, while this fact is already known.
- **The shell holds the notice rather than writing it once.** `render()` owns the note strip,
  so a message written straight into `el("chartNote")` is wiped by the next render — and a pan
  is a render. The notice lives in a variable and is re-applied every frame, which is why it
  survives a scroll wheel. The harness asserts exactly that, because "it appeared" and "it
  stayed" are different claims and only the second one helps.

**The first version of that notice also closed the socket, and that was wrong** — found on
2026-09-18 by running the workspace suite, which had not been run when the notice was added. Two
tests in `crates/api-gateway/tests/ws_flow.rs` went red: they publish a candle with
`feed_candle` and assert it reaches the client, and a channel that closes cannot forward anything.

The temptation is to call the tests outdated and teach them about the feed mode. They are right
and the notice was wrong, for a reason worth writing down: **`FeedMode::Off` means "this gateway
will not open a feed", not "no candle will ever be published"** — its own documentation says a bot
receives whatever something else publishes into the bus, and `feed_candle` is exactly that
something. The order book may close after its notice because it *waited* for a book and has
evidence; the market channel decided from configuration, which is a guess about a bus it does not
own. Closing on a guess refused data that exists — and it put the resolution filter in
`market_loop` beyond the reach of the only two tests that witness it, which is the defect this
document exists to catch, wearing the fix as a disguise. The notice explains the silence; the
channel keeps its contract. `a_channel_that_has_been_told_there_is_no_feed_still_forwards_a_candle`
pins it, and was shown to fail by re-adding the close.

The shell needed the matching change: a `data` frame now sets `live.state = "open"`, so a candle
that arrives after a notice clears it. Without that the badge would tick its age while still
saying `no feed` — contradicting the evidence it is carrying. `guard_check.py` patches the line out
and requires the check that names it to fail.

**Two things were found while proving it, and both are the same shape.**

*Every candle arrived twice.* `BinanceCollector` owns a `MultiTimeframeCandleBuilder` per symbol
and publishes closed candles into the bus itself; `run_binance_feed` built a **second** builder
from the same trades and published again. Identical values, because both were fed the same
trades — so the shell's replace-on-equal-`open_time` hid it and the chart looked correct. The
duplicate was only visible on the wire. **One builder, one publisher:** the gateway's builder is
gone and `run_binance_feed` now only holds the collector open and watches the bus.

*`stale_market_data` could not fire however dead the feed was.* Its input, `MD_FEED_AGE`
(`market_data_feed_age_seconds`), was written **only** by `BotSupervisor::feed_candle` — which
only tests call. The live path publishes through the collector, so `/metrics` carried no
`market_data_feed_age_seconds` at all while candles were demonstrably flowing, and the alert rule
read a metric nothing wrote. That is the organising rule in its purest form: **who writes this,
and what happens if they don't?** The answer was "nobody, and the alert silently never fires".
`feed_candle` is now split into `note_candle` (stamp the age) and the publish half, and the live
path calls the stamp from a bus watcher. The test that covers it was **shown to fail** against
the bug: commenting the `note_candle` call out makes it report *"a candle the collector published
must age the feed"*.

**And the third thing, which is the one the report was actually about, and it is not fixed.**
The gateway's live feed is **never written to the database, and that is now the design rather
than the defect.** This section used to describe a 13.1-hour gap between the live feed and the
stored series, and `docs/19` row 21 was opened for it. The row was closed the other way round:
the database is a free tier with 6 GB for every symbol of every market, one symbol's trades are
~110 MB a day, so persisting the feed would fill it in weeks. `docs/19` rows 21 and 22 record
what replaced it.

Market data now has exactly two homes, and neither is Postgres:

| data | where it lives | how far back it goes |
|---|---|---|
| candles, recent | `market_data::history`, a bounded RAM buffer (1500 bars per symbol and resolution) | 1500 bars -- 25 hours at 1m |
| candles, older | the venue's REST klines, fetched on demand and dropped | as far back as the venue serves |
| trades | `market_data::tape`, a bounded RAM ring (100,000 per symbol) | **~33 minutes of BTCUSDT** at ~50 trades/s |
| the book | `market_data::BookCache`, newest snapshot per symbol | the current book only |

So the two chart modes behave the same way *while the page is open*, and differ only in how far
back they can look:

| mode | while the page is open | how far back |
|---|---|---|
| **candles** | moves -- the socket appends each closed bar | unlimited; the older part costs one REST call per 1000 bars |
| **footprint** | moves, but is rebuilt from `/footprint` over the tape | **the tape's span only** |

**The footprint limit is real and is not going away.** A footprint is built from individual
trades, and trades are the one thing that cannot be stored under this constraint. 100,000 trades
is about 33 minutes of BTCUSDT; a window older than the tape is filled from the venue's aggTrades
up to a 10-minute gap (one request per thousand trades -- an hour of a liquid symbol is ~180
requests), and beyond that `/footprint` returns a `note` saying which part of the ladder is
missing. `GET /footprint/coverage` exists so the UI can ask what window actually works instead of
guessing. If deeper footprint history is ever wanted, the tape size is the knob, and the cost is
RAM rather than disk.

### The live badge: evidence, not a claim

Added 2026-09-17, from "about the real time binance data I am not sure and I can't prove it —
maybe you add a message on the dashboard saying that the live data is working". The request is
for a **proof**, and the trap is obvious once stated: a green light that cannot go out is exactly
the thing this user would check, and it would have been green through the seven-hour silence that
started all of this. So the badge is built to be able to deny its own headline.

**It reports the age of the last frame that arrived, not the socket's `readyState`.** A socket can
be open and silent — that is precisely the failure being reported, and it is invisible to
connection state. The page keeps `live.at` (when a frame last arrived) and `live.bar` (the
`open_time` it carried), and the badge is a function of those plus the clock:

| state | when | reads |
|---|---|---|
| `idle` | no channel has been opened | `no channel yet` |
| `connecting` | a channel is opening | `connecting…` |
| `nofeed` | the server sent a `Notice` and closed | `no feed` |
| `offline` | the socket closed for any other reason | `offline` |
| `idle` (open, no bar) | connected, nothing delivered yet | `waiting for a bar` |
| `stale` | quiet for more than `bar * 1.5 + 30s` | `quiet 11m` |
| `behind` | frames arriving, drawn ladder older than 2 bars | `feed live · ladder 13h behind` |
| `live` | a frame within the threshold | `live · BTCUSDT 5m · 21:10 · 0s ago` |

**The `behind` state is the honest half, and it exists because of row 21.** In footprint mode the
chart is not drawn from the channel at all — the ladder comes from `/footprint`, which reads
stored trades — so the feed can be perfectly healthy while what is on screen is hours old. A badge
that only knew about the socket would read `live` in that case, which is a green light over a
frozen chart: the same defect, wearing the fix as a disguise. `ladderLagMs()` compares the newest
`open_time` in the drawn ladder against the newest bar the channel delivered, and when the two
disagree by more than two bars the badge names **which** of the two is stale rather than reporting
one number for both. Until the persistence gap is closed this is the correct reading, and it is
the reading that makes the badge worth trusting.

**Three details, each of which was wrong in a version that looked right:**

- **The age must tick on its own timer, not as a side effect of `render`.** `liveTimer` is a
  one-second interval. A badge refreshed by the render loop can never report the case it exists
  for, because the case is *nothing is arriving and therefore nothing is redrawing*.
  `guard_check.py` patches `liveTimer = 0` and requires the staleness check to fail, which is how
  the timer was shown to be load-bearing rather than incidental.
- **A refused channel is not an offline one.** `onclose` sets `offline`, and it used to overwrite
  the `nofeed` the notice had just set — so the one case where the server *explains itself* was
  the one case that lost the explanation. The close handler now leaves `nofeed` alone.
- **`idle` covered three different facts** (`no channel yet`, `connecting…`, `waiting for a bar`).
  An open channel with nothing on it read as "no channel yet", which is the wrong answer to the
  user's question and the more reassuring one. Split.

**The page's clock can be moved without moving the harness's.** Reaching `stale` means waiting
`bar * 1.5 + 30s` — 11 minutes of real time for a 5m chart — so `tools/shell_check.mjs` replaces
`window.Date.now` with `realPageNow() + pageClock.offset` before injecting the page. Only the
page's clock is fake; `waitFor`'s deadlines keep the real one, so a hung check still times out.
The check then advances `pageClock.offset` past the threshold and **waits 1200 ms for the timer**
— with no gesture and no redraw, because that is the claim. An earlier version of this check
forced a render first, which would have passed for the wrong reason: it would have been testing
`render`, not the timer.

**And the fixture had to be moved out of the epoch.** `footprintColumns` built its timestamps as
`i * 300_000_000_000`, which is tidy and made the `behind` comparison read *thirteen thousand
hours behind* — a number so absurd that the check would have passed against almost any threshold.
The fixture now ends at a real recent millisecond (`FIXTURE_NOW_MS`), so the assertion is against
a plausible lag and the check can fail for the right reason. **A fixture tidier than production
hides production's defects**, which is the second time this project has paid for that lesson in
`/symbols` fixtures alone.

## Done criteria
- A user can load a symbol, switch to footprint mode, see live-updating footprint cells
  driven by real trade data, ask the AI a question, and see the response's referenced
  price levels highlighted on the chart — all without a full page reload.
- The chosen hosting architecture (pure Rust/WASM vs. React shell + WASM chart module)
  is documented here as an explicit decision with the date and rationale it was made.

## DECISION — 2026-09-24: chart object engine (`docs/21`)
The drawing system is now a subsystem, not a list of buttons: kinds + tool
registry live in `chart-engine::drawing` (the one place a tool is declared),
the toolbar is built from the registry over the ABI, the magnet snaps
engine-side from OHLC + volume-profile levels, and undo/redo operates on
guarded commands found by shape rather than id. Eight kinds ship
(trendline, hline, vline, ray, extended, rect, fib, measure) in three groups.
Protocol, invariants, and the phase-2 roadmap (fib family, channels,
positions, AI analysis layers) are in **`docs/21-CHART-OBJECT-ENGINE.md`**.
