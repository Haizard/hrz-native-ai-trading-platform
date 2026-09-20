-- Conversation and promotion records for AI-generated indicator workspaces.
-- Revisions remain immutable. A draft names the revision it was created from
-- so a later indicator edit can never alter an approved or running bot.

CREATE TABLE indicator_workspace_messages (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID        NOT NULL REFERENCES indicator_workspaces (id) ON DELETE CASCADE,
    role         TEXT        NOT NULL CHECK (role IN ('user', 'assistant', 'system')),
    kind         TEXT        NOT NULL DEFAULT 'message',
    content      TEXT        NOT NULL,
    payload      JSONB       NOT NULL DEFAULT '{}'::jsonb,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX indicator_workspace_messages_workspace_idx
    ON indicator_workspace_messages (workspace_id, created_at ASC);

CREATE TABLE indicator_bot_drafts (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id        UUID        NOT NULL REFERENCES indicator_workspaces (id) ON DELETE CASCADE,
    revision_id         UUID        NOT NULL REFERENCES indicator_revisions (id),
    strategy_id         UUID        NOT NULL REFERENCES strategies (id),
    backtest_id         UUID        REFERENCES backtests (id),
    mode                TEXT        NOT NULL DEFAULT 'paper' CHECK (mode IN ('paper', 'live')),
    venue               TEXT,
    risk                JSONB       NOT NULL DEFAULT '{}'::jsonb,
    status              TEXT        NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'approved', 'promoted')),
    approved_at         TIMESTAMPTZ,
    bot_id              UUID        REFERENCES bots (id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX indicator_bot_drafts_workspace_idx
    ON indicator_bot_drafts (workspace_id, created_at DESC);
