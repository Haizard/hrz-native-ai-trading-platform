# Phase 6 — Paper Trading: verification record

What `docs/11-BOT-TRADING-ENGINE.md`, `docs/15-RISK-COMPLIANCE.md` and the
Phase 6 roadmap section asked for, and what is actually true of the code in this
commit. Written after the fact from real runs, not from intent.

---

## The done criteria, one at a time

| Criterion (docs/11) | Status | Evidence |
|---|---|---|
| Subscribes to live `MarketState` for the declared symbol/timeframes | **verified for 2 minutes** | `paper-cli live` connected to the Binance trade feed, built `[M5, H4]` from it and made a real decision on a closed 5m candle. The 48h observation is the part that has not happened. |
| Feeds each closed candle into the strategy *exactly as the backtester does* | **verified** | `paper-cli run` diffs the bot against a replay of the same candles: **MATCH on all 219 trades** over 53,172 decision candles of real BTCUSDT data. |
| Simulated order/position/PnL with a configurable slippage/fee model | **verified** | The fill model is `strategy_runtime::simulator`, the same code the backtester uses. |
| Persists every trade **and** every `on_candle` decision, including "no signal" | **built, not yet run against the database** | `BotSession` writes `trades_executed` and `audit_log` (`bot.decision`, `bot.risk_breach`). `--persist` is off by default and has not been exercised against production Postgres. |
| Risk engine always active: per-trade cap clamped to the platform ceiling | **verified** | `a_twenty_percent_strategy_is_clamped_to_the_platform_ceiling`, `a_document_asking_for_more_than_the_ceiling_is_refused_not_traded`, `a_strategy_asking_for_more_than_the_ceiling_is_refused`. |
| Daily/weekly loss limits | **verified** | `a_daily_loss_breach_trips_the_kill_switch`, `a_weekly_breach_trips_even_when_no_single_day_does`, `a_gap_in_candles_does_not_carry_an_old_loss_into_today`. |
| Max concurrent positions | **verified** | `a_second_position_is_refused_at_the_concurrency_limit`. |
| Manual + automatic kill-switch | **verified** | `a_manual_kill_is_reported_and_holds`, `a_manual_kill_needs_no_market_data_to_work`. |
| Kill-switch works while the agent/market-data is degraded | **verified by construction** | `RiskEngine` has no I/O, no async and no clock of its own. There is nothing to degrade. |
| **Tight limit → kill-switch fires** | **verified on real data** | Below. |
| **≥48h continuous run** | **not done** | The runner exists and builds. The observation has not been made. |

---

## The exit criterion, run for real

`docs/11`: *"produces simulated trades consistent with what a manual replay of
the same period through the backtester would produce"*.

```
$ paper-cli run --strategy strategies/liquidity-sweep-btcusdt-5m.yaml       --symbol BTCUSDT --from 2026-03-13 --to 2026-09-13       --daily-loss-limit-r 100000 --weekly-loss-limit-r 100000

loaded entry: 53172 candles
loaded trend: 1110 candles

paper bot:  53172 decisions, 219 trades
backtester: 53172 decisions, 219 trades
context candles delivered: 1107

MATCH: the paper bot and the replay agree on every trade.
bot cumulative R: -49.9317   replay final R: -49.9317
```

Every one of the 219 trades has the same `entry_time`, `entry_price`,
`exit_time`, `exit_price` and `r_multiple` on both paths, to 1e-9. Six months of
real BTCUSDT 5m candles, the reference strategy, and the two paths agree
exactly -- including the cumulative -49.93R, which is also what the committed
Phase 3 baseline reports.

Risk limits were widened for this run on purpose: a limit that refuses an entry
is a *legitimate* reason for the two paths to differ, so widening them means the
comparison tests the execution path rather than the limits.

### The same strategy with the limits in force

```
$ paper-cli run --strategy strategies/liquidity-sweep-btcusdt-5m.yaml       --symbol BTCUSDT --from 2026-03-13 --to 2026-09-13
                                    # defaults: 3R daily, 8R weekly

paper bot:  53172 decisions, 5 trades
backtester: 53172 decisions, 219 trades

bot cumulative R: 1.0576   replay final R: -49.9317
kill-switch engaged: daily loss limit breached: 3.51R of 3.00R
```

This is the second half of the done criterion -- *"verified by deliberately
configuring a tight limit and confirming the kill-switch fires"* -- and it is the
clearest argument for having a risk engine at all. The unbounded strategy loses
**49.93R** over six months. With the default 3R daily budget the bot stops after
five trades, having lost 3.51R in a day, and finishes **+1.06R**.

The limit did not make the strategy better. It stopped it.

---

## A real bug this found

The first run of `paper-cli run` reported:

```
  - trade 2: the bot entered at 77741.535198 (1789133100000000000)
             but the replay at 77725.131918 (1789132800000000000)
```

One 5m bar late. The cause was in `PaperBot::decide`, which returned early
after a fill or a close instead of asking the strategy on that bar — while the
replay always asks. The bot still traded, so nothing looked broken; it was
simply always one bar behind the setup. Exactly the class of silent drift that
sharing one windowing implementation is meant to prevent, and it survived the
unit tests because no test compared the two paths.

Fixed, and pinned by `the_bot_asks_the_strategy_on_every_decision_bar`, which
asserts the exact entry times and prices of the fixture's fills.

---

## Refactor: one implementation, not two that agree today

`docs/03` forbids `trading-engine` from depending on `backtester`, and
`docs/11` requires the two to execute identically. Both hold only if the shared
parts live below both crates, so they moved into `strategy-runtime`:

- `rolling` — the per-timeframe window and the visibility rule.
  `backtester::replay` now drives it instead of its own `TimeframeCache`.
- `simulator` — the fill model, with `TradeRecord` and `FillAssumptions`.
  `backtester` re-exports all of it, so no downstream call site changed.

**This is proven equivalent on real data, not just on a fixture.**

1. `crates/backtester/tests/replay_golden.rs` replays a synthetic 5m/1h document
   and pins every number each trade carries. It was run against the
   pre-refactor implementation and the post-refactor one and produced identical
   values to 1e-9.
2. Re-running the committed Phase 3 backtest -- 53,172 real BTCUSDT 5m candles,
   the reference strategy -- reproduces
   `reports/liquidity-sweep-btcusdt-5m-2026h1.json` **exactly**:

   ```
   summary fields: 18 | differing: NONE
   trades: 219 219 | field differences: 0
   ```

### One rule the ladder had to get right

`RollingLadder::context` requires only the **decision** frame to be warm. A
coarser frame that has not closed yet is omitted, and the engine already treats
a condition on a missing view as not fired. Gating on all frames would have
looked safer and been a second, subtler rule — `any_of` would lose its chance to
fire on the frames that *are* ready — and the replay and the live bot would each
have needed the same exception to stay in agreement.

---

## The data gap, and how it was closed

At the start of this phase the `candles` table held two days of 1m data, six
months of 5m, and **no 4h or 1h at all**:

```
  1m     2880  2026-09-10 00:00 .. 2026-09-11 23:59
  5m    53172  2026-03-13 00:00 .. 2026-09-13 14:55
```

The reference strategy declares `trend: 4h`, so that series was rebuilt from 1m
-- twelve candles covering two days of a six-month window. The 4h view was cold
for 99% of the run, `market_structure.trend == "bullish"` was false everywhere
it mattered, and the six-month backtest reported **0 trades**.

That was a data problem, not a regression, and the two checks above are what
establish it rather than a hopeful re-run. It is now fixed by backfilling from
the exchange:

```
$ xtask backfill --symbol BTCUSDT --timeframe 4h --from 2026-03-13 --to 2026-09-14
inserted 1110 candles (upsert)
$ xtask backfill --symbol BTCUSDT --timeframe 1h --from 2026-03-13 --to 2026-09-14
inserted 4440 candles (upsert)

  1h     4440  2026-03-13 00:00 .. 2026-09-13 23:00
  4h     1110  2026-03-13 00:00 .. 2026-09-13 20:00
```

The silent version of the trap is now loud: a stored series is only trusted if
it spans at least `MIN_COVERAGE` (90%) of the window, otherwise it is resampled,
and any series -- direct *or* resampled -- that still falls short logs a warning
saying a low trade count means missing data, not a selective strategy.

1m still covers only two days, so anything that resamples *from* 1m is still
limited to that window. That does not matter for these strategies, whose finest
declared timeframe is 5m, but it would for a 1m document.

---

## Suite state

```
cargo fmt --all --check                       clean
cargo clippy --workspace --all-targets        clean
cargo test --workspace                        544 passed, 0 failed
```

New tests this phase: 16 in `trading-engine::risk`, 12 in
`trading-engine::paper`, 4 in `trading-engine::store`, 2 in
`backtester::replay_golden`, 5 in `strategy-runtime::rolling`.

---

## Not verified

- **The 48h run.** `paper-cli live` has been verified end to end -- it connected,
  built `[M5, H4]` from the live trade feed and decided on a closed candle:

  ```
  $ paper-cli live --strategy strategies/liquidity-sweep-btcusdt-5m.yaml         --symbol BTCUSDT --minutes 2 --flush-secs 20
  paper bot live on BTCUSDT ([M5, H4]); 2 minutes
  trades=0 decisions_pending=1 cumulative_r=0.0000
  reached the configured run length
  ```

  What has not happened is leaving it running for 48 hours, which is a
  scheduling matter rather than missing code:

  ```
  paper-cli live --strategy strategies/liquidity-sweep-btcusdt-5m.yaml       --symbol BTCUSDT --minutes 2880
  ```
- **Persistence against production Postgres.** `BotSession`, `db::paper` and
  the `--persist` flag are written but have never been executed. Nothing has
  been written to the `bots`, `trades_executed` or `audit_log` tables. A run
  needs a real `users` row (`--user-email`); the code deliberately will not
  invent one.
- **Partial fills.** `docs/11` marks them optional at this stage; they are not
  modelled.
- **Notifications.** `docs/11` asks for an in-app notification on a breach. The
  audit row is written; nothing pushes it anywhere yet.
- **Live trading (Phase 8).** `ExchangeAdapter`, idempotent order IDs and
  reconciliation are untouched, as intended.
