-- 0018_drawing_points.sql
--
-- The object model behind the tool vocabulary (`docs/21`): a drawing is a
-- **list of points**, and its kind says how many.
--
-- ## Why the columns become one JSONB
--
-- `0011` added a third anchor as two more nullable columns and argued, correctly
-- at the time, that "a third as JSONB would make one drawing's anchors live in
-- two data models". That argument had an expiry date it did not state: it holds
-- while the largest tool takes three points. The vocabulary this table is about
-- to meet takes four (an ellipse's box plus a rotation), five (a harmonic pattern),
-- and -- for a polyline, a brush stroke or Elliott waves -- however many the user
-- places. Column pairs cannot do that, and `a4`/`a5`/`a6` would be a schema whose
-- shape is decided by whichever tool happened to need the most.
--
-- So there is now **one** model for an anchor: the `points` array, in placement
-- order, each element `{"time": <milliseconds>, "price": <number>}`. Not two
-- models with a rule between them, which was the thing 0011 was avoiding.
--
-- ## What the database enforces, and what it never did
--
-- `points` is `NOT NULL` and must be a non-empty JSON array. That is all SQL can
-- say about it, and it is the same division of labour the table has always had:
-- *which* kinds need how many points is one rule with N cases and lives in Rust
-- beside the enum (`DrawingKind::anchors`), because a CHECK would have to be
-- edited every time a tool is added -- and the CHECK would still not be the
-- authority, since a row that breaks it is refused at draw time with the
-- drawing's id in the scene's note.
--
-- `options` is the other new column and the same argument applies to it: per-tool
-- settings (a line's extension, a Fibonacci's direction, a profile's value area)
-- are a bag the *engine* interprets, per kind, and the kinds that read nothing
-- store `{}`. It is here rather than added later by its own migration because a
-- table shape is cheapest to change once.
--
-- ## The backfill, and why it is not a straight copy
--
-- Every existing row is dropped into the list **in the order its kind declares**
-- -- and a kind that declares one point gets one point. `hline` and `vline` rows
-- were allowed to carry a second anchor that their geometry never read; copying
-- it would produce a stored drawing the new exact-arity rule refuses, which is
-- the user's level vanishing from the chart after an upgrade. The old rows for
-- the three-point kinds (`channel`, `arc`, `triangle`) keep all three.
--
-- Milliseconds come out of `TIMESTAMPTZ` as whole integers: the columns were
-- written as `round(ms)` going in, so the round trip is exact rather than a
-- float that drifted by a fraction of a millisecond.
--
-- ## The old columns go with it
--
-- `a1_time`…`a3_price` are dropped, and the two whole-or-absent CHECKs with them
-- (Postgres drops a CHECK that references a dropped column). Nothing is left to
-- keep in step, which is the point: two representations of one anchor list is how
-- a drawing comes to be saved in one and drawn from the other.

ALTER TABLE drawings
    ADD COLUMN points  JSONB NOT NULL DEFAULT '[]',
    ADD COLUMN options JSONB NOT NULL DEFAULT '{}';

UPDATE drawings
SET points =
    jsonb_build_array(
        jsonb_build_object(
            'time',  round(extract(epoch FROM a1_time) * 1000)::bigint,
            'price', a1_price
        )
    )
    -- The second point, for every kind except the two that read one.
    || CASE
        WHEN kind IN ('hline', 'vline')
             OR a2_time IS NULL OR a2_price IS NULL THEN '[]'::jsonb
        ELSE jsonb_build_array(
            jsonb_build_object(
                'time',  round(extract(epoch FROM a2_time) * 1000)::bigint,
                'price', a2_price
            )
        )
    END
    -- The third, for the three kinds that need it. An unknown kind is refused
    -- before it reaches the chart, so the two-point branch is the safe reading.
    || CASE
        WHEN kind NOT IN ('channel', 'arc', 'triangle')
             OR a3_time IS NULL OR a3_price IS NULL THEN '[]'::jsonb
        ELSE jsonb_build_array(
            jsonb_build_object(
                'time',  round(extract(epoch FROM a3_time) * 1000)::bigint,
                'price', a3_price
            )
        )
    END;

ALTER TABLE drawings
    DROP COLUMN a1_time,
    DROP COLUMN a1_price,
    DROP COLUMN a2_time,
    DROP COLUMN a2_price,
    DROP COLUMN a3_time,
    DROP COLUMN a3_price,
    ADD CONSTRAINT drawings_points_is_a_non_empty_array
        CHECK (jsonb_typeof(points) = 'array' AND jsonb_array_length(points) > 0);
