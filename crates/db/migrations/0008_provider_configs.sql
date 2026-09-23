-- 0008_provider_configs.sql
--
-- The user's own AI provider (bring-your-own model), rather than the
-- deployment's.
--
-- Until this table, "use the AI" meant "the operator set AI_PROVIDER /
-- AI_API_KEY in the deployment environment", so every user on the platform
-- reasoned with one model billed to one account. A user now stores *their*
-- provider, model and key, and their /agent calls run through it.
--
-- ## The key material is ciphertext, and the key to it is not here
--
-- `api_key_ciphertext` is an AES-256-GCM blob produced by
-- `trading_engine::secrets`, sealed under the key-encryption key read from
-- `BROKER_KEK` at startup -- the same vault that seals exchange keys. That
-- separation is the entire design: a database dump does not carry the value
-- that makes this readable, and no query in `db`, no log line, and no API
-- response ever sees plaintext. The blob is authenticated against the
-- (user_id, "ai-provider") scope, so a row moved between users fails to
-- decrypt rather than decrypting into somebody else's key.
--
-- ## One row per user, upserted
--
-- Unlike skills (append-only, versioned), a provider config is a *setting*:
-- the user edits it, and the newest value is the only one that matters. There
-- is no history to preserve -- a past thesis cites its tool results, not the
-- provider config that drove them -- so the primary key is the user and an
-- update overwrites.
--
-- ## `kek_fingerprint` is not a key
--
-- Same rule as `broker_accounts`: a truncated SHA-256 of the KEK so a
-- rotation can find the rows still sealed under the old one.

CREATE TABLE provider_configs (
    user_id            UUID        PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    provider           TEXT        NOT NULL,
    model_id           TEXT        NOT NULL,
    api_key_ciphertext BYTEA       NOT NULL,
    base_url           TEXT,
    extra_headers      JSONB       NOT NULL DEFAULT '{}',
    kek_fingerprint    TEXT        NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The provider column is checked against the platform's known set. The
-- client-facing list lives in `ai_agent::providers::ProviderId`; this
-- constraint only has to be wide enough to refuse garbage, and adding a
-- provider is an ALTER here plus a variant there -- deliberately two places,
-- because a typo'd provider name in a stored row would fail at use time with
-- an error the user cannot act on.
ALTER TABLE provider_configs
    ADD CONSTRAINT provider_configs_provider_known CHECK (provider IN (
        'bedrock', 'openai', 'anthropic', 'openrouter',
        'deepseek', 'grok', 'huggingface', 'openai-compat'
    ));
