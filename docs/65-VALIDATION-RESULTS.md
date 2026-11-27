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

## Second pass: five independent instruments (2026-11-27)

Live Binance histories were captured for BTCUSDT, ETHUSDT, SOLUSDT, BNBUSDT and
XRPUSDT: 1,501 completed 5M and 15M bars (~3.8 and ~15 days) plus 744 1H bars
(~31 days) each. Every discipline was compared on the same chronological split.
A hit rate is now reported only with at least 30 directional calls — a 0/1
result is no evidence, not a 0% rate.

Full-hierarchy minus single-frame hit rate on the test segment:

| Discipline | BTC | ETH | SOL | BNB | XRP | Wins | Mean |
|---|---:|---:|---:|---:|---:|---:|---:|
| structure | +0.009 | −0.019 | +0.016 | +0.012 | −0.001 | 3/5 | +0.003 |
| statistical | +0.003 | +0.001 | +0.007 | −0.002 | −0.004 | 3/5 | +0.001 |
| change_point | +0.013 | −0.031 | −0.030 | −0.009 | +0.017 | 2/5 | −0.008 |
| bayesian | — | — | −0.083 | — | — | 0/1 | −0.083 |
| hidden_state | +0.054 | +0.051 | −0.008 | −0.039 | −0.025 | 2/5 | +0.007 |

`hidden_state` is the only discipline with two meaningful gains (BTC and ETH),
but it loses on three other instruments. `bayesian` is suppressed by the veto
rule to almost no directional calls, so most of its rates are now correctly
unreported rather than shown as catastrophic.

## Decision

**Do not promote any multiscale discipline yet.** Across five independent
instruments the ablation test still fails: removing higher frames does not
reduce performance consistently for any discipline. The remaining next step is
regime-separated windows on these same histories, not a larger hierarchy.

## Third pass: regime-separated validation (2026-11-27)

`classify_regime` labels each test decision from returns that closed before it:
Kaufman efficiency ratio for trend, and recent realized volatility against a
four-times-longer baseline for volatility. Regimes do not see the label they are
used to explain. Each variant now carries a `regime_breakdown` with a hit rate
withheld below 30 directional calls.

Full-hierarchy minus single-frame hit rate inside each regime:

| Discipline | Regime | BTC | ETH | SOL | BNB | XRP | Wins |
|---|---|---:|---:|---:|---:|---:|---:|
| hidden_state | Ranging | +0.100 | +0.124 | +0.076 | +0.007 | +0.068 | 5/5 |
| hidden_state | Volatile | — | −0.061 | — | — | — | 0/1 |
| structure | Ranging | +0.034 | +0.004 | +0.018 | +0.034 | +0.007 | 5/5 |
| structure | Trending | +0.003 | +0.003 | +0.006 | +0.025 | +0.003 | 5/5 |
| structure | Volatile | −0.198 | −0.153 | −0.013 | −0.158 | — | 0/4 |

This is the first **consistent** cross-instrument result: multiscale hidden-state
analysis helps in ranging markets on every instrument tested, and structure
analysis consistently loses value in volatile markets. The regimes were
examined after seeing the aggregate results, so this is a post-hoc subgroup
finding with five observations and no corrected multiple-testing procedure.

### Out-of-sample instrument holdout

The hypothesis was then tested on two instruments not used in discovery:
ADAUSDT and DOGEUSDT, captured the same way.

| Discipline | Regime | ADA single → full | DOGE single → full |
|---|---|---:|---:|
| hidden_state | Ranging | 0.321 → **0.410** (+0.089) | 0.316 → **0.471** (+0.155) |
| hidden_state | Trending | 0.545 → 0.548 (+0.003) | — |
| structure | Ranging | 0.295 → 0.302 (+0.007) | 0.285 → 0.300 (+0.015) |
| structure | Trending | 0.404 → 0.459 (+0.054) | 0.384 → 0.426 (+0.042) |

The ranging hidden-state benefit survives on both holdout instruments (7/7
positive ranging deltas including discovery). This is now a credible research
hypothesis, but **still not a promoted capability**: there is no untouched
*time* holdout, no trading-cost/slippage model, no corrected multiple-testing
procedure, and no evidence of net expected value. Any future use of the regime
label must be validated on a later period before it can guide decisions.

Correctness fixes from these passes:
- `ValidationConfig::min_directional_samples` prevents tiny-sample hit rates.
- `classify_regime` reads only completed returns, so regime labels cannot leak
  the forward outcome they are used to interpret.
- `MultiscaleAnalysisResult::conditioned_lower` makes higher frames participate
  in the finest frame's call; without it, every hierarchy variant was identical.
- Real-data runs exposed and fixed an instrument-identity mismatch that had
  made every stored-history sample zero.

The harness and tools are kept so this can be re-run as new data arrives:

```
cargo run -p analytics-core --example validate_history -- history.json structure
cargo xtask validate-hierarchy --symbol BTCUSDT --timeframes 15m,5m \
    --discipline structure --from 2026-11-11 --to 2026-11-18
```
