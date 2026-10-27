-- 0015_subscriptions.sql
--
-- Subscriptions, monthly usage counters, and the billing event log.
--
-- ## Why the plan catalog is not in this table
--
-- The matrix (names, prices, limits, feature flags) lives in code, in
-- `api-gateway/src/billing/plans.rs`. It is a product decision with a review
-- process, and a deployment must not be able to disagree with the code that
-- enforces it. This table stores only *which* plan a user is on and when the
-- period ends -- the facts that change per user, not the facts that define the
-- product.
--
-- `plan_id` is therefore plain TEXT, validated on the code side. A CHECK with
-- the five names would have to be migrated every time the matrix gains a rung,
-- which turns a pricing change into a schema deployment.
--
-- ## Status vocabulary
--
--   trialing  -- inside the no-card trial; access is the paid plan's
--   active    -- paid (or internally granted) and inside the period
--   past_due  -- renewal failed; still inside the period, so still entitled
--   canceled  -- will not renew; entitled until period_end
--   expired   -- period ended without renewal; access falls back to Free
--
-- `canceled` and `past_due` are deliberately not "no access": a user who
-- canceled has paid through the end of the period, and a failed renewal is a
-- payment problem, not a reason to cut an active session off mid-month.
--
-- ## One subscription per user
--
-- `user_id` is the primary key. Plan changes update the row; they do not
-- stack. History lives in `billing_events`, which is append-only, so "what did
-- this user's subscription look like on the 3rd" is answerable without a pile
-- of `valid_from`/`valid_to` rows in the hot path.
--
-- ## Usage periods are dates, not timestamps
--
-- A counter is keyed by the date its period started. A monthly subscriber who
-- starts on the 17th resets on the 17th; a free user resets on the 1st. The
-- service, not the schema, chooses the anchor -- the schema only insists that
-- there is one row per (user, period, metric), which is what makes the
-- increment atomic.
--
-- ## Billing events are idempotent on (provider, event_id)
--
-- Webhooks retry. Stripe (or any provider) will deliver the same event more
-- than once, and a second delivery must not extend a period twice. The unique
-- key is the provider's own event id, so the insert itself decides whether the
-- event is new -- the same shape as `bots.idempotency_key` in 0004, for the
-- same reason: a read-then-write check has a window, and a retry is exactly
-- the traffic that finds it.

CREATE TABLE subscriptions (
    user_id              UUID        PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    plan_id              TEXT        NOT NULL DEFAULT 'free',
    -- monthly | annual. Annual is paid up front and renews yearly.
    cycle                TEXT        NOT NULL DEFAULT 'monthly'
        CONSTRAINT subscriptions_cycle_named CHECK (cycle IN ('monthly', 'annual')),
    status               TEXT        NOT NULL DEFAULT 'active'
        CONSTRAINT subscriptions_status_named
        CHECK (status IN ('trialing', 'active', 'past_due', 'canceled', 'expired')),
    period_start         TIMESTAMPTZ NOT NULL DEFAULT now(),
    period_end           TIMESTAMPTZ NOT NULL,
    -- Set during a trial; NULL otherwise. The trial converts to `active` at
    -- period_end, or ends early if the user subscribes.
    trial_end            TIMESTAMPTZ,
    cancel_at_period_end BOOLEAN     NOT NULL DEFAULT FALSE,
    -- One trial per user, ever. Enforced here rather than by remembering it in
    -- the client, which is not a place a commercial fact can live.
    trial_used           BOOLEAN     NOT NULL DEFAULT FALSE,
    -- internal | stripe | paddle | ... The provider seam's identity.
    provider             TEXT        NOT NULL DEFAULT 'internal',
    provider_ref         TEXT,
    -- The last client idempotency key this row was written with, so a retried
    -- subscribe call returns the same subscription instead of extending it.
    last_idempotency_key TEXT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT subscriptions_period_ordered CHECK (period_end > period_start)
);

CREATE INDEX subscriptions_status_idx ON subscriptions (status, period_end);

CREATE TABLE usage_counters (
    user_id      UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- The first day of the usage period this row belongs to.
    period_start DATE        NOT NULL,
    -- ai_credit | backtest | ... The code owns the vocabulary; a new metric
    -- must not need a migration.
    metric       TEXT        NOT NULL,
    used         BIGINT      NOT NULL DEFAULT 0
        CONSTRAINT usage_counters_nonnegative CHECK (used >= 0),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, period_start, metric)
);

-- "What has this user used this period" is the only read, and the primary key
-- already serves it; no second index is needed.

CREATE TABLE billing_events (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- SET NULL, not CASCADE: deleting a user must not erase the record that
    -- money moved. The event keeps its provider id and payload; only the link
    -- to the account goes.
    user_id    UUID        REFERENCES users (id) ON DELETE SET NULL,
    provider   TEXT        NOT NULL,
    event_id   TEXT        NOT NULL,
    kind       TEXT        NOT NULL,
    payload    JSONB       NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The idempotency the webhook path relies on, decided by the insert.
    CONSTRAINT billing_events_provider_event_unique UNIQUE (provider, event_id)
);

CREATE INDEX billing_events_user_idx ON billing_events (user_id, created_at DESC);
