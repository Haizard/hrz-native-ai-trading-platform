# 24 — Strategy execution: from signal-only stubs to simulated orders on the chart

Status: **S1 + S2 implemented (VM sim, header knobs, preview stats, chart trades + equity panes); S3 + S4 pending** · Supersedes: none · Extends: docs/23 Phase 7
Authored 2026-09-30 after the v1.1 language phase (while loops, parameter
defaults, 256-object heap) landed.

## 1. The problem

The studio generates strategy scripts today, but the chart cannot show what
they *do*. `strategy.entry` / `strategy.exit` / `strategy.close` are accepted
and vetted, yet the interpreter records them as inert intents and stubs every
strategy builtin at `0.0`, and the preview pipeline drops those intents on the
floor. A user who asks for "an ATR trailing-stop strategy with session
filtering" gets lines and markers, never trades, positions or equity.

The docs/23 Phase 7 spec already called the target: intents feed the existing
deterministic replay, and "a strategy script renders exactly like an indicator
script plus trade markers." That rendering never happened, and the strategy
builtins (`strategy.position_size`, `strategy.equity`, …) were stubbed to make
generated scripts *run*, not to make them *work*.

## 2. Where the system actually stands (audited 2026-09-30)

The build is further along than "stub" suggests. Three facts shape this
design:

- **The VM already records intents.** `pine_lite::interp::Intent` is a real
  enum — `Entry { id, long }`, `Exit { id, stop, limit }`,
  `Close { id: Option<String> }` — stamped with the firing bar in
  `Output.intents`, and `Output.strategy_used` flips true. What is missing is
  qty/qty_type on the intent (see §4.1).
- **The replay adapter exists and is tested but wired to nothing.**
  `backtester::script_strategy::ScriptStrategy` wraps a vetted script as a
  `strategy_runtime::Strategy`: per-candle windowed execution, intent→signal
  lowering with Pine's fill-on-next-open rule, a stop carried from
  `strategy.exit(stop=)`, an ATR(14)×2 default stop when the script declares
  none, and entry-while-in-position refused (pyramiding is out of scope, as
  docs/23 says). Its tests pass in the backtester suite. **No production code
  path constructs it** — grep finds only the module's own tests.
- **The browser chart can already paint positions.** The shell has a
  position-box layer with `position_long: "#089981"` / `position_short:
  "#f23645"` vocabulary (frontend/app/app.js) fed by
  engine-positioned rectangles; the wasm engine positions script objects
  (lines/labels/boxes) through the frame today.

Meanwhile the honest gaps: the interpreter's `strategy.*` builtins are literal
`0.0` stubs (`strategy.position_size`, `strategy.position_avg_price`,
`strategy.equity`, `strategy.openprofit`, `strategy.closedtrades`,
`strategy.wintrades`), the header parser reads only `version / overlay / title
/ sec / max_bars_back` — a `strategy(initial_capital=…)` header is vetted as
prose today — and the preview JSON has no strategy shape at all
(`strategy_used` is set by the VM but never surfaced).

## 3. Design principles (inherited, not invented)

1. **One simulator.** Pine's fill rule is already the simulator's rule
   (decide on closed bar, fill next open, slippage on top). We do not add a
   second fill model in the VM or the gateway; the chart's "simulation" is
   the same `backtester::replay` the DSL strategies use.
2. **Same-shape runs.** A strategy script runs in preview exactly like an
   indicator (fuel budget, 2016-bar window, same vet layers) plus a
   deterministic simulation pass. No user-visible mode switch.
3. **Stubs become real only through the VM's own state.** The strategy
   builtins read the VM's simulated account state, not host data — so the
   same script vets and runs identically in gateway preview and browser.
4. **Refuse the unknowable.** Recursion is refused with a message that names
   recursion; a strategy builtin that cannot be honored (e.g. reading
   `strategy.equity` in a script whose header is not a strategy header) is a
   vet-time refusal, not a silent zero.

## 4. Architecture

```
pine-lite VM (per bar)                    backtester (existing)
┌──────────────────────────────┐          ┌───────────────────────────┐
│ strategy.* calls → Intent[]  │  intents │ ScriptStrategy adapter    │
│ strategy builtins → account  │ ───────► │ (exists; wire in S1)      │
│   state read from sim so far │          │ → Signal lowering         │
│ header: strategy(...) knobs  │          │ → backtester::replay      │
└──────────────────────────────┘          │ → BacktestReport          │
                                          └────────────┬──────────────┘
        ┌──────────────────────────────────────────────┘
        ▼
gateway preview JSON (S2): { plots, hlines, objects, orders[], equity[], report }
        ▼
wasm engine positions orders/equity through the frame (S3)
        ▼
shell paints trade markers + equity pane + report card (S4)
```

### 4.1 The output contract (new, versioned)

`Output` gains one field; the preview JSON gains one object. Nothing existing
is reshaped.

```rust
// pine-lite
pub struct Output {
    // ... plots, hlines, shapes, objects, intents (unchanged) ...
    pub simulation: Option<Simulation>,   // None for indicator scripts
}

pub struct Simulation {
    pub orders: Vec<SimOrder>,   // chronological; every order the sim saw
    pub equity: Vec<f64>,        // per-bar equity, len == window
    pub report: SimStats,
}

pub struct SimOrder {
    pub bar: usize,              // decision bar; fill = next open
    pub kind: OrderKind,         // Entry | Exit | Close (mirror of Intent)
    pub long: bool,
    pub qty: f64,
    pub price: f64,              // fill price (next open + slippage)
    pub stop: Option<f64>,       // carried from strategy.exit(stop=)
    pub pnl: Option<f64>,        // realized pnl on the closing fill
}

pub struct SimStats {            // same numbers as the DSL report's headline
    pub net_profit: f64,
    pub total_trades: usize,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub max_drawdown: f64,
}
```

The gateway's preview JSON carries it verbatim (serde already available);
`strategy_used && simulation.is_none()` is a bug, not a fallback path — the
S1 tests pin the pairing.

### 4.2 Where the simulation lives: inside the VM run, after the bar loop

The interpreter already knows every intent and the candles; running the
simulation inside `pine_lite::run` (after the per-bar loop, against the
recorded intents) buys three things at once:

- the strategy builtins need a **two-pass shape**: pass 1 records intents
  with builtins stubbed (exactly today's behavior); pass 2 runs only when the
  script *reads* strategy state builtins — it simulates pass 1's intents,
  then re-runs the bar loop with the builtins reading the simulated account.
  Two passes maximum, fuel-metered separately; scripts that never read
  strategy state pay nothing.
- the browser engine gets the identical `Simulation` for free (same crate,
  compiled to wasm) — no gateway round trip for chart rendering, and the
  "re-runs on every candle" live path stays true.
- `ScriptStrategy` (the replay adapter) stays the *backtest* path and its
  intent-lowering is refactored in S1 to share one `simulate()` function
  with the VM so the fill rules cannot drift apart.

The alternative — gateway-side simulation only — was rejected: the browser
could not render trades without duplicating the simulator in JS, and the live
re-run path would lose position awareness.

### 4.3 The header: `strategy(...)` becomes real

The lexer's header parser gains the knobs docs/23 already promised:

```
//@pine_lite version=1 overlay=true title="T" strategy(initial_capital=10000, default_qty_type="percent_of_equity", default_qty_value=10, commission_type="percent", commission_value=0.04)
```

- `initial_capital` (default 10 000), `default_qty_type` = `"fixed" |
  "percent_of_equity"` (default `"percent_of_equity"`), `default_qty_value`
  (default 10 = 10%), `commission_type` = `"percent" | "absolute"`,
  `commission_value` (default 0.04% like the backtester's fee model),
  `slippage` in percent (default 0). `pyramiding` is parsed and refused >0
  with a message naming v2.
- The header parser is `lex.rs`'s `key=value` scanner — extending it is a
  closed, testable change (quoted-value splitting already exists for
  `title`/`sec`).
- Scripts that call `strategy.*` without the header knob are not refused
  (generation keeps working; defaults apply), but the preview JSON reports
  `strategy_header: false` so the UI can hint. Reading a strategy-state
  builtin (`strategy.equity` etc.) without the header IS a vet error —
  see the refuse-the-unknowable principle.

### 4.4 What the strategy builtins read

Pass 2 maintains the account the simulator produces (all Pine-standard):

| Builtin | Reads |
|---|---|
| `strategy.position_size` | signed open qty, 0 when flat |
| `strategy.position_avg_price` | avg entry price, na when flat |
| `strategy.equity` | initial_capital + realized + open PnL |
| `strategy.openprofit` | open PnL at the bar's close |
| `strategy.closedtrades` | count of closed trades so far |
| `strategy.wintrades` | closed trades with pnl > 0 |

These are series-context reads; history on them follows the same
assign-then-offset rule as everything else (`e = strategy.equity` then
`e[1]`). Pine's own equity semantics (mark-to-market per bar) are honored by
the simulator's per-bar valuation pass, which the backtester already does for
its reports.

## 5. Phased plan

### S1 — The simulator in the VM + one simulate() for both paths ✅ done
*Deliverable: a strategy script runs and reports; `ScriptStrategy` shares the
same core; gateway preview surfaces orders/equity/report.*

- `pine-lite/src/sim.rs`: `simulate(intents, candles, &StrategyKnobs) ->
  Simulation` — port of `ScriptStrategy`'s lowering rules (next-open fills,
  carried stop, ATR×2 default stop, one position, close-cancels-same-bar-
  entry), so VM and replay path share fill semantics verbatim.
- Two-pass interpreter as in §4.2; `Output.simulation` filled;
  `strategy_used` gating preserved.
- Header knobs (§4.3) in the lexer with the closed-scanner tests.
- `ScriptStrategy` refactored to call `sim::simulate`-equivalent lowering;
  its existing tests must pass unchanged (they pin fill rules).
- Gateway: `indicator_preview` includes `simulation` in the preview JSON;
  the chat row adds "N trades, X% net, max DD Y%" after the plots summary.
- **Tests**: sim unit tests (entry/exit/stop/close, next-open fill, default
  ATR stop, qty from knobs, commission+slippage math); two-pass tests (a
  script reading `strategy.equity` matches a hand-computed equity curve);
  header knob tests; preview JSON shape test in `indicator_preview.rs`;
  ScriptStrategy tests unchanged-green.

### S2 — Orders and equity on the chart (wasm + shell) ✅ done
*Deliverable: the chart shows every trade and the equity curve.*

- Engine: position `SimOrder` fills as trade markers (triangle-up/down at
  fill price, reuse the shape vocabulary) and the equity series as a
  positioned line, through the same frame transform objects already use.
- Shell: paint markers over the price pane; equity renders as its own
  sub-pane when the script is `overlay=false`, or as a thin line in the
  price pane's footer strip when overlay. Report card (net/win-rate/PF/DD)
  in the script chip's popover, next to the existing plots summary.
- The live path: on each new candle the engine re-runs and the simulation
  updates; open position renders as a dashed box from entry to the live bar
  (the shell's `position_long/short` box vocabulary).
- **Tests**: engine positioning tests mirror
  `drawing_objects_position_through_the_frame`; shell has no unit harness —
  verified by the visual-check script docs/23 Phase 10 already prescribes.

### S3 — The studio loop: generation knows strategies
*Deliverable: "make me a strategy" produces a working backtest in one chat.*

- System prompt gains a Strategy section: header template, the builtin table
  (§4.4), the one-position rule, "exit before re-entry," qty via
  `default_qty_*`, and a reference strategy script that must vet AND
  simulate (the reference-script test extends to it).
- Cheat sheet gains the strategy block (header knobs, builtin table,
  fill-on-next-open, one position, no pyramiding).
- Repair loop unchanged — vet refusals already carry line/col; sim failures
  (e.g. zero fills because the entry condition never fires) surface as a
  non-fatal `simulation_note` in the preview JSON, and as a warning in the
  chat row, so the model's *next* repair round sees it via the existing
  repaired-errors channel.
- **Tests**: `the_taught_strategy_idiom_vets_and_runs` in `pine_codegen.rs`
  mirroring the SMT idiom test; preview note test.

### S4 — Real strategy parity (stretch, gated)
*Deliverable: generated strategies replay identically through
`ScriptStrategy` and the VM sim, and strategy-cli can backtest a workspace
script.*

- CLI/integration: `POST /strategy/backtest` accepts `{"representation":
  "pine-lite", "source": ...}` alongside DSL documents (the route exists;
  the payload branch is new). Parity test: a fixed script's VM simulation
  vs `ScriptStrategy` replay produce identical trade lists.
- Explicit non-goals stay refused with messages: pyramiding,
  `strategy.risk.*`, OCA groups, bracket id-theft (`strategy.exit(from_entry=)`
  beyond the carried stop), broker handoff.

## 6. Why not the alternatives

- **Simulate in the gateway only** (reject): the browser would need a JS
  re-implementation to draw trades on live candles — the exact drift the
  one-simulator principle forbids; and the generated script's builtins would
  disagree between vet-time run and chart run.
- **Simulate inside the bar loop** (reject): `strategy.equity` on bar 100
  depends on fills from intents on bars 0–99 — a causality loop. The
  two-pass shape is Pine's own resolution (its broker emulator also runs
  after the script calculates; the documented divergence window is one bar,
  same as ours).
- **Extend `Intent` with qty and let the host sim** (partially reject): the
  *shared* simulator (§4.2) still lives in pine-lite so both wasm and
  gateway use it; `Intent` gains qty in S1 only because the simulator and
  the lowering both need it — it stays an internal type.

## 7. Test matrix (the whole feature)

| Layer | Test |
|---|---|
| Header | knob parse tests in `lex.rs`; `pyramiding>0` refusal; unknown knob refused with the known list |
| Sim core | entry/exit/close/stop; fill = next open ± slippage; ATR×2 default stop; percent-of-equity sizing; commission math; zero-fill note |
| Two-pass | `strategy.equity` script vs hand-computed curve; no-strategy-read scripts stay single-pass (fuel unchanged); `simulation` None for indicators |
| Parity | `ScriptStrategy` replay == VM sim trade list on 5 fixed scripts |
| Preview | JSON shape incl. `simulation`, `strategy_header`, `simulation_note` |
| Codegen | reference strategy script vets+simulates; taught idiom test |
| Engine (wasm) | order/equity positioning tests |
| Shell | visual-check additions: markers, equity pane, report card, live re-run |
| Refusals | strategy-state builtin without header; pyramiding; `strategy.risk.*` unknown-namespace message |

## 8. Open decisions for the owner

1. **Equity pane placement** (S2): dedicated sub-pane for non-overlay
   strategies — my recommendation — vs always-footer-strip. Affects pane
   management (docs/23 Phase 10's pending inputs-UI work touches the same
   code).
2. **Slippage default** (S1): 0% (TradingView-like, optimistic) vs the
   backtester's current default. Recommend 0 default, knob in the header,
   consistent with commission being a knob.
3. **Report vocabulary** (S2): reuse the DSL strategy report's exact terms
   (recommended — one vocabulary platform-wide) vs TradingView's Strategy
   Tester labels.
4. **Scope of S4**: parity + CLI route are cheap; broker handoff is a
   different compliance posture and stays out of this doc.
