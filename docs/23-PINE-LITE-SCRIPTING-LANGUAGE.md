# 23 — Pine-lite: an imperative scripting language for indicators and strategies

## Purpose

Amend the one-representation principle (`docs/06`) for the indicator layer. The
platform gains a **second** representation, alongside the Strategy DSL, that
follows TradingView's Pine Script design instead of the declarative `concepts`
vocabulary:

- **Code, not data.** An indicator is an imperative script — math, loops,
  functions, series — written by the AI studio or a developer, exactly the way
  Pine Script is written.
- **Interpreted & executed per bar.** The script runs once per closed candle with
  series history indexing (`close[1]`), Pine-like `na` semantics, and state that
  accumulates across bars (`var`).
- **The platform vets scripts.** Static validation (parse, type check, limits)
  and runtime budgets refuse invalid or resource-heavy code before it draws.
  Current status: the browser chart engine runs Pine-Lite in its WASM artifact;
  gateway preview runs the metered interpreter in-process. The server-side
  `sandbox-guest` currently serves the Strategy DSL and does not link
  `pine-lite`, so Pine-Lite guest isolation remains pending.
- **Arbitrary plots.** `plot()`, `plotshape()`, `plotchar()`, `hline()`,
  `fill()`, and annotations, on the price pane (`overlay=true`) or in their own
  sub-pane, with per-plot inputs.

Everything `docs/06` says about `kind: indicator` documents, `concepts`, and the
`strategy-dsl` remains true. This document adds the code path; the two live
side by side, and Phase 9's routes treat a workspace as carrying either
representation.

## Why Pine's model and not a second DSL

Pine Script is the most-imitated indicator language in the industry for one
reason: the **series-per-bar execution model**. A script is a program run once
per bar; every variable is implicitly a series; history is an index away. That
model, not the syntax, is what users and LLMs already know how to produce. The
platform therefore adopts the semantics wholesale and calls the result
**Pine-lite**.

Non-goals (deliberate):

- No hosting of other users' scripts, no marketplace, no publisher trust model.
- No broker emulator parity with TradingView's `strategy.*` defaults beyond
  what Phase 7 specifies (we already own a deterministic backtester and reuse
  it).
- No Pine source compatibility — scripts that run on TradingView will need
  small edits. The syntax aims at the common subset with v5 naming.

## Crate and dependency rules

New crate `crates/pine-lite` (leaf, like `analytics-core`):

Current Pine-Lite dependency edges:

```
pine-lite -> analytics-core
chart-engine -> pine-lite
api-gateway -> pine-lite
backtester -> pine-lite
```

`sandbox-guest` currently depends on `strategy-runtime` and `strategy-dsl`, not
`pine-lite`. Whether to add a Pine-Lite guest front-end or retain the
fuel-metered in-process gateway preview is an open isolation decision; do not
describe the current preview path as WASM-sandboxed.

`ai-agent` must never depend on `pine-lite` (its rule stands): the AI emits
*text*; the gateway calls `pine-lite` to vet it. `analytics-core` is the only
source of indicator math, so `ta.*` is implemented **by calling** the existing
functions, never by reimplementing them.

## The language

### Execution model

- A script is entered once **per bar** (Pine: "the bar-close event"), over the
  window the host passes in. The host owns the candles; the script cannot fetch
  data, call I/O, or read the clock.
- Every expression evaluates per bar. Assignments create **series**: a
  variable holds one value per bar, and `x[3]` reads the value three bars ago.
- `na` is a real value in every numeric series. Arithmetic with `na` yields
  `na`; comparisons with `na` are false; `na(x)`, `nz(x, y)`, `fixnan(x)` are
  the handling builtins. Warm-up periods are `na`, never `None`-masked away at
  the plot layer: plots simply skip `na`.
- State across bars requires `var` (initialized once, reassigned per bar) or
  the accumulation builtins. This is Pine's rule and it makes determinism
  auditable: the only cross-bar state is visible in the source.

### Types

`float` (the default numeric; `int` is a float subtype as in Pine — bool and
string exist but cannot be plotted), `bool`, `string`, `color` literals
(`color.red`, `#RRGGBB`/`#RRGGBBAA`), and series-ness is implicit: a scalar
literal used in series context promotes to a constant series (Pine's
"series-ness is viral" rule, simplified: everything is a series, scalars are
just constant ones).

### Grammar (informal; the parser is the arbiter)

```ebnf
script       = { annotation } statement
annotation   = "//" "@" "pine_lite" version "=" number , [attr { "," attr }] NEWLINE
statement    = var_decl | assign | if_stmt | for_stmt | while_stmt
             | func_def | call_stmt | plot_stmt | break | continue
var_decl     = ( "var" | "varip" )? [type] IDENT [ ":" type ] "=" expr
assign       = [":="] IDENT ( "+=" | "-=" | "*" | "/=" | "%=" )? "=" expr
if_stmt      = "if" expr block { "else" "if" expr block } [ "else" block ]
for_stmt     = "for" IDENT "=" expr "to" expr [ "by" expr ] block
func_def     = IDENT "(" [ params ] ")" "=" ">" block
plot_stmt    = ("plot" | "plotshape" | "plotchar" | "plotarrow"
              | "hline" | "fill" | "bgcolor" | "barcolor") "(" args ")"
expr         = ternary
ternary      = or_expr [ "?" expr ":" expr ]
or_expr      = and_expr { "or" and_expr }
and_expr     = not_expr { "and" not_expr }
not_expr     = comparison { "not" comparison }
comparison   = additive [ ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) additive ]
additive     = unary { ( "+" | "-" ) unary }
unary        = [ "-" | "+" ] power
power        = postfix [ "**" postfix ]
postfix      = primary { "[" expr "]" | "." IDENT | "(" args ")" }
primary      = number | string | bool | "na" | color | IDENT | "(" expr ")"
```

A script **must** begin with the version annotation:

```
//@pine_lite version=1 overlay=true title="My RSI" max_bars_back=300 sec="ETHUSDT"
```

Unknown annotations are a validation error (the AI must not be able to invent
host knobs). `overlay=true` sends plots to the price pane; `overlay=false`
(the default) gives the script its own sub-pane. `sec="SYMBOL"` (optional)
declares a second instrument for cross-market math — see *Cross-market reads*
below.

### Operators, precedence, history

Standard arithmetic, comparison, boolean short-circuit, `?:`, and the logic
words `and` / `or` / `not`. History indexing `name[n]` is allowed on a
variable or a builtin series (`close[1]`, `ma[3]`) — **not** on a call
result: `ta.rsi(close, 14)[1]` is refused at vet time, so assign first
(`r = ta.rsi(close, 14)` then `r[1]`). The maximum history depth is
`max_bars_back`, default 300, hard cap 5000. Requesting deeper history is a
**compile error**, not a runtime `na`.

Multi-value calls are taken apart on the assignment line, one name per
output: `macd_line, signal_line, hist = ta.macd(close, 12, 26, 9)`,
`basis, upper, lower = ta.bb(close, 20, 2)`, `k, d = ta.stoch(close, 14)`.
A multi-output call read as a single value is a compile error naming the
fix; `ta.stoch` alone still reads as its %K. Other multi-output calls are
`ta.dmi(length)`, `ta.kc(length, mult)`, and
`ta.supertrend(mult, atr_length)`.

### Cross-market reads (`sec=` / `request.*`)

A script can see one instrument besides its chart. The pair is declared once
in the header — `sec="ETHUSDT"` — and read through **zero-argument** calls:
`request.symbol()`, `request.open()`, `request.high()`, `request.low()`,
`request.close()`, `request.volume()`, `request.time()` (the aligned bar's
open time — a change in it is the pair's new bar). The per-call form
`request.security("SYM", "tf", expression)` (Phase 11) evaluates the
expression over any pooled symbol+timeframe, aligned the same way; the symbol
and timeframe must be quoted literals, the pool caps at 8 keys per script.

- `sec=` is fetched at the chart timeframe. The host emits exactly one
  secondary candle slot for each primary candle, preserving indexes. At an
  exact matching open time it uses the fetched OHLCV. Before the first
  secondary candle it uses that candle's close as flat OHLC with zero volume.
  At a missing secondary timestamp after data has begun, it carries the last
  close as flat OHLC with zero total/buy/sell volume. Thus a gap is not a
  duplicated trade-volume observation. The host fetches the pair (stored data
  first, then supported venue backfill); the script never performs I/O.
- Every zero-argument `request.*` call is vet-refused when the header has no
  `sec=` (`typecheck::check_with_header`), and the runtime refuses again with
  the fix in the message when the host supplies no series — the same honesty
  as the rest of the vetting. `request.security` instead names its pool key,
  which the runtime reports when the host did not fill it.
- `request.*` calls are plain series expressions: history applies after
  assignment (`rc = request.close()` then `rc[1]`), and they compose with all
  ta functions — spreads (`close - request.close()`), ratios, and SMT
  divergence (our new swing high while the pair makes a lower high) are
  ordinary arithmetic over them.
- **Multi-timeframe reads** go through `request.security` with a timeframe
  literal. The gateway aligns the latest pooled bar whose open time is at or
  before each chart bar; before the pooled series starts, it supplies the
  first pooled close as flat OHLC and zero volume. The VM's default
  `gaps_off`/`lookahead_off` reveals the previous higher-timeframe value
  during the bucket and the current value at the bucket's last available chart
  bar. `gaps_on` returns `na` between reveal bars; `lookahead_on` exposes the
  current bucket value from its first chart bar and can therefore leak future
  values in historical analysis.

  **Current limitation:** pooled candles are expanded onto chart-bar indexes
  before the expression runs. A simple expression such as `close` is
  well-defined, but history indexing and stateful/rolling functions inside
  `request.security` currently advance at chart-bar cadence over repeated
  higher-timeframe candles, not once per distinct higher-timeframe candle.
  The expanded candle's `open_time` is also the chart bar's time, so
  `request.security(..., time)` is not currently a reliable higher-timeframe
  boundary timestamp.
  The merge also treats the last available chart bar as the bucket end, even
  when the supplied window ends partway through a bucket. Do not rely on
  higher-timeframe rolling calculations or the partial final bucket for
  non-repainting decisions until those cases are corrected and covered by
  executable tests. Time-anchored drawing objects still require absolute
  timestamps; an HTF zone's edges are not chart bar indexes (`docs/28`).

### Host-data alignment and missing values

- The script's primary candles are supplied in ascending open-time order. Each
  candle's `open_time` is UTC nanoseconds; script execution and `barstate.isconfirmed`
  represent the host's supplied closed-bar window, not a live tick callback.
  History length and the current window are host decisions.
- `request.security_lower_tf` receives raw lower-timeframe candles and returns
  the values whose open times fall in `[chart_open, chart_open + chart_tf)`.
  It does not carry missing sub-bars forward. With a supplied empty pool the
  result is an empty array; if the host could not populate the requested pool,
  execution fails with a missing-series diagnostic.
- `request.data("SYMBOL.field")` supports ticker fields (`change_pct`,
  `quote_volume`, `high`, `low`, `last`) as one point-in-time ticker snapshot
  repeated over the requested window. `funding` and `oi` use provider points
  carried forward to chart bars by timestamp; bars before the first point are
  `na`. A feed the host did not supply raises a runtime missing-data error;
  it never silently becomes zero.
- Pine-Lite currently exposes candle OHLCV, not `Candle.buy_volume` /
  `sell_volume`, individual trades, order-book depth, or footprint levels.
  The Rust candle type's buy/sell split is not by itself a script capability.
  No new order-flow API is specified here: add one only with a real provider,
  timestamp/coverage semantics, and executable conformance tests.

## Builtin namespaces

Call arity, static result kinds, and multi-output shapes are registered in
`pine-lite::builtins::CATALOG` (schema version 2). The generator prompt and
repair cheatsheet include a section rendered from that catalog, and the type
checker reads the same metadata. Parameter names/defaults, host-data
requirements, and runtime semantics are still documented and implemented
separately; the registry does not yet replace those contracts.

| Namespace | Contents (v1) |
| --- | --- |
| `ta.` | sma, ema, rma (Wilder), wma, rsi, atr, tr, change, mom, roc, highest, lowest, UTC-day session VWAP, crossover/crossunder/cross; multi-value: macd → (line, signal, hist), bb → (basis, upper, lower), stoch → (k, d) |
| `math.` | abs, min, max, floor, ceil, round, pow, sqrt, log, exp, sign, avg, sum |
| `input.` | int, float, bool, string, color — with `defval`, `minval`, `maxval`, `title`; values are host-supplied per chart |
| `strategy.` | entry, exit, close, close_all, cancel — see Phase 7 |
| `color.` | 17 named colors + hex |
| free functions | `na`, `nz`, `fixnan`, `barstate.isconfirmed` (always true in this platform — the host only runs on closed bars), `bar_index`, `last_bar_index`, `time`, `time_close`, `open/high/low/close/volume`, `hl2/hlc3/ohlc4`, `syminfo.ticker` |

`ta.sma`, `ta.ema`, `ta.rsi`, and `ta.atr` currently wrap their
`analytics-core` counterparts; `ta.vwap` uses its per-bar UTC-session series
API. Other formulas live in `pine-lite/src/ta.rs`, including functions with
existing `analytics-core` equivalents (for example MACD, Bollinger bands,
stochastic, CCI, MFI, Williams %R, OBV, and Supertrend). Their warm-up and
edge-case semantics must be compared before consolidating them; do not assume
cross-crate numerical parity from the function name alone. New shared
indicator formulas should be added to `analytics-core` first, then exposed by
Pine-Lite with the language's explicit `na`/series mapping.

## Plot contract

- `plot(series, title=, color=, linewidth=, style=, display=)` — line by
  default; styles: line, linebr, histogram, columns, circles, stepline, areabr.
- `hline(price, title=, color=, linestyle=)` — a level in the script's pane.
- `plotshape`/`plotchar`/`plotarrow` — point evidence at a bar, with
  `location=abovebar/belowbar/absolute/top/bottom`.
- `fill(plot1, plot2, color=)` — between two plots of the same script.
- `bgcolor`/`barcolor` — pane/candle tinting.
- Budgets (static, enforced by the vetting layer): ≤ 64 plots, ≤ 8 fills,
  ≤ 500 statements, ≤ 32 user functions, history depth ≤ 5000. The hard rule
  from docs/14 stands: the script produces **series data**, and the chart
  engine — not the script, not JavaScript — positions it into the scene.
- The engine's output contract gains a `ScriptPane` payload next to
  `IndicatorOutput`: one pane (overlay or sub-pane), plots as
  positioned polylines/rects with stable ids and titles, input values echoed
  for the UI.

## Strategy semantics (Phase 7)

`strategy.*` calls feed the **existing** deterministic replay
(`backtester::replay`), not a new simulator:

- `strategy.entry(id, direction, qty=)` / `strategy.exit(...)` /
  `strategy.close(...)` record intents; the host translates intents into
  `ReplaySignal`s using Pine's fill-on-next-tick rule with a configurable
  slippage/fee model identical to the backtester's.
- `strategy()` header sets defaults (`initial_capital`, `default_qty_type`,
  `commission_type/value`, `pyramiding=0`).
- Outputs: the standard backtest report (same schema as `strategy-cli`
  reports) plus a trade list; on the chart, entries/exits become
  `plotshape`-class evidence so a strategy script renders exactly like an
  indicator script plus trade markers.

> **Status:** strategy simulation is implemented in the Pine-Lite VM and
> rendered by the chart engine (docs/24, phases S1–S3). S4 remains pending.
> `backtester::script_strategy` is a separate replay adapter; it is tested,
> but the audit found no production path constructing it. Do not treat the
> adapter or chart preview as live-bot deployment: the production bot path
> remains based on the Strategy DSL and its sandbox.

## Vetting: "the platform vets scripts" — concretely

Layered, cheapest first, all of it deterministic:

Vetting diagnostics expose stable phase codes (`PINE-LEX-001`,
`PINE-PARSE-001`, `PINE-TYPE-001`, `PINE-LIMIT-001`) and a structured
`span: { line, col }`. Codes classify the producing layer, not a particular
message; human-readable wording may improve without changing the code.
`/scripts/vet` keeps the existing top-level `line` and `col` fields for
compatibility, while AI repair feedback carries both those fields and `span`.

1. **Parse** — grammar refusal with line/col, error list not first-error (the
   AI repair loop needs the full list).
2. **Type check** — unknown identifiers/functions, arity, Pine casting rules
   (int→float ok, float→int refused, na in float context ok, bool in numeric
   context refused).
3. **Static analyser** — the budget caps above; recursion refused; loop
   nesting capped (4). `while` (v1.1) is allowed: its iterations are fuel
   metered at run time exactly like `for`, so a condition that never turns
   false dies on the step budget with the same message any runaway loop
   gets — statically proving termination is not required, only bounding it.
   Assignments to builtins refused; `varip` allowed but excluded from
   backtest replay equivalence (documented divergence, matching Pine).
4. **Sandbox** — the script runs inside the existing WASM guest with the same
   fuel metering and memory ceilings `docs/08` already enforces; the guest's
   `interpret` gains a Pine-lite front-end. A script that outlives its fuel is
   killed and reported as a validation failure, never drawn half-way.

Dynamic budgets scale with window: fuel = 1M steps per 1000 bars (raised from
200k in v1.1 after live generation showed real strategies burn ~200 steps per
bar; measured at n=2016 and n=3000 bars), so a
300-bar chart costs ~60k steps. A script that cannot finish cannot ship.

## The AI generation loop (studio integration)

The workspace chat's prompt changes from "emit YAML" to "emit a Pine-lite
script" for code-first workspaces:

1. Model emits script text (one file, one revision).
2. Gateway vets it through layers 1–3 and returns the **full error list**.
3. The agent re-prompts with the script + errors, up to 5 attempts — the
   existing `max_attempts`/`repaired_errors` loop, unchanged in shape.
4. The vetted script runs through the fuel-metered interpreter in the gateway
   process over the preview window (7 days, the same `indicator_preview`
   path), producing plot series and the evidence preview. This path is not
   currently executed in `sandbox-guest`.
5. Revision stored with `status: validated|rejected`, exactly like today.

Because the validator is deterministic, "the platform vets scripts" is not a
trust decision about the model: an unvetted script cannot reach a chart, a
backtest, or a bot.

## Frontend integration (Phase 10)

- The studio's Code panel becomes the script editor (it already saves
  revisions Pine-editor style); the language switcher on a workspace decides
  YAML-document vs script.
- The chart gains per-script pane management: legend row per plot (title +
  live value at cursor), pane collapse/resize (the engine's `SUB_PANE_HEIGHT`
  layout already exists), settings popover per input (rendered from the
  script's `input.*` declarations, values sent per request), remove (the
  existing chip).
- Requests grow a `scripts: [{ id, source, inputs }]` field; the engine
  currently vets and interprets scripts when building each scene; it does not
  cache a compiled script by source hash. The current settings popover edits
  boolean and numeric inputs; string/color declarations are shown as
  code-only.

## Phased plan

| Phase | Delivers | Acceptance criterion | Status and source/test evidence |
| --- | --- | --- | --- |
| 1 | Crate skeleton, lexer, error list, no deps beyond analytics-core | Round-trip tests over fixtures | **done** — lexer/header tests in `crates/pine-lite/src/lex.rs`; leaf dependency is visible in `crates/pine-lite/Cargo.toml`. |
| 2 | Parser → AST with spans | Golden parse tests incl. error cases | **done** — parser and span/error cases in `crates/pine-lite/src/parse.rs`. |
| 3 | Type checker + casting rules | Refusal tests for every casting rule | **done** — checker refusal and arity tests in `crates/pine-lite/src/typecheck.rs`; public vet path in `src/lib.rs`. |
| 4 | Series interpreter: per-bar exec, history, na, var, inputs | Equivalence: hand-computed series fixtures | **done** for the implemented value model — runtime and fuel tests in `crates/pine-lite/src/interp.rs`, `tests/model_ergonomics.rs`, and `tests/strategy_execution.rs`. |
| 5 | `ta.*` over analytics-core (incl. new macd/stoch/bb there) | Parity tests vs analytics-core units | **done** for the exposed, tested set — implementations and numeric fixtures in `crates/pine-lite/src/ta.rs`, including pinned SMA/EMA/RSI/ATR parity. This is not full Pine TA parity. |
| 6 | Plot statements → `ScriptPane` output contract | Positioning tests in chart-engine | **done** for current plot/drawing types — projection tests in `frontend/chart-engine/src/script.rs` and `scene.rs`; frontend painter in `frontend/app/app.js`. |
| 7 | Strategy calls → backtester replay intents | Pine execution and `ScriptStrategy` replay agree on supported intents; runtime failures are observable | **done** for the adapter's supported intents — `crates/backtester/src/script_strategy.rs` and `crates/backtester/tests/script_parity.rs`; runtime failures now abort replay with diagnostics. |
| 8 | Static analyser + sandbox front-end + fuel budgets | Adversarial tests mirror docs/08's list | **partial** — Pine-Lite static limits and fuel/call/array/object guards are in `crates/pine-lite/src/limits.rs` and `interp.rs`, with runaway-loop tests in `tests/model_ergonomics.rs`. The gateway still invokes native `pine_lite::run`; Pine-Lite is not in the Wasmtime guest. See the isolation decision in `docs/08-SANDBOX-WASM.md`. |
| 9 | Gateway: `/scripts/*` routes, workspace language switch, AI loop | `indicator_workspace_flow`-style gateway tests | **partial** — `/scripts/vet`, generation/edit/repair/preview, and revision storage are implemented in `crates/api-gateway/src/script_routes.rs` and `indicator_workspace_routes.rs`, covered by `indicator_workspace_flow.rs` and `indicator_preview_flow.rs`. Strategy-document and Pine-Lite code representations use distinct flows; no single in-place workspace language switch was found. |
| 10 | Chart engine + shell: rendering, inputs UI, pane management | Shell check script + visual parity vs RSI sub-pane | **partial** — render/projection and layer tests are in `frontend/chart-engine/src/script.rs` and `scene.rs`; layer controls and boolean/numeric settings are in `frontend/app/app.js`. String/color settings and full pane/visual acceptance remain open. |

Phases 1–5 are the language core and land as one reviewable unit; 6–8 make it
visible and safe; 9–10 wire it to users.

### Phases 11–17: implementation status (reviewed 2026-11)

These are scoped Light Pine capabilities, not a claim of full TradingView Pine
parity. Status is based on the implementation and named tests; known host-data
limitations remain explicit even when the language construct itself is tested.

| Phase | Delivered capability | Status, evidence, and remaining limitation |
| --- | --- | --- |
| 11 | **Multi-pair + per-call timeframe**: `request.security("ETHUSDT", "15m", expression)`, keyed host pools, eight-key cap, and `sec=` shorthand. | **partial** — literal collection, cap, VM evaluation, host fetch/alignment, and chart-engine paths exist in `crates/pine-lite/src/typecheck.rs`, `interp.rs`, `crates/api-gateway/src/indicator_preview.rs`, and `frontend/chart-engine/src/script.rs`; tests include `tests/request_security.rs` and chart-engine MTF cases. HTF rolling/history expressions advance at chart cadence, `time` uses chart-bar timestamps after host alignment, and the final bucket is treated as complete. These semantic gaps are documented above and block a complete status. |
| 12 | **User-defined functions** with typed call sites, series-passing semantics, and recursion limits. | **done** — parser/checker/runtime in `parse.rs`, `typecheck.rs`, and `interp.rs`; `tests/user_functions.rs` covers execution, TA composition, arity, and recursion refusal. |
| 13 | **Drawing objects** with bounded VM heap and chart-coordinate projection. | **done** for implemented line/label/box/Fibonacci objects — `tests/drawing_objects.rs`, `frontend/chart-engine/src/scene.rs` projection tests, and `frontend/app/app.js` painter. This does not imply every requested visualization type exists. |
| 14 | **Provider-backed `request.data`** for ticker snapshots and timestamped funding/open-interest series. Generic HTTP is explicitly not part of this phase. | **done** for the supported provider fields — collection/runtime behavior in `typecheck.rs` and `interp.rs`, gateway loading in `indicator_preview.rs`, and `tests/request_data.rs`. Missing feeds fail visibly; generic HTTP remains unimplemented and permission-gated future work. |
| 15 | **v1.1 expressiveness**: `while`, user-function parameter defaults, and 256-object cap. | **done** — implementation in `interp.rs`, `limits.rs`, and `typecheck.rs`; behavior and runaway-loop coverage in `tests/model_ergonomics.rs`, with the heap cap in `tests/drawing_objects.rs`. |
| 16 | **Time-anchored drawings** and aligned-secondary `request.time()`. | **partial** — VM and chart time-coordinate mapping are tested in `tests/time_anchored_objects.rs` and `frontend/chart-engine/src/scene.rs`; `request.time()` is verified for `sec=`. The gateway's per-call MTF aligner rewrites pooled `open_time` to chart time, so `request.security(..., time)` cannot currently anchor a true HTF boundary. |
| 17 | **Optional box borders** with backward-compatible serialization. | **done** — constructor/runtime and wire behavior are covered by `tests/drawing_objects.rs` and `frontend/chart-engine/src/scene.rs`; unset borders preserve the fill-only rendering. |

The next work is correctness-first: close the MTF history/timestamp limitations
in phases 11 and 16 before claiming higher-timeframe parity. Phase 14 is
provider-backed data only; generic HTTP remains out of scope until permissions,
response-size caps, and caching are designed and tested.

## Implementation notes (2026-09)

Decisions the code made that the spec above only implied:

- **The VM provides budgets, not process isolation.** The interpreter meters
  itself: `FUEL_PER_1000_BARS` (200k steps per 1000 bars) bounds total work,
  `MAX_CALL_DEPTH` (16) refuses recursion, zero-step `for` loops do not run,
  and a script that exceeds fuel is an error, never a half-drawn pane. Gateway
  preview still executes this VM in-process; the separate server-side WASM
  guest integration remains pending and must be designed before claiming
  process isolation for untrusted scripts.
- **hline acts once.** `hline()` has no series; running it per bar would
  collect one level per bar. It acts at bar 0 only, like Pine's global
  declarations.
- **Plot calls defer.** A `plot` expression is replayed *after* the bar loop
  against completed buffers -- replaying at bar 0 would read half-empty
  history. One AST call site is one plot, however many bars reach it.
- **Loop variables bind.** `for i = a to b` pushes `i` onto a loop scope;
  the checker types it, the VM binds it, and `s := s + close[k]` works.
- **Test fixtures use `concat!`, not `\n\` continuations.** Rust's line
  continuation strips leading whitespace, which would flatten an indented
  block into top-level statements -- a script-language trap the fixtures
  must not fall into.

## Risks

- **Scope.** Pine is a decade of accretions. v1 is the common subset; the
  grammar table above is the contract, and anything not listed is refused with
  a message naming what exists.
- **Two representations.** docs/06's principle is amended, not broken: the
  Strategy DSL remains the bot-facing contract; a script strategy compiles to
  replay intents, which are the same thing the DSL produces. The compile
  target is shared, the source is not.
- **Sandbox equivalence.** `varip` and any future intrabar feature are
  excluded from the native/WASM equivalence guarantee until they have tests
  that pin their divergence, the same way the existing guest treats
  time-dependent calls.
