# 06 — Strategy DSL (Shared Indicator/Strategy/Bot Representation)

## Purpose
Define **one** declarative representation of trading logic that can be produced by
(a) natural language via the AI agent, (b) a visual no-code builder, or (c) a developer
writing it directly — and that can be executed unchanged as a chart indicator, a
backtest, a paper-trading bot, or a live bot. This is principle #5 from
`00-VISION-AND-PRINCIPLES.md`.

## Why not clone Pine Script
Pine Script is TradingView's own scripting language, tightly coupled to their servers
and rendering. This platform's DSL is purpose-built to be: (1) safely sandboxable,
(2) directly generatable/validatable by an LLM, and (3) shareable verbatim across
indicator/backtest/bot execution modes.

## Document shape (YAML shown; JSON is equivalent and is what the AI agent emits)

```yaml
name: "Liquidity Sweep + Absorption"
version: "2.1"
kind: strategy            # indicator | strategy | bot — same schema, different consumer
market: "BTCUSDT"
timeframes:
  trend: "4h"
  setup: "1h"
  entry: "5m"

entry:
  all_of:
    - timeframe: trend
      condition: market_structure.trend == "bullish"
    - timeframe: setup
      condition: absorption.detected == true
    - timeframe: entry
      condition: liquidity.swept == "sell_side"
    - timeframe: entry
      condition: delta > threshold(1500)

risk:
  max_risk_pct: 1.0
  stop: "below_sweep_low"
  take_profit:
    type: "risk_multiple"
    value: 2.5

invalidation:
  - timeframe: entry
    condition: "close_below(stop_price)"

metadata:
  created_by: "ai_agent"     # ai_agent | visual_builder | developer_sdk
  skill_ref: "liquidity-sweep-absorption-v2"
```

## Condition language
Conditions are small boolean expressions over named fields exposed by `analytics-core`'s
`MarketState` (see `docs/05-ANALYTICS-ENGINE.md`), plus helper functions:
`threshold(x)`, `above(a, b)`, `below(a, b)`, `crosses_above(a, b)`, `new_low()`,
`new_high()`, `close_below(x)`, `close_above(x)`. Keep the condition grammar small and
explicit rather than a general-purpose expression language — every operator supported
must be enumerable and individually testable by the validator.

## Schema & validation (`strategy-dsl` crate)
- `schema.rs`: serde structs mirroring the document shape above, with strict
  `deny_unknown_fields` so malformed or hallucinated fields fail fast.
- `parser.rs`: YAML/JSON → typed `StrategyDocument`.
- `validator.rs`: semantic checks beyond schema shape:
  - referenced timeframes must be declared in `timeframes`.
  - referenced fields/functions must exist in the known condition vocabulary.
  - risk block must have a positive `max_risk_pct` under a configured hard ceiling
    (e.g. never above 5%, regardless of what was requested).
  - `invalidation` must reference at least one concrete condition.

**Any document that fails validation is rejected before it ever reaches the sandbox or
runtime — the AI agent must be told the specific validation error and asked to correct
it, never silently patched.**

## Execution (`strategy-runtime` crate)
```rust
pub trait Strategy {
    fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal>;
}
```
A `StrategyDocument` compiles into a runtime `Strategy` implementation (interpreted, not
codegen'd, for Phase 3 — a compiled/JIT path can be a later optimization if profiling
shows it's needed). `MarketContext` wraps the current `MarketState` for every declared
timeframe plus position/account state when running inside the backtester or a bot.

## Developer SDK (optional, later)
Advanced users may eventually write a `Strategy` implementation directly in Rust,
compiled to the same sandbox target — but that is explicitly out of scope until Phase 3
is stable; do not build it prematurely.

## Three creation modes, one schema
1. **AI / natural language** — user describes intent; agent emits a validated
   `StrategyDocument`.
2. **Visual builder** — frontend UI composes the same schema via drag-and-drop; no new
   backend representation needed.
3. **Developer SDK** — later; still targets the same `Strategy` trait.

## Done criteria
- Schema + parser + validator implemented with unit tests for every rejection case
  listed above.
- A hand-written sample document (the liquidity-sweep example) round-trips: parse →
  validate → execute against a small fixture dataset → produces expected signals.
