The work is split across three phases:

- **Phase 1 — Market Data Engine**  
  Handles each symbol’s live and historical data: candles, trades, and order-book updates.  
  See `02-ROADMAP.md:60` and `docs/04-MARKET-DATA-ENGINE.md`.

- **Phase 2 — Analytics Core**  
  Converts raw symbol data into chart-ready analytics such as footprint, delta, CVD, VWAP, and volume profile.

- **Phase 7 — API Gateway & Frontend**  
  Owns the actual chart UI, symbol/timeframe selection, `/symbols` and `/candles` endpoints, and live WebSocket updates.  
  See `02-ROADMAP.md:186`, `docs/12-API-GATEWAY.md`, and `docs/14-FRONTEND-CHART-ENGINE.md`.

One nuance: a **minimal chart was pulled forward to the end of Phase 5** as the MVP checkpoint, but the full chart and multi-symbol interface belong to **Phase 7**.

So, in short: **symbol data starts in Phase 1; calculated chart data comes from Phase 2; chart display and symbol selection are Phase 7.**

now what about those engine that automatic calculate the chart sysmbol data

The calculation is not owned by one engine. It is a pipeline: market data builds each symbol’s stream, Analytics Core derives trading metrics, and the chart engine converts those results into drawable geometry. I’ll map the boundary precisely.
Here is the automatic calculation path for every selected symbol.
Yes. The system already has engines that **automatically calculate chart data for each symbol**.

### 1. Market Data Engine — Phase 1

For every configured live symbol, such as `BTCUSDT`, it automatically:

- Receives individual trades from Binance.
- Builds OHLCV candles.
- Calculates buy and sell volume.
- Produces six timeframes simultaneously:
  - `1m`
  - `5m`
  - `15m`
  - `1h`
  - `4h`
  - `1d`
- Keeps recent candles in RAM.
- Fetches older candles from the exchange when requested.

Opening a chart also calls `ensure_feed_for(symbol)`, so the feed can start automatically for that symbol.

### 2. Analytics Core — Phase 2

This is the main **automatic trading-calculation engine**. From the selected symbol’s candles and trades, it calculates:

- Delta and CVD
- VWAP
- Volume profile
- POC, VAH, and VAL
- Footprint cells
- Buy/sell imbalances
- Absorption
- Liquidity levels
- Swing highs and lows
- BOS and CHoCH
- Supply/demand zones
- EMA, SMA, RSI, and ATR
- Client-defined concepts

This logic is shared by the chart, AI agent, backtester, and trading bot. Therefore:

> What the chart displays should equal what the backtester tested and what the bot executes.

### 3. Chart Engine — Phase 7

The Rust/WASM chart engine takes the symbol data and automatically calculates:

- Visible price and time range
- Candle coordinates
- Price scaling
- Zoom and viewport
- Footprint cell sizes
- Volume-profile bar widths
- Indicator lines and labels
- Zone rectangles
- Drawing positions

It returns a complete scene to the browser. JavaScript only paints that scene; it does **not** calculate trading data.

### When calculations happen

| Calculation | When it runs |
|---|---|
| Candle aggregation | Continuously as live trades arrive |
| Delta/CVD and live metrics | As new data is processed |
| Chart indicators and zones | When the chart is built or updated |
| Scale and viewport | Whenever the user zooms, resizes, or changes the window |
| Historical candles | Fetched when the selected window is older than RAM history |
| Symbol change | Starts/uses that symbol’s feed and rebuilds its chart |

So the direct answer is: **Phase 1 automatically constructs symbol data, Phase 2 automatically calculates its trading analytics, and Phase 7 automatically calculates and renders the visual chart.** The API Gateway transports the data but should not perform the trading mathematics.


now tell how are these  automatic trading-calculation engine going to work if we dont save chart sysmbols data cause at first we had ony one sysmbol which is btcusdt but our platform must go beyound one chart sysmbol and we dont have that kind of database to keep all those chart symbol from binance though we need its data to calculate  automatic trading-calculation engine of week chart data sysmbol for better chart analysis


You’ve found the important gap: “don’t persist market data” is workable, but only if every analysis request can reconstruct its required history. I’m checking whether this codebase currently supports a true weekly timeframe or merely the storage strategy around it.
The right model is not “save every symbol forever.” It is **fetch the requested window, calculate from that window, cache it briefly, then discard it**. But the current code has two real gaps that matter: weekly (`1w`) is not supported yet, and the AI analysis route still reads market data from Postgres instead of the RAM-plus-Binance path used by the chart.
You are right to question this. **The calculations need historical candles, but those candles do not need permanent database storage.** Binance can remain the source of truth.

## How it should work

When someone requests, for example, `ETHUSDT` on the weekly chart:

1. Check the bounded RAM cache for `ETHUSDT/1w`.
2. Fetch missing historical weekly candles from Binance REST.
3. Merge them with the current live/forming candle.
4. Give that temporary candle window to `analytics-core`.
5. Calculate VWAP, volume profile, market structure, zones, indicators, and other available analytics.
6. Return the calculated chart scene and AI analysis.
7. Retain only a bounded recent window in RAM; eventually evict inactive symbols.

A restart simply causes another Binance fetch. We lose cache, **not market history**, because Binance owns the historical source.

Binance officially supports `1w` candles and allows up to 1,000 candles per REST request. That is enough to retrieve the complete weekly history of most Binance symbols in one request.

## Multiple-symbol operating model

We should not maintain permanent live feeds for every Binance symbol.

| Symbol category | Data handling |
|---|---|
| Chart currently open | REST history + live WebSocket |
| Running trading bot | Permanent live feed while the bot runs |
| User watchlist | Optional lightweight candle-close monitoring |
| Inactive symbol | No WebSocket; retrieve through REST when requested |
| Market-wide scanner | Scheduled REST scan using daily/weekly candles |
| Recently accessed symbol | Bounded RAM cache with idle expiry |

The automatic engine therefore becomes **event-driven**, not “load every symbol forever”:

- User opens a chart → calculate that symbol.
- A weekly candle closes → recalculate weekly signals.
- A scanner cycle starts → fetch and process symbols through a rate-limited queue.
- A bot is running → continuously update only that bot’s required symbols.

## Weekly analysis is inexpensive

There are two possible meanings:

### A true `1w` chart

Only a few hundred candles are normally required. The engine can fetch them, calculate the analysis, and discard them without touching Postgres.

### One week of intraday history

- `5m`: 2,016 candles
- `15m`: 672 candles
- `1h`: 168 candles
- `1m`: 10,080 candles

The current route permits at most 5,000 venue candles per request window. Therefore, one week of `5m` data fits, while a full week of `1m` requires pagination or a narrower analysis window.

## Important limitation: footprint data

Weekly OHLCV-based calculations can be reconstructed from Binance candles:

- Price structure
- VWAP
- Candle-derived volume profile
- Zones and concepts
- EMA/SMA/RSI/ATR
- Approximate delta/CVD from taker-buy volume

But a **true historical footprint**, per-price imbalance, and absorption analysis require individual trades at each price. Binance weekly candles do not contain that detail.

Therefore:

- Recent active symbols can have real footprint analysis from the bounded live trade tape.
- Old weekly periods should report footprint analysis as unavailable.
- We must never fabricate a footprint by spreading candle volume across prices.

## What the project currently does

The chart data path is mostly correct already:

- `HistoryRegistry` keeps 1,500 bars per symbol/timeframe in RAM.
- Missing historical candles are fetched from Binance.
- `/candles` combines RAM and exchange data.
- Opening a symbol starts its live feed.
- The documentation estimates roughly **1 MB of RAM per symbol across the current six resolutions**.

However, I found three real gaps:

1. **No weekly timeframe yet**  
   `Timeframe` currently ends at `1d`; `1w` is missing.

2. **The AI engine still reads Postgres**  
   `api-gateway/src/market_data.rs` implements `DbMarketData`, and the agent routes use it. Consequently, the chart can obtain candles from RAM plus Binance while the AI analysis may fail because those candles were never persisted.

3. **Live symbol feeds are not evicted**  
   `ensure_feed()` starts one task per requested symbol and retains it. That is acceptable for a small watchlist, but not an unrestricted Binance symbol universe.

## Required correction

The next data slice should:

1. Add `Timeframe::W1` throughout the engine, API, strategy DSL, tests, and frontend.
2. Replace `DbMarketData` with an on-demand source using:
   - `HistoryRegistry`
   - `LiveRegistry`
   - `BackfillClient`
3. Make charts, AI analysis, backtests, and bots consume the same reconstructed candle window.
4. Add an idle timeout and maximum active-feed count.
5. Keep `MARKET_SYMBOLS` as the configured watchlist, while allowing validated Binance symbols to be opened on demand.
6. Add rate-limited request coalescing so multiple users requesting `ETHUSDT/1w` share one Binance fetch.

So the architecture is valid without a large database—but **the current implementation is not completely connected yet**. The chart follows the correct RAM-plus-Binance model; the AI analysis adapter and weekly timeframe still need to be brought onto that same path.


now tell is there something we can before we close the gape cause my platform i primary goal is to be advanced in chart and analysis where ai is native coperative in the platform so customer can use the platform without boundary on doing anaysis

There is a practical bridge, but I would not pretend it delivers “analysis without boundaries.” It can give customers broad multi-symbol analysis now while we repair the underlying data path properly.
Yes. We can make useful progress **before the full data gap is closed**, but we must separate a temporary operating bridge from the final architecture.

## What customers can use immediately

### 1. Expand beyond BTCUSDT through a controlled watchlist

Configure several high-priority symbols through `MARKET_SYMBOLS`, for example:

```env
MARKET_SYMBOLS=BTCUSDT,ETHUSDT,BNBUSDT,SOLUSDT,XRPUSDT
MARKET_FEED=binance
```

For charting, the existing system can:

- List these symbols.
- Fetch historical candles from Binance when selected.
- Start a live feed when a chart opens.
- Keep recent candles in RAM.
- Run chart-side analytics such as VWAP, volume profile, zones, and concepts.

This already permits multi-symbol chart exploration without permanently storing all Binance data.

### 2. Use the existing higher-timeframe ladder

Until native `1w` support is added, AI analysis can use:

- `1d` for the broad trend.
- `4h` for structure and major levels.
- `1h` for setup development.
- `15m` or `5m` for confirmation.

This is useful, but it must be labelled honestly as **daily multi-timeframe analysis**, not weekly analysis.

### 3. Temporarily warm only selected AI-analysis windows

The current AI adapter still reads candles from Postgres. As a short-lived bridge, we can backfill only small candle windows for approved symbols:

```bash
cargo xtask backfill --symbol ETHUSDT --timeframe 1d \
  --from 2025-01-01 --to 2026-09-19
```

Do the same only for the required timeframes and watchlist symbols.

Important constraints:

- Store candles only—not months of raw trades or order-book snapshots.
- Keep the symbol list small.
- Treat this as temporary.
- Do not build a scheduled permanent market-data warehouse around it.

This makes the current AI tools operational for more symbols while we replace their database dependency.

## Work we can safely build in parallel

These capabilities will remain valuable after the data source is fixed:

### Symbol discovery

Build a searchable symbol catalogue instead of hard-coded dropdown entries:

- Search Binance instruments.
- Validate whether a symbol exists.
- Show spot/futures market type.
- Display supported timeframes.
- Allow favourites and recent symbols.

The catalogue contains metadata, not historical market data, so it is cheap to store.

### Chart-to-AI context

Every AI question should automatically include:

```text
symbol
market
timeframe
visible candle range
selected chart region
enabled indicators
active strategy/concepts
data freshness
available analytical capabilities
```

That enables questions such as:

- “Analyse what I am currently viewing.”
- “Why is this zone important?”
- “Compare this structure with the daily chart.”
- “Build a strategy from the area I selected.”
- “Show where this thesis becomes invalid.”

The customer should not need to repeatedly explain which chart or timeframe they mean.

### Capability-aware analysis

Each response should say what data was actually available:

```json
{
  "candles": "available",
  "volume_profile": "candle_derived",
  "live_orderbook": "available",
  "historical_footprint": "unavailable",
  "forming_candle": true,
  "data_freshness_seconds": 2
}
```

This prevents the AI from claiming it checked footprint imbalance when only OHLCV candles were available.

### Evidence-based AI output

The AI should not perform trading arithmetic itself. It should coordinate deterministic Rust tools and return:

- The conclusion.
- Timeframes inspected.
- Exact levels and metrics supporting it.
- Conflicting evidence.
- Data limitations.
- Invalidation conditions.
- Suggested chart overlays.

That is the correct meaning of **AI-native cooperation**: AI controls the analysis workflow, while deterministic engines calculate the numbers.

### Request caching and deduplication

If ten users request `ETHUSDT/1d`, the platform should make one Binance request and share the result. Add:

- Cache keyed by exchange, symbol, timeframe, and range.
- Short timeframe-aware expiration.
- In-flight request coalescing.
- Rate-limit queue.
- Retry and backoff.
- Data-gap validation.

This is essential for scaling without owning a large market database.

## What should be built first

I recommend this order:

1. **Unified RAM + Binance data source for charts and AI.**
2. **Native `1w` timeframe end to end.**
3. **Maximum active-feed count and idle feed eviction.**
4. **Symbol search and validation beyond `MARKET_SYMBOLS`.**
5. **Chart-to-AI context packet.**
6. **Capability and freshness reporting.**
7. **Advanced AI analysis and chart overlays.**
8. **Market-wide scanners and additional exchanges.**

I would **not add many more indicators before steps 1–3**. More calculations on an incomplete or inconsistent data path would make the product look advanced while producing unreliable analysis.

## What “without boundary” should mean

It cannot safely mean unlimited simultaneous feeds or unlimited API requests. Binance and our server will always have rate and compute limits.

It should mean:

- Any valid supported symbol can be requested.
- Customers are not restricted to a hard-coded coin list.
- History is loaded automatically.
- AI understands the active chart without repeated prompting.
- Analysis expands to additional timeframes automatically.
- Infrastructure limits are handled transparently through caching, queuing, and eviction.
- Missing data reduces the feature set honestly instead of producing invented conclusions.

So yes—we can offer broader charting now through a watchlist and selective candle warm-up. But the **unified on-demand data source is the next foundational slice**. That is what turns the platform from a BTC demonstration into the advanced AI-native chart-analysis product you intend to build.



okey then implement all those gaps and this order:
Unified RAM + Binance data source for charts and AI.
Native 1w timeframe end to end.
Maximum active-feed count and idle feed eviction.
Symbol search and validation beyond MARKET_SYMBOLS.
Chart-to-AI context packet.
Capability and freshness reporting.
Advanced AI analysis and chart overlays.
Market-wide scanners and additional exchanges.
and add chart screen shoot feature so user can screen short the chart for mor ai chat analysis

---

# Implementation record — the eight gaps, as built

Written 2026-09-19 for the next coding agent. Every item below was implemented against
this repository's own rules, and each entry says **where the code is**, **what the
verification was**, and **what is genuinely still open**. Read `README.md` → `00`..`03` →
`docs/` first; `02-ROADMAP.md` is the checklist and this file is the work log.

## The constraint that shapes everything: 6 GB, and market data is never persisted

`docs/04` and `.workbuddy-ai/memory/MEMORY.md` are unambiguous. Recent bars live in
`market_data::history` (RAM, 1500 bars/series); recent trades in `market_data::tape`
(100,000/symbol); anything older is fetched from the venue's REST **on demand and then
dropped**. One symbol's trades are ~110 MB/day and the database is a free tier with 6 GB
for every symbol of every market. **Any change that adds a market-data write path is
wrong however natural it looks** — and two of the eight gaps below were designed entirely
around not adding one.

## 1. Unified RAM + Binance data source for charts and AI — DONE

`market_data::WindowService` (`crates/market-data/src/window.rs`) is the single place a
candle window is assembled. The chart route and the agent **both** read through it, over
the same `HistoryRegistry` the bots' feed fills.

Why it mattered: the chart merged RAM with the venue, while the agent read Postgres — so a
symbol the agent had no rows for answered "no data" at the same moment its chart drew
perfectly. One source of truth, or the two disagree in a way the user cannot see.

`crates/api-gateway/src/market_data.rs`; `docs/04-MARKET-DATA-ENGINE.md`.

## 2. Native `1w` timeframe end to end — DONE

`Timeframe::W1` exists in `analytics-core/src/types.rs` with `bucket_of` **Monday-aligned**
(`W1_MONDAY_OFFSET`). The calendar reasoning is worth keeping: the epoch began on a
**Thursday**, so the epoch's own week runs Thursday–Wednesday and the first Monday sits
*four days into* week 0. What makes the two agree is that a weekday repeats every seven
days — the first Monday is at offset 4, and every later week's Monday is at that same
offset 4, so every week bucket after 0 starts on a Monday.

Reaches the chart, the strategy DSL, the backtester and the venue on both transports
(`backfill.rs`'s `interval_for` and the kline stream). `Timeframe::ALL` was added this
session so callers *list* the ladder from the type instead of restating it — that is how a
"known timeframes" list goes stale the day a variant is added.

## 3. Maximum active-feed count and idle feed eviction — DONE

`BotSupervisor`'s feed map with an LRU eviction and an `AbortHandle` per feed
(`crates/api-gateway/src/bots.rs`). A feed that is not being read is closed rather than
left holding a socket.

## 4. Symbol search and validation beyond `MARKET_SYMBOLS` — DONE

`market_data::SymbolIndex` (`crates/market-data/src/symbols.rs`) holds the venue's
`exchangeInfo` (~2 MB) in RAM, refreshed on a 6-hour TTL, behind `GET /symbols/search` and
`GET /symbols/validate`.

`MARKET_SYMBOLS` is a **watchlist** — "keep these feeds warm" — and using it as the limit
of what exists was a real defect: a chart that could draw any Binance symbol refused one
nobody had listed, and the error named a variable the user had never heard of. The venue
is the authority; the watchlist is a warm-start hint.

## 5. Chart-to-AI context packet — DONE

The shell builds a packet describing the active pane — symbol, timeframe, the visible
window (`scene.from` / `scene.to`) and the candle summary — and sends it with the
question, so the model sees what the user sees without being told. `frontend/app/app.js`:
`chartPacket()`, `anchorPrice()`.

A real bug found here: the window check used **truthiness** on `scene.from`, and `0` is a
valid timestamp (the epoch), so the packet silently omitted the window for one value of a
legitimate input. Fixed with explicit `!== null && !== undefined`.

## 6. Capability and freshness reporting — DONE

`crates/api-gateway/src/capabilities.rs`, served at `GET /capabilities`.

Two ideas, kept deliberately separate:

- **Capability** is a *deployment* property — "is Bedrock configured", "is the DB
  reachable". It changes when the deployment changes, i.e. almost never.
- **Freshness** is a *moment* property — "how old is the newest bar for this symbol". It
  changes every second.

Merging them would make one field that is sometimes stale for one reason and sometimes for
another. `readiness` (`ready` / `not_configured` / `degraded`) is also separate from
`verified`, because proving Bedrock actually answers needs a **paid call on every poll** —
so a report may say "configured but not exercised" and mean it.

## 7. Advanced AI analysis and chart overlays — DONE

`crates/api-gateway/src/agent_routes.rs` for the analysis; `frontend/chart-engine/src/drawing.rs`
for the overlays.

### The rule this went to war with

`docs/14`: **the shell has no price scale.** Every price-to-pixel conversion lives in Rust,
because one calculation with one implementation cannot disagree with itself.

The shell already drew the AI's entry/stop/target — with **its own copy** of the scale
(`y = plot.y + h - …`). Two copies drift on every resize and zoom, so the stop-to-target
band sat a few pixels off the candles it described, which reads as *"the level moved"*
rather than *"the overlay is stale"*.

**The fix is the design:** the shell sends **prices**, the engine answers with **pixels**.

- `Overlay { price, label, role, band_to, filled }` — the request half.
- `OverlayRole { Entry, Stop, Target, Level, Other }` — a closed vocabulary **with
  `Other`**, because an answer that says "watch 101250 for a reclaim" is making a real
  claim about a real price and must not be dropped for not being an entry/stop/target.
- `SceneOverlay { y, price, label, role, band_y, filled }` — the response half. The shell
  strokes and computes **nothing**.
- A band is **one** overlay with a `band_to`, not two levels, so the engine orders the
  edges (a short's stop is above its entry) and the shell never guesses.

Z-order is a claim: overlays draw **above** derived geometry (profile, zones) and **below**
the user's drawings. A volume profile is a measurement, an answer's entry is an opinion,
and the user's own mark is the last word.

### Two real bugs, both invisible from the outside

1. **`render()` had no `symbol` binding.** `thesis && thesis.symbol === symbol` — the only
   `symbol` in scope belonged to `loadCandles`, a *different function*. The `&&` guard hid
   it: with `thesis` null the reference was never evaluated, so it threw only once a thesis
   existed.
2. **`redrawThesis()` called `pane.draw()`, not `pane.redraw()`.** `draw()` repaints the
   cached scene and never re-asks the engine — correct when the shell mapped prices itself,
   wrong the moment the engine positions them.

Settled by **instrumenting, not reasoning**: a temporary `window.__probe` counter showed
`overlays: []` with matching symbols across 64 renders, which is what pointed at the
render path rather than the overlay logic.

Also: a `NaN` guard whose **reachability had to be checked**. `Overlay::validate` refuses a
non-finite price — but JSON has no `NaN` literal, and serde rejects `null` **before**
`validate` runs, so the wire route to that guard is closed. Pinned with
`a_null_price_never_reaches_the_validator`, because a guard that cannot fire is worse than
no guard.

**Verification:** `chart-engine` 143 lib tests; `tools/wasm_abi_check.mjs` checks the
prices-in / pixels-out contract *by writing the request in the shell's own spelling*
(that is the one place a rename is invisible — serde takes a `default`, so a misspelled key
is a level that never appears, not a compile error); `tools/shell_check.mjs` 132 checks.

## 8. Market-wide scanners and additional exchanges — DONE

### 8a. `market_data::scanner` + `GET /scan`

`crates/market-data/src/scanner.rs`, `crates/api-gateway/src/scan_routes.rs`,
`crates/api-gateway/tests/scan_flow.rs` (8 route tests).

Every other market endpoint answers about **one symbol you already chose**. A scan answers
the question that comes first: *which* symbol.

**It ranks and keeps nothing.** Symbols are visited behind a `Semaphore` permit **held for
the whole fetch**, so peak footprint is `concurrency * BARS_PER_SYMBOL` bars *regardless of
symbol count* — 300 symbols cost the same memory as 3. That is the only shape available
under the 6 GB rule: a scan that wrote its bars is an unbounded write path, one that kept
them is the history buffer's budget spent on symbols nobody is looking at. Reading through
`WindowService` means a symbol already on a chart is answered **from RAM** and spends no
REST budget — the venue's limit is per IP and shared with every interactive chart.

Design calls that are easy to get wrong later:

- A failure is a **row with a reason**, never a null and never a hole. "We could not
  measure this" and "this ranked last" are opposite claims.
- The summary distinguishes **"over the ceiling"** from **"see the failures for why"** —
  different problems, different fixes, so naming both when one applies sends the reader at
  the wrong limit.
- `total_cmp`, never `partial_cmp().unwrap()`. A sort is not worth a panic.
- The universe defaults to the **venue's** indexed trading instruments, not
  `MARKET_SYMBOLS` (see gap 4 for why that variable is not an authority).

### 8b. `Venue`: the adapter `docs/19` row 25 asked for

`crates/market-data/src/exchanges/venue.rs` (14 tests). The row's own words: *"a second
market needs a venue adapter and not just a URL."* `BackfillClient` now holds
`Arc<dyn Venue>` and reads through it; `new(url)` is kept as
`at_venue(BinanceVenue::at(url))` so every existing call site is unchanged.

Bybit v5 differs from Binance in five ways, **each of which fails silently**:

| | Binance | Bybit |
|---|---|---|
| Path | `/api/v3/klines` | `/v5/market/kline` |
| Interval | `1m`, `1h`, `1w` | `1`, `60`, `W` (minutes as integers) |
| Envelope | bare array | `{"result":{"list":[…]}}` |
| Row order | ascending | **descending** |
| Row width / types | 12 cols, mixed | 7 cols, **all strings** |

A wrong interval is a 400. A wrong **row order** is a chart drawn backwards. A wrong
**path** is an empty window reported as "the venue has no data". The last two read as
market conditions, which is why they are pinned by tests.

**A field a venue cannot supply is an `Option`, never a plausible default.**
`RawKline::taker_buy_base` is `Option<f64>` and **Bybit is `None`**: its seventh column is
*turnover*, a quote-asset figure. Filling it with `volume / 2` would make a volume-delta
indicator chart a flat line and a reader take "no imbalance" for a finding about the
market. `Candle`'s `buy_volume`/`sell_volume` are non-optional, so `to_candle`
direction-attributes on a `None` (up→buyers) and `has_order_flow_split()` is what a
consumer checks before drawing delta.

Two paging facts are **per-venue data, not assumptions**: `end_is_inclusive()` (Binance's
`endTime` is inclusive, hence the `-1ms`; applying it to a venue that does not need it
widens the window and double-counts) and `short_page_means_done()` (a *descending* venue's
short page means "clipped", not "history ran out" — stopping there truncates the old half
of the chart while looking healthy).

`window.rs::candle_from_kline` was collapsed into `RawKline::to_candle` rather than keeping
a second copy of the buy/sell arithmetic.

### 8d. The live half: a second `ExchangeCollector` — DONE 2026-09-20

`crates/market-data/src/exchanges/{codec,binance_codec,bybit_codec,collector}.rs` and
`crates/market-data/tests/bybit_wire.rs` (10 recorded-sample tests).

`docs/19` row 25 deferred this because it "cannot be exercised without a route to the
venue". That premise was **tested rather than repeated** and it was false — Bybit's REST
answered in 2.7 s and a hand-rolled WebSocket client captured real frames off
`stream.bybit.com`. The design is `WireCodec` beside `Venue`: a venue describes its socket
and decodes its own frames, and **one** `Collector<C>` owns the reconnect/backoff loop, the
bounded diff buffer, the trade-id gap detector, the candle fanout and `MD_CLOSE_LATENCY`.
A second `impl ExchangeCollector` would have duplicated every one of those, and each was
itself a previously-fixed defect.

Three of the four findings came from **capturing frames, not reading documentation**, and
all three would have passed a test written from the docs: there is no `u == 1` reset on
`orderbook.*`; a `publicTrade` frame is an array of 8–16 trades, not one; and `S` is the
*taker* side, so `is_buyer_maker` is an inversion. See `bybit_codec.rs`'s module doc for
the captured frames themselves.

Bybit carries its book **in-band**, so `subscribe_order_book` makes no REST call for it at
all — `BookBootstrap::InBand` exists so a venue that can sync on its first frame is not
made to poll. Witnessed live, both venues through the same collector: Bybit `order book
synced` after 29 diffs, 560 messages, 0 gaps, **zero REST requests**; Binance unchanged at
576 messages, 0 gaps, synced after 21 diffs.

### 8c. Chart screenshot to the AI — DONE

`frontend/app/app.js`: `captureChart()` (downscales to `MAX_SCREENSHOT_EDGE = 1600`, fills
`#0d1117` so the PNG is not transparent), `screenshotFromUrl()` (byte math
`(data.length * 3) / 4`, allowlist, `MAX_SCREENSHOT_BYTES = 4 * 1024 * 1024`), `paintAttach()`
and the `attachChart` toggle. Sent as a Bedrock Converse image block
(`{"image":{"format":..,"source":{"bytes":..}}}`).

## What is genuinely NOT done

- **A second live collector — built 2026-09-20, and the note that said it was not is left here
  because it was right for a year and wrong for a day.** It said the collector was not built
  "because it cannot be exercised without a route to the venue". That premise was *tested* rather
  than repeated on 2026-09-20: Bybit's REST answered in 2.7 s and a hand-rolled WebSocket client
  captured real `orderbook` and `publicTrade` frames from `stream.bybit.com`. The premise was
  false, so the work was done. The seam is `WireCodec` beside `Venue`, one `Collector<C>` holds
  the loop, and both venues now run through it —
  `./target/debug/xtask collect --venue bybit --symbol BTCUSDT --no-persist` logs
  `order book synced` after 29 diffs with zero REST requests. What that run **could not** have
  told you is the thing worth carrying forward: three of the defects were found by capturing
  frames, not by reading documentation — Bybit's documented `u == 1` reset boundary does not exist
  on `orderbook.*` (a fresh subscribe returns a full book at a real id such as `219167606`), a
  `publicTrade` frame carries 8–16 trades in an array, and `S` is the taker side. All three would
  have passed every unit test written from the docs.
- **`exchange_info` and `aggTrades` remain Binance-shaped on purpose.** They feed
  Binance-schema parsers (`SymbolIndex::install`, `AggTrade`), so swapping only the URL
  would hand those parsers a payload they would turn into nonsense. A second venue needs
  its own instrument endpoint *and* its own trade-history endpoint.
- **`tools/agent-cli/src/main.rs`** still carries a duplicate copy of `DbMarketData`
  (around lines 176–243).
- The scanner has **no shell surface yet** — the route and its tests are done, and the
  response shape is pinned, but no UI reads it.

## How to verify any of this

```
cargo test --workspace                       # the whole suite
cargo test -p market-data --lib              # unit tests, incl. scanner, venue, and the two codecs
cargo test -p market-data --test bybit_wire  # the Bybit decoder against frames captured off the socket
cargo test -p api-gateway --test scan_flow   # 8 route tests, needs DATABASE_URL
cargo test -p chart-engine --lib             # 143 tests
node tools/wasm_abi_check.mjs                # the wasm boundary
node tools/shell_check.mjs                   # 151 named checks against the real engine in jsdom
python tools/guard_check.py                  # patches defects one at a time, requires each named check to fail
```

**And hit the venue.** Every guard above tests the shape of data; none tests that data arrived.
`xtask collect` is the only thing that does, and it now takes a venue:

```
# Bybit carries its book in-band, so this makes no REST request at all.
NO_PROXY='bybit.com' ./target/debug/xtask collect --venue bybit --symbol BTCUSDT --no-persist
# Binance, unchanged, for the control.
NO_PROXY='binance.com' ./target/debug/xtask collect --venue binance --symbol BTCUSDT --no-persist
```

Both should log `order book synced` within about thirty diffs and then a status line every 30s
with `connected=true`, `gaps=0`. `--no-persist` keeps it off the 6 GB database.

**Read `tools/guard_check.py` before touching `app.js`.** It restores from its own backup on
the way out, so anything written while it runs is silently reverted — and the next
`git diff` looks clean, which reads like the change was never made.

## Three rules this repo keeps re-learning, worth repeating to a new agent

1. **A thing that exists on paper but cannot fire is worse than a thing that is missing.**
   Ask of every new guard: *who writes this, and what happens if they don't?* `MD_FEED_AGE`'s
   only writer was a function only tests called, so `stale_market_data` could not fire
   however dead the feed was.
2. **Every guard must be shown to fail against the bug it names.** A guard that cannot fail
   is worse than no guard.
3. **Every guard in this repo tests the shape of data; none tests that data arrived.** A
   green sweep is not a green suite. After touching a collector, hit the live venue.
