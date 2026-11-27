# 64. Top-down Core Analytics (first implementation phase)

This document records the first implemented slice of the market reverse-engineering
research programme. It is deliberately narrower than the roadmap: the roadmap is a
multi-month research agenda, and nothing here claims an unvalidated predictive
edge.

## What is implemented

### Multiscale observation (`crates/analytics-core/src/multiscale.rs`)

`MultiTimeframeObservation` is the shared market representation for every
Core Analytics discipline:

- frames are ordered **coarsest to finest** (the top-down analytical direction);
- timeframes are data-driven — the caller supplies the ladder it actually has;
- completed candles are separated from the currently forming candle, so a
  discipline cannot read a close that was not known at the decision instant;
- a candle that *opens* after the decision is rejected as look-ahead;
- gaps, duplicates and out-of-order input are measured in `DataQuality`;
- `from_fine` reuses `resample_all`, so the buy/sell split and candle deltas are
  preserved by the existing bucket math rather than reimplemented.

### Hierarchical analysis (`crates/analytics-core/src/hierarchy.rs`)

`HierarchicalAnalyzer` runs a discipline's own per-frame function across the
observation and produces a standard `MultiscaleAnalysisResult`:

- one `TimeframeAnalysis` per usable frame;
- descriptive cross-timeframe relationships (alignment, conflict, state
  dependency, volatility transmission);
- `OpportunityCandidate`s on the finest frame, with supporting and conflicting
  evidence from higher frames;
- an `UncertaintyReport` listing limitations.

Two honesty rules are built into the types:

1. **Evidence strength is not a probability.** It lives in `f64 strength`
   fields. A `DirectionDistribution` can only be supplied by a caller with
   calibrated empirical frequencies, and the framework sets it to `None`.
2. **Relationships are descriptive.** `statistical_significance` is `None`
   until an out-of-sample research evaluation supplies it.

### Deterministic discipline adapters (`crates/analytics-core/src/disciplines.rs`)

Seven adapters reuse existing mathematics; none trains a model:

| Discipline | Existing function reused |
|---|---|
| `structure` | `detect_market_structure` |
| `hidden_state` | `hmm_regime` + `bayesian_drift` |
| `bayesian` | `bayesian_drift` |
| `volatility` | `garch` |
| `information` | `shannon_entropy` |
| `change_point` | `cusum_stat` |
| `statistical` | `zscore` |

`analyze_hierarchy(observation, discipline, model_version)` is the single entry
point used by callers.

### Agent tool (`crates/ai-agent/src/tools.rs`)

`analyze_multiscale(symbol, timeframes[], discipline, lookback?)` loads each
requested frame, builds the observation, calls `analytics_core::analyze_hierarchy`,
and returns the serialized result. The LLM still calculates nothing: the tool is
a thin data-loading wrapper. It is available to `market_analyst`, `whale_flow`,
`smc`, `risk`, and the orchestrator; its schema advertises exactly the discipline
names the core supports.

### Validation harness (`crates/analytics-core/src/validation.rs`)

`compare_hierarchies` evaluates single-frame, two-frame, three-frame, full and
ablation variants on an untouched chronological test segment:

- predictions use only candles that closed before each decision;
- labels are computed offline from future candles, after the decision;
- train and test are separated by an embargo of finest-frame bars so forward
  returns cannot overlap the boundary;
- reports include hit rate, false-signal rate and an **uncalibrated** strength
  error, plus explicit limitations;
- results are deterministic and a report with zero test samples is returned
  honestly rather than fabricated.

This satisfies the temporal-integrity requirements. It does **not** by itself
show that a hierarchy helps on real data: that requires running the harness on
multiple instruments and regimes and inspecting the ablations.

### Cross-scale dependence (`crates/analytics-core/src/cross_scale.rs`)

`reconstruct_multiscale` reconstructs each frame's bar-direction condition and
measures pairwise categorical mutual information between a higher frame's bar
state and a lower frame's bar state, aligning lower bars to higher buckets by
timestamp. Every estimate reports its sample count; significance is `None`
because it requires out-of-sample research. This is the explicit cross-timeframe
dependency layer the roadmap requires — running one model per frame is not
hierarchical by itself.

### Market Reverse Engineering Lab (`crates/analytics-core/src/research_lab.rs`)

`run_lab` composes the six engines from the research programme:

1. **Structural discovery** — reuses `analyze_structure`.
2. **Participant behaviour** — reports the buy/sell aggressor split visible in
   candles. It explicitly does not claim to identify a participant class.
3. **Market state** — reuses the hidden-state/HMM adapter.
4. **Information discovery** — uses the strongest measured cross-scale
   categorical dependence, with sample count and no invented significance.
5. **Scenario simulation** — reuses the seeded stationary-bootstrap cone. It
   is reported as a scenario with zero evidence strength, never as evidence.
6. **Prediction validation** — reports that it requires historical outcomes and
   points to `validation::compare_hierarchies`; it never scores an unlabelled
   observation.

The lab is exposed as the agent tool `reverse_engineer_market(symbol,
timeframes[])`, which only loads market data and returns the core's report.
Its findings are hypotheses, not calibrated probabilities, and cannot bypass
strategy rules or the risk gate.

`measure_dependency` skips lower bars whose higher bucket was not supplied, so
ladders with different coverage report only genuinely paired observations.
When no pair exists, the information engine reports zero strength with an
explicit explanation instead of omitting the engine.

### Opportunity scanner (`crates/analytics-core/src/opportunity_scanner.rs`)

`scan_instrument` implements the five-stage funnel: data quality → fast signal →
deep hierarchical analysis → independent confirmation → chart-ready setup. An
`InstrumentProfile` supplies the timeframes a symbol actually supports, so no
ladder is hardcoded. `SetupProposal` carries entry zone, stop, two reward
targets, an invalidation rule and the evidence that produced it. The ranking
score is explicitly not a probability, and a setup is still subject to strategy
rules and the existing risk gate.

### Continuous learning (`crates/analytics-core/src/continuous_learning.rs`)

`OutcomeLog` records predictions at decision time and settles them later with
labels computed from future candles; duplicate records and mismatched labels are
rejected. `detect_drift` compares the most recent settled outcomes against the
earlier baseline with a two-proportion z-statistic, and `evaluate_promotion`
requires both operator authorisation and actual improvement. It never promotes
a model on its own and never treats evidence strength as a probability.

### HTTP surface (`GET /scan/multiscale`)

`crates/api-gateway/src/multiscale_routes.rs` exposes the scanner as a public
read. Callers supply explicit `symbols` and a `timeframes` ladder; the route
loads completed candles through the same `WindowService` the charts and agent
read, builds one strict observation per symbol, and returns the scan verdict.
A proposed setup is converted into the chart engine's existing
`Overlay` shape (entry band, stop, targets), so no new scene field or drawing
type was needed. Three integration tests exercise the route through the real
router with candles seated in RAM.

The scan panel has a **Core scan** button. It calls the route for the named
symbol with a 4H/1H/15M/5M ladder and stores any proposed overlays, scoped to
that symbol, for injection into the next chart scene request. The shell never
calculates a price level: every overlay comes from the Rust scanner.

### Durable predictions (`0023_core_analytics_predictions.sql`)

The gateway persists decisions at `POST /analytics/predictions` and settles them
once from realized market data at `POST /analytics/predictions/settle`. Decision
fields are never rewritten: the same one-shot contract as `markov_evidence`
prevents hindsight from entering a reliability report. `GET /analytics/predictions`
lists a user's rows; `GET /analytics/drift` rebuilds the in-memory outcome ledger
from those rows and reports the core's drift and promotion decision. Promotion
requires explicit operator authorisation and remains a recommendation only.

## What is explicitly not claimed

- no trained neural network, foundation model or learned weights;
- no information-transfer measurement across frames (only descriptive
  relationships);
- no calibrated probability of success;
- no automatic trade generation — candidates still require the strategy rules,
  opportunity validation and the existing risk gate;
- no evidence that multiscale analysis outperforms a single-timeframe baseline
  on any instrument.

## Next phases from the roadmap

1. Run the validation harness across multiple instruments and regimes, inspect
   ablations, and keep only disciplines that show a reproducible benefit.
2. Opportunity validation, chart evidence display and scanner integration.
3. Only after validation: learned representations with an explicit training
   boundary outside the deterministic Rust runtime.

The real-data validation passes are recorded in `docs/65`. Aggregate results
fail the ablation test. A post-hoc regime split found a ranging-market benefit
for hidden-state analysis that survived an out-of-sample instrument holdout but
**did not replicate on an untouched older time window**, and every variant is
net-negative after a 10 bps round-trip cost. Nothing is promoted.

`neuro_market` provides the deterministic NEURO-MARKET primitives (masked-candle
reconstruction, packed signatures, similarity) without learned weights, exposed
as the `representation` discipline. A trained model remains a future decision.
