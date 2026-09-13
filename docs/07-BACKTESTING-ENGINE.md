# 07 — Backtesting Engine

## Purpose
Deterministically replay historical market data through a `StrategyDocument` (via
`strategy-runtime`) and produce trustworthy performance statistics — this is what makes
an AI-generated thesis credible ("historical win rate: 68.5%").

## Design
- **Event-driven replay**: iterate historical candles (and optionally trades, for
  finer-grained fills) in chronological order per declared timeframe, feeding each into
  the compiled strategy's `on_candle`.
- **Order/position simulation**: on a `Signal`, open a simulated position sized per the
  strategy's risk block; track stop/target; close on invalidation, stop, target, or
  explicit exit signal.
- **No look-ahead bias**: the strategy must only ever see data up to and including the
  current candle close at each step — validate this with a dedicated test that fails if
  future data leaks into a decision.

## Fill assumptions (Phase 3, keep simple; refine later if needed)
- Market orders fill at next candle's open (or a configurable slippage offset).
- Stops/targets fill at the touched price within the candle (document the simplification
  explicitly in the report output so results aren't over-trusted).

## Performance report
```rust
pub struct BacktestReport {
    pub trades: Vec<TradeRecord>,
    pub total_trades: u32,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub net_return_pct: f64,
    pub max_drawdown_pct: f64,
    pub sharpe_ratio: f64,
    pub average_r: f64,
    pub best_timeframe: Option<String>,
    pub worst_regime: Option<String>,
}
```
Each `TradeRecord` retains enough detail (entry/exit time & price, R multiple, which
DSL conditions fired) that a later "why did it lose money in June?" investigation
(agent-driven, see `docs/09-AI-AGENT-SYSTEM.md`) can inspect actual trades rather than
just aggregate stats.

## Parallelization
- Backtests across multiple symbols/timeframes/parameter sets should run in parallel
  (e.g. via `rayon` or spawned tokio blocking tasks) — this is one of Rust's clearest
  wins here per the source research. Design the replay function to be trivially
  shardable per (symbol, date-range) unit of work.

## Parameter optimization / walk-forward (later refinement, stub the interface now)
```rust
pub trait ParameterSweep {
    fn candidate_documents(&self, base: &StrategyDocument) -> Vec<StrategyDocument>;
}
```
Leave the concrete grid/walk-forward search algorithm for a later iteration once basic
single-run backtesting is solid — but define the trait now so it slots in without a
redesign.

## CLI
```
strategy-cli backtest run \
  --strategy strategies/liquidity-sweep.yaml \
  --symbol BTCUSDT --from 2024-01-01 --to 2024-07-01 \
  --report-out reports/liquidity-sweep-2024h1.json
```

## Done criteria
- Sample strategy backtested over ≥6 months of real BTCUSDT data; a handful of trades
  spot-checked by hand against the raw candle data confirm entries/exits are correct.
- No-look-ahead test passes (a strategy that "cheats" by reading future data is
  detectable and the harness catches it).
- Backtests for independent symbol/date shards run concurrently without shared mutable
  state issues (verified with `cargo test` under `--test-threads` stress or a loom-style
  concurrency test if warranted).
