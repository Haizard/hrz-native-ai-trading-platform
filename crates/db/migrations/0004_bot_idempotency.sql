-- 0004_bot_idempotency.sql
--
-- `docs/19` row 16: `POST /bots` had no idempotency key, so a retry was a second
-- live bot.
--
-- Target: plain PostgreSQL 13+, same as 0001. No extensions, no superuser.

-- ---------------------------------------------------------------------------
-- Why this column exists at all
-- ---------------------------------------------------------------------------
--
-- `POST /bots` created a fresh row every time it was called. A client that
-- retried -- because a response was lost, a proxy timed out, a user pressed the
-- button twice -- got a second bot, and for `mode: "live"` each of those places
-- orders against the real account. That is the difference between this and the
-- same bug fixed in `POST /venues/{venue}/opt-in` on 2026-09-17: there the
-- consequence was a misleading log line, here it is money.
--
-- The existing guard does not help, and it is worth being exact about why.
-- `BotSupervisor::is_running` keys on `bot_id`, so it refuses to start *the same
-- bot* twice. Two overlapping requests do not produce the same id -- they
-- produce two -- so both pass the guard and both start. It reads as protection
-- against a double start and is protection against a retry of one id.
--
-- Several live bots on one account is *intended* (`bot_routes.rs` puts the bot
-- id in the client order id precisely so two bots cannot collide), so this is a
-- missing idempotency key, not a broken guard. The fix has to distinguish "the
-- same request arriving twice" from "a deliberate second bot", and only the
-- client can say which it means -- hence a key the client supplies.

ALTER TABLE bots ADD COLUMN idempotency_key TEXT;

-- ---------------------------------------------------------------------------
-- Why a unique index, and not a SELECT before the INSERT
-- ---------------------------------------------------------------------------
--
-- The obvious implementation is to look for a bot with this key and return it if
-- found, else insert. That has a time-of-check-to-time-of-use window: two
-- requests that overlap both find nothing, both insert, and the bug survives the
-- fix. The window is small and a retry usually arrives well after the first
-- request finished, which is exactly what makes it the kind of defect that
-- passes a test and fails in production.
--
-- The index closes it at the only place that can: the database. The insert
-- becomes `ON CONFLICT (user_id, idempotency_key) DO NOTHING RETURNING id`, and
-- a request that gets no row back knows another one won and reads the winner's
-- row. Two concurrent requests cannot both insert because there is nothing for
-- the second to insert into.
--
-- ## Why NULL is allowed, and why that is not a hole
--
-- The key is optional: a client that does not send one keeps today's behaviour,
-- and the column stays NULL. In PostgreSQL a unique index treats NULLs as
-- **distinct**, so any number of keyless bots coexist and the constraint only
-- binds rows that actually carry a key. This is load-bearing and worth a test
-- rather than a comment -- a reader who assumes "unique means one NULL" would
-- conclude, wrongly, that keyless creation was broken by this migration.
--
-- ## Scoped to the user, not global
--
-- Two users may pick the same key string; they are not the same request. The
-- index is on the pair.
--
-- ## What this does not cover, stated rather than implied
--
-- Deleting a bot removes its row and therefore frees its key, so a retry that
-- arrives *after* a deliberate delete would create a new bot. That is left as
-- it is: a delete is an explicit act, and the gap between it and a stray retry
-- is measured in hours, while the case this exists for is measured in seconds.
-- Widening it would mean a separate keys table that outlives the bots, which is
-- a real cost for a window nothing has been observed in.

CREATE UNIQUE INDEX bots_user_idempotency_idx ON bots (user_id, idempotency_key);

-- ---------------------------------------------------------------------------
-- The empty string is refused, at both ends
-- ---------------------------------------------------------------------------
--
-- An empty key is worse than no key. Every request carrying `""` would conflict
-- with every other, so the *first* bot a user created would be returned for all
-- their later creates -- a client that meant "no key" would silently get a
-- single bot instead of many, and the symptom would look like the bot ignoring
-- its own settings.
--
-- The route rejects an empty or whitespace-only key with a 400 before it reaches
-- the database, because a client error deserves a client error. This constraint
-- is the second line of defence, and it is honest about what it guards: not that
-- path, which never gets here, but any *other* writer -- a future call site, a
-- script, a manual insert -- so the invariant holds at the level that can
-- enforce it for everyone.

ALTER TABLE bots ADD CONSTRAINT bots_idempotency_key_not_empty
    CHECK (idempotency_key IS NULL OR idempotency_key <> '');
