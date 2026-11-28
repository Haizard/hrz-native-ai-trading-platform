# 66. Implementation status of the market reverse-engineering discussion

This records what from `reverse engeneering.md` is actually implemented, what
is deliberately not, and how to reach each capability. It is written so a
reader can distinguish working code from research intent.

## Implemented

| Discussion item | Where | Exposed as |
|---|---|---|
| Synchronized multiscale observations, no look-ahead, data quality | `analytics-core/src/multiscale.rs` | `MultiTimeframeObservation` |
| Top-down analysis with higher-frame reinforcement/veto | `analytics-core/src/hierarchy.rs` | `HierarchicalAnalyzer`, `conditioned_lower` |
| Deterministic discipline adapters (11) | `analytics-core/src/disciplines.rs` | `structure`, `hidden_state`, `bayesian`, `volatility`, `information`, `change_point`, `statistical`, `representation`, `nonlinear`, `bayesian_online`, `rough_volatility` |
| Cross-scale categorical dependence and reconstruction | `analytics-core/src/cross_scale.rs` | `reconstruct_multiscale`, `measure_dependency` |
| Six-engine market reverse-engineering lab | `analytics-core/src/research_lab.rs` | `run_lab`, agent tool `reverse_engineer_market` |
| Deterministic NEURO-MARKET primitives (masked reconstruction, signatures, similarity) | `analytics-core/src/neuro_market.rs` | `representation` discipline |
| Transfer entropy, Hurst/Haar/recurrence, particle filter, BOCPD, EGARCH | `analytics-core/src/research_methods.rs` | `transfer_entropy`, `hurst_exponent`, `particle_filter`, `bocpd`, `egarch`, plus `nonlinear`, `bayesian_online`, `rough_volatility` disciplines |
| Participant simulation, participant/dealer proxies, cross-market TE | `analytics-core/src/market_mechanics.rs` | `simulate_participants`, `estimate_participant_mix`, `estimate_dealer_state`, `cross_market_transfer_entropy` |
| Hierarchical coupling of the seven Markov engines | `crates/hierarchical-intelligence` | `analyze_markov_hierarchy` tool |
| Strategy Intelligence Framework: per-chart policies, unified pipeline, discovery | `analytics-core/src/strategy_intelligence.rs` | `unify_opportunities` tool |
| Market Opportunity Memory | `analytics-core/src/opportunity_memory.rs`, migration `0024/0025` | `OpportunityMemory` core + `POST/GET /analytics/opportunities`, `/settle` |
| Five-stage opportunity scanner and setup proposals | `analytics-core/src/opportunity_scanner.rs` | `scan_instrument`, `SetupProposal` |
| Chronological validation with ablations, regimes and costs | `analytics-core/src/validation.rs` | `compare_hierarchies`, `validate_history` example, `cargo xtask validate-hierarchy` |
| Outcome recording and drift/promotion gate | `analytics-core/src/continuous_learning.rs`, migration `0023` | `POST /analytics/predictions`, `/settle`, `GET /analytics/drift` |
| Agent tools | `ai-agent/src/tools.rs`, `modes.rs` | `analyze_multiscale`, `reverse_engineer_market`, `analyze_markov_hierarchy`, `unify_opportunities` |
| HTTP and shell | `api-gateway/src/multiscale_routes.rs`, `analytics_routes.rs`, `kronos_routes.rs`, `frontend/app/app.js` | `GET /scan/multiscale`, `/analytics/*`, `GET /kronos/forecast`, **Core scan** button, chart overlays |

## Remaining research items — now implemented

| Discussion item | Where | Exposed as |
|---|---|---|
| Hierarchical/coupled hidden states and switching state-space | `analytics-core/src/hierarchical_state_space.rs` | `analyze_coupled_states`, `switching_kalman` |
| Options positioning and dealer behaviour | `analytics-core/src/options_positioning.rs` | `analyze_positioning` from a supplied chain |
| Rough volatility | `analytics-core/src/complexity_methods.rs` | `rough_volatility` |
| Full transfer-entropy market network | `analytics-core/src/complexity_methods.rs` | `market_network` |
| Fractal/multifractal and recurrence quantification | `analytics-core/src/complexity_methods.rs` | `multifractal_exponents`, `recurrence_quantification` |
| Market manipulation / liquidity engineering | `analytics-core/src/liquidity_forensics.rs` | `analyze_liquidity_forensics` |
| Strategy discovery with validation | `analytics-core/src/strategy_discovery.rs` | `validate_discovered_rules` |

## Pretrained neural inference (no training step)

- `crates/kronos-intelligence` loads the MIT-licensed pretrained Kronos-mini
  checkpoint (~16 MB) and Kronos-Tokenizer-2k (~16 MB) from `models/kronos` and
  implements the full forward pass in pure Rust: hierarchical token embedding,
  temporal embeddings, four Transformer blocks with RoPE, dual s1/s2 heads,
  dependency-aware cross-attention, the binary spherical tokenizer and
  deterministic argmax autoregressive generation.
- Exposed as the agent tool `kronos_forecast(symbol, timeframe, lookback?, horizon?)`.
  No training, Python runtime, GPU or external service is involved.
- The forecast is research output only. It is not validated for QUANTERA's
  instruments and is never a trading signal.

## Not implemented (deliberate)

- **Training neural networks locally.** The choice was to use pretrained
  weights and deterministic encoders, not to train on this machine. Future
  work could train a dedicated model, but it is not required to use the neural
  feature.
- **Universal multi-broker scanner across Forex/equities.** The funnel and
  instrument profiles exist; live data sources are still Binance/Deriv gold.
- **Full agent-based market simulation as a live signal.** The four-class
  simulation is a research instrument; it is not fitted to a venue or used to
  make decisions.
- **Real options/dealer data feeds.** Proxies accept a caller-supplied chain;
  the platform does not fetch options data.
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
