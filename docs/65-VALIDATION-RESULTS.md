# 65. Multiscale validation results (first real-data pass, 2026-11-27)

This is the first run of `compare_hierarchies` on real market data. It is a
research finding, not a promotion decision.

## Method

- Chronological split: 60% train, 6-bar embargo, untouched test segment.
- Finest-frame decision clock; labels computed from future candles offline.
- Variants: `single`, `two_frame`, `three_frame`, `full`, and one ablation per
  removed timeframe.
- Prediction source: `MultiscaleAnalysisResult::conditioned_lower` — the finest
  frame's own discipline result after higher-frame agreement reinforces it or
  opposition vetoes it. Each discipline still computes its own per-frame result;
  the hierarchy only conditions the relationship.
- Two data sources:
  - BTCUSDT and ETHUSDT live Binance histories captured through `GET /candles`
    (601 completed 1H/15M/5M bars each, ~50 hours).
  - Stored Postgres history for BTCUSDT and XAUUSD (15M/5M, Nov 11–18 2026),
    via `cargo xtask validate-hierarchy`.

## Results (test segment, hit rate on directional calls)

| Discipline | Instrument | single | two-frame | three-frame | full |
|---|---|---:|---:|---:|---:|
| structure | BTCUSDT (live) | 0.197 | **0.240** | 0.223 | 0.223 |
| structure | ETHUSDT (live) | **0.258** | 0.196 | 0.229 | 0.229 |
| statistical | BTCUSDT (live) | **0.140** | 0.134 | 0.139 | 0.139 |
| statistical | ETHUSDT (live) | **0.284** | 0.284 | 0.273 | 0.273 |
| change_point | BTCUSDT (live) | **0.116** | 0.108 | 0.104 | 0.104 |
| change_point | ETHUSDT (live) | **0.227** | 0.220 | 0.202 | 0.202 |
| bayesian | BTCUSDT | 0.236 | 0.236 | 0.236 | 0.236 |
| bayesian | ETHUSDT | 0.245 | 0.245 | 0.245 | 0.245 |

Stored BTCUSDT and XAUUSD produced **identical** metrics after scaling: their
histories are scaled copies of one synthetic pattern, so they cannot support a
cross-instrument conclusion. They are useful only as a determinism check.

## Findings

1. The hierarchy now genuinely participates: higher frames change the
   directional call and the number of directional calls (e.g. structure BTC
   233 → 171, ETH 233 → 184). The earlier versions, where every variant was
   identical, were measuring nothing.
2. There is **no reproducible benefit** from adding higher timeframes. The only
   improvement (BTC structure, single 0.197 → two-frame 0.240) reverses on ETH
   (0.258 → 0.196). Single-frame baselines win on most disciplines.
3. Absolute hit rates are low (roughly 0.10–0.28) and false-signal rates are
   high (roughly 0.7–0.9) under a 0.2% neutral threshold and a six-bar horizon.
   No result here is evidence of a tradable edge.
4. `bayesian` produces no cross-frame signal: its higher-frame conclusions are
   neutral under the current threshold, so conditioning has nothing to apply.
   `volatility` and `information` produce no directional calls at all by design
   and are correctly reported as `None` rather than fabricated.

## Decision

**Do not promote any multiscale discipline yet.** The ablation test fails: on
the available instruments, removing higher frames does not reduce performance
consistently. The correct next step is more diverse, genuinely independent
instrument histories and regime-separated tests — not a larger hierarchy.

The harness and tools are kept so this can be re-run as new data arrives:

```
cargo run -p analytics-core --example validate_history -- history.json structure
cargo xtask validate-hierarchy --symbol BTCUSDT --timeframes 15m,5m \
    --discipline structure --from 2026-11-11 --to 2026-11-18
```
