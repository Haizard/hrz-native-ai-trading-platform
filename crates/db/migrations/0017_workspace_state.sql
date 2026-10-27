-- The shell's workspace, as one opaque snapshot (`docs/54`).
--
-- A pane is a view, not a saved layout: it must not consume the plan's layout
-- allowance, and restoring it is one read rather than N chart-session reads.
-- The server stores the blob the shell wrote; it only enforces ownership,
-- shape and size.

CREATE TABLE IF NOT EXISTS workspace_state (
    user_id    UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    state      JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
