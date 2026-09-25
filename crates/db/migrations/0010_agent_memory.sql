-- 0010_agent_memory.sql
--
-- Persistent AI memory (`docs/09`, the "session that survives a reload").
--
-- ## The gap this table exists for
--
-- The agent could already read the user's drawings and the market, but every
-- conversation started from zero: it would rediscover "4H resistance at
-- 108,500" on Tuesday what it had established on Monday, because the thesis is
-- derived fresh from tools and never kept. This table is the fact store
-- between conversations: one row per fact, addressed by the user, a scope,
-- and a key.
--
-- ## One fact per row, and why the key is unique
--
-- A row is a *claim the agent stated*, not a chat log: "4H resistance =
-- 108,500" keyed `4h_resistance`. When the agent states the same key again --
-- the level moved, the thesis changed -- the row is **updated**, not
-- appended. An append-only fact store would make "what does the agent
-- currently believe" a GROUP BY over history, and the newest-wins semantics
-- are the whole point of memory that gets consulted before an answer.
--
-- ## Why scope is a column and not two tables
--
-- The same shape serves both "facts about BTCUSDT" and "facts about how this
-- user works" (`scope = 'global'`). Two tables would duplicate the read/write
-- paths for a distinction one column already carries.
--
-- ## Why two partial unique indexes, not one UNIQUE constraint
--
-- PostgreSQL 13 treats NULLs as distinct in UNIQUE, so `(user_id, scope,
-- symbol, key)` would allow a thousand `global` rows with the same key. The
-- partial-index form is the version-safe spelling of "one fact per key".
--
-- ## Retention is code, not schema
--
-- A cap (keep the most recent facts per user) lives in `db::agent_memory`'s
-- write path, because the right limit is a product decision that will change,
-- and a trigger is the one place a product decision becomes invisible.

CREATE TABLE agent_memory (
    id         UUID             PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID             NOT NULL REFERENCES users (id),
    scope      TEXT             NOT NULL, -- 'symbol' | 'global'
    symbol     TEXT,                     -- set exactly when scope = 'symbol'
    key        TEXT             NOT NULL, -- what the fact is about
    content    TEXT             NOT NULL, -- the fact, in the agent's own words
    created_at TIMESTAMPTZ      NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ      NOT NULL DEFAULT now(),
    CONSTRAINT agent_memory_scope_named CHECK (scope IN ('symbol', 'global')),
    CONSTRAINT agent_memory_symbol_scope_shape CHECK (
        (scope = 'symbol' AND symbol IS NOT NULL)
        OR (scope <> 'symbol' AND symbol IS NULL)
    )
);

-- Newest-wins reads: "what the agent believes about this symbol", most
-- recently touched first, is the order the prompt injects.
CREATE INDEX agent_memory_user_scope_idx
    ON agent_memory (user_id, scope, symbol, updated_at DESC);

-- One fact per key, per scope shape (see the migration comment: NULLs are
-- distinct in a plain UNIQUE on PG13, so the two cases get one index each).
CREATE UNIQUE INDEX agent_memory_symbol_key_uq
    ON agent_memory (user_id, symbol, key) WHERE scope = 'symbol';
CREATE UNIQUE INDEX agent_memory_global_key_uq
    ON agent_memory (user_id, key) WHERE scope <> 'symbol';
