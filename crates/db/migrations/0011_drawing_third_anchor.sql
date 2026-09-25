-- 0011_drawing_third_anchor.sql
--
-- The TradingView-parity tool vocabulary (`docs/21`), step one: a third anchor.
--
-- ## Why the table shape changes at all
--
-- Every existing kind is a pair of points and a rule. The parity vocabulary
-- adds tools whose rule needs a *third* point: a parallel channel is two
-- points for the line and one for its width; an arc and a circle are a centre
-- and a radius; a triangle is one anchor per vertex. The wire (`Anchor`,
-- optional) and the storage (columns, nullable) meet here, exactly as the
-- second anchor did in 0003 -- including the same whole-or-absent rule, which
-- a CHECK can express and a convention cannot enforce.
--
-- ## Why nullable columns and not a JSONB blob
--
-- The second anchor is already two typed columns; a third as JSONB would make
-- one drawing's anchors live in two data models. `a3_time`/`a3_price` follow
-- the same rules as `a2_*`: both null or neither, converted at the
-- `db::drawings` boundary and nowhere else.
--
-- ## What the database does *not* enforce
--
-- Which kinds *need* three anchors is one rule with N cases, and it lives in
-- Rust beside the enum (`DrawingKind::needs_third_anchor`), pinned against
-- the storage-side copy by a test -- the same arrangement 0003 chose, for the
-- same reason: a kind nobody can draw must be refused at draw time, not
-- stored half-written.

ALTER TABLE drawings
    ADD COLUMN a3_time  TIMESTAMPTZ,
    ADD COLUMN a3_price DOUBLE PRECISION;

ALTER TABLE drawings
    ADD CONSTRAINT drawings_anchor_triple_is_whole
    CHECK ((a3_time IS NULL) = (a3_price IS NULL));
