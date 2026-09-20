-- Durable AI indicator workspaces. Revisions are append-only: an attached chart
-- or a bot must always be able to name the exact generated source it used.

CREATE TABLE indicator_workspaces (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id             UUID        NOT NULL REFERENCES users (id),
    name                TEXT        NOT NULL,
    symbol              TEXT        NOT NULL,
    timeframe           TEXT        NOT NULL,
    memory              JSONB       NOT NULL DEFAULT '{}'::jsonb,
    active_revision_id  UUID,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX indicator_workspaces_user_idx
    ON indicator_workspaces (user_id, updated_at DESC);

CREATE TABLE indicator_revisions (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id        UUID        NOT NULL REFERENCES indicator_workspaces (id) ON DELETE CASCADE,
    parent_revision_id  UUID        REFERENCES indicator_revisions (id),
    revision_number     INTEGER     NOT NULL,
    source              TEXT        NOT NULL,
    summary             TEXT        NOT NULL,
    change_summary      TEXT        NOT NULL,
    validation          JSONB       NOT NULL,
    preview             JSONB       NOT NULL,
    status              TEXT        NOT NULL, -- validated | rejected
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, revision_number)
);

CREATE INDEX indicator_revisions_workspace_idx
    ON indicator_revisions (workspace_id, revision_number DESC);

ALTER TABLE indicator_workspaces
    ADD CONSTRAINT indicator_workspaces_active_revision_fk
    FOREIGN KEY (active_revision_id) REFERENCES indicator_revisions (id);

CREATE TABLE indicator_alert_preferences (
    workspace_id        UUID        NOT NULL REFERENCES indicator_workspaces (id) ON DELETE CASCADE,
    revision_id         UUID        NOT NULL REFERENCES indicator_revisions (id) ON DELETE CASCADE,
    event_name          TEXT        NOT NULL,
    enabled             BOOLEAN     NOT NULL DEFAULT FALSE,
    channels            JSONB       NOT NULL DEFAULT '[]'::jsonb,
    PRIMARY KEY (workspace_id, revision_id, event_name)
);
