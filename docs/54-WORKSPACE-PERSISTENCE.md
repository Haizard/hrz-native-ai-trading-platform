# 54 — Workspace Persistence

## The bug

Reloading the page used to reset the workstation: one pane, the default symbol,
no indicator layers, no scripts, whatever tab the shell booted on. Everything a
user arranged lived only in memory. Drawings survived because they are rows in
the database, and named chart sessions survived because they are explicit saves
— but the everyday state, the thing nobody thinks to "save", was lost on every
refresh.

This document describes the fix: one durable snapshot per user, written as the
workspace changes and restored before the first market fetch.

## The snapshot

```json
{
  "version": 1,
  "layout": { "mode": "grid", "columns": 3 },
  "activePane": 1,
  "activeTab": "book",
  "indicatorWorkspaceId": "uuid-or-null",
  "panes": [
    {
      "symbol": "BTCUSDT",
      "timeframe": "5m",
      "mode": "candles",
      "limit": "500",
      "indicator": { "…": "the workspace's indicator definition, or null" },
      "scripts": [
        {
          "source": "…",
          "inputs": {},
          "name": "…",
          "inputsSpec": [],
          "visible": true
        }
      ]
    }
  ]
}
```

The server treats this as **opaque**. `PUT /workspace` checks that it is a JSON
object, that `version` is `1`, that `panes` is a non-empty array of at most 100
entries, and that the encoded document is at most 2 MiB. It does not interpret
a symbol, a timeframe or an indicator. The shell is the only writer and the only
reader; a second interpretation on the server would be a schema that drifts from
the page it describes.

`version` exists so a future shell can change the shape without an old tab
corrupting it: a PUT with any other version is refused (`400
WORKSPACE_STATE_INVALID`), and a GET returns whatever the account last wrote.
The shell only restores a snapshot whose `panes` array it understands; anything
else falls back to the default pane.

`activePane` is an index into `panes`, not a DOM id — pane ids are rebuilt per
page load. `indicatorWorkspaceId` reopens the Studio conversation the user was
in the middle of.

## One row per user

`crates/db/migrations/0017_workspace_state.sql`:

```sql
CREATE TABLE workspace_state (
    user_id    UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    state      JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

PUT is one `INSERT … ON CONFLICT (user_id) DO UPDATE`; GET is one primary-key
read. There is no history and no naming: the workspace is a view, not a
document. Signing in on a second device replaces the first device's snapshot on
the next change — the last writer wins, deliberately.

### Why not `chart_sessions`?

A chart session is a **named layout** the user creates, lists, restores by id
and counts against a plan's "saved layouts" cap (`docs/45`, `docs/52`). The
workspace snapshot is none of those things:

- it has no name and no list — nobody opens "my current view" from a menu;
- it must be readable in one row on boot, without choosing among candidates;
- it is written automatically, many times per session; charging it against the
  saved-layout allowance would make ordinary panning consume a paid slot;
- a session restore is an explicit action that can replace the current panes; a
  workspace restore is what the page does before it has any panes at all.

Mixing the two would force every automatic save to create a session row, or
every session to compete with a single implicit row. A dedicated table keeps
each concern one query wide.

## Routes

| Method | Path | Response |
|---|---|---|
| GET | `/workspace` | `{ "state": <object> \| null }` |
| PUT | `/workspace` | `{ "saved": true, "updated_at": <unix ms> }` |

Both require a bearer token (`UserContext`); the row is keyed by the caller's
user id, so there is no way to read or write another account's snapshot. With
no database configured both answer `503`, matching the other persistence
routes. An invalid body is `400 WORKSPACE_STATE_INVALID` with the reason:
non-object, wrong version, missing/empty `panes`, more than 100 panes, or over
2 MiB.

The 2 MiB bound is far above a realistic snapshot (100 panes of a few KiB of
source) and exists so a bug or a compromised shell cannot turn the row into an
unbounded blob. The 100-pane bound matches the largest plan's pane cap.

## Writing

The shell calls `scheduleWorkspaceSave()` from every mutation point:

- layout: `setLayoutMode`, `setGridColumns`;
- panes: `addPane`, `closePane`, `setActive`, attaching/clearing an indicator,
  attaching/removing/showing/hiding a script;
- series: `fillSeries` (symbol, timeframe, mode, limit changes);
- tabs: `selectPane`;
- Studio: choosing a conversation (`selectWorkspace`, `showWorkspaceList`).

`scheduleWorkspaceSave` is a **debounce**, not a queue: the first change starts
an 800 ms timer and later changes inside the window are absorbed by the write
that follows. Dragging a layout, typing in a search box, or a burst of attach
events becomes one PUT. A change made in the last 800 ms before a hard refresh
can be lost; the next change rewrites the whole snapshot, and the snapshot is
always complete rather than incremental, so there is nothing to repair.

`saveWorkspaceState` serializes the snapshot, compares it with the JSON most
recently saved, and returns without a request when nothing changed. A failed
save is swallowed silently — a toast in the middle of a drag would be worse
than losing one snapshot, and the next change retries with the full state.

## Restoring

Boot order matters, because panes cannot be built until the instrument list
exists:

1. `loadCoverage()` — the instruments every pane's selector needs;
2. `GET /workspace` — the snapshot (skipped when signed out);
3. layout — mode and column count, so new panes land in the right container;
4. pane count — close extras down to one, add empty panes up to the saved count
   (bounded by the plan's `paneCap()`);
5. per pane — `applyState` (mode, bar limit, indicator, script layers), then
   `fillSeries` (symbol and timeframe);
6. active pane — `setActive` by saved index;
7. active tab — `selectPane`, which also reloads the pane's data (community,
   billing, brokers, Studio list) as it would on a click;
8. Studio conversation — `pendingIndicatorWorkspaceId` survives until the
   workspace list loads and reopens that conversation.

Only after the restore completes does `workspaceReady` become `true`. Until
then, `scheduleWorkspaceSave` is a no-op. Without that gate, a half-built
restore would save the panes that exist so far over the snapshot being read —
the page would eat its own state on every load. Instead the shell records the
restored snapshot as `workspaceSavedJSON`, so an unchanged restore does not
immediately PUT it back.

When there is no snapshot (new account, signed out, or any read failure), the
shell fills the default pane, sets `workspaceReady`, and schedules a save. A
signed-out page keeps `workspaceReady = false`; signing out also clears the gate
and the saved-JSON marker in `paintSession`, so a visitor can never write their
default view over the account's snapshot.

After sign-in (without a reload), `authenticate` reads the snapshot and runs the
same restore, then refreshes and reconnects every restored pane. The boot path
and the sign-in path share `restoreWorkspace`, so there is one definition of
what "the saved workspace" means.

## What is *not* persisted

- **Chart viewport (pan/zoom).** It is a moment-to-moment reading position, not
  a workspace property; the next live candle would move it anyway. Restoring a
  stale zoom would fight the price action the user came back to watch.
- **In-progress drawing or tool state.** A line being dragged has no meaning
  after a reload; finished drawings are already server-backed rows (`docs/14`)
  and survive independently.
- **Open popovers, modals, hover and focus.** Transient UI, not layout.
- **Live streams.** Each restored pane reconnects after its series loads; a
  socket cannot be serialized, and the stream registry already shares one
  connection per symbol/timeframe.
- **Conversation scroll position and draft composer text.** Those belong to the
  chat/storage path, not the pane layout.

The snapshot is also deliberately **not** the place for shared state: drawings,
saved sessions, indicators and strategies remain in their own tables. If a
future pane needs a durable artifact, it gets a row; the workspace snapshot only
points at the view.

## Tests

`crates/api-gateway/tests/workspace_flow.rs`:

- a fresh account reads `state: null`; a PUT saves; a GET returns exactly the
  saved object; another account reads `null`; the save survives a second PUT
  (upsert) — the row is owner-scoped;
- a string state, version 2, an empty `panes` list and 101 panes are all
  `400 WORKSPACE_STATE_INVALID`.

The shell is verified with `node --check frontend/app/app.js`, then manually:
sign in, add a pane, pin a different symbol/timeframe, attach an indicator and a
script, switch tabs, refresh — every part returns. Switch to a grid, add a third
pane, refresh — the grid and the third pane return. Sign out, refresh — the
default page; sign back in, refresh — the saved workspace returns.
