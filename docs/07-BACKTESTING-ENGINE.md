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
    pub equity_curve: Vec<f64>,
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

The same binary carries the check that answers the first done criterion below:

```
strategy-cli verify --report reports/liquidity-sweep-2024h1.json --trades 40
strategy-cli verify --symbol BTCUSDT          # the newest stored run instead of a file
```

`verify` re-derives each recorded trade from sources the backtester did not produce. Ten
checks per trade, four of which read the `candles` table — written by the collector and the
backfill, never by the backtester. It exits non-zero if any checked trade fails, so it can
gate a release.

What it deliberately is **not**: `tests/replay_golden.rs` compares against a fixture
generated from this implementation, so it detects a *changed* implementation and can never
detect a wrong one. `verify` asks the opposite question — given the trades that were
recorded, does the market data actually support them? Results in
`reports/phase3-spot-checks.md`.

One trap worth knowing: `BacktestReport::decision_timeframe` holds the **declared name**
(`"entry"`), not a resolution, so `verify` discovers the resolution from the candle table
rather than reading it off the report. Reading that field as a timeframe fails with
`unknown timeframe 'entry'`.

## Done criteria
- Sample strategy backtested over ≥6 months of real BTCUSDT data; a handful of trades
  spot-checked by hand against the raw candle data confirm entries/exits are correct.
  **Done, and mechanised.** The phrase "spot-checked by hand" had no record anywhere in the
  repository until 2026-09-17; `strategy-cli verify` is that spot check as a tool.
  `reports/phase3-spot-checks.md` records 40 of 219 trades passing all ten checks, and —
  the part that makes it evidence — the checks shown to **fail** against six single-field
  corruptions of the report.
- No-look-ahead test passes (a strategy that "cheats" by reading future data is
  detectable and the harness catches it).
- Backtests for independent symbol/date shards run concurrently without shared mutable
  state issues (verified with `cargo test` under `--test-threads` stress or a loom-style
  concurrency test if warranted).

## Implementation notes (Phase 3)

### Results are in R multiples, not currency
1R is the risk a trade accepted at entry. Phase 3 deliberately does **not** compound:
position size is not recomputed from a growing or shrinking balance, so a run of wins
cannot inflate the size of the next trade and flatter the equity curve. `net_return_pct`
is derived from summed R, and `FillAssumptions.return_units` says so in the report output
itself — the numbers are never separated from the assumptions that produced them.

### The equity curve is kept, and it is in R
`equity_curve` is the cumulative R after each trade, in trade order, starting from flat —
one point per trade, and empty when nothing traded. Because results do not compound it is
a random walk in R, not an account balance, and the field is named after what it is rather
than after what a dashboard might like it to be.

It is stored rather than recomputed on read for the reason the whole report is stored: a
backtest is an observation made at a moment, and the candles underneath it change. The
series is also what max drawdown is measured over — both walk `cumulative_r`, because two
walks of the same trades is two chances to disagree.

`#[serde(default)]` on the field is load-bearing, not defensive. `backtests.report` is
JSONB, so every run stored before the field existed is a document without it, and without
the attribute reading one back would fail outright
(`a_report_stored_before_the_curve_existed_still_loads`).

### The ambiguous bar resolves against the trade
When a single candle's range touches both the stop and the target, the simulator assumes
the **stop** was hit. Intrabar ordering is unknowable from OHLC data, and the other choice
would systematically flatter results. `FillAssumptions.ambiguous_bar` names the assumption
so a reader never has to read the simulator to find it.

### Fills
Entries and condition-driven exits fill at the **next decision candle's open**, adjusted by
slippage *against* the trade. Stop and target fills occur at the touched level, with no
slippage — the simplification the design section above asks to be documented, and it is, in
`FillAssumptions`.

### No look-ahead is structural, not careful
`replay` is the only component that decides visibility, and it exposes a candle to the
strategy only once `open_time + width <= now`. `MarketContext` has no API that can reach a
future bar, so the invariant is a property of the types rather than of reviewer attention.
The dedicated test asserts it on **every** bar of a run rather than spot-checking a few.

### Shardability
`replay` takes its input and returns its output, holding no shared mutable state, so
independent `(symbol, date-range)` shards run concurrently. A test runs four shards at once
and asserts each is byte-identical to running alone.

### A declared timeframe with no data is an error
Rather than leaving that timeframe's context permanently absent — which would silently
disable a condition and yield a plausible-looking but wrong backtest — `replay` rejects the
input up front. The decision timeframe with no candles is `MissingData`; a context
timeframe is `MissingTimeframe(name)`.

### Aggregating a coarser timeframe from a finer one
A backfill normally populates a single resolution. `analytics-core::resample` aggregates
OHLCV **exactly** — high/low are extrema, and volume, `buy_volume` and `sell_volume` are
sums — so a multi-timeframe document can run off one source series without the coarser
candles disagreeing with the finer ones about volume or delta. The CLI does this
automatically for any declared timeframe that has no candles of its own
(`--source-timeframe`, default `1m`), padding the source window so the first and last
bucket of every resampled series are whole.
