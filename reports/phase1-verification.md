# Phase 1 — Market data: verification record

Date: 2026-09-17. Symbol: BTCUSDT, against the live Binance feed and the managed
Postgres instance. Nothing here is mocked.

## The criteria this answers

`docs/04-MARKET-DATA-ENGINE.md:95-97`:

> - 24h continuous run against BTCUSDT with zero unhandled panics.
> - Backfilled and live-collected candles for the same historical window are identical.
> - Health endpoint accurately reflects induced disconnects during a chaos test.

Until now **no run of any length had ever been recorded against the first two**, and the
second could not have passed: the collector did not write candles at all. `docs/19` row 13.

## Row 12 had to be fixed first, or this record would be meaningless

`tools/xtask/src/main.rs`'s `collect()` spawned two pumps — `pump_trades` and
`pump_books` — and `repositories::insert_candles` had exactly one caller repo-wide:
`xtask backfill`. The aggregator built all six resolutions and published them to the
internal bus, and nothing outside `market-data` could reach that bus, because
`ExchangeCollector` had no `candle_stream` method. A 24h collector run added **zero**
candles, so a 24h soak would have proved only that a process stayed up.

The fix is a `candle_stream` trait method, a third pump in `collect()`, and a
successes-only counter. `reports/` cannot verify a criterion that the code cannot meet,
so this record starts with the code.

## The run

```
xtask collect --symbol BTCUSDT        # persisting; bounded by Ctrl-C, not by a flag
```

| | |
|---|---|
| Started | 2026-09-17 07:14:58 UTC |
| Stopped | 2026-09-17 08:06:02 UTC |
| **Duration** | **51 minutes 4 seconds** |
| Panics | **0** |
| Gaps | **0** |
| Reconnects | **0** |
| Health samples | 103, one every 30s, `connected=true` in every one |
| Messages consumed | 70,582 |

The stop was a `taskkill` terminate rather than a Ctrl-C, so the process's own
`shutting down` and "N candles written this run" lines never printed. That is a gap in
*this record*, not in the collector: the run counter is read from the last 30-second
health line, and the database count below is independent of it anyway.

## The candle table grew, and by how much

Row counts taken by a **separate process** (`psycopg`, outside the repository) rather than
from the collector's own counter — a counter inside the thing under test is the crate
agreeing with itself.

"Before" is a **point-in-time snapshot taken at 07:17:01 UTC**, filtered to
`open_time < 07:14:58Z`. The filter exists to strip the two 1m bars the run had already
written in its first two minutes; the rest of the baseline is the backfill's data, which no
longer changes. "After" is the whole table once the run had stopped.

That snapshot is point-in-time rather than window-based, and it has to be, for a reason the
next section explains: two of the bars this run wrote carry an `open_time` from **before**
the run started, so no window filter can both keep them and drop the run's first candles.

| timeframe | before | after | delta |
|---|---|---|---|
| 1m | 2,880 | 2,930 | **+50** |
| 5m | 53,172 | 53,182 | **+10** |
| 15m | 0 | 3 | **+3** |
| 1h | 4,440 | 4,441 | **+1** |
| 4h | 1,110 | 1,111 | **+1** |
| **total** | **61,602** | **61,667** | **+65** |

**Five resolutions were written: 1m, 5m, 15m, 1h and 4h.** The `15m` row did not exist
in the table before this run — the backfill never covered it — so that column went from
nothing to three bars.

The independent count (+65) and the collector's own counter (65, from its last health
line) agree exactly. They were produced by different processes reading different things,
which is the only reason the agreement means anything.

### The one trap in this measurement, written down because it caught me

A candle's `open_time` is the **start** of its bar, so a bar that *closes* during the run
carries an `open_time` from before the run began. The 1h bar the collector wrote opened at
07:00 and closed at 08:00; the 4h bar opened at 04:00 and closed at 08:00. A filter of
`open_time >= run_start` — the obvious way to ask "what did this run write" — returns
**zero** rows for both, and would have reported a run that wrote no hourly candle at all.

That is why the table above is a whole-table difference rather than a window query. The
first draft of this measurement used the window query and produced exactly that wrong
answer.

## The live candles match the venue's own published klines

`docs/04` requires candles to be built from the **trade stream**, not from exchange
klines. That makes Binance's own kline for the same minute an independent witness: two
different paths to the same number.

```
GET https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&startTime=...
```

Every candle the collector wrote for that window was compared field by field against the
venue's own candle for the same minute — `open`, `high`, `low`, `close`, `volume`,
`buy_volume` (Binance's `takerBuyBaseAssetVolume`) and `sell_volume` (`volume` minus it).

**Result: 50 of 50 compared candles matched on every field, 0 differed.** That is every
1m bar the run wrote, 07:15 through 08:04 — the whole run, not a sample.

This is the same check the second done criterion asks for, in a stronger form. The
criterion compares *backfilled* candles against *live-collected* ones, and both of those
are produced by this codebase; this compares live-collected candles against the venue's
independent record, which is not.

## What this does and does not establish

**Establishes.** A collector run persists candles into the same table `backfill` fills, at
five of the six resolutions the criterion names; it does so while holding a live connection
for 51 minutes with zero gaps and zero reconnects; and every candle it writes is numerically
identical to the venue's own published kline for the same minute.

**Does not establish.**

- **A 24h run.** **51 minutes elapsed, not 24 hours.** The run was bounded by how long it
  could be observed, not by the criterion. Wall clock cannot be compressed and the number
  recorded is the number that elapsed. This is the residual, and it is tracked in `docs/19`
  row 15 — closing row 13 needed the record to exist, and it does now; it did not need the
  record to overstate itself.
- **The `1d` resolution.** A daily candle closes at a UTC midnight, which a 07:15–08:06
  window does not contain. Five resolutions are observed; `1d` is inferred from the same
  aggregation path, and inferred is not observed.
- **Induced-disconnect behaviour.** The third criterion needs a chaos test, and the
  reconnect/gap counters read zero here because the connection was never dropped. Zero
  gaps during a clean run says nothing about a dirty one; `docs/04`'s chaos test and the
  `MD_*` metrics are what cover it.
- **Anything about the order book.** `pump_books` ran, but this record does not count or
  check what it wrote.

## Reproducing

```
# one process writes
./target/debug/xtask.exe collect --symbol BTCUSDT

# another, outside the repo, counts
python count_candles.py BTCUSDT        # the delta, per resolution
python soak_final.py BTCUSDT           # the same, with the open_time trap handled
python compare_live_vs_rest.py BTCUSDT 1m 2026-09-17T07:15:00Z
```

The raw console log is `reports/phase1-collector-soak.log`, which is git-ignored by
`.gitignore` the way `reports/paper-bot-*.log` is — the audit trail that matters is in
Postgres, and the log is mostly sqlx slow-statement warnings.
