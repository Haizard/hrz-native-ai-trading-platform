# Multi-symbol verification — 2026-09-18

## The question

The platform's stated goal is **many symbols across several markets**, and the whole
"do not persist market data" design was sized against that goal: ~1 MB of candle buffer
and ~6.4 MB of trade tape per symbol, against a database with 6 GB total for everything.

Every number in that arithmetic was derived, and **none of it had ever been observed** —
every run of this platform before today used the single default symbol, `BTCUSDT`. A
per-symbol budget that has only ever been exercised at N=1 has not been tested at the one
thing it is a budget *for*.

This is a bounded observation of N=3, recorded so the claim has a witness and so the
limit of the witness is on the record too.

## How

```
BIND_ADDR=127.0.0.1:8099 MARKET_SYMBOLS=BTCUSDT,ETHUSDT,SOLUSDT RUST_LOG=info ./target/debug/api-gateway.exe
```

No database was required for any of it: the market path reads RAM and the venue's REST
API. (`DATABASE_URL` was set from `.env.local` and connected in 5.18s — the free tier is
slow to hand out a connection, and that delay sits in front of boot. It is not on the
market path.)

## What was observed

**Feeds.** All three connected, each on its own collector:

```
INFO api_gateway::bots: market feed connected symbol="BTCUSDT"
INFO api_gateway::bots: market feed connected symbol="ETHUSDT"
INFO api_gateway::bots: market feed connected symbol="SOLUSDT"
```

**Order books.** All three bridged onto their snapshots — the failure `docs/19` row 24
was about, now observed across symbols rather than on one:

```
INFO market_data::exchanges::binance: order book synced symbol="SOLUSDT" diffs=56
INFO market_data::exchanges::binance: order book synced symbol="BTCUSDT" diffs=73
INFO market_data::exchanges::binance: order book synced symbol="ETHUSDT" diffs=73
```

`GET /orderbook` for each: 50 bids / 50 asks, spreads `0.0100`, `0.0100`, `0.0100`.

**Book liveness, per symbol.** The metric added for row 24 labels correctly across
symbols rather than collapsing to one series:

```
market_data_book_age_seconds{symbol="BTCUSDT"} 0.9040845
market_data_book_age_seconds{symbol="ETHUSDT"} 0.9360353
market_data_book_age_seconds{symbol="SOLUSDT"} 0.7324078
```

**Charts.** `GET /candles?timeframe=5m&limit=200`, one request per symbol. 201 bars each,
2 from memory (the forming bar and one closed) and 199 fetched from the venue on demand,
with real prices:

| Symbol | Bars | `source.memory` | `source.venue` | Last close |
|---|---|---|---|---|
| BTCUSDT | 201 | 2 | 199 | 80906.6 |
| ETHUSDT | 201 | 2 | 199 | 2594.36 |
| SOLUSDT | 201 | 2 | 199 | 111.44 |

**Tapes.** `GET /footprint/coverage`: 5309 / 5579 / 1283 trades, after ~85 seconds live.

**Buffer registration.** 18 series — 3 symbols × 6 resolutions — all reported by
`market_data_history_bars` with correct per-symbol and per-timeframe labels. Every one
read 0, which is correct: in 85 seconds no `1m` bar had closed yet, and a closed bar is
the only thing the buffer counts.

**Memory.** RSS **21,268 K (~21 MB)** with three feeds open, three books cached and
~12,000 trades on the tapes.

## What that means for the budget

The tapes were ~4% full. At their 100,000-trade ceiling the three symbols would cost
roughly 6.4 MB each, so a fair steady-state estimate is **~8 MB per symbol** including
the candle buffer, or **~450–500 MB at 60 symbols**. That is affordable in a container and
it is not free — it is the number to plan the deployment around, and it is RAM rather than
the 6 GB of disk, which is the trade that was made on purpose.

Scaling note, and it is a design fact rather than a defect: **one socket per symbol.**
`ensure_feed_for` spawns a `BinanceCollector` per symbol, and the combined `/stream`
endpoint is used *within* a symbol to carry trades and depth together, not *across*
symbols. Sixty symbols is sixty sockets. That is within Binance's limits, and it is the
thing to revisit first if the symbol count grows a lot.

## What this does not prove

- **Three symbols, not sixty.** The extrapolations above are arithmetic again — the same
  kind of number this report was written to replace. Sixty symbols has not been run.
- **85 seconds, not a soak.** Nothing here says the buffers, tapes or sockets are stable
  over hours; `docs/19` row 15 is the same caution about a different criterion.
- **One market.** All three symbols are Binance crypto pairs. The forex case — a second
  venue behind the same `ExchangeCollector` trait, with its own REST shape for history —
  has not been attempted at all, and `BackfillClient` still speaks Binance's
  klines/aggTrades shape regardless of what `MARKET_REST_URL` points at.
- **A debug build.** RSS was read from `target/debug`, not a release binary.
