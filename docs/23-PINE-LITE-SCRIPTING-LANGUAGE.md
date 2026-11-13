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
  gateway preview runs the interpreter in the Wasmtime `sandbox-guest` with
  fuel, epoch, memory, request/response, and concurrent-job limits. On Linux,
  the gateway invokes a separate worker process with Landlock/seccomp and
  OS resource limits; unsupported isolation fails closed. Non-Linux
  development remains in-process. The guest receives only host-fetched data
  and has no network, filesystem, broker, or order-placement capability; see
  `docs/08-SANDBOX-WASM.md`.
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

### Version and migration contract

The current source-language version is **1**. `GET /scripts/capabilities`
returns that version together with the machine-readable builtin catalog; its
`catalog.schema_version` versions the catalog wire shape independently of
script syntax. Additive builtin entries do not change either version. A
breaking syntax or execution-semantics change requires a language-version
bump, and a breaking catalog JSON change requires a catalog-schema bump.
Pine-Lite does not auto-convert TradingView Pine: migrate by adapting the
script to the documented v1 subset, then run `POST /scripts/vet` and preview
it on representative candles before activating it.

## Crate and dependency rules

New crate `crates/pine-lite` (leaf, like `analytics-core`):

Current Pine-Lite dependency edges:

```
pine-lite -> analytics-core
chart-engine -> pine-lite
api-gateway -> pine-lite
backtester -> pine-lite
sandbox-guest -> pine-lite
sandbox -> sandbox-guest (Wasmtime host)
```

`sandbox-guest` also contains the existing strategy interpreter. Pine-Lite
previews use the same Wasmtime host; on Linux, the additional worker process
isolates that host from the gateway process. Non-Linux development continues
to use an in-process WebAssembly boundary; see `docs/08-SANDBOX-WASM.md`.

`ai-agent` must never depend on `pine-lite` (its rule stands): the AI emits
*text*; the gateway calls `pine-lite` to vet it. `analytics-core` is the
canonical shared source of indicator math. Compatible `ta.*` adapters call
its implementations; formulas without a matching shared API remain in
`pine-lite/src/ta.rs` where Pine-Lite's input or output semantics require a
local adapter.

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

The scalar value kinds are `float` (the default numeric; `int` is a float
subtype as in Pine), `bool`, `string`, and `color` literals (`color.red`,
`#RRGGBB`/`#RRGGBBAA`). `plot` and `hline` require numeric series; other kinds
are not implicitly converted. Series-ness is implicit: a scalar literal used
in series context promotes to a constant series (Pine's "series-ness is
viral" rule, simplified: everything is a series, scalars are just constant
ones). Ternary branches must have compatible value types; `na` takes the
type of the other branch. A mismatched branch is a vet error, not an implicit
conversion. Explicit `float`/`int`, `bool`, `string`, `color`, and drawing
handle declarations check the initializer; `na` adopts the declared kind.
After a series has a concrete kind, reassignment to another kind is refused.
An initial untyped `na` does not constrain the later kind.

`if`/`while` conditions must be boolean and `for` bounds/steps numeric.
User-function parameters and returns support strings when the function body
uses the parameter in string concatenation, string ternaries/switches, or a
string-valued local assignment. These parameters accept strings and numeric
values (formatted as text at runtime); defaults are checked against the
inferred kind. Persistent (`var`/`varip`) string locals inside
functions are refused; other inferred function parameter kinds are checked at
the call site. UDT constructors and field assignments check declared
`string`, `bool`, and `color` fields distinctly; other implemented fields are
numeric/handles.
Input defaults are checked against their supported kind (`input.bool` retains
the legacy numeric default form). Rendering calls check the value kind their
series slots require. Builtin parameter type/name checks are guaranteed only
for catalog entries whose `metadata_complete` flag is set; other entries
still receive their registered arity checks, but are not claimed to have a
complete static argument contract.

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
output: `macd_line, signal_line, hist = ta.macd(close)` (defaults 12/26/9),
`basis, upper, lower = ta.bb(close)` (defaults 20/2),
`k, d = ta.stoch(close, 14)`.
A multi-output call read as a single value is a compile error naming the
fix; `ta.stoch` alone still reads as its %K. Other multi-output calls are
`ta.dmi(length=14)`, `ta.kc(length=20, mult=2)`, and
`ta.supertrend(mult=3, atr_length=10)`. Named and positional arguments use
the same catalog signatures. Rolling calculations return `na` until the
requested lookback is available; they do not shrink the window to fit the
loaded history.

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
  literal. The host supplies the raw, timestamped source candles, ordered by
  `open_time`; it does not expand them onto chart-bar slots. The VM evaluates
  the expression once per distinct source candle, then merges values onto the
  chart bars by timestamp. The full source history must be retained for
  rolling functions when the chart is zoomed. `request.security(..., time)`
  reports the source candle's open timestamp. Before the first source candle
  is available the result is `na`. The default `gaps_off`/`lookahead_off`
  carries the last revealed value during a bucket (including across missing
  source bars) and reveals a completed source value only when a chart bar
  closes that source candle; an incomplete final bucket is not revealed.
  `gaps_on` returns `na` except on reveal bars. `lookahead_on` exposes the
  current bucket value from its first chart bar and can therefore leak future
  values in historical analysis. Empty or out-of-order pools are errors.

  **Coverage:** runtime tests cover raw source cadence, missing source bars,
  and final-bucket reveal semantics; chart-engine coverage verifies full-source
  warm-up for a zoomed viewport. Broader chart-preview/renderer visual
  acceptance remains open.
  Time-anchored drawing objects require absolute timestamps; an HTF zone's
  edges are not chart bar indexes (`docs/28`).

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
  `na`. For the chart symbol only, `buy_volume`, `sell_volume`, and
  `volume_delta` use the aligned candle split, when the loaded window contains
  split-volume data. A feed the host did not supply raises a runtime
  missing-data error; it never silently becomes zero. Split quality is
  provider-specific: Binance klines expose a real taker-buy split, while
  Bybit's REST candle split is direction-attributed from candle direction and
  is not an aggressor-side measurement.
- `trade_volume`, `trade_buy_volume`, `trade_sell_volume`, `trade_delta`, and
  `trade_count` aggregate retained trade records into chart bars. Stored
  records are queried by bar bucket; the live tape overlays its covered bars.
  Bars without an observation remain `na`, not zero. History is limited to
  what collectors/backfills actually persisted and the database retention
  window; the current storage key is symbol-based, and the series is not yet a
  provider-neutral input contract for external backtests. No Deriv tick is
  reclassified as an execution.
- The chart symbol's retained order-book snapshots supply `book_bid_depth`,
  `book_ask_depth`, `book_imbalance`, `book_spread`, and `book_mid`. Depth
  sums the levels in the actual stored snapshot; the other fields use the best
  bid and ask. The newest snapshot at or before each chart bar's open is used
  and expires after 30 seconds; unavailable or stale bars are `na`. These are
  captured observations, not reconstructed order-book depth or footprint levels.

## Builtin namespaces

Call arity, static result kinds, multi-output shapes, parameter names/defaults,
host-data requirements, and runtime support are registered in
`pine-lite::builtins::CATALOG` (schema version 2) for the registered builtin
surface. `GET /scripts/capabilities` exposes this catalog without a second
hand-maintained copy; the generator prompt and repair cheatsheet render their
call reference from the same source. The type checker and interpreter use the
registered signatures. `metadata_complete` is the boundary of the stronger
parameter/host-data contract: when it is false, consumers may rely on the
registered call bounds and result shape, but must not infer omitted parameter
or host-support details. The market/provider capability report remains
`GET /capabilities`.

| Namespace | Contents (v1) |
| --- | --- |
| `ta.` | Averages/oscillators: sma, ema, rma, wma, hma, tema, swma, alma, vwma, rsi, stoch, macd, cci, mfi, willr, ao. Windows/statistics: highest, lowest, highestbars, lowestbars, stdev, variance, deviation, median, mode, percentrank, percentile_linear_interpolation, percentile_nearest_rank, range, correlation, covariance, linreg/lsma. OHLCV: atr, tr, vwap, cmf, obv, pvt, nvi, wad, ad. Events: change, mom, roc, cum, crossover/crossunder/cross, rising/falling (current strictly above/below all values in prior lookback), barssince, valuewhen, pivothigh/pivotlow. Multi-value: macd → (line, signal, hist), bb → (basis, upper, lower), stoch → (k, d), dmi → (+DI, -DI, ADX), kc → (basis, upper, lower), supertrend → (line, direction). |
| `math.` | abs, min, max, floor, ceil, round, pow, sqrt, log, exp, sign, avg, sum |
| `input.` | int, float, bool, string, color — with `defval`, `minval`, `maxval`, `title`; values are host-supplied per chart |
| `strategy.` | entry, exit, close, close_all, cancel — see Phase 7 |
| `color.` | 17 named colors + hex |
| free functions | `na`, `nz`, `fixnan`, `barstate.isconfirmed` (always true in this platform — the host only runs on closed bars), `bar_index`, `last_bar_index`, `time`, `time_close`, `open/high/low/close/volume`, `hl2/hlc3/ohlc4`, `syminfo.ticker` |

`ta.sma`, `ta.ema`, `ta.rsi`, `ta.atr`, `ta.cmf`, and `ta.ao` wrap
`analytics-core`; `ta.vwap` uses its per-bar UTC-session series API. `ta.ao()`
uses the standard 5/34 midpoint SMAs, and `ta.cmf(length)` requires OHLCV
volume. Other formulas are implemented in `pine-lite/src/ta.rs`. Phase 3
consolidated compatible implementations and added numeric fixtures; formulas
with different output warm-up or input shape remain explicit Pine-Lite
implementations. Same names alone do not guarantee parity. New shared formulas
should be added to `analytics-core` first, then exposed by Pine-Lite with the
language's explicit `na`/series mapping.

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
4. The vetted script runs in the Pine-Lite `sandbox-guest` over the preview
   window (7 days, the same `indicator_preview` path), producing bounded
   chart evidence. The gateway fetches data and enforces input, output, and
   concurrency limits; the guest has no network, filesystem, or trading
   capabilities. Linux previews additionally run in a restricted worker
   process; other development platforms use the in-process Wasmtime host.
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
  boolean, numeric, color, and string inputs. The VM supports string-valued
  assignments, concatenation, history reads, host-supplied `input.string`
  values, inferred string parameters/returns in user functions, and string UDT
  fields. Persistent string locals inside functions and complete string `na`
  propagation remain unsupported.

## Phased plan

| Phase | Delivers | Acceptance criterion | Status and source/test evidence |
| --- | --- | --- | --- |
| 1 | Crate skeleton, lexer, error list, no deps beyond analytics-core | Round-trip tests over fixtures | **done** — lexer/header tests in `crates/pine-lite/src/lex.rs`; leaf dependency is visible in `crates/pine-lite/Cargo.toml`. |
| 2 | Parser → AST with spans | Golden parse tests incl. error cases | **done** — parser and span/error cases in `crates/pine-lite/src/parse.rs`. |
| 3 | Type checker + casting rules | Refusal tests for every casting rule | **done** — checker refusal and arity tests in `crates/pine-lite/src/typecheck.rs`; public vet path in `src/lib.rs`. |
| 4 | Series interpreter: per-bar exec, history, na, var, inputs | Equivalence: hand-computed series fixtures | **done** for the implemented value model — runtime and fuel tests in `crates/pine-lite/src/interp.rs`, `tests/model_ergonomics.rs`, and `tests/strategy_execution.rs`. |
| 5 | Quantitative standard library: complete advertised `ta.*` surface | Numeric fixtures, complete catalog metadata, and AI-reference coverage | **done** for every advertised builtin — numeric tests in `crates/pine-lite/src/ta.rs` and `tests/ta_ohlcv_pack.rs`, completeness checks in `builtins.rs`, and prompt discovery coverage in `crates/api-gateway/src/pine_codegen.rs`. This is not the full TradingView Pine API. |
| 6 | Plot statements → `ScriptPane` output contract | Positioning tests in chart-engine | **done** for current plot/drawing types — projection tests in `frontend/chart-engine/src/script.rs` and `scene.rs`; frontend painter in `frontend/app/app.js`. |
| 7 | Strategy calls → backtester replay intents | Pine execution and `ScriptStrategy` replay agree on supported intents; runtime failures are observable | **done** for the adapter's supported intents — `crates/backtester/src/script_strategy.rs` and `crates/backtester/tests/script_parity.rs`; runtime failures now abort replay with diagnostics. |
| 8 | Static analyser + sandbox front-end + fuel budgets | Adversarial tests mirror docs/08's list | **implemented; Linux runtime verification pending** — the gateway runs Pine-Lite in `sandbox-guest` under Wasmtime fuel, epoch, and linear-memory limits, then executes it in recycled worker processes with Landlock, thread-synchronized seccomp, OS resource limits, bounded IPC, and fail-closed startup validation. Wasm parity/adversarial tests are in `crates/sandbox/tests/pine_preview.rs`; the Linux-only process-policy probe is in `crates/api-gateway/tests/pine_preview_worker.rs` and must pass on the deployment kernel. Non-Linux development uses the in-process backend. |
| 9 | Gateway: `/scripts/*` routes, workspace language switch, AI loop | `indicator_workspace_flow`-style gateway tests | **done for the indicator path; distinct strategy flow is intentional** — `/scripts/vet`, generation/edit/repair/preview, and revision storage live in `crates/api-gateway/src/script_routes.rs` and `indicator_workspace_routes.rs`; generated previews use the isolated guest. Indicator workspaces use Pine-Lite, while strategy-document workspaces use the Strategy DSL; there is no implicit in-place conversion between these different contracts. |
| 10 | Chart engine + shell: rendering, inputs UI, pane management | Shell check script + visual parity vs RSI sub-pane | **partial** — render/projection and layer controls are implemented; boolean/numeric/color/string settings are editable in the shell. The VM supports string assignment, concatenation, history, `input.string`, inferred string parameters/returns in user functions, and string UDT fields. Persistent string locals inside functions and complete string-`na` semantics remain open. Full visual acceptance remains open. |

Phases 1–5 are the language core and land as one reviewable unit; 6–8 make it
visible and safe; 9–10 wire it to users.

### Phases 11–17: implementation status (reviewed 2026-11)

These are scoped Light Pine capabilities, not a claim of full TradingView Pine
parity. Status is based on the implementation and named tests; known host-data
limitations remain explicit even when the language construct itself is tested.

| Phase | Delivered capability | Status, evidence, and remaining limitation |
| --- | --- | --- |
| 11 | **Multi-pair + per-call timeframe**: `request.security("ETHUSDT", "15m", expression)`, keyed host pools, eight-key cap, and `sec=` shorthand. | **partial** — literal collection, cap, raw source fetching, timestamp merge, and chart-engine paths exist in `crates/pine-lite/src/typecheck.rs`, `interp.rs`, `crates/api-gateway/src/indicator_preview.rs`, and `frontend/chart-engine/src/script.rs`. Sub-VM history/rolling evaluation advances once per source candle, missing source bars carry only the last revealed value, and an unfinished final bucket is not revealed. Runtime tests cover cadence and sparse source bars; the chart engine tests raw-history warm-up for a zoomed window. Broader chart-preview/renderer visual acceptance remains open. |
| 12 | **User-defined functions** with typed call sites, series-passing semantics, and recursion limits. | **done** — parser/checker/runtime in `parse.rs`, `typecheck.rs`, and `interp.rs`; `tests/user_functions.rs` covers execution, TA composition, arity, and recursion refusal. |
| 13 | **Drawing objects** with bounded VM heap and chart-coordinate projection. | **done** for implemented line/label/box/Fibonacci objects — `tests/drawing_objects.rs`, `frontend/chart-engine/src/scene.rs` projection tests, and `frontend/app/app.js` painter. This does not imply every requested visualization type exists. |
| 14 | **Provider-backed `request.data`** for ticker snapshots, timestamped futures series, and captured flow/depth. Generic HTTP is explicitly not part of this phase. | **implemented for the defined feeds** — ticker/funding/OI and trade/book series are implemented in `indicator_preview.rs`; flow aggregates retained records and overlays live ticks only when their source matches, while depth reads source-specific snapshots as-of each bar. `db::MarketDataSource` is the provider-neutral persistence contract, with source-aware pumps, writes, and history queries for Binance, Bybit, legacy rows, and external dataset adapters. `MARKET_HISTORY_SOURCE` selects a non-default retained dataset. Availability still reflects actual capture/backfill coverage and retention; missing data remains `na`. Generic HTTP is permission-gated future work. |
| 15 | **v1.1 expressiveness**: `while`, user-function parameter defaults, and 256-object cap. | **done** — implementation in `interp.rs`, `limits.rs`, and `typecheck.rs`; behavior and runaway-loop coverage in `tests/model_ergonomics.rs`, with the heap cap in `tests/drawing_objects.rs`. |
| 16 | **Time-anchored drawings** and aligned-secondary `request.time()`. | **partial** — VM and chart time-coordinate mapping are tested in `tests/time_anchored_objects.rs` and `frontend/chart-engine/src/scene.rs`; aligned script series now retain source timestamps, including MTF pool bars. Broader chart-preview/renderer visual acceptance remains open. |
| 17 | **Optional box borders** with backward-compatible serialization. | **done** — constructor/runtime and wire behavior are covered by `tests/drawing_objects.rs` and `frontend/chart-engine/src/scene.rs`; unset borders preserve the fill-only rendering. |

Phase 2's implemented MTF and value contracts have runtime and chart-engine
conformance coverage; broader chart-preview/renderer visual acceptance remains
open and is not claimed as full TradingView parity. The next roadmap phase is
the quantitative standard library. Phase 14 is provider-backed data only;
generic HTTP remains out of scope until permissions, response-size caps, and
caching are designed and tested.

Flow-history source IDs are stable and provider-neutral: built-in collectors
use `binance_spot` and `bybit_spot`, while rows created before migration 0021
remain under `legacy`. `MARKET_HISTORY_SOURCE` can select an alternate retained
dataset, using lowercase ASCII IDs of at most 96 characters. An external
backtest adapter should stamp its own ID with the source-aware database pumps
or repository writers and read with the matching `*_from_source` functions.
The API gateway deliberately keeps live ticks in its bounded in-memory tape;
it does not persist every live trade, which would violate the documented
database budget. Therefore source-specific long-window flow is available only
when a collector or adapter has explicitly retained that source's records.

## Implementation notes (2026-11)

Decisions the code made that the spec above only implied:

- **Linux server previews use OS-process isolation.** The interpreter meters
  itself, and the Wasmtime guest adds fuel, epoch, and linear-memory limits
  plus an import allowlist. Linux production uses a separate restricted worker;
  startup fails if the worker or kernel policies are unavailable. Non-Linux
  development remains in-process and does not provide that OS boundary.
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
