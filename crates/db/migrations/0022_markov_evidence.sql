-- ## The gap
--
-- Markov evidence was computed and shown once, then discarded. The platform
-- could not later say whether a regime call was right, which is the only way
-- to learn which engine deserves trust in which market condition.
--
-- ## Why a table, not audit_log or agent_memory
--
-- audit_log is append-only by contract and has no settlement column.
-- agent_memory is newest-wins and overwrites old claims. Evidence must be
-- immutable, user-scoped, and settleable once the forecast horizon matures.
-- The full result is JSONB so the calibration formula can change without a
-- migration, the same contract chat_turns uses for thesis payloads.

CREATE TABLE markov_evidence (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id),
    symbol TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    evidence JSONB NOT NULL,
    model_version TEXT NOT NULL,
    horizon_bars DOUBLE PRECISION,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    settled_at TIMESTAMPTZ,
    outcome JSONB,
    CONSTRAINT markov_evidence_outcome_pair CHECK (
        (settled_at IS NULL AND outcome IS NULL) OR
        (settled_at IS NOT NULL AND outcome IS NOT NULL)
    )
);

CREATE INDEX markov_evidence_user_symbol_idx
    ON markov_evidence (user_id, symbol, observed_at DESC);

CREATE INDEX markov_evidence_unsettled_idx
    ON markov_evidence (symbol, observed_at)
    WHERE settled_at IS NULL;
