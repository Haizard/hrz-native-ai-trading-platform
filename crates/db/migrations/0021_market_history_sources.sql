-- Keep history from distinct providers and external datasets separate.
ALTER TABLE trades
    ADD COLUMN source_id TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE trades DROP CONSTRAINT trades_pkey;
ALTER TABLE trades ADD PRIMARY KEY (source_id, symbol, trade_id);
DROP INDEX IF EXISTS trades_symbol_ts_idx;
CREATE INDEX trades_source_symbol_ts_idx ON trades (source_id, symbol, ts DESC);

ALTER TABLE orderbook_snapshots
    ADD COLUMN source_id TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE orderbook_snapshots DROP CONSTRAINT orderbook_snapshots_pkey;
ALTER TABLE orderbook_snapshots ADD PRIMARY KEY (source_id, symbol, ts);
CREATE INDEX orderbook_source_symbol_ts_idx
    ON orderbook_snapshots (source_id, symbol, ts DESC);
