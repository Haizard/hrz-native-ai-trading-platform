# 63 — Ensemble Intelligence

- **Status:** deterministic fusion core landed — `crates/ensemble-intelligence`
  with typed evidence, correlation-aware weighting, a reliability registry and
  five passing tests. No model is included.
- **Kronos:** deliberately not included. A future adapter supplies evidence the
  same way Markov, SMC, footprint and VWAP do.

## Contract

`Evidence` carries a source id, category, direction, strength, reliability,
validity, timestamp and an audit note. `RiskGate` carries data quality, risk
limits and the kill-switch. `fuse()` returns a `DecisionProposal` of **BUY /
SELL / WAIT / NO_TRADE** with score, confidence, explanations, disagreements,
used sources and ignored sources.

## Rules

1. Invalid evidence is ignored and named, never treated as neutral support.
2. Engines in the same category are correlated: the strongest contribution
   counts fully, further ones count at half weight.
3. Agreement produces a directional proposal; material disagreement damps the
   score and normally returns WAIT.
4. WAIT means the opportunity is unclear. NO_TRADE is reserved for a specific
   data-quality, risk-limit or kill-switch reason.
5. A proposal is not an order. The risk gate and `LiveAuthorization` remain the
   only execution authority.

`ReliabilityRegistry` tracks settled hits per source with one prior success and
one prior failure, so a new engine is neither trusted nor dismissed on a single
observation.

## Verification

`cargo test -p ensemble-intelligence` — 5 tests pass: agreement, disagreement,
risk block, reliability prior, invalid-evidence handling.
