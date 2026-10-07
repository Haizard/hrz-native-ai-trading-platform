# 47 — Skill Authoring Interview & AI Platform Control

## The idea

Every trader reads charts differently, but they all draw with the same two
tools: the rectangle and the trendline. So the platform does not ship a Rust
detector per trading concept, and it does not ask traders to write code or a
document grammar. A trader **describes their skill in normal language** — any
length, any messiness — and the platform builds the skill **with** them,
step by step, then saves it as a pure-language skill. From then on the AI
agent reads the skill, finds the pattern in the candles itself, and draws the
zones on the chart.

No code is generated. No concept grammar is exposed. The skill is language;
the agent is the detector; the rect and the trendline are the renderers.

## What already carries the weight

- The drawing machinery (docs/46): DRAW-duty gate, answer-phase drawing, the
  auto-draw floor, the never-empty thesis, and the frontend drawing reload.
- Multi-panel chart layout, per-pane drawings, and the screenshot ladder
  (the chart packet) — pre-existing.
- Chart snapshots: `take_snapshot` / `get_snapshot` / `compare_snapshots`
  agent tools and `/chart-snapshots` routes (docs/45).
- Versioned user skills: `POST /skills`, append-only `PUT /skills/{id}`
  (docs/10 semantics).
- `/chart-sessions` layouts and `/pattern-library` saves (docs/45).

## Section 1 — The Skill Interview

A new agent conversation **mode** — `mode: "author"` on the agent socket —
started from the Skills page ("Create with AI") and living in the existing AI
chat. The transcript is kept by the client and sent back whole with every
message (`history`), so the interview survives a deploy or a re-login and the
server keeps no session table.

The flow:

1. **The trader describes** the skill in their own words.
2. **The agent interviews** — one question at a time, building the picture:
   - what the pattern *is* (it brings the known knowledge: an FVG is the
     three-candle gap where the first and third candles never overlap the
     middle one; an order block is the last opposite candle before the
     displacement that broke structure),
   - which variant the trader trades, and what confirms it,
   - the entry trigger (first touch / close into the zone / rejection
     candle), stop placement, target logic,
   - preferred markets, timeframes, sessions,
   - **what to draw**: rect for the zone, trendline for structure, a
     position marker for the entry — with the anchor recipe spelled out in
     words.
3. **The agent generates the skill document** — `knowledge`, numbered
   `rules` including explicit `DRAW ...` rules with anchor recipes, entry and
   `invalidation` rules, `preferred_markets` / `preferred_timeframes` — and
   shows the draft in the chat.
4. **The trader says "save"** → the agent calls `save_skill_draft`, which
   validates the document and stores it through the existing skills table
   (append-only versioning kept). The skill is immediately pinnable.

Invalid drafts are corrected in-conversation: the tool result names the exact
field and why, and the agent fixes it — the same posture as the strategy
generator, but conversational.

## Section 2 — Drawing language-defined zones reliably

- `get_candles` gains a per-candle `index` (0 = oldest in the returned
  window), so a DRAW recipe can say "the third candle from the right" and
  the model maps index → `time_ms` without counting raw arrays.
- The interview writes the anchor recipes *into the skill*, so the model
  never improvises geometry at analysis time.
- The auto-draw floor keeps the `detect_zones` tool (supply/demand bands
  from `regions`, S/R clustering from `sr_zones`): two generic primitives,
  not a per-concept zoo, so a failed run still leaves generic levels on the
  chart and the model has anchored candidates to copy into `create_drawing`.

## Section 3 — Skill composition

Pin **multiple** skills on one chart: the pin dropdown becomes a
multi-select, `AskRequest` gains `skill_ids: Vec<String>` alongside the
single `skill_id` (one pin is one pin, whichever field carried it), and every
pinned skill's rules reach the prompt under its own name so the thesis can
name which skill each finding came from. Selection (automatic skill matching)
stays single-skill; pinning is the trader's explicit stack.

## Section 4 — AI platform control

The agent drives the trader's UI through the agent socket, so a trader can
watch the AI work instead of driving it:

- New agent tools:
  - `open_chart(symbol, timeframe)` — open a new panel (or focus the
    matching existing one),
  - `set_chart(symbol?, timeframe?)` — retarget the active panel (at least
    one field required; a command that changes nothing is refused),
  - `list_charts` — **deferred**: an honest answer needs the client to report
    its open panes live; until the chart packet carries them, a
    server-invented list would violate the absent-means-absent posture.
- The tools return the command in their result (`ui_command`); the
  orchestrator relays it as a `Progress::UiCommand` step over the session's
  agent socket, so the panel changes *during* the run rather than after the
  answer about it; the frontend executes it (add pane / set selects /
  refocus) and the next chart packet and screenshots reflect it.
- Commands are confirmed-not-seen: the tool result tells the model the
  command was issued, never that it was seen; a fresh screenshot of the new
  panel is a later axis (the capture path is ask-time today).

This is the first brick of the end-state: the trader's skills live in the
library, the AI opens the charts, draws the zones, narrates the thesis, and
the trader watches.

## Section 5 — Explicitly later

- **Continuous monitoring**: re-invoking the agent with the active skills as
  new candles arrive (no script to schedule — an agent loop), drawing new
  zones live and posting to chat.
- **Entry alerts / setup-of-the-day**: the same loop pushing entry cards;
  multi-symbol scans with the pinned skill stack.
- **Web-fetch during the interview** for concepts outside the model's
  knowledge.
- **Mid-run screenshots** after a UI command (the agent re-photographing the
  panel it just opened).
- **`list_charts`** once the chart packet carries the client's live pane set.

## Error handling

- Interview abandonment (the trader closes the chat): nothing is saved; a
  skill exists only after `save_skill_draft` validates it.
- UI command for a disconnected chart page: the relay rides the progress
  channel, which drops silently when the client is gone; the tool result
  already says "issued, not confirmed", so the model never believes a panel
  exists that nobody saw.
- Multi-pin with a missing skill id: the ask fails with `NoMatchingSkill`
  naming the id, as today.
- A skill that no longer parses after a schema change: skipped with a WARN
  in listings (existing behaviour), never a 500.

## Testing

- Scripted-conversation tests for the interview: questions asked, then
  `save_skill_draft`; an invalid draft returns the exact correction and the
  conversation continues.
- `get_candles` index field: tool test pinning the wire shape.
- Multi-pin: agent tests that both skills' rules reach the prompt with
  names; gateway test that `skill_ids` resolves all or fails naming the
  missing one.
- UI commands: unit tests on the command envelope; frontend executes
  `open_chart` / `set_chart` against the pane registry.
- End to end: author an FVG skill in a scripted client, pin it, ask, and
  assert a rect drawing was created (the drawing-gate test pattern).
