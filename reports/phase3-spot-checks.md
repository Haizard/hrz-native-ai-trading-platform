# Phase 3 spot checks — independent verification of backtest trades

Date: 2026-09-17. Data: the real BTCUSDT 5m candle table in the managed Postgres
instance (53,172 bars) and the stored Phase 3 run in
`reports/liquidity-sweep-btcusdt-5m-2026h1.json`. Nothing here is mocked.

## The criterion this answers

`02-ROADMAP.md:112` closes Phase 3 partly on "manually-verified spot checks" of the
backtester's output. Until now that phrase appeared in the roadmap and nowhere else in
the repository — no record, no procedure, no result. `docs/19` row 13.

## Why the existing test could not stand in

`crates/backtester/tests/replay_golden.rs` compares against a fixture that was
**generated from this implementation**. Its own header says so. A golden file detects a
*changed* implementation; it can never detect a wrong one, because the thing under test
told it what the answer should be.

So this check asks the opposite question: **given the trades the backtester recorded,
does the market data actually support them?** The candle table is produced by the
collector and the backfill, not by the backtester, so it is genuinely external.

## What was run

```
strategy-cli verify --report reports/liquidity-sweep-btcusdt-5m-2026h1.json --trades 40
```

Run: `Liquidity Sweep + Reclaim (BTCUSDT 5m) v1.0`, window 2026-03-13 .. 2026-09-15,
219 completed trades, reported win rate 0.2694, PF 0.7346, average R −0.2280, slippage
assumption 2 bps.

The resolution was not supplied. The stored report names its decision timeframe only as
`"entry"`, which is a *declared name* and not a resolution, so `verify` derived `5m` from
the candle table: the resolutions with a bar opening at the probe trade's entry instant,
narrowed by which one divides every trade's holding period exactly.

**Result: 40 of 219 trades checked (18.3% of the run), 40 passed, 0 failed.**

An earlier run at `--trades 25` also passed 25/25. The sample is spread across the run,
first trade and last trade included.

## What each check asserts

| Check | Claim | Source of truth |
|---|---|---|
| stop side | the stop is below the reference price on a long, above it on a short | the record |
| risk per unit | `risk_per_unit == abs(reference_price − stop_price)` | two record fields |
| target side | the target is beyond the reference price in the trade's direction | the record |
| R multiple | `r == signed(exit − entry) / risk_per_unit` | three record fields |
| bars held | `bars_held == (exit_time − entry_time) / bar length` | record vs timestamps |
| window | both timestamps fall inside the run's own window | the record |
| entry fill | `entry_price == candle(entry_time).open` moved against the trade by the recorded slippage | **the candle table** |
| no look-ahead | the bar *before* the entry bar closed at `reference_price` | **the candle table** |
| exit level | the bar at `exit_time − one bar` genuinely touched the level the trigger names | **the candle table** |
| exit fill | a stop or target exit filled exactly at its level, per the documented simplification | the record |

The last four are the ones that matter: they are the only checks that use data the
backtester did not produce.

## The checks were shown to fail

A check that has never failed is not evidence. Six single-field corruptions were applied
to a copy of the report — each one to trade #9, which is inside the sample — and every one
was caught with a non-zero exit and a message naming the field:

| Corruption | Caught by |
|---|---|
| `r_multiple` + 0.5 | r multiple — *"recorded −0.7207 but long at 66665.07 to 66591.33 over risk 60.41 implies −1.2207"* |
| `exit_price` − 500 | r multiple, exit fill |
| `stop_price` moved above the reference | stop side, risk per unit, exit fill |
| `entry_price` + 50 | entry fill — *"bar 1775186100000000000 opened at 66651.74 and 2 bps against the trade gives 66665.07"* |
| `bars_held` + 3 | bars held — *"recorded 8 bars but the timestamps span 5"* |
| `exit_time` + 7 ns | bars held, exit bar — *"not a whole number of M5 bars"* |

The last one initially failed for the wrong reason: timeframe discovery filtered the only
candidate resolution out and reported *"no resolution explains the holding period"*,
which named the wrong fault and skipped the per-trade check that would have named the
right one. Discovery now defers to the per-trade check when only one resolution is
possible. That is recorded here because it is the kind of thing a verification record is
for — the first version of the checker was wrong about what it had found.

## What this does and does not establish

**Establishes.** For 40 of 219 trades in this run: the recorded trade is internally
consistent, the fill is the bar's open moved by the recorded slippage, the decision was
made on a *closed* bar rather than the entry bar's own close, and the level the trade
says it exited at was actually reached in the stored candles.

**Does not establish.**

- That the strategy is profitable, or that −0.2280 average R is the right number. The
  strategy loses money, and nothing here changes that.
- That the fills are realistic. The model is documented and deliberately simple — entries
  at the next open with 2 bps against the trade, stops and targets at the touched level
  with no slippage beyond it, a bar touching both resolved to the stop. The checks verify
  that the model was applied, not that the model is right.
- That the other 179 trades are clean. Every check is a property of one trade, so a clean
  sample is evidence and not proof.
- Anything about look-ahead in the *strategy* — only that the recorded reference price is
  the previous bar's close, which is the mechanism by which look-ahead would show up.
- Anything about the paper or live paths. This is the backtester only.

## Reproducing

```
cargo run -p strategy-cli -- verify \
  --report reports/liquidity-sweep-btcusdt-5m-2026h1.json --trades 40
```

Exit code 0 means every checked trade passed; 1 means at least one did not. `--symbol
BTCUSDT` reads the newest *stored* run instead of a file, and `--timeframe 5m` skips the
resolution discovery.

Note on the stored runs: at the time of writing, every `backtests` row for BTCUSDT has an
empty trade list. The 219-trade run this record is about was written to a file by
`strategy-cli backtest run --report-out` and was never stored. That is why `verify` reads
files as well as rows, and it is why the criterion is answered against the file.
