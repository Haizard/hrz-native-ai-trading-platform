-- Community indicators (`docs/53`): a catalog row per published indicator and
-- immutable versions beneath it. Install records pin one version to one owned
-- indicator workspace, so the existing workspace/revision read paths need no
-- community-specific branch.

CREATE TABLE IF NOT EXISTS community_indicators (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    author_user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    slug               TEXT NOT NULL UNIQUE,
    title              TEXT NOT NULL,
    summary            TEXT NOT NULL,
    tags               TEXT[] NOT NULL DEFAULT '{}',
    author_name        TEXT NOT NULL,
    author_tier        TEXT NOT NULL DEFAULT 'publish'
                       CHECK (author_tier IN ('publish', 'priority')),
    status             TEXT NOT NULL DEFAULT 'listed'
                       CHECK (status IN ('listed', 'unlisted', 'removed')),
    current_version_id UUID,
    install_count      BIGINT NOT NULL DEFAULT 0 CHECK (install_count >= 0),
    report_count       INTEGER NOT NULL DEFAULT 0 CHECK (report_count >= 0),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS community_indicator_versions (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    indicator_id   UUID NOT NULL REFERENCES community_indicators (id) ON DELETE CASCADE,
    version_number INTEGER NOT NULL,
    source         TEXT NOT NULL,
    summary        TEXT NOT NULL,
    symbol         TEXT NOT NULL,
    timeframe      TEXT NOT NULL,
    validation     JSONB NOT NULL,
    preview        JSONB NOT NULL,
    published_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (indicator_id, version_number)
);

ALTER TABLE community_indicators
    ADD CONSTRAINT community_indicators_current_version_fk
    FOREIGN KEY (current_version_id) REFERENCES community_indicator_versions (id);

CREATE TABLE IF NOT EXISTS community_indicator_installs (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    indicator_id UUID NOT NULL REFERENCES community_indicators (id) ON DELETE CASCADE,
    user_id      UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES indicator_workspaces (id) ON DELETE CASCADE,
    version_id   UUID NOT NULL REFERENCES community_indicator_versions (id),
    installed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, indicator_id)
);

CREATE TABLE IF NOT EXISTS community_indicator_reports (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    indicator_id     UUID NOT NULL REFERENCES community_indicators (id) ON DELETE CASCADE,
    reporter_user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    reason           TEXT NOT NULL
                     CHECK (reason IN ('spam', 'misleading', 'malicious', 'copyright', 'other')),
    note             TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (indicator_id, reporter_user_id)
);

CREATE INDEX IF NOT EXISTS community_indicators_featured_idx
    ON community_indicators (status, author_tier DESC, install_count DESC, created_at DESC);
CREATE INDEX IF NOT EXISTS community_indicators_new_idx
    ON community_indicators (status, created_at DESC);
CREATE INDEX IF NOT EXISTS community_indicators_popular_idx
    ON community_indicators (status, install_count DESC, created_at DESC);
CREATE INDEX IF NOT EXISTS community_indicators_tags_idx
    ON community_indicators USING GIN (tags);
CREATE INDEX IF NOT EXISTS community_indicators_author_idx
    ON community_indicators (author_user_id, created_at DESC);
