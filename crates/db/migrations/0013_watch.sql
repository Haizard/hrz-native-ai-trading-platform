-- The zone watcher: the agent's drawings, watched around the clock.
--
-- A monitor is one user's standing instruction: "watch my symbol+timeframe for
-- price touching the zones the agent drew, and when one is touched, wake the
-- agent with this skill pinned." An event is one firing of that instruction:
-- what price did, what the agent concluded, and when.
--
-- ## Why a monitor is per user+symbol+timeframe, not per drawing
--
-- The zones come and go -- the agent adjusts them as the chart moves, which is
-- the entire point of the feature -- so a monitor bound to drawing ids would
-- die with its first zone edit. The monitor names a scope; the watcher reads
-- whatever AI drawings live in that scope at check time, and a zone drawn
-- after the monitor was set is watched from the next tick on.
--
-- ## Cooldowns are data, not memory
--
-- The watcher is a loop that can restart; a cooldown kept in its RAM would be
-- forgotten on every deploy, and the user would be re-notified about the same
-- touch every restart. The events table is the cooldown record: a zone with a
-- recent event is quiet until the monitor's cooldown has passed.

CREATE TABLE watch_monitors (
    id               UUID      PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id          UUID      NOT NULL REFERENCES users (id),
    symbol           TEXT      NOT NULL,
    timeframe        TEXT      NOT NULL,
    -- Pinned for the triggered run. NULL means "no pin": the run reads the
    -- chart without a doctrine, which is still a better answer than no run.
    skill_id         TEXT,
    enabled          BOOLEAN   NOT NULL DEFAULT TRUE,
    -- Minutes before the SAME zone may fire again. A floor, not a schedule:
    -- an untouched zone never fires, however long it has been.
    cooldown_minutes INTEGER   NOT NULL DEFAULT 60
        CONSTRAINT watch_monitors_cooldown_is_sane CHECK (cooldown_minutes BETWEEN 5 AND 1440),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One monitor per scope: the UI toggle updates the row, never duplicates it.
CREATE UNIQUE INDEX watch_monitors_user_scope_idx
    ON watch_monitors (user_id, symbol, timeframe);

-- The loop's read: every enabled monitor, all users, every tick.
CREATE INDEX watch_monitors_enabled_idx ON watch_monitors (enabled) WHERE enabled;

CREATE TABLE watch_events (
    id          UUID      PRIMARY KEY DEFAULT gen_random_uuid(),
    monitor_id  UUID      NOT NULL REFERENCES watch_monitors (id) ON DELETE CASCADE,
    -- Denormalised like the monitor's own scope: the events panel reads
    -- "this user's recent events" without a join on every row.
    user_id     UUID      NOT NULL REFERENCES users (id),
    symbol      TEXT      NOT NULL,
    timeframe   TEXT      NOT NULL,
    -- The zone that was touched, when one was. NULL for a monitor-floor
    -- skip record, which exists so a burst of touches leaves one row.
    drawing_id  UUID,
    reason      TEXT      NOT NULL,
    -- done | failed: a failed run is still an event, because "the watcher
    -- fired and the run errored" is a fact the user should see.
    status      TEXT      NOT NULL
        CONSTRAINT watch_events_status_named CHECK (status IN ('done', 'failed')),
    -- The run's thesis narrative, when it produced one. Bounded at write.
    summary     TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The cooldown reads: "this zone's latest event" and "this monitor's latest".
CREATE INDEX watch_events_monitor_idx ON watch_events (monitor_id, created_at DESC);
-- The panel's read: "this user's recent events".
CREATE INDEX watch_events_user_idx ON watch_events (user_id, created_at DESC);
