# 30 — Preference learning: the workspace remembers what the user does

## Purpose

Roadmap #5: the system should adjust future generations based on user
modifications. The modifications that carry intent are the ones made in the
layer UI (docs/25): tuning a setting in the ⚙ popover, resetting it, removing
a layer. Those are standing instructions — "I wanted Length at 21" should
shape the *next* "make me an RSI" on this workspace without being asked
again.

## The shape

- **Preferences are prose notes** in the workspace memory's `preferences`
  array: "The user set Length to 21 on \"RSI\"", "The user removed the
  \"HTF FVG\" layer", "The user reset \"Bands\" to its declared defaults".
  Prose, not structured data, because the reader is the model.
- **Recording is fire-and-forget** from the shell
  (`POST /indicator-workspaces/{id}/preferences`): it never blocks the chart
  interaction that produced it, never toasts, never throws. No active
  workspace means nowhere honest to attach the note, so the observation is
  dropped.
- **Merging dedups and caps**: an exact duplicate is a no-op; past 20 notes
  the oldest falls off. The list is small enough to ride every generation
  request.
- **Memory rewrites preserve the list.** The generation and script-submit
  paths rewrite the memory document on every revision; before this document
  they would have wiped it.
- **Injection is into the request text**, not the system prompt: the
  preferences are instructions the user gave *by doing*, and the model must
  read them where it reads the ask — "Standing preferences on this workspace
  (the user set these by doing; honor them unless this request contradicts
  them)". The explicit contradiction clause keeps a fresh "actually, length
  14 this time" in charge.

## What this is not

- **Not cross-workspace learning.** Preferences belong to the workspace they
  were observed in; a global profile would need a user-level store and a
  much more careful privacy story.
- **Not inference.** Only deliberate actions are recorded (a committed field
  change, a reset, a removal). Hovers, pans, and opens are not preferences.
- **Not weighting.** Notes are equal and FIFO; recency weighting is a
  prompt-level nicety the cap already approximates.

## Tests

`indicator_workspace_routes::tests` — the merge (dedup, cap, key
preservation, non-object legacy memory) and the injection (labeled block,
ask first, untouched when empty).
