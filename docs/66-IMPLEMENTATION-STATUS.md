# 66. Implementation status of the market reverse-engineering discussion

This records what from `reverse engeneering.md` is actually implemented, what
is deliberately not, and how to reach each capability. It is written so a
reader can distinguish working code from research intent.

## Implemented

| Discussion item | Where | Exposed as |
|---|---|---|
| Synchronized multiscale observations, no look-ahead, data quality | `analytics-core/src/multiscale.rs` | `MultiTimeframeObservation` |
| Top-down analysis with higher-frame reinforcement/veto | `analytics-core/src/hierarchy.rs` | `HierarchicalAnalyzer`, `conditioned_lower` |
| Deterministic discipline adapters (8) | `analytics-core/src/disciplines.rs` | `structure`, `hidden_state`, `bayesian`, `volatility`, `information`, `change_point`, `statistical`, `representation` |
| Cross-scale categorical dependence and reconstruction | `analytics-core/src/cross_scale.rs` | `reconstruct_multiscale`, `measure_dependency` |
| Six-engine market reverse-engineering lab | `analytics-core/src/research_lab.rs` | `run_lab`, agent tool `reverse_engineer_market` |
| Deterministic NEURO-MARKET primitives (masked reconstruction, signatures, similarity) | `analytics-core/src/neuro_market.rs` | `representation` discipline |
| Five-stage opportunity scanner and setup proposals | `analytics-core/src/opportunity_scanner.rs` | `scan_instrument`, `SetupProposal` |
| Chronological validation with ablations, regimes and costs | `analytics-core/src/validation.rs` | `compare_hierarchies`, `validate_history` example, `cargo xtask validate-hierarchy` |
| Outcome recording and drift/promotion gate | `analytics-core/src/continuous_learning.rs`, migration `0023` | `POST /analytics/predictions`, `/settle`, `GET /analytics/drift` |
| Agent tools | `ai-agent/src/tools.rs`, `modes.rs` | `analyze_multiscale`, `reverse_engineer_market` |
| HTTP and shell | `api-gateway/src/multiscale_routes.rs`, `analytics_routes.rs`, `frontend/app/app.js` | `GET /scan/multiscale`, **Core scan** button, chart overlays |

## Not implemented (deliberate)

- **Trained neural networks / learned weights.** The choice was pure-Rust
  deterministic encoders only. `representation` is an inspectable bucket
  encoder, not a trained model.
- **Universal multi-broker scanner across Forex/equities.** The funnel and
  instrument profiles exist; live data sources are still Binance/Deriv gold.
- **Automatic execution from Core Analytics findings.** Setups remain proposals
  behind strategy rules and the existing risk/live gate.
- **Promoted multiscale signal.** Validation does not support promotion
  (`docs/65`).

## Validation outcome

- Aggregate: no hierarchy variant consistently beats a single-timeframe
  baseline across five independent instruments.
- Post-hoc ranging-market hidden-state benefit survived a two-instrument
  holdout but **failed on an untouched older time window**.
- Every variant is net-negative after a 10 bps round-trip cost; slippage is not
  modelled. No trading edge is claimed.

## Running locally

```
cargo build -p api-gateway
DATABASE_URL=postgresql://macbook@127.0.0.1:5432/ai_trading \
BIND_ADDR=127.0.0.1:8080 MARKET_FEED=binance RUN_MIGRATIONS=true \
target/debug/api-gateway
node frontend/dev-proxy.js 3000
```

Open http://127.0.0.1:3000. The scan panel's **Core scan** button calls
`GET /scan/multiscale` with a 4H/1H/15M/5M ladder and injects any proposed
entry/stop/target overlays into the next chart scene.

Hosting is intentionally left to the account owner. No Hostless configuration
was changed during this implementation pass.
