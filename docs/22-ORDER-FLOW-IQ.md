# 22 — Order-flow intelligence roadmap: the "Order Flow IQ" gap analysis

## Purpose

A widely shared "Order Flow IQ" TradingView suite (footprint + delta-by-order-size +
profile-with-memory + iceberg detection, all Lee-Ready classified) was reviewed against
this platform. This document answers three questions: **what does it have that we do
not**, **what do we build to close and exceed the gap**, and **what does each feature
buy the AI thesis** (principle #8: explainable, base-rated, invalidatable signals).

The headline: nothing in the suite is out of reach, and on the decisive axes we can
**exceed** it. TradingView offers no level-2 and classifies aggressor side with the Lee
Ready approximation on resampled tick data. We run our own Binance collector:
`aggTrade` events carry the aggressor side natively, and the `@depth@100ms` +
`@aggTrade` streams are exactly the input the modern iceberg "Resistance" detection
algorithm needs. Every feature below also widens the Strategy DSL surface and therefore
the backtester's ability to base-rate agent theses.

## Where things go

The architecture decides this before any code is written (principles #1, #2):

* math: `crates/analytics-core/src/` (pure, wasm-safe, property-tested)
* HTTP: `crates/api-gateway/src/` (routes answer from the same numbers the agent reads)
* agent tools: `crates/ai-agent/src/tools.rs` (the LLM never computes a number)
* rendering: `frontend/chart-engine/src/` (native + wasm, positioned rectangles)

---

## Feature ledger

Legend: **have** = shipped, **partial** = the data exists but the number is not exposed,
**gap** = absent. Priority is the build order.

| # | Feature (source) | What it is | Our status | Priority | Lands in |
|---|---|---|---|---|---|
| 1 | Delta by order size (video) | Bucket every trade by notional into S/M/L and track per-class delta/CVD | **gap** (trades carry quantity; aggregation missing) | P1 | `size_classes.rs` |
| 2 | Max/min intra-bar delta (video) | Highest positive and lowest negative cumulative delta *inside* the bar | **partial** (per-trade tape exists; only end-of-bar delta exposed) | P1 | `bar_delta.rs` |
| 3 | Intrabar VWAP (video) | Volume-weighted mean trade price within each bar | **partial** (session VWAP exists; intrabar missing) | P1 | `bar_delta.rs` |
| 4 | Per-class CVD (video "CVD by order size") | Running delta per size class, session-reset like `cvd.rs` | **gap** | P1 | `size_classes.rs` |
| 5 | Size-classed footprint cells (video + web) | Each ladder cell split S/M/L, not just bid x ask | **gap** | P2 | `/footprint` response |
| 6 | Volume filter by order size (video wish, web) | Profile/footprint computed over one class only | **gap** | P2 | `size_classes.rs` predicate |
| 7 | Volume profile with memory (video flagship) | Per-level delta history over a session: level flip, delta at extreme | **gap** (final snapshot only) | P2 | `profile_memory.rs` |
| 8 | Iceberg detection (video, web: Bookmap Resistance method) | Volume at a level far exceeding max visible size => hidden refill | **gap** | P2 | `iceberg.rs` |
| 9 | VPIN (web: Easley/O'Hara) | Order-flow toxicity per volume bucket | **gap** (absent from TradingView entirely) | P3 | `vpin.rs` |
| 10 | Live order feed (video) | Tick tape with sizes, class-colored | **gap** | P3 | frontend lane |
| 11 | Lee Ready classification (video) | Tick-rule aggressor fallback for venues without native side | **n/a for Binance** (native side), P3 for multi-venue future | P3 | `delta.rs` |
| 12 | Footprint pattern taxonomy (web: ATAS) | Exhaustion, balanced, P/b-shape as computed classifiers | **gap** | P3 | `footprint_patterns.rs` |
| 13 | CVD divergence by size class (web idea) | Score divergence between small/large CVD as a DSL-able signal | **gap** | P3 | `size_classes.rs` |

## Build order

### Phase 1 — Size classes (P1, this phase)

`size_classes.rs`: `SizeClassConfig` (small/medium notional thresholds in quote
currency, defaults 10k/50k, mirroring the video's configurable ranges), a `SizeClass`
enum (`Small | Medium | Large`), `classify(trade) -> SizeClass`, and:

* `delta_by_size(trades) -> SizeDeltaBreakdown` (per-class buy/sell/delta/notional)
* `DeltaClassTracker`: stateful per-class CVD with the same UTC-session reset as
  `cvd::Cvd`
* `calculate_cvd_by_size(trades, config, reset_at_session) -> Vec<SizeClassCvdPoint>`
* per-candle breakdown, and `classify_volume_profile(trades, ..., predicate)` producing
  a `VolumeProfile` from one class only — the volume filter (#6) for free.

Value to the thesis: turns "absorption" from a pattern into a **causal story** ("price
made a low while small orders aggressively bought and large orders stopped selling —
large passive flow absorbed retail aggression"), and makes the story **base-ratable**:
`backtest_similar_setups` can score the same size-class signature historically.

### Phase 2 — Bar delta statistics (P1, this phase)

`bar_delta.rs`: for each candle built from its own trades —

* `max_delta` / `min_delta`: extremes of the *running* cumulative delta inside the bar
  (the "trapped traders" evidence: a bar that printed +X peak delta but closed at −Y
  trapped late buyers)
* `delta_close_position`: where the bar's closing delta sits between those extremes
* `intrabar_vwap`: Σ(price·qty)/Σ(qty) over the bar's trades
* trade count and median notional

Lands in `MarketState` (per candle) and as a `GET /bar-delta-stats` route. These are
exactly the numbers principle #2 forbids the LLM from approximating.

### Phase 3 — Volume profile with memory (P2)

`profile_memory.rs`: a session-accumulating profile where each node carries its
per-candle delta history (`levels: Vec<LevelDeltaVisit>`): the level's delta at every
candle that touched it, running max/min delta, and whether the level **flipped control**
(signed delta changed sign after having been strongly one-sided). Reset period defaults
to the UTC day, matching `cvd.rs`. The frontend draws the "protruding lines" the video
shows; the agent can finally cite *"this level was defended, flipped control at HH:MM,
and is currently held by buyers"* as a structured fact.

### Phase 4 — Iceberg detection (P2)

`iceberg.rs`, following Bookmap's documented **Resistance** method (their modern
algorithm; with well-ordered MBP data its accuracy is close to native MBO):

* input: ordered L2 snapshots + the trade tape (both already collected per symbol)
* a level is an iceberg candidate when executed volume at that price over a window
  exceeds `max_visible_size × ratio_threshold` (default 3x, matching the imbalance
  convention) while the level's displayed size keeps **refilling** after fills
* emits `IcebergEvent { price, side, executed, max_visible, ratio, confidence }`
* honesty rule: a candidate is *probabilistic*, always labeled as such (the video says
  the same), and the tool refuses with an explicit note when depth history is missing —
  same degradation pattern as every other footprint-level tool.

Bridges liquidity detection (resting levels) to absorption (flow outcome): "someone big
is defending this price" becomes mechanical evidence.

### Phase 5 — VPIN (P3)

`vpin.rs`: Volume-Synchronized Probability of Informed Trading (Easley, López de
Prado, O'Hara). Volume buckets (not time buckets); per bucket, |V_buy − V_sell| / V
classified with the standard bulk-volume classification on trades; VPIN = rolling mean
of bucket imbalances. One honest 0–1 number: *"flow toxicity elevated — size down,
widen invalidation."* Absent from TradingView entirely; a differentiator for the
agent's risk language.

### Phase 6 — Frontend: footprint facelift + new lanes

* per-cell size-class visualization and imbalance glow on the ladder
* a `barDeltaStats` strip beside `footprintStats` (max/min delta, intrabar VWAP)
* profile-with-memory lane in `scene.rs`
* VPIN + per-class CVD readouts

### Phase 7 — Agent tools + DSL

`get_delta_by_size`, `get_bar_delta_stats`, `get_profile_memory`,
`detect_iceberg`, `get_vpin`, `detect_size_class_divergence` — each a thin wrapper over
the core, each returning structured numbers with the honest-unavailable pattern. New
Strategy DSL conditions follow (size-class divergence, VPIN threshold).

---

## AI thesis value model (why each feature earns its place)

| Feature | Claim it makes possible | Scoreable how |
|---|---|---|
| Size classes | "Whales were selling into retail buying at the high" | Base rate of that size signature historically |
| Max/min delta | "Late buyers are trapped above" | Post-bar drift after the same delta whipsaw |
| Intrabar VWAP | "Price is stretched above fair value for this bar" | Mean-reversion frequency |
| Profile memory | "This level flipped control and is now defended" | Level-flip → continuation stats |
| Icebergs | "A large passive buyer is defending this price" | Post-iceberg continuation |
| VPIN | "Flow toxicity is spiking — regime is changing" | Volatility burst prediction |

The compounding effect: today the agent reasons over end-state aggregates; after this
roadmap every candle carries **who** (size class), **how the bar developed** (delta
extremes), and **what the level did over the session** (memory) — three structured
dimensions, each directly scoreable.

## Done criteria

* Each phase lands with unit + property tests in `analytics-core` (invariants: class
  volumes sum to total, per-class CVD sums to plain CVD, VPIN ∈ [0,1], iceberg ratio
  monotone in executed volume, no fabrication without tape).
* Routes answer from the same code path the agent reads (one implementation).
* The frontend renders every new field with an explicit "unavailable without tick data"
  state rather than an empty one.
* Every feature is exercised live on the web (preview browser) before the phase is
  called done.
