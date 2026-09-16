-- 0002_live_trading.sql
--
-- Phase 8: the two things live trading needs that paper trading did not.
--
-- Target: plain PostgreSQL 13+, same as 0001. No extensions, no superuser.

-- ---------------------------------------------------------------------------
-- Venue opt-in (docs/15)
-- ---------------------------------------------------------------------------
--
-- Live trading for a venue requires an explicit opt-in *per venue*, and a
-- revoke has to be possible without a redeploy. Append-only, like every other
-- user-authored record here: the current state is the newest row for a
-- (user, venue) pair, so "who turned this on, when, and why" survives the
-- revoke that followed. A boolean column would have destroyed exactly the
-- history an incident review needs.

CREATE TABLE venue_opt_ins (
    id      UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID        NOT NULL REFERENCES users (id),
    venue   TEXT        NOT NULL,
    action  TEXT        NOT NULL, -- opt_in | revoke
    reason  TEXT,
    ts      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX venue_opt_ins_user_venue_idx ON venue_opt_ins (user_id, venue, ts DESC);

-- ---------------------------------------------------------------------------
-- Orders sent to a venue (docs/11)
-- ---------------------------------------------------------------------------
--
-- `client_order_id` is the primary key because it *is* the idempotency key:
-- the platform generates it deterministically from the decision, and the
-- exchange treats a repeat of it as the same order. Making it the key means the
-- database enforces the same rule, so two tasks racing cannot both record a
-- placement for one decision.
--
-- `status` mirrors the venue's vocabulary (NEW, PARTIALLY_FILLED, FILLED,
-- CANCELED, REJECTED, EXPIRED) rather than a platform-specific one, so a
-- reconciliation mismatch reads as a difference between two comparable things.

CREATE TABLE live_orders (
    client_order_id   TEXT             PRIMARY KEY,
    bot_id            UUID             NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    venue             TEXT             NOT NULL,
    symbol            TEXT             NOT NULL,
    side              TEXT             NOT NULL, -- BUY | SELL
    order_type        TEXT             NOT NULL, -- MARKET | LIMIT | STOP_LOSS_MARKET | TAKE_PROFIT_LIMIT
    quantity          DOUBLE PRECISION NOT NULL,
    status            TEXT             NOT NULL,
    exchange_order_id TEXT,
    filled_qty        DOUBLE PRECISION NOT NULL DEFAULT 0,
    avg_price         DOUBLE PRECISION,
    placed_at         TIMESTAMPTZ      NOT NULL,
    updated_at        TIMESTAMPTZ      NOT NULL DEFAULT now()
);

CREATE INDEX live_orders_bot_idx ON live_orders (bot_id, placed_at DESC);

-- The reconciliation query: "what do we think is live for this bot?". A partial
-- index keeps it small, since most rows are terminal within minutes.
CREATE INDEX live_orders_open_idx
    ON live_orders (bot_id)
    WHERE status NOT IN ('FILLED', 'CANCELED', 'REJECTED', 'EXPIRED');
