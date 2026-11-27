-- Durable Core Analytics predictions and their one-shot settlements (docs/64).
--
-- The in-memory OutcomeLog proves the arithmetic, but a reliability system that
-- forgets every prediction on restart cannot measure drift. A row is written at
-- decision time and never updated again except by settle_prediction, which fills
-- the realized outcome once. The same one-shot contract as markov_evidence keeps
-- a prediction from being silently re-scored with hindsight.

CREATE TABLE core_analytics_predictions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id),
    symbol TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    discipline TEXT NOT NULL,
    direction TEXT NOT NULL,
    evidence_strength DOUBLE PRECISION NOT NULL,
    model_version TEXT NOT NULL,
    decision_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    settled_at TIMESTAMPTZ,
    realized_return DOUBLE PRECISION,
    realized_direction TEXT,
    hit BOOLEAN,
    CONSTRAINT core_analytics_predictions_direction CHECK (
        direction IN ('Bullish', 'Bearish', 'Neutral')
    ),
    CONSTRAINT core_analytics_predictions_strength CHECK (
        evidence_strength >= 0.0 AND evidence_strength <= 1.0
    ),
    CONSTRAINT core_analytics_predictions_settlement_pair CHECK (
        (settled_at IS NULL AND realized_return IS NULL AND realized_direction IS NULL AND hit IS NULL)
        OR (settled_at IS NOT NULL AND realized_return IS NOT NULL AND realized_direction IS NOT NULL AND hit IS NOT NULL)
    )
);

CREATE INDEX core_analytics_predictions_user_symbol_idx
    ON core_analytics_predictions (user_id, symbol, decision_at DESC);

CREATE INDEX core_analytics_predictions_unsettled_idx
    ON core_analytics_predictions (user_id, symbol, decision_at)
    WHERE settled_at IS NULL;
