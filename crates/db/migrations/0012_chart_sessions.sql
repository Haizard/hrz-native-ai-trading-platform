-- 0012_chart_sessions.sql
--
-- Chart sessions, chart snapshots, and the pattern library (`docs/45`).
--
-- ## What a chart session is
--
-- The multi-panel grid (docs/26) already exists in the frontend, but its layout
-- lives in the tab: close the browser and the 4-pane BTC 4h/1h/5m + ETH 4h
-- workspace is gone. A chart session is that layout, named and kept: one row
-- for the session, one row per panel. `is_template` folds "chart templates"
-- into the same table rather than a second one -- a template is a session
-- nobody has traded from, and a second table would duplicate the read/write
-- paths for a distinction one boolean already carries.
--
-- ## Why panels are a table and not a JSONB blob on the session
--
-- A blob would make "which of my sessions watch SOL on the 5m" a scan of every
-- session's JSON. A panel row is one line: session, slot, symbol, timeframe,
-- plus the panel's configured indicators as JSONB -- the indicators are opaque
-- configuration (name + params), not relational data, so they do not earn
-- columns. `position` is the grid slot, unique within the session, so a panel
-- cannot silently overlap another.
--
-- ## What a chart snapshot is
--
-- The point-in-time record the agent's `take_snapshot` tool writes and the
-- user's snapshot button writes: what the chart showed at capture -- the last
-- close, the drawings on the symbol, and a structure digest (trend + swings) --
-- with a note and tags for retrieval. Drawings and structure are JSONB because
-- they are a *frozen copy* of state that lives elsewhere; freezing them as
-- columns would claim they stay in sync, and they do not -- that is the entire
-- reason snapshots exist. `compare_snapshots` answers "what changed" by
-- diffing two rows of this table.
--
-- ## What the pattern library is
--
-- The detected patterns worth keeping. The `detect_pattern` tool is
-- deterministic and cheap, so a match is not inherently valuable -- the value
-- is the ones a user or the agent chose to record, with the anchors, levels and
-- confidence at detection time. Saving is explicit (`save_pattern`), not
-- automatic: an auto-saved library would be a log of every detection the
-- engine ever made, and a library you cannot find anything in.
--
-- ## Provenance, again
--
-- `created_by` follows the drawings precedent (0009): absent or 'user' is
-- hand-captured, 'ai' is the agent's. The vocabulary is a CHECK here rather
-- than convention, because these rows are written by exactly two paths and the
-- CHECK keeps a third from sneaking in.

CREATE TABLE chart_sessions (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID        NOT NULL REFERENCES users (id),
    name        TEXT        NOT NULL,
    is_template BOOLEAN     NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- "This user's sessions" is the one list query; templates are filtered in Rust
-- because they are the rare case, not the index's job.
CREATE INDEX chart_sessions_user_idx ON chart_sessions (user_id, updated_at DESC);

CREATE TABLE chart_panels (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID        NOT NULL REFERENCES chart_sessions (id) ON DELETE CASCADE,
    position   INTEGER     NOT NULL, -- grid slot, 0-based, unique per session
    symbol     TEXT        NOT NULL,
    timeframe  TEXT        NOT NULL,
    chart_type TEXT,                 -- candlesticks | heikin-ashi | line | ...
    indicators JSONB       NOT NULL DEFAULT '[]', -- [{name, params, ...}]
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chart_panels_slot_unique UNIQUE (session_id, position),
    CONSTRAINT chart_panels_position_nonnegative CHECK (position >= 0)
);

CREATE INDEX chart_panels_session_idx ON chart_panels (session_id, position);

CREATE TABLE chart_snapshots (
    id         UUID             PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID             NOT NULL REFERENCES users (id),
    symbol     TEXT             NOT NULL,
    timeframe  TEXT             NOT NULL,
    price      DOUBLE PRECISION NOT NULL, -- last close at capture
    drawings   JSONB            NOT NULL, -- the user's drawings on the symbol, frozen
    structure  JSONB            NOT NULL, -- trend + swings digest at capture
    note       TEXT,
    tags       TEXT[]           NOT NULL DEFAULT '{}',
    created_by TEXT             NOT NULL DEFAULT 'user',
    created_at TIMESTAMPTZ      NOT NULL DEFAULT now(),
    CONSTRAINT chart_snapshots_created_by_named CHECK (created_by IN ('user', 'ai'))
);

-- The two reads there are: "my snapshots of this symbol, newest first" and
-- (via tags) "the ones I marked". Newest first is the order the agent's
-- compare tool wants, and the order a picker shows.
CREATE INDEX chart_snapshots_user_symbol_idx
    ON chart_snapshots (user_id, symbol, created_at DESC);

CREATE TABLE pattern_library (
    id           UUID             PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      UUID             NOT NULL REFERENCES users (id),
    symbol       TEXT             NOT NULL,
    timeframe    TEXT             NOT NULL,
    kind         TEXT             NOT NULL, -- the detect_pattern vocabulary
    direction    TEXT             NOT NULL, -- bullish | bearish | neutral
    confidence   DOUBLE PRECISION NOT NULL,
    anchors      JSONB            NOT NULL, -- the swings that defined it, frozen
    entry_level  DOUBLE PRECISION NOT NULL,
    target       DOUBLE PRECISION NOT NULL, -- measured-move objective
    invalidation DOUBLE PRECISION NOT NULL, -- the level that kills the read
    summary      TEXT             NOT NULL, -- one line, in the detector's words
    note         TEXT,
    created_by   TEXT             NOT NULL DEFAULT 'user',
    created_at   TIMESTAMPTZ      NOT NULL DEFAULT now(),
    CONSTRAINT pattern_library_direction_named CHECK (direction IN ('bullish', 'bearish', 'neutral')),
    CONSTRAINT pattern_library_created_by_named CHECK (created_by IN ('user', 'ai')),
    CONSTRAINT pattern_library_confidence_range CHECK (confidence >= 0.0 AND confidence <= 1.0)
);

CREATE INDEX pattern_library_user_idx
    ON pattern_library (user_id, symbol, created_at DESC);
