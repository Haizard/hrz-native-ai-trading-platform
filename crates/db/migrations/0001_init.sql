-- 0001_init.sql
--
-- Initial schema. Mirrors docs/13-DATABASE-SCHEMA.md.
--
-- Target: plain PostgreSQL 13+ (gen_random_uuid() is built in from PG13).
-- Deliberately written so it works on a managed instance without superuser
-- access or extra extensions -- i.e. it runs as-is on Northflank Postgres.
--
-- TimescaleDB is OPTIONAL. If your instance has the extension available, see
-- the commented block at the bottom of this file to convert the three
-- high-volume time-series tables into hypertables. Without it, the indexes
-- below give the same query patterns acceptable performance; revisit if a
-- single symbol accumulates > ~50M rows.

-- ---------------------------------------------------------------------------
-- Time series (high volume)
-- ---------------------------------------------------------------------------

CREATE TABLE candles (
    symbol      TEXT             NOT NULL,
    timeframe   TEXT             NOT NULL,
    open_time   TIMESTAMPTZ      NOT NULL,
    open        DOUBLE PRECISION NOT NULL,
    high        DOUBLE PRECISION NOT NULL,
    low         DOUBLE PRECISION NOT NULL,
    close       DOUBLE PRECISION NOT NULL,
    volume      DOUBLE PRECISION NOT NULL,
    buy_volume  DOUBLE PRECISION NOT NULL,
    sell_volume DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (symbol, timeframe, open_time)
);

-- Primary read pattern: range scan for one symbol+timeframe over a window.
-- The PK already covers (symbol, timeframe, open_time) as a btree, so no extra
-- index is needed for the hot path; this one serves symbol-only lookups.
CREATE INDEX candles_symbol_open_time_idx ON candles (symbol, open_time DESC);

CREATE TABLE trades (
    symbol        TEXT             NOT NULL,
    trade_id      BIGINT           NOT NULL,
    price         DOUBLE PRECISION NOT NULL,
    quantity      DOUBLE PRECISION NOT NULL,
    is_buyer_maker BOOLEAN         NOT NULL,
    ts            TIMESTAMPTZ      NOT NULL,
    PRIMARY KEY (symbol, trade_id)
);

-- Footprint / volume-profile rebuilds scan trades for a symbol over a window.
CREATE INDEX trades_symbol_ts_idx ON trades (symbol, ts DESC);

CREATE TABLE orderbook_snapshots (
    symbol TEXT        NOT NULL,
    ts     TIMESTAMPTZ NOT NULL,
    bids   JSONB       NOT NULL,
    asks   JSONB       NOT NULL,
    PRIMARY KEY (symbol, ts)
);

-- ---------------------------------------------------------------------------
-- Users & auth
-- ---------------------------------------------------------------------------

CREATE TABLE users (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    email         TEXT        UNIQUE NOT NULL,
    password_hash TEXT        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Skills -- versioned, NEVER mutated in place (docs/10-SKILLS-SYSTEM.md)
-- An "edit" writes a new row with a new version so historical theses remain
-- explainable against the exact skill version that produced them.
-- ---------------------------------------------------------------------------

CREATE TABLE skills (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID        NOT NULL REFERENCES users (id),
    name       TEXT        NOT NULL,
    version    TEXT        NOT NULL,
    category   TEXT        NOT NULL,
    document   JSONB       NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, name, version)
);

CREATE INDEX skills_user_category_idx ON skills (user_id, category);

-- ---------------------------------------------------------------------------
-- Strategies -- the DSL document, versioned like skills
-- ---------------------------------------------------------------------------

CREATE TABLE strategies (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID        NOT NULL REFERENCES users (id),
    name       TEXT        NOT NULL,
    version    TEXT        NOT NULL,
    document   JSONB       NOT NULL,
    created_by TEXT        NOT NULL, -- ai_agent | visual_builder | developer_sdk
    skill_ref  UUID        REFERENCES skills (id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX strategies_user_idx ON strategies (user_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Backtests
-- ---------------------------------------------------------------------------

CREATE TABLE backtests (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    strategy_id UUID        NOT NULL REFERENCES strategies (id),
    symbol      TEXT        NOT NULL,
    date_from   TIMESTAMPTZ NOT NULL,
    date_to     TIMESTAMPTZ NOT NULL,
    report      JSONB       NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX backtests_strategy_idx ON backtests (strategy_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Bots (paper | live)
-- ---------------------------------------------------------------------------

CREATE TABLE bots (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID        NOT NULL REFERENCES users (id),
    strategy_id UUID        NOT NULL REFERENCES strategies (id),
    mode        TEXT        NOT NULL, -- paper | live
    status      TEXT        NOT NULL, -- running | paused | stopped | killed
    venue       TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX bots_user_status_idx ON bots (user_id, status);

-- ---------------------------------------------------------------------------
-- Executed trades (paper and real share this table; `bots.mode` distinguishes)
-- ---------------------------------------------------------------------------

CREATE TABLE trades_executed (
    id              UUID             PRIMARY KEY DEFAULT gen_random_uuid(),
    bot_id          UUID             NOT NULL REFERENCES bots (id),
    symbol          TEXT             NOT NULL,
    side            TEXT             NOT NULL,
    entry_price     DOUBLE PRECISION NOT NULL,
    stop_price      DOUBLE PRECISION,
    target_price    DOUBLE PRECISION,
    exit_price      DOUBLE PRECISION,
    r_multiple      DOUBLE PRECISION,
    opened_at       TIMESTAMPTZ      NOT NULL,
    closed_at       TIMESTAMPTZ,
    conditions_fired JSONB
);

CREATE INDEX trades_executed_bot_idx ON trades_executed (bot_id, opened_at DESC);

-- ---------------------------------------------------------------------------
-- Audit log -- append only.
-- Nothing in normal application code updates or deletes from this table
-- (docs/15-RISK-COMPLIANCE.md). Credentials are redacted before being written.
-- ---------------------------------------------------------------------------

CREATE TABLE audit_log (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID        REFERENCES users (id),
    event_type TEXT        NOT NULL,
    payload    JSONB       NOT NULL,
    ts         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX audit_log_ts_idx ON audit_log (ts DESC);
CREATE INDEX audit_log_user_event_idx ON audit_log (user_id, event_type);

-- ---------------------------------------------------------------------------
-- OPTIONAL: TimescaleDB hypertables.
-- Only run this if your Postgres instance has the timescaledb extension
-- available. It is a no-op risk otherwise -- leave it commented out.
--
--   CREATE EXTENSION IF NOT EXISTS timescaledb;
--
--   SELECT create_hypertable('candles', 'open_time', chunk_time_interval => INTERVAL '7 days', migrate_data => TRUE);
--   SELECT create_hypertable('trades', 'ts',           chunk_time_interval => INTERVAL '1 day',  migrate_data => TRUE);
--   SELECT create_hypertable('orderbook_snapshots','ts',chunk_time_interval => INTERVAL '1 hour', migrate_data => TRUE);
--
-- Retention policy example (docs/13: keep raw trades N months, aggregate older):
--
--   SELECT add_retention_policy('trades', INTERVAL '6 months');
--   SELECT add_retention_policy('orderbook_snapshots', INTERVAL '3 months');
-- ---------------------------------------------------------------------------
