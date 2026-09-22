# 13 — Database Schema (PostgreSQL / Timescale-style)

## Purpose
Persist market data, strategies, skills, backtests, bots/trades, and users. Use
hypertable-style partitioning (Timescale extension, or manual time-based partitioning if
Timescale isn't available in the deployment target) for the high-volume time-series
tables.

## Core tables (illustrative DDL — adjust types/indexes as the engine dictates)

```sql
-- Time-series (high volume, partition by time)
CREATE TABLE candles (
    symbol TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    open_time TIMESTAMPTZ NOT NULL,
    open DOUBLE PRECISION NOT NULL,
    high DOUBLE PRECISION NOT NULL,
    low DOUBLE PRECISION NOT NULL,
    close DOUBLE PRECISION NOT NULL,
    volume DOUBLE PRECISION NOT NULL,
    buy_volume DOUBLE PRECISION NOT NULL,
    sell_volume DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (symbol, timeframe, open_time)
);

CREATE TABLE trades (
    symbol TEXT NOT NULL,
    trade_id BIGINT NOT NULL,
    price DOUBLE PRECISION NOT NULL,
    quantity DOUBLE PRECISION NOT NULL,
    is_buyer_maker BOOLEAN NOT NULL,
    ts TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (symbol, trade_id)
);

CREATE TABLE orderbook_snapshots (
    symbol TEXT NOT NULL,
    ts TIMESTAMPTZ NOT NULL,
    bids JSONB NOT NULL,
    asks JSONB NOT NULL,
    PRIMARY KEY (symbol, ts)
);

-- Users & auth
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email TEXT UNIQUE NOT NULL,
    password_hash TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Skills (versioned, never mutated in place)
CREATE TABLE skills (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    category TEXT NOT NULL,
    document JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, name, version)
);

-- Strategies (the DSL document, versioned similarly)
CREATE TABLE strategies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    document JSONB NOT NULL,
    created_by TEXT NOT NULL, -- ai_agent | visual_builder | developer_sdk
    skill_ref UUID REFERENCES skills(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE backtests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    strategy_id UUID NOT NULL REFERENCES strategies(id),
    symbol TEXT NOT NULL,
    date_from TIMESTAMPTZ NOT NULL,
    date_to TIMESTAMPTZ NOT NULL,
    report JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE bots (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    strategy_id UUID NOT NULL REFERENCES strategies(id),
    mode TEXT NOT NULL,        -- paper | live
    status TEXT NOT NULL,      -- running | paused | stopped | killed
    venue TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE trades_executed (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bot_id UUID NOT NULL REFERENCES bots(id),
    symbol TEXT NOT NULL,
    side TEXT NOT NULL,
    entry_price DOUBLE PRECISION NOT NULL,
    stop_price DOUBLE PRECISION,
    target_price DOUBLE PRECISION,
    exit_price DOUBLE PRECISION,
    r_multiple DOUBLE PRECISION,
    opened_at TIMESTAMPTZ NOT NULL,
    closed_at TIMESTAMPTZ,
    conditions_fired JSONB
);

CREATE TABLE audit_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID REFERENCES users(id),
    event_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    ts TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

## Partitioning / retention
- `candles`, `trades`, `orderbook_snapshots` should be partitioned by time (Timescale
  hypertables if available) with a documented retention policy per resolution — e.g.
  keep raw trades for N months, downsample/aggregate older data into higher timeframe
  candles only.
- `orderbook_snapshots` volume grows fast; snapshot frequency (from
  `docs/04-MARKET-DATA-ENGINE.md`) directly trades off storage vs. footprint/DOM replay
  fidelity — make this a tunable config, not a hardcoded constant.

## Migrations
- Use `sqlx migrate` (or `sea-orm-cli`) with one migration file per schema change,
  checked into `crates/db/migrations/`, applied automatically in CI and on deploy.

## Done criteria
- All tables above created via migrations, with indexes appropriate to the query
  patterns each engine doc describes (e.g. `(symbol, timeframe, open_time)` range scans
  for candles).
- A documented retention/downsampling job exists and is tested against a synthetic
  dataset. **Done 2026-09-22** (closing `docs/19` row 3): `crates/db/src/retention.rs`
  holds the policy (`RETENTION_TRADES_DAYS`, `RETENTION_ORDERBOOK_DAYS`,
  `RETENTION_INTERVAL_SECS`, with defaults), an age-bounded chunked delete for `trades`
  and `orderbook_snapshots`, and a per-pass report of what was removed. The gateway
  spawns it at boot (first pass immediate), and `cargo xtask retention` runs one pass
  by hand. Candles are deliberately never touched — the chart cannot reconstruct them,
  and the two tables the job does bound are the ones the footprint/DOM replay reads
  recent, not old.
