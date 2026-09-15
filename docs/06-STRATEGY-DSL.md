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

concepts:                    # optional — measurements this document defines itself
  - name: gap                # see "Concepts: measurements the client defines"
    side: buy
    window: 3
    lower: {high: 0}
    upper: {low: 2}
    require:
      - left: {high: 0}
        op: below
        right: {low: 2}

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
    - timeframe: entry
      condition: "concepts.gap.fresh"

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

## Concepts: measurements the client defines

The condition vocabulary is closed. This is what keeps it closed **and** still lets a client
trade an idea the platform has never heard of.

A **concept** is a measurement written as data: a window of candles, two selectors giving the
band its edges, and the requirements that make the pattern the pattern.

```yaml
concepts:
  - name: gap                  # an identifier — a condition references it by this name
    label: fvg                 # how it reads on the chart; defaults to the name, opened out
    side: buy                  # which side is expected to react from the band
    window: 3                  # how many candles the pattern spans
    lower: {high: 0}           # the band's cheaper edge: the first candle's high
    upper: {low: 2}            # its dearer edge: the third candle's low
    require:                   # what makes it this pattern rather than a coincidence
      - left: {high: 0}
        op: below
        right: {low: 2}
    min_band_ratio: 0.2        # optional: the band must be this share of the window's range
```

A selector is `open | high | low | close | mid | volume` of a candle **inside the window**,
counted from the oldest (`0`), written as a one-key mapping: `{high: 0}`. `op` is
`below | above | below_or_equal | above_or_equal`. `window` is 2..=8.

A condition reads one of five properties of a concept the document declares:

| reference | type | what it says |
|---|---|---|
| `concepts.<name>.exists` | bool | the concept found a band |
| `concepts.<name>.fresh` | bool | the newest band has not been traded back into |
| `concepts.<name>.mitigated` | number | how much of it has been, `0.0..=1.0` |
| `concepts.<name>.top` | number | the newest band's dearer edge |
| `concepts.<name>.bottom` | number | the newest band's cheaper edge |

```yaml
entry:
  all_of:
    - timeframe: entry
      condition: "concepts.gap.fresh"
    - timeframe: entry
      condition: "close > concepts.gap.top"
```

Four rules, all enforced rather than documented-and-hoped:

- **A reference must name a concept the document declares.** `concepts.foo.fresh` with no
  `foo` in the block is a validation error, not a `false` — the same rule that applies to a
  condition naming an undeclared timeframe.
- **The parts describe the newest band.** A pattern is not a detector with a notion of "the"
  band: one impulse can leave several, so one has to be picked and the newest is the one a
  trader reading left to right is looking at. "The newest one is fresh" is not the same claim
  as "some band is fresh", and the grammar cannot make the second one yet.
- **A concept that found nothing is absent, not zero.** A boolean part is `false`; a numeric
  part is absent, so `close_below(concepts.gap.bottom)` on a chart with no gap is false
  rather than true against a fabricated `0.0`.
- **A band is measured only from candles up to the decision bar.** The runtime re-derives the
  band from the view's own history, which ends at the strategy's "now", so a band cannot
  report itself already mitigated on the bar it formed.

The operators did not grow. "Keep the grammar small and explicit" is a rule about the
**grammar**, not about the set of measurements, and this is where that line is drawn: a new
*operator* is a change to the DSL and needs a spec change, while a new *measurement* is a
document.

Every concept is data. Nothing a client writes is ever compiled — the sandbox's rule is that
we compile the interpreter and pass the document in, and a concept does not bend it.

The concepts a document declares are also what a chart draws: the same document sent to
`chart-engine` renders each one as a band (see `docs/14-FRONTEND-CHART-ENGINE.md`).

## Schema & validation (`strategy-dsl` crate)
- `schema.rs`: serde structs mirroring the document shape above, with strict
  `deny_unknown_fields` so malformed or hallucinated fields fail fast.
- `parser.rs`: YAML/JSON → typed `StrategyDocument`.
- `validator.rs`: semantic checks beyond schema shape:
  - referenced timeframes must be declared in `timeframes`.
  - referenced fields/functions must exist in the known condition vocabulary.
  - referenced concepts must be declared in `concepts`, and each declared concept must pass
    `analytics_core::concepts::validate` (an identifier name, a window in range, selectors
    that exist and fit the window, one side per comparison).
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

## Implementation notes (Phase 3)

Where the implementation had to decide something the sections above leave open, the
decision is recorded here rather than left implicit in the code. Every one of these is
enforced by `strategy-dsl`; none of them widens what a document is allowed to ask for.

The sample document lives at `strategies/liquidity-sweep.yaml` and is pulled into the
validator's test suite with `include_str!`, so "the sample validates" is a claim about the
artifact a user gets rather than about a fixture that could drift from it.

### `entry.direction` is optional, and never guessed
`entry.direction` may be omitted when the stop rule already implies a side
(`below_sweep_low` and `below_recent_low` are long-only; `above_swing_high` is short-only).
When the stop rule does *not* imply a side — an ATR multiple, a fixed distance — the
direction becomes **required**. Supplying a direction that contradicts the stop rule's
implied side is a validation error. What the validator will not do is pick a side on the
document's behalf.

### `risk.stop` also accepts a mapping form
The document above writes `stop: "below_sweep_low"`. Parameterised rules need an argument,
so they serialize as a mapping:

```yaml
risk:
  stop: {kind: below_recent_low, bars: 20}
```

The bare-string form is unchanged and stays the canonical spelling for rules that take no
parameters, so the example in this document round-trips byte-for-byte.

### Position-scoped fields in the condition vocabulary
`close_below(stop_price)` — the invalidation in the example above — cannot be evaluated
from `MarketState` alone: the stop price is a property of the open position, not of the
market. The vocabulary therefore also exposes `stop_price`, `entry_price`,
`position_size`, `unrealized_r`, `bars_in_trade` and `in_position`.

While flat, these fields read as **absent**, and any comparison against an absent field is
**false** rather than true-against-zero. That distinction is load-bearing: a naive `0.0`
default would make `close_below(stop_price)` fire on every bar before an entry.

### `liquidity.swept` describes the newest candle
`"sell_side"` means the newest candle traded *down through* a resting low — sell stops
taken out below the lows. `"buy_side"` is the mirror: up through a high. When the newest
candle swept nothing, the field falls back to the most recently formed swept level, so the
value is never stale-but-arbitrary. A bar that sweeps both sides reports whichever side it
overshot further, relative to the level it took.

### `liquidity.swept_level` — the field a sweep setup actually needs
`swept` alone answers "did a sweep happen?", which is not enough to trade. The bar that
*takes* a level often closes below it, and a long entered there has no valid stop:
`below_sweep_low` would resolve above the entry price. Run against six months of real
BTCUSDT 5m candles, that mismatch refused **6,234** fired setups and produced 4,952
trades at a meaningless mean of −1.78R.

The canonical setup is a sweep followed by a **reclaim**, and that is a comparison against
the swept level:

```yaml
- timeframe: entry
  condition: liquidity.swept == "sell_side"
- timeframe: entry
  condition: close > liquidity.swept_level
```

`swept_level` is the price of the level the newest candle cleared, on the side `swept`
reports, and `absent` when it cleared nothing — so a comparison against it is false rather
than true against a price of zero. Adding it turns the setup from a description of a bar
into something tradeable: with the reclaim condition the same six months produce **219**
trades, **zero** refusals, and a sane mean of **−0.228R**.

`liquidity-sweep-btcusdt-5m.yaml` is that calibrated variant; `liquidity-sweep.yaml`
remains the spec's example, unmodified.

### Stops reference the level the bar cleared
`below_sweep_low` and `above_sweep_high` resolve against the level the newest candle
actually traded through — not merely the most recently *formed* swept level. Selecting by
formation index lets the stop land on a level price has already fallen through, which on
real data produced long stops *above* their entry. When a bar clears several levels at
once, the **extreme** one counts: the lowest low for a long, the highest high for a short,
because that is the level whose loss genuinely invalidates the sweep.

### Stops, targets and invalidation all exist before entry
A signal resolves its stop and target against the decision candle's already-closed state,
so a position is never opened without a defined risk. `take_profit: risk_multiple`
resolves off the **resolved stop distance**, not off the reference price — which means the
realised R of a stop-out is not exactly −1 whenever the fill differs from the decision
close. That is deliberate and visible in the report rather than smoothed away.

### Validation limits
`MAX_DOCUMENT_BYTES` is 256 KiB; a document may declare at most 8 timeframes, 64
conditions, and 512 characters per expression. `max_risk_pct` is bounded by a hard ceiling
of **5.0%** that a document cannot raise — the ceiling lives in the validator, not in the
document, which is what makes it a ceiling rather than a suggestion.
