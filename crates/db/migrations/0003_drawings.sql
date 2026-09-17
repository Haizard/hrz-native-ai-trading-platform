-- 0003_drawings.sql
--
-- Phase 7: the drawing layer the chart's required views have always asked for.
--
-- Target: plain PostgreSQL 13+, same as 0001. No extensions, no superuser.

-- ---------------------------------------------------------------------------
-- Drawings (docs/14)
-- ---------------------------------------------------------------------------
--
-- A drawing is **two anchors and a kind**: two (time, price) points with a rule
-- for what goes between them. Every shape a trader draws is that -- a trendline
-- is the segment, a rectangle is the area, a Fibonacci is the levels, and a
-- horizontal line is the degenerate case that needs only one anchor.
--
-- ## Why this table is mutable, when the other user-authored ones are not
--
-- `venue_opt_ins` is append-only because "who turned this on, when, and why" is
-- what an incident review asks, and a boolean column would have destroyed the
-- answer. A drawing is not a consent record. It is a shape the user is still
-- moving, so an append-only table would collect one row per drag of the mouse,
-- and the history that matters for a drawing is the drawing itself.
--
-- ## Why the second anchor is nullable, and what that does *not* mean
--
-- `hline` has one anchor; the other three have two. Storing a second anchor for
-- a horizontal line would be a column whose value is ignored, which is how a
-- later reader comes to believe it is used. The CHECK below keeps the pair
-- whole -- both null or neither -- because a row with a time and no price is not
-- a degenerate drawing, it is a half-written one.
--
-- What the database does not enforce is *which* kinds need two anchors. That is
-- one rule with four cases, and it lives in Rust beside the enum so it cannot
-- drift from the list of kinds. A row that breaks it is refused at draw time
-- with the drawing's id in the scene's note, rather than drawn wrong.
--
-- ## Why `symbol` is a plain TEXT and not a foreign key
--
-- There is no symbols table to point at: candles are keyed by (symbol,
-- timeframe) with no parent row, so a foreign key here would have to invent one.
-- The symbol is normalised on write instead.
--
-- ## Time is TIMESTAMPTZ like everything else, and the wire is not
--
-- The browser sees milliseconds (`docs/14`): JSON numbers are doubles, and a
-- nanosecond timestamp is past 2^53, so one read into JavaScript and written
-- back is a different number. The conversion between the platform's nanoseconds
-- and this column happens in `db::drawings`, at this boundary, and nowhere else.

CREATE TABLE drawings (
    id         UUID             PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID             NOT NULL REFERENCES users (id),
    symbol     TEXT             NOT NULL,
    kind       TEXT             NOT NULL, -- trendline | hline | rect | fib
    a1_time    TIMESTAMPTZ      NOT NULL,
    a1_price   DOUBLE PRECISION NOT NULL,
    a2_time    TIMESTAMPTZ,
    a2_price   DOUBLE PRECISION,
    label      TEXT,
    created_at TIMESTAMPTZ      NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ      NOT NULL DEFAULT now(),
    CONSTRAINT drawings_anchor_pair_is_whole
        CHECK ((a2_time IS NULL) = (a2_price IS NULL))
);

-- The one query there is: "this user's drawings on this symbol". Read on every
-- symbol change, after every reload, and by whatever draws them next.
CREATE INDEX drawings_user_symbol_idx ON drawings (user_id, symbol, created_at);
