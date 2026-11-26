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
2. Multiscale reconstruction and explicit cross-timeframe dependency measures.
3. Opportunity validation, chart evidence display and scanner integration.
4. Only after validation: learned representations with an explicit training
   boundary outside the deterministic Rust runtime.
