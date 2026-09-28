# 23 — Pine-lite: an imperative scripting language for indicators and strategies

## Purpose

Amend the one-representation principle (`docs/06`) for the indicator layer. The
platform gains a **second** representation, alongside the Strategy DSL, that
follows TradingView's Pine Script design instead of the declarative `concepts`
vocabulary:

- **Code, not data.** An indicator is an imperative script — math, loops,
  functions, series — written by the AI studio or a developer, exactly the way
  Pine Script is written.
- **Compiled & executed per bar.** The script runs once per closed candle with
  series history indexing (`close[1]`), Pine-like `na` semantics, and state that
  accumulates across bars (`var`).
- **The platform vets scripts.** Static validation (parse, type check, limits)
  plus dynamic isolation (the existing WASM sandbox with fuel and memory caps)
  refuse unsafe or resource-heavy code before it ever draws.
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

```
pine-lite -> analytics-core (and nothing else internal; must build for wasm32)
sandbox-guest -> pine-lite, strategy-runtime
```

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
//@pine_lite version=1 overlay=true title="My RSI" max_bars_back=300
```

Unknown annotations are a validation error (the AI must not be able to invent
host knobs). `overlay=true` sends plots to the price pane; `overlay=false`
(the default) gives the script its own sub-pane.

### Operators, precedence, history

Standard arithmetic, comparison, boolean short-circuit, `?:`. History indexing
`expr[n]` is allowed on any series expression and on builtin-call results
(`ta.rsi(close, 14)[1]`). The maximum history depth is `max_bars_back`,
default 300, hard cap 5000. Requesting deeper history is a **compile error**,
not a runtime `na`.

## Builtin namespaces

| Namespace | Contents (v1) |
| --- | --- |
| `ta.` | sma, ema, rma (Wilder), wma, rsi, macd, stoch, atr, tr, bb, crossover/crossunder/cross, change, mom, roc, highest, lowest, vwap, sar |
| `math.` | abs, min, max, floor, ceil, round, pow, sqrt, log, exp, sign, avg, sum |
| `input.` | int, float, bool, string, color — with `defval`, `minval`, `maxval`, `title`; values are host-supplied per chart |
| `strategy.` | entry, exit, close, close_all, cancel — see Phase 7 |
| `color.` | 17 named colors + hex |
| free functions | `na`, `nz`, `fixnan`, `barstate.isconfirmed` (always true in this platform — the host only runs on closed bars), `bar_index`, `last_bar_index`, `time`, `time_close`, `open/high/low/close/volume`, `hl2/hlc3/ohlc4`, `syminfo.ticker` |

`ta.*` is implemented over `analytics-core`'s existing functions — `ta.rsi` is
`indicators::rsi` (Wilder smoothing), `ta.ema` is `indicators::ema` (SMA-seeded)
— so a script's numbers are the same numbers the chart, scanner and validator
already produce. New ta functions that do not exist in `analytics-core` (macd,
stoch, bb…) are added **there first**, then exposed; `pine-lite` never owns
indicator math.

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

## Vetting: "the platform vets scripts" — concretely

Layered, cheapest first, all of it deterministic:

1. **Parse** — grammar refusal with line/col, error list not first-error (the
   AI repair loop needs the full list).
2. **Type check** — unknown identifiers/functions, arity, Pine casting rules
   (int→float ok, float→int refused, na in float context ok, bool in numeric
   context refused).
3. **Static analyser** — the budget caps above; recursion refused; `for`/
   `while` bodies with unbounded step expressions refused (`for i = 0 to
   bar_index` is fine; `while true` is refused unless the body provably
   mutates the condition — in practice v1 refuses `while` entirely and offers
   `for`); assignments to builtins refused; `varip` allowed but excluded from
   backtest replay equivalence (documented divergence, matching Pine).
4. **Sandbox** — the script runs inside the existing WASM guest with the same
   fuel metering and memory ceilings `docs/08` already enforces; the guest's
   `interpret` gains a Pine-lite front-end. A script that outlives its fuel is
   killed and reported as a validation failure, never drawn half-way.

Dynamic budgets scale with window: fuel = 200k steps per 1000 bars, so a
300-bar chart costs ~60k steps. A script that cannot finish cannot ship.

## The AI generation loop (studio integration)

The workspace chat's prompt changes from "emit YAML" to "emit a Pine-lite
script" for code-first workspaces:

1. Model emits script text (one file, one revision).
2. Gateway vets it through layers 1–3 and returns the **full error list**.
3. The agent re-prompts with the script + errors, up to 5 attempts — the
   existing `max_attempts`/`repaired_errors` loop, unchanged in shape.
4. The vetted script runs sandboxed over the preview window (7 days, the same
   `indicator_preview` path), producing plot series and the evidence preview.
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
  compiles once per source change (cache by hash) and re-runs per frame
  exactly like `live_indicator` concepts today.

## Phased plan

| Phase | Delivers | Done when | Status |
| --- | --- | --- | --- |
| 1 | Crate skeleton, lexer, error list, no deps beyond analytics-core | Round-trip tests over fixtures | **done** |
| 2 | Parser → AST with spans | Golden parse tests incl. error cases | **done** |
| 3 | Type checker + casting rules | Refusal tests for every casting rule | **done** |
| 4 | Series interpreter: per-bar exec, history, na, var, inputs | Equivalence: hand-computed series fixtures | **done** |
| 5 | `ta.*` over analytics-core (incl. new macd/stoch/bb there) | Parity tests vs analytics-core units | **done** (`ta.rsi`/`ta.atr` parity-pinned) |
| 6 | Plot statements → `ScriptPane` output contract | Positioning tests in chart-engine | **done** (`chart_engine::script`, shell `drawScriptPanes`) |
| 7 | Strategy calls → backtester replay intents | Golden replay vs a YAML equivalent | **done** (`backtester::script_strategy`) |
| 8 | Static analyser + sandbox front-end + fuel budgets | Adversarial tests mirror docs/08's list | **partial** (static limits, VM fuel + recursion cap in; WASM guest front-end pending) |
| 9 | Gateway: `/scripts/*` routes, workspace language switch, AI loop | `indicator_workspace_flow`-style gateway tests | **partial** (`POST /scripts/vet` done; workspace switch + AI loop pending) |
| 10 | Chart engine + shell: rendering, inputs UI, pane management | Shell check script + visual parity vs RSI sub-pane | **partial** (rendering + `attachScript` done; inputs UI pending) |

Phases 1–5 are the language core and land as one reviewable unit; 6–8 make it
visible and safe; 9–10 wire it to users.

## Implementation notes (2026-09)

Decisions the code made that the spec above only implied:

- **The VM is the isolation boundary until the guest lands.** Phase 8's
  sandbox front-end is still pending, but the interpreter already meters
  itself: `FUEL_PER_1000_BARS` (200k steps per 1000 bars) bounds total work,
  `MAX_CALL_DEPTH` (16) refuses recursion, zero-step `for` loops do not run,
  and a script that exceeds fuel is an error, never a half-drawn pane. The
  remaining guest work moves the *same* interpreter into WASM so untrusted
  execution cannot touch host memory at all; the budgets transfer as-is.
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
