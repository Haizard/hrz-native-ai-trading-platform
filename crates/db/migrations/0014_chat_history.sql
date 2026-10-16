-- 0014_chat_history.sql
--
-- Persistent chat history for the agent panel.
--
-- ## The gap this pair of tables exists for
--
-- The agent transcript lived in one JavaScript array in the browser: a reload
-- ended every conversation, and the machine you asked a question on was the
-- only machine that remembered it. `agent_memory` (0010) does not cover this
-- -- it stores the *facts* the agent stated, newest-wins, not the
-- conversation that produced them. This is the conversation itself: questions
-- the user asked and the answers (or refusals) they got, per user, restorable
-- on any device.
--
-- ## A turn is a row, not a message pair
--
-- The transcript's unit is the *turn*: one user question plus the ending it
-- reached -- an `answer` document (the thesis payload the shell renders) or an
-- `error` (the refusal/notice text), never both and often neither yet (a run
-- in flight is simply not saved; the client writes a turn when it finishes).
-- Storing question and answer as separate rows would let a crash leave an
-- orphan answer; one row keeps the two halves atomic.
--
-- `answer` is JSONB for the same reason skills are documents (0010, 0001):
-- the thesis payload grows fields as the agent learns, and a stored row must
-- keep rendering against the shape that produced it.
--
-- ## Attachments are names, not bytes
--
-- A turn records *that* images and a document rode the question -- their
-- names -- not the bytes. Re-showing a thumbnail row is all the history view
-- needs; multi-megabyte screenshots in the database would punish every
-- listing for a record nobody re-opens for the pixels.
--
-- ## Order is a position, not a timestamp
--
-- Turns arrive from the client as they finish, and a single writer appends
-- `max(position) + 1`. Timestamps tie too easily (two turns in the same
-- millisecond) and say nothing about intent; the position is the transcript
-- order, and `UNIQUE (conversation_id, position)` makes a double-save a
-- constraint violation rather than a duplicated bubble.
--
-- ## The title is derived, not asked for
--
-- Nobody names a chat before it has content. The title starts empty and the
-- first question fills it (truncated in code, where the limit can change),
-- the way every chat tool does it. `updated_at` sorts the listing by "most
-- recently talked to", which is the order a history list reads in.

CREATE TABLE chat_conversations (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID        NOT NULL REFERENCES users (id),
    title      TEXT        NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The history list: one user's conversations, most recently touched first.
CREATE INDEX chat_conversations_user_idx
    ON chat_conversations (user_id, updated_at DESC);

CREATE TABLE chat_turns (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id UUID        NOT NULL REFERENCES chat_conversations (id) ON DELETE CASCADE,
    position        INT         NOT NULL,
    question        TEXT        NOT NULL,
    attachments     JSONB,       -- {images: [names], document: name}; names only
    answer          JSONB,       -- the thesis payload; NULL when the run errored
    error           TEXT,        -- the refusal/failure text; NULL on success
    symbol          TEXT,        -- the chart the question was asked on
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Transcript order, with a hard uniqueness so a double-save fails loudly.
CREATE UNIQUE INDEX chat_turns_conversation_position_uq
    ON chat_turns (conversation_id, position);
