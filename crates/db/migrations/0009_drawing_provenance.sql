-- 0009_drawing_provenance.sql
--
-- The AI write path (`docs/21`): the agent learns to create, move and delete
-- chart objects — and every object it touches says so.
--
-- ## Why provenance is columns, not a convention
--
-- An AI-drawn order block and a hand-drawn one are the same kind of object and
-- render the same way; what differs is who is accountable for it. That fact has
-- to survive a reload, stay answerable years later ("why is this on my
-- chart?"), and be cheap to filter ("hide everything the AI drew"). A comment
-- convention inside `label` satisfies none of the three.
--
-- ## Why nullable, and why there is no CHECK on `confidence`
--
-- Every human row predates this migration, so `created_by` is nullable and
-- NULL **means human** — a default of 'user' would rewrite the meaning of
-- half a million existing rows and every reader would have to remember the
-- mapping. `confidence` is nullable because a human drawing has no model
-- confidence to state; inventing 1.0 for it would be a claim nobody made.
-- `reason` is the model's own words for why it drew the object, and is what
-- "why did you draw this?" reads before re-asking the model.
--
-- The columns are descriptive, not advisory: nothing branches on them except
-- the shell's layer filter and the agent's own provenance report.

ALTER TABLE drawings
    ADD COLUMN created_by  TEXT,
    ADD COLUMN agent       TEXT,
    ADD COLUMN confidence  DOUBLE PRECISION,
    ADD COLUMN reason      TEXT;
