-- 0007_broker_accounts.sql
--
-- Phase 9: the user's own exchange account, rather than the deployment's.
--
-- Target: plain PostgreSQL 13+, same as 0001. No extensions, no superuser.

-- ---------------------------------------------------------------------------
-- Broker accounts (docs/15)
-- ---------------------------------------------------------------------------
--
-- Until this table, "connect a venue" meant "the operator sets BINANCE_API_KEY
-- in the deployment's environment", so every bot on the platform traded one
-- account that belonged to nobody in particular. A user now connects *their*
-- exchange account and the orders a bot places are theirs.
--
-- ## The key material is ciphertext, and the key to it is not here
--
-- `key_ciphertext` and `secret_ciphertext` are AES-256-GCM blobs produced by
-- `trading_engine::secrets`, sealed under a key-encryption key read from the
-- `BROKER_KEK` environment variable at startup. That separation is the entire
-- design: a database dump, a backup, or a replica leak does not carry the value
-- that makes these readable. `credentials.rs` said a per-user key "needs a
-- purpose-built secret store -- not a column with a password in front of it",
-- and this is the store; the column is not the secret.
--
-- Nothing in the schema, no query in `db`, and no log line ever sees plaintext.
-- The blobs are opaque to this layer -- `db::broker_accounts` moves bytes and
-- never interprets them -- which is what keeps the one place that can decrypt
-- them down to one place. The ciphertext is also authenticated against the
-- *(user_id, venue)* pair, so a row moved between users fails to decrypt rather
-- than decrypting into somebody else's key.
--
-- ## `kek_fingerprint` is not a key
--
-- It is a truncated SHA-256 of the KEK, so a rotation can find the rows still
-- sealed under the old one. One-way and 8 bytes: enough to answer "same key or
-- not", useless for deriving the key.
--
-- ## Why the status is a column when `venue_opt_ins` is append-only
--
-- Different question, different shape. `venue_opt_ins` answers "who consented,
-- when, and why" -- history an incident review reads, so a boolean would
-- destroy the answer. This column answers "does the key work right now", which
-- is a *current measurement* of the venue's opinion, not a user's decision. The
-- history that matters is the rows in `audit_log`, which is where connecting
-- and disconnecting are recorded.

CREATE TABLE broker_accounts (
    id                UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id           UUID        NOT NULL REFERENCES users (id),
    venue             TEXT        NOT NULL,
    label             TEXT        NOT NULL,
    key_ciphertext    BYTEA       NOT NULL,
    secret_ciphertext BYTEA       NOT NULL,
    kek_fingerprint   TEXT        NOT NULL,
    -- pending: stored, not yet checked against the venue.
    -- verified: the venue accepted the key and it may trade.
    -- invalid:  the venue refused it, or it is unusable. `last_error` says why.
    status            TEXT        NOT NULL DEFAULT 'pending'
                      CHECK (status IN ('pending', 'verified', 'invalid')),
    -- What the venue said the key may do: `can_trade`, `can_withdraw`,
    -- `permissions`, commissions. JSONB rather than columns because it is the
    -- venue's vocabulary, not ours, and it is read as a whole by a settings
    -- page -- normalizing it would invent a schema for another company's model.
    permissions       JSONB       NOT NULL DEFAULT '{}'::jsonb,
    last_verified_at  TIMESTAMPTZ,
    -- The venue's own refusal message, or ours. Never a credential: a failed
    -- authentication is recorded by *that it failed*, not by what was sent.
    last_error        TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A user may hold more than one account per venue -- a mainnet and a testnet,
-- or two desks -- but not two with the same name, or the settings page shows
-- two identical rows and neither can be referred to in a support thread.
-- Case-insensitive so "My Binance" and "my binance" are the same name.
CREATE UNIQUE INDEX broker_accounts_user_venue_label_idx
    ON broker_accounts (user_id, venue, lower(label));

-- The list query: this user's accounts, newest first.
CREATE INDEX broker_accounts_user_idx
    ON broker_accounts (user_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Which account a bot trades (docs/11)
-- ---------------------------------------------------------------------------
--
-- Nullable, because a paper bot has no account and every bot that already
-- exists was created before accounts existed. `ON DELETE SET NULL` rather than
-- a cascade or a restrict: disconnecting an account must not delete the bot's
-- trade history (that is the record of what happened to real money), and it must
-- not be *blocked* by a bot row either, or a user could never disconnect. The
-- disconnect path kills the running bots first -- see `broker_routes::disconnect`
-- -- and the venue stays on the bot row, so what is lost is the link to a key,
-- not the fact that a live bot ran.

ALTER TABLE bots
    ADD COLUMN broker_account_id UUID REFERENCES broker_accounts (id) ON DELETE SET NULL;

CREATE INDEX bots_broker_account_idx
    ON bots (broker_account_id)
    WHERE broker_account_id IS NOT NULL;
