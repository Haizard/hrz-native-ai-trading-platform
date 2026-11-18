# 60 — The strategy/bot boundary

- **Status:** Phase 5 boundary landed; golden strategy/live parity remains open
  (see §8). Companion to [`docs/24`](./24-STRATEGY-EXECUTION.md) and
  [`docs/55`](./55-LIGHT-PINE-EVOLUTION-ROADMAP.md) Phase 5.
- **Date:** 18 November 2026
- **Scope:** what a Pine-lite script can reach, what it cannot, and which
  component owns each step between a source file and a live order.

## 1. The invariant

Four components, four jobs, and no component does another's:

| Component | Job | May talk to a venue? |
|---|---|---|
| `pine-lite` | measure price, record intents | no |
| `backtester` | turn intents into simulated fills | no |
| `strategy-runtime` | run a validated strategy document | no |
| `trading-engine` | risk, orders, credentials, the gate | yes |

The separation is structural, not conventional. `crates/trading-engine/Cargo.toml`
does not depend on `pine-lite`, `backtester`, or `frontend/chart-engine`, and
`crates/pine-lite/Cargo.toml` depends only on `analytics-core`. A script cannot
name a broker because no broker symbol is in its dependency graph: even a
script that calls `broker.order(...)` is refused at vetting, before execution.

That is the first half of "a script cannot bypass risk or execution
authorization". The second half is that the live path itself refuses to start
without a gate-minted token (§7).

## 2. The three paths a source can take

| Path | Entry point | Decides with | Reaches money? |
|---|---|---|---|
| Preview / chart | `POST /indicator-workspaces/{id}/preview`, chart-engine WASM | `pine_lite::run` | no |
| Backtest | `POST /strategies/{id}/backtest` (script branch) | `ScriptStrategy` + deterministic replay | no |
| Paper bot | `POST /bots` with `mode=paper` | `PaperBot` on a validated document | no |
| Live bot | `POST /bots` with `mode=live` | `LiveBot` on a validated document, after `LiveGate` | **yes** |

The script paths and the bot paths do not meet. A Pine source never becomes a
`LiveBot` strategy directly; it must first be promoted into a strategy document
(§3), and that document is what a bot runs.

## 3. Source to execution, step by step

### 3.1 Preview

1. The source arrives at the workspace preview route or the chart engine.
2. `pine_lite::vet` parses and type-checks it. Capability, allocation and
   drawing limits are enforced here, not at draw time.
3. `pine_lite::run` produces `Output`: plots, drawings, labels, and (for a
   strategy script) recorded intents plus the language's own simulation.
4. The output is serialized for the chart. No order type is involved, and the
   preview process has no venue client to call.

### 3.2 Backtest

1. The same `vet`, then `backtester::script_strategy::ScriptStrategy` wraps the
   script as a `strategy_runtime::Strategy`.
2. On each decision candle the adapter runs the script over a rolling window
   and lowers the intents that fired on that bar into `Signal::Enter` or
   `Signal::Exit` (`intents_to_signal`).
3. The replay simulator owns fills (decide on a closed bar, fill at the next
   open, slippage on top), stops, position sizing and the report. `pine-lite`'s
   fill rule and the simulator's rule are the same rule, which is why the two
   produce identical trade lists (`crates/backtester/tests/script_parity.rs`).

### 3.3 Promotion (script becomes a strategy document)

`POST /indicator-workspaces/{id}/promote` (`indicator_workspace_routes.rs`)
turns a generated **indicator** revision into a tradeable **strategy**:

1. The revision's concepts and registry declarations are carried over
   verbatim; the route rejects a generated document that changed their ids,
   versions, aliases or parameters.
2. The agent writes fresh entry, risk and invalidation rules around those
   measurements. The trading logic is a new DSL document — it is not the
   indicator's source, and the indicator's own plots are not part of it.
3. The document is validated, then `Decisions::sandboxed` must accept it
   before it is stored. Promotion is the one place an indicator becomes
   tradeable, so the sandbox check is mandatory here rather than skipped for
   indicator kinds.
4. The stored row is an ordinary strategy. Drafts, pinning and the bot routes
   see nothing special about it.

### 3.4 Paper bot

`PaperBot::with_strategy` constructs the same `Decisions` the backtester uses
and runs it in `sandbox` when the document is agent-authored. Paper has no
venue adapter, so no credential and no order leaves the process. It exercises
the same `RiskEngine` a live bot does, which is what makes a paper record
meaningful to the gate.

### 3.5 Live bot

1. The gateway loads the stored document, validates it, and sandbox-checks it
   as `Decisions::sandboxed` — a pasted document is not trusted just because a
   row exists.
2. `LiveGate::authorize(venue, record, limits)` checks all three
   `docs/15` conditions together (paper record, per-venue opt-in, configured
   limits). On success it mints a `LiveAuthorization`; on failure it returns
   every reason at once.
3. The adapter is built for the resolved broker account.
4. `LiveBot::with_strategy` takes the authorization **and** the adapter. It
   refuses construction when the token's venue differs from the adapter's
   (`ExecutionError::AuthorizationVenueMismatch`).
5. `RiskEngine::check_entry` runs before every entry, and the order gateway is
   the only code that signs and sends an order. The kill-switch is checked in
   the same pass.

## 4. Lifecycle and order/position semantics

- **One decision per bar.** The replay and both bots decide once on a closed
  candle, so an entry and an exit cannot both fill on the bar that decided
  them.
- **Fill.** Next bar's open plus slippage. A stop that is touched fills
  intrabar; a target likewise. A condition exit is a market order on the next
  open.
- **Position.** v1 is single-position. An entry while a position is open is
  refused, not pyramided (`pyramiding=0`; a header asking for more is refused
  at vetting). A `strategy.exit` carrying a stop arms a standing stop; a naked
  `strategy.exit` or `strategy.close` exits at market.
- **Brackets.** The stop declared at entry is carried with the position and
  is the level both the simulator and the live bot enforce. Re-arming it while
  open (trailing) is a deliberate v2 axis and is refused rather than
  half-implemented.
- **Reconciliation.** The live bot reads venue order reports on every decision
  and treats a filled stop as the close, then cancels the orphaned target so
  it cannot trade against the account. A mismatch is surfaced, not papered
  over.

## 5. Sizing, stops, targets

- **Sizing is the platform's.** The script's `strategy.entry(qty=...)` is
  recorded but **not carried** into the signal. `intents_to_signal` emits
  `max_risk_pct: 1.0`, and the replay/live risk engine sizes from equity and
  the stop distance. A script cannot buy a size; it can only declare a
  direction.
- **Stops are price levels, not percentages.** The script's declared stop is
  used; without one, the shared `pine_lite::sim::default_stop` rule (2 ×
  ATR(14) from the decision close) applies. The same function backs the VM
  simulation and the replay lowering, so the default cannot drift between
  them.
- **Risk figures are checked, not trusted.** `strategy.risk.*` is recorded as
  an intent and ignored by the adapter. The request that reaches
  `RiskEngine::check_entry` is the platform's own figure, and a document
  asking for more than `PLATFORM_MAX_RISK_PCT` (5%) is denied rather than
  executed. `RiskLimits::default()` is 1% per trade, 3R daily, 8R weekly, one
  concurrent position.
- **Targets.** A script's `strategy.exit(limit=)` is recorded; the current
  lowering does not carry it into `EnterSignal::take_profit_price`. The replay
  therefore exits on the stop or a condition, and a script-declared target is
  an open parity item (§8), not a silently different behaviour.

## 6. Historical/live parity

The intent is one simulator and one rule set: the VM simulation, the replay,
and the live bot must agree on what a signal means.

What is proven today:

- `script_parity.rs` runs the same script through the VM's simulator and
  through `ScriptStrategy` replay and compares the trade lists exactly (long,
  short, close-on-signal, re-entry, never-trades).
- `replay_golden.rs` pins the DSL strategy's trade list against a fixed
  history.
- The live bot uses the same `Decisions`, `Shape` and `RiskEngine` as paper;
  only the adapter differs.

What is not proven yet: a script-backed strategy's **live** trade sequence
against its backtest on the same candles. That needs the provider/venue
harness described in §8 and is not claimed here.

## 7. The authorization model

`trading-engine` used to document "the caller must have passed the gate" above
`LiveBot::new`. A comment is not a boundary: any new caller could skip it.

It is now a type:

```rust
pub struct LiveAuthorization { venue: String }
```

- The field is private and `LiveGate::authorize` is the only constructor. It
  runs the same `check` the refusal path uses and mints a token only when
  every condition holds.
- `LiveBot::new`, `with_strategy` and `with_composition` take the token and
  return `Result<Self, ExecutionError>`. A caller that has not run the gate
  does not compile.
- `from_shape` compares the token's venue with `adapter.venue()` and refuses a
  mismatch, so an approval for one exchange cannot start a bot on another.
- The token names exactly one venue, is not serializable, and carries no
  credentials — it is proof of a check, not a capability to trade by itself.

The gateway starts live bots only through `LiveGate::authorize`; tests mint
tokens the same way, so the test path exercises the production constructor
rather than a bypass.

## 8. Proven and open

Proven by tests:

| Property | Test |
|---|---|
| no broker/account vocabulary is callable | `script_boundary.rs::a_script_has_no_broker_or_account_vocabulary` |
| script `qty` is not carried to the signal | `script_boundary.rs::a_script_declared_quantity_is_not_carried_to_the_signal` |
| `strategy.risk.*` cannot change the risk figure | `script_boundary.rs::a_script_risk_override_cannot_change_the_risk_figure` |
| `Strategy::on_candle` can return only a `Signal` | `script_boundary.rs::the_strategy_trait_gives_a_script_nothing_to_return_but_a_signal` |
| VM simulation == `ScriptStrategy` replay | `script_parity.rs` (5 scripts) |
| gate mints only when every condition holds | `gate.rs::authorization_is_minted_only_when_every_condition_holds` |
| token names its venue | `gate.rs::authorization_names_the_venue_it_was_checked_against` |
| token for venue A cannot start a bot on venue B | `live.rs::a_gate_approval_for_one_venue_cannot_start_a_bot_on_another` |

Open, and owned by later work:

1. **Script-declared targets** (`strategy.exit(limit=)`) are not carried into
   the replay signal. Either carry them or refuse them explicitly.
2. **Trailing stops** are refused (v2 axis); the refusal is tested, the
   capability is not built.
3. **Golden strategy-backed backtests** for promoted documents — the DSL
   golden fixture exists; a script→promote→backtest golden does not.
4. **Live parity**: replay vs live trade sequence on the same candles, which
   needs a venue/provider harness.
5. **Promotion parity**: a promoted strategy's concepts mapping back to the
   indicator revision is validated by the route, but no end-to-end visual
   acceptance has passed with a real model.
