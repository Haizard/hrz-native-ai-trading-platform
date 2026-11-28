-- Durable Market Opportunity Memory (docs/64).
--
-- Compact OHLC representations and their one-shot outcomes are stored so a
-- later decision can recall comparable past opportunities. Decision fields
-- are immutable; only the settlement columns may be filled once.

CREATE TABLE opportunity_memory (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id),
    symbol TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    direction TEXT NOT NULL,
    signature SMALLINT NOT NULL,
    evidence_strength DOUBLE PRECISION NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    settled_at TIMESTAMPTZ,
    realized_return DOUBLE PRECISION,
    hit BOOLEAN,
    CONSTRAINT opportunity_memory_direction CHECK (
        direction IN ('Bullish', 'Bearish', 'Neutral')
    ),
    CONSTRAINT opportunity_memory_signature CHECK (
        signature >= 0 AND signature <= 255
    ),
    CONSTRAINT opportunity_memory_strength CHECK (
        evidence_strength >= 0.0 AND evidence_strength <= 1.0
    ),
    CONSTRAINT opportunity_memory_settlement_pair CHECK (
        (settled_at IS NULL AND realized_return IS NULL AND hit IS NULL)
        OR (settled_at IS NOT NULL AND realized_return IS NOT NULL AND hit IS NOT NULL)
    )
);

CREATE INDEX opportunity_memory_user_symbol_idx
    ON opportunity_memory (user_id, symbol, observed_at DESC);

CREATE INDEX opportunity_memory_signature_idx
    ON opportunity_memory (user_id, signature)
    WHERE settled_at IS NOT NULL;
