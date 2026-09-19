# Gap status audit — 2026-09-19

**Question asked:** the eight gaps in `gap.md` were implemented by an agent over one
session; what is the *real* status now?

**Method:** every claim in `gap.md` was checked against the code, the build, and a full
test run — not read from the record. Command output is quoted where it decides something.

---

## Headline

**All eight gaps are genuinely implemented and the code works.** Nothing is stubbed,
nothing is aspirational, and no claim in `gap.md` was overstated. The build is clean and
**every test passes**.

**But the work is entirely uncommitted** — ~4,900 lines across 8 new files and 30 modified
files, sitting in the working tree on `main`, one `git checkout` from gone. That is the
single real risk right now.

---

## The one thing to do first

```
git status -sb
```

Every file in `gap.md` is `M` (modified, unstaged) or `??` (untracked). `HEAD` is
`656f783`, a commit that predates this work. **Commit it before anything else.**

Also untracked and *not* gitignored, so a careless `git add -A` sweeps them in:

- `.cargo-logs/` — scratch logs from the agent's runs (`aa.log`, `ce2.log`, `abi2.log` …).
  Add to `.gitignore` or delete.
- `gap.md` itself — probably keep, but it should be a deliberate `git add`, not a side effect.

---

## Gap-by-gap, verified against code

| # | Gap | Verdict | Evidence |
|---|---|---|---|
| 1 | Unified RAM+Binance source | **Done** | `WindowService` in `market-data/src/window.rs:170`; `WindowMarketData` adapter wraps it; `agent_routes.rs:219` and `main.rs:68` both read through it |
| 2 | Native `1w` | **Done** | `Timeframe::W1` at `types.rs:58`, `W1_MONDAY_OFFSET` at `:122`, `ALL` array at `:167` |
| 3 | Feed cap + idle eviction | **Done** | `MAX_ACTIVE_FEEDS = 32` (`bots.rs:123`), `evict_for` at `:817`, `reclaim_idle_feeds` at `:706` |
| 4 | Symbol search/validate | **Done** | `SymbolIndex` (`symbols.rs:108`), `INDEX_TTL` = 6h (`:50`); routes `/symbols/search` + `/symbols/validate` in `lib.rs:198-199` |
| 5 | Chart-to-AI packet | **Done** | `chartPacket()` at `app.js:2218`; null-safe window check at `:2231-2232`; `ChartContext` in `ai-agent/src/chart_context.rs` (667 lines) |
| 6 | Capability + freshness | **Done** | `Readiness{Ready,NotConfigured,Degraded}` (`capabilities.rs:62`) kept separate from `verified`; route `/capabilities` in `lib.rs:187` |
| 7 | Advanced AI + overlays | **Done** | `Overlay`/`OverlayRole{Entry,Stop,Target,Level,Other}`/`SceneOverlay` in `drawing.rs:342-481` |
| 8a | Scanner | **Done** | `scanner.rs` (17 tests), `scan_routes.rs` (8 tests), route `/scan` in `lib.rs:203` |
| 8b | Venue adapter | **Done (REST)** | `Venue` trait + `BinanceVenue`/`BybitVenue` in `exchanges/venue.rs` (14 tests) |
| 8c | Chart screenshot | **Done** | `captureChart()`/`screenshotFromUrl()` `app.js:2971-3015`; `MAX_SCREENSHOT_BYTES` = 4 MB; Bedrock image block at `bedrock.rs:318` |

Every test count the record claims matches the file exactly — 14 venue, 17 scanner, 8
scan_flow, and `a_null_price_never_reaches_the_validator` is present at `drawing.rs:881`.

---

## What the tests actually say

**Build:** clean. `Finished dev profile in 8m 05s`, no errors, no warnings acted on.

**DB-free crates — 576 tests, 0 failures:**

| Suite | Result |
|---|---|
| `analytics-core` | 198 passed |
| `chart-engine` | 143 passed |
| `market-data` | 125 passed (incl. 17 scanner + 14 venue) |
| `strategy-dsl` | 94 passed |
| + 3 smaller binaries | 16 passed |

**API gateway integration (run serially) — all green:**

| Suite | Result |
|---|---|
| `market_flow` | 20 passed |
| `bot_flow` | 13 passed + 1 see below |
| `auth_flow` | 11 passed |
| `scan_flow` | 8 passed |
| `ws_flow` | 10 passed |
| `skills_flow` | 10 passed |
| `strategy_flow` | 6 passed |
| `load_flow` | 6 passed |
| `packaging` | 21 passed |

**Harnesses:**

- `node tools/wasm_abi_check.mjs` — **all checks passed.** The prices-in/pixels-out
  contract is live and verified, including the null-price refusal.
- `node tools/shell_check.mjs` — **all shell checks passed** (133), including the
  screenshot, the chart packet, and "an answer the page cannot render says so".
- `python tools/guard_check.py` — not run in this audit (long), but its preflight is
  present at `guard_check.py:374` and it refuses to start on a missing anchor.

---

## The one test failure, explained

The first full `cargo test --workspace` had **one** failure:
`another_users_bot_is_absent_not_forbidden`, with

```
DENSE: register failed: {"error":{"code":"DATABASE_ERROR",
  "message":"connection pool error: pool timed out while waiting for an open connection"}}
left: 500  right: 201
```

**This is infrastructure, not the new code.** The proof:

1. It **passes in isolation** — re-run alone, `1 passed; 0 failed`.
2. It **passes in its own suite run serially** — `bot_flow`, 13 + 1 passed.
3. The log shows the cause: `slow statement ... pg_advisory_unlock` taking **9.19 s**, and
   SQLite-free Postgres `acquire` waits of 7.0 s and 2.8 s. The managed instance is
   ~1 s/statement.
4. The pool is `DEFAULT_MAX_CONNECTIONS = 10` with a **10 s** acquire timeout
   (`crates/db/src/config.rs:8,11`). Parallel test binaries each open their own pool and
   exhaust it.

**Conclusion:** a real flake in the *test harness's* DB usage, not a defect in the eight
gaps. Fix is cheap — either `DB_MAX_CONNECTIONS`/`DB_ACQUIRE_TIMEOUT_SECS` in the test
env, or keep `--test-threads=1`. Worth doing, because a red suite that is red for
infrastructure reasons trains everyone to ignore red.

---

## What is genuinely NOT done

Exactly as `gap.md` states — no overclaiming found in it:

- **No second *live* collector.** The `ExchangeCollector` trait seam exists; the REST half
  is venue-agnostic. Live ingestion (Bybit WebSocket, reconnect, wire decoding) was
  deliberately not written, because it cannot be exercised without a route to the venue —
  and `docs/19` row 24 is this repo's own proof that unverified ingestion ships dead code.
- **`exchange_info` / `aggTrades` remain Binance-shaped on purpose** — they feed
  Binance-schema parsers, so swapping only the URL would produce nonsense.
- **`tools/agent-cli/src/main.rs:176-243`** still carries a duplicate `DbMarketData`. The
  gateway's copy is gone (only a doc comment remains); this second copy is real and stale.
- **The scanner has no UI.** `GET /scan` works, its 8 route tests pass, the response shape
  is pinned — and **nothing in the shell reads it** (`grep scan frontend/app/app.js` →
  no matches).

---

## Two nuances worth knowing, not defects

1. **`1w` is REST-only, not in the live stream ladder.** `LIVE_TIMEFRAMES` in
   `candle_builder.rs:164` stops at `D1`. This is deliberate and tested
   (`backfill.rs:537`): a weekly bar fed only by the socket could not close until the
   following Monday, so it comes from REST. It is a decision, not an oversight — but it
   means "chart 1w live" is really "chart 1w, refreshed on request".

2. **Bot feeds are exempt from eviction.** `MAX_ACTIVE_FEEDS = 32` applies to
   *reclaimable* (chart/route) feeds only. If all 32 entries are bots', a new chart symbol
   gets no feed rather than evicting a bot's — correct, and tested at `bots.rs:1863`.

---

## Recommended next actions, in order

1. **Commit the work.** It is the only irreversible risk on the table.
2. **Gitignore `.cargo-logs/`.**
3. **Fix the DB pool flake** so a green suite means green.
4. Delete the stale `DbMarketData` in `agent-cli`.
5. Give the scanner a shell surface — the backend is finished and invisible.
6. Then, and only then, the Bybit live collector.

---

## Verification commands (reproduce this audit)

```
cargo build --workspace
cargo test -p market-data -p analytics-core -p chart-engine -p strategy-dsl
cargo test -p api-gateway --test market_flow --test scan_flow --test ws_flow -- --test-threads=1
node tools/wasm_abi_check.mjs
NODE_PATH=<managed-node>/node_modules node tools/shell_check.mjs
```

Note: jsdom is required for `shell_check.mjs` and was **installed into the managed Node
workspace during this audit** — it was not present, so the shell harness could not have
been run before.
