# 21 — Chart Object Engine

The drawing system as a subsystem, not a pile of buttons. Every visual thing on
the chart is a structured object with a machine-readable representation, so the
same vocabulary serves the user's toolbar, the storage layer, and the AI —
instead of three lists that drift.

## The rule

> **Every visual thing on the chart has a structured representation.**

A trendline is not pixels; it is `{ kind: "trendline", points: [{time, price},
{time, price}] }`, and the length of that list is the kind's own declaration.
The canvas is a *projection* of that object, never the object itself.
Consequences:

- The shell never converts a pixel into a price. A placement or drag writes a
  `fraction` point into the engine request; the engine's scene answers with
  the same drawing in absolute terms, and *that* is what gets stored. There is
  one implementation of "which price is under the pointer", in Rust.
- The toolbar is **built from** the engine's registry over the ABI, not from a
  second list typed into the markup. A tool the engine cannot draw is a button
  the shell cannot grow, and vice versa.
- The AI's drawing vocabulary is the registry. Adding a tool is adding one
  enum variant, one `shapes` arm, and one registry row — every consumer
  follows by construction.

## What shipped (phase 1)

### Object model — `chart-engine::drawing`

- `DrawingKind`: 77 kinds across 12 groups — trendline, hline, vline, ray, extended,
  hray, crossline, infoline, regression, arrow, arrow_mark_up/down, angle,
  channel, disjoint_channel, flat_top_bottom, pitchfork family (4), rect,
  triangle, circle, arc, rotated_rect, ellipse, polyline, curve, double_curve,
  brush, highlighter, fib family (12), gann family (4), pattern family (6),
  elliott family (5), cyclic family (3), price_range, date_range, forecast,
  fixed_range_vp, anchored_vp, anchored_vwap, measure, dateprice_range,
  position_long, position_short, text_note, sticky_note, callout, price_label,
  flag_mark, icon_mark. Each declares its arity via `AnchorCount::Fixed(n)` or
  `AnchorCount::Many{min}`.
- `Anchor`: `absolute {time, price}` (what is stored) or `fraction {x, y}`
  (what a pointer gives). `Absolute` uses milliseconds — f64 is exact there and
  loses precision past 2^53, so epoch-ms is the boundary unit everywhere.
- `ToolGroup`: `lines, channels, pitchforks, shapes, fibonacci, gann, patterns,
  elliott, cyclic, measurement, positions, annotation` — 12 flyouts.
- `ToolSpec` / `REGISTRY`: the one declaration of each tool — kind, label,
  tooltip, group, group label, `anchors` (how many points it takes).
- The arity rule is `DrawingKind::anchors`, and it is the only one. The
  registry's `anchors` field, the storage list length, the shell's click
  counter and the route's refusal all read that declaration, and a test pins
  the registry row to it — so `db::drawings::KINDS` and
  `needs_second_anchor`/`needs_third_anchor` are gone with the columns they
  described.

### Tool registry over the ABI — `chart-engine::lib`

`tool_registry() -> i32` serialises `REGISTRY` into the scene result buffer
(read via `scene_ptr`/`scene_len`). The shell calls it once at engine load and
rebuilds every pane's toolbar.

### The harnesses read the registry, not a list of names

`wasm_abi_check.mjs` now decodes `tool_registry()` itself and asserts its shape
— non-empty, every row carrying `kind`/`label`/`title`/`group`/`group_label`/
`anchors`, kinds unique and snake_case (the wire names storage stores), one
`group_label` per group, every tool declaring its point count. `shell_check.mjs`
counts the toolbar's buttons from the same list rather than from four literals,
and checks that every registry kind is stroked in both theme palettes.

That mattered while the vocabulary was closed, and decides the growth plan: with
literals, each new tool meant editing the harness before the harness could tell
whether the tool was wired. Now a registry row that renders nothing — no button,
no palette key, an unnamed group — fails a check on the commit that adds it.
The registry is grouped by first appearance in the row order, not by contiguous
runs, so `measurement`'s rows need not be adjacent to share a flyout.

### Geometry & rendering — `chart-engine::scene`

One `Frame` (price↔y, time↔x, `bar_nanos` for real bar counts) drives every
kind's `shapes` arm. The engine emits `handle` parts at every movable anchor;
the shell's hit-testing looks for targets *the engine put there*.

### Snap engine (magnet)

`request.snap` asks the engine for candidate points: the visible candles'
OHLC plus volume-profile VAH/VAL/POC. `snap_anchor` pulls a fraction anchor
to the nearest candidate within `SNAP_PX` canvas pixels — zoom-independent by
construction, because the tolerance is in pixels and the comparison happens
after projection. The shell keeps no snap logic; it only sets the flag.

### Command history (undo/redo) — `app.js`

Commands, not snapshots: `{ label, do(), undo() }` on two stacks
(Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y). Each direction re-issues its own API write,
so a reload agrees with the screen either way. Two invariants the tests pin:

1. **Record after settle.** A command is pushed only after its network write
   resolved (`finishPlacing` awaits the save; `finishMoving` awaits the PUT;
   `clearDrawings` awaits the deletes). An undo racing its own write would
   re-create rows the write was removing.
2. **Find by shape, not id.** A save replaces the shell's `new-N` id with the
   server's UUID, so the reconciliation helpers (`byShapeId`, `shapeKey`)
   locate a drawing by kind+anchors+label. Guarded do-halves keep a fresh
   edit from double-saving: they act only when the world is not already in
   the commanded state, which is exactly the redo-after-undo case.

### Toolbar from the registry — `app.js` / `index.html`

The markup ships a flat five-button fallback so the toolbar exists before the
engine loads; on load it is replaced by registry-driven groups (one labelled
flyout per `ToolGroup`, cursor first, magnet/undo/redo/delete/clear as
controls). `selectTool` relabels the group trigger ("Lines · Ray") so the
closed toolbar says what is armed. `shell_check.mjs` asserts the generated
set, flyout behaviour, magnet, and history buttons.

## Out of scope for phase 1 (deliberately)

All phase-1 next kinds have since shipped (phases 2–4). What remains:

- **AI analysis layers**: AI-drawn objects as a *separate, toggleable layer*
  with provenance (`createdBy, agent, confidence, reason`) and an
  accept/reject session, per the original architecture brief.
- **AI write path**: shipped — see below.

## What shipped (phase 2): the AI write path

The agent can now **create, move and delete** chart objects — through the same
storage, the same validation, and the same scoping as the user's own hand.

### The capability, split the way the postures differ

- `UserDrawingsSource` (read) stays what it was: the agent may always *see*
  the user's marks.
- `DrawingWriter` (write) is a separate trait in `ai-agent::user_drawings`,
  with `create` / `update` / `delete`. A host that attaches no writer gets a
  read-only agent, and the write tools say so honestly ("present the analysis
  as text instead; do not pretend the object was drawn").
- `DrawingsContext` carries both under **one identity**: `with_writer` takes
  no user id of its own, so the write identity cannot diverge from the read
  identity even by a confused host.

### Tools, schemas, and provenance

- `create_drawing`, `update_drawing`, `delete_drawing` are registered in
  `ToolRegistry::market_analysis()` beside the read tool.
- `kind` is a free string, **not** a schema enum: an unknown kind gets a
  correctable error naming the valid ones (the same lesson `timeframe_arg`
  records), and the storage vocabulary stays the single source of truth.
- `confidence` (clamped to 0..=1) and `reason` travel as provenance and are
  stored in the new `created_by` / `agent` / `confidence` / `reason` columns
  (`0009_drawing_provenance.sql`). `NULL` provenance means *human* — every
  pre-existing row keeps its meaning, and the agent's adapter stamps
  `created_by = "ai"` unconditionally, so there is no path by which a model
  writes a row that pretends a person drew it.
- Update does not touch provenance: a drag on an AI-drawn level is still an
  AI-proposed level the user moved.

### Validation: one rule, two doors

The HTTP route and the agent's `DbDrawingWriter` both validate with the
**engine's own** `Drawing::validate_anchors` and the same kind list. A second
copy of the rule would eventually disagree, and the disagreement would be a
drawing the agent believes exists and the chart refuses to draw.

### Where the shell fits (next)

The chart already renders any stored drawing; an AI-created one needs only the
layer toggle and its provenance badge — phase 3, below.

## What shipped (phase 3): the AI analysis layer

The shell's half of the deal. An AI-created object is on the chart only when
the user asks for it, and says why it is there when selected.

- **The `AI` toggle**, beside the magnet, off by default. An annotation the
  agent inserted between the user's marks and the candles is a claim that
  needs opting into, not wallpaper. The button's tooltip carries a live count
  ("3 AI-drawn objects hidden"), so a closed toolbar answers "is there
  anything to see?" without a click.
- **The filter is presentation only.** It withholds `created_by: "ai"` rows
  from the *engine request* — the engine draws what it is sent, and the shell
  is what decides what was sent. The rows stay in `drawings`, so undo, delete,
  and every storage count are unaffected by what is being shown. Hiding is
  not deleting.
- **The provenance badge is the note strip's third author.** Priority order:
  the feed's notice, the engine's build note, then — only when an AI drawing
  is selected — `AI: <reason> (confidence N%)`, read from storage. Derived in
  `render`, not written by `select`: render owns the strip, and a note written
  before the frame it belongs to is wiped by it. That was the save-failure bug
  one layer down, back again; the rule generalises — **whoever owns the strip
  writes it last**.
- **Provenance survives the round trip**: `GET /drawings` reports
  `created_by`/`confidence`/`reason` (absent for human rows), `fromServer`
  carries them beside `label`, and the update path never rewrites them.

`shell_check.mjs` covers the whole layer: hidden by default, appearing on
toggle, the user's rows unaffected, the reason surfacing on selection, and a
re-hide that leaves the row in storage.

### Out of scope for phase 2 (deliberately)

## Testing map

| Layer | Test |
| --- | --- |
| Engine kinds/registry | `cargo test -p chart-engine` |
| Storage rule pinned to engine | `cargo test -p db` (`drawings`) |
| Route validation | `cargo test -p api-gateway --lib drawing_routes` |
| ABI export | `tools/wasm_abi_check.mjs` |
| Shell gestures (place/move/delete/clear/undo/redo/magnet/flyouts) | `node tools/shell_check.mjs` |

## What shipped (2026-09): TradingView tool parity

### The registry grew to sixteen kinds, and one rule grew with it

The 2026-09 parity extension added the TradingView staples: `channel` (two
points for the line, one for its width), `angle`, `arc` (centre, horizontal
radius, vertical radius), `circle` (centre plus one radius point), `triangle`
(one point per vertex), `position_long`/`position_short` (entry plus a 1:1
profit/stop band), and `dateprice_range` (a measure box that also counts bars).
Each is one `DrawingKind` variant, one `draw/` arm, one registry row, one
`KINDS` entry — the same one-each arithmetic phase 1 recorded, still holding.

- **A kind could take three points.** When the vocabulary grew past two, the
  columns grew an `a3_time`/`a3_price` pair (`0011_drawing_third_anchor.sql`)
  behind the same whole-or-absent CHECK the second pair had, and
  `needs_third_anchor` was written in both the engine and storage and pinned to
  itself by a gateway test. A circle stayed at **two**: the radius is the
  distance between centre and edge, and a third point would be a second way to
  say it that could disagree with the first. Both of those mechanisms are
  replaced by the list described in the 2026-10 section below — which is what
  makes "a `channel` the route believes stops at two" unrepresentable rather
  than merely tested-against.
- **The gesture grew a third click.** Placing a channel, arc or triangle is
  click, move, click, move, click — between clicks the point still being drawn
  follows the pointer on *hover* (no button held), because a placement the user
  cannot aim is not a placement. Which point that is used to be an explicit
  field (`placing.pending`) rather than something inferred from `null` versus
  `undefined`, because a mis-encoded drag would store the newest point in a
  slot the kind does not take and the shape would vanish with a note; the
  point list made that state redundant, and it is now the index of the fixed
  points. The part shapes, the provenance trio and the wire carry every point
  the same way.
- **Two new part shapes, and the shell paints them.** `DrawingPart::Ellipse`
  (`cx/cy/rx/ry`, `half` for the upper arc, `filled`) and
  `DrawingPart::Polygon` (`points`, `filled`) join Segment/Rect/Text/Handle.
  A triangle is grabbable by its body — `insidePolygon`, the ray-crossing
  test — the same rule a rectangle follows; a circle's radius is computed in
  canvas pixels so it stays a circle whatever the drag direction.
- **The dual toolbar.** The right-click menu still *hosts* the pane's own
  tool row — the buttons move into the menu and back, one wiring. The new
  global toolbar (`#globalTools`) is the inverse arrangement: one permanent
  row owned by the page, built from the same registry, and every interaction
  **delegated** to the active pane through its existing API. Two toolbars,
  one state. It binds to whichever chart the user last touched (the same
  activation gesture the aside follows), names that chart in its label, hides
  when no pane exists, and follows the survivor when one is closed. It is
  deliberately *not* class `tools`: every pane-scoped lookup of `.tools` must
  resolve to a pane's own row, never to the page-level one.

`shell_check.mjs` covers both toolbars agreeing, the three-click gesture
storing one drawing with three absolute points, and a two-point drag
sending exactly two points.

## What shipped (2026-10): a drawing is a list of points

Before this, a drawing was `a1` plus an optional `a2` plus an optional `a3`, and
"how many does a `channel` take" was answered in four places: the engine's
`needs_second_anchor`/`needs_third_anchor`, storage's copy of them, the HTTP
route's body fields, and the toolbar's click counts. A sixth kind of shape would
have needed a fourth column, and a shape could be stored with a second timestamp
and no second price, because "half an anchor" had a spelling.

Now `Drawing` carries `points: Vec<Anchor>` and the arity is
`DrawingKind::anchors() -> AnchorCount`, which is the crate's only statement of
it. That one change is what makes the open-ended tools possible: a polyline, a
brush and a harmonic with a seventh pivot are a `Many { min }` and a longer
list, not a schema migration.

- **`AnchorCount`** is `Fixed { points }` or `Many { min }`, and it crosses the
  ABI as an object tagged `form` — a bare `2` cannot say whether `3` is allowed,
  which is the difference between a tool the toolbar can place and one it
  cannot.
- **`validate_anchors`** checks arity, then one-unit-per-drawing, then
  finiteness, and names the *index* of the offending point. It never looks at
  the id, so a route validating a request body has nothing to invent.
- **The columns are gone.** `0018_drawing_points.sql` backfills one JSONB
  `points` array per row — in the order each kind reads them, which is the
  placement order, not the numeric order: a ray's origin is `points[0]`, not
  the lowest timestamp — then drops the six anchor columns and adds a
  non-empty-array CHECK. A half-anchor is now unrepresentable rather than
  forbidden.
- **Both units, always.** The scene reports `ScenePoint { absolute, fraction }`
  for every point, so the shell stores the first and drags the body with the
  second, and neither needs to invert the price mapping. `docs/14`'s rule is
  what the shape of the type enforces.
- **The agent's wire is the same list.** `create_drawing` takes
  `points: [{time_ms, price}, …]` — the spelling `pattern_library` anchors
  already used. `ai-agent` depends on neither chart-engine nor db, so it cannot
  read `anchors()`; the count lives in that one schema description as prose and
  `DbDrawingWriter::validate` re-runs the engine's own validator, which is where
  a wrong count actually gets refused, in the engine's words.
- **The toolbar's click counter reads the registry.** `anchorsNeeded()` asks the
  engine instead of keeping a list of kind names, and the placement's live point
  is `points[clicks]` — a count that could not be reconciled with the kind
  stored one shape and the chart drew nothing.

`wasm_abi_check.mjs` pins the registry's `anchors` shape and the refusal
messages a mixed-unit list, a short list and a long list each get;
`shell_check.mjs` places a three-point tool chosen from the registry rather than
named in the harness.

## What shipped (2026-10): the full parity vocabulary (77 kinds)

The 2026-10 parity plan extended the engine from 17 to 77 kinds across 12
groups, closing the gap with TradingView, LuxAlgo, and MetaTrader 5's tool
catalogs. Each addition follows the same arithmetic: one `DrawingKind` variant,
one `anchors()` arm, one registry `ToolSpec` row, one `db::drawings::KINDS`
entry, one `draw/` geometry function, one dispatch arm.

Groups added or extended beyond the 2026-09 set:

| Group | Kinds | Arity range |
|-------|-------|-------------|
| Lines (extended) | hray, crossline, infoline, regression, arrow, arrow_mark_up/down, angle | 1–2 |
| Channels | channel, disjoint_channel, flat_top_bottom | 2–4 |
| Pitchforks | pitchfork, schiff, modified_schiff, inside | 3 |
| Shapes (extended) | rotated_rect, ellipse, polyline, curve, double_curve, brush, highlighter | 2–Many |
| Fibonacci | 12: retracement, extension, trend_ext, trend_time, time_zone, channel, fan, pitchfan, arcs, circles, spiral, wedge | 2–3 |
| Gann | box, fan, square, square_fixed | 2 |
| Patterns | xabcd, abcd, cypher, head_shoulders, three_drives, triangle_pattern | 4–6 |
| Elliott | impulse, correction, triangle, double_combo, triple_combo | 3–7 |
| Cyclic | lines, bands, sine | 2 |
| Measurement | price_range, date_range, forecast, fixed_range_vp, anchored_vp, anchored_vwap, measure, dateprice_range | 1–2 |
| Positions | long, short | 2 |
| Annotation | text_note, sticky_note, callout, price_label, flag_mark, icon_mark | 1–2 |

The completeness gate (`draw::completeness_gate`) tests that every kind in the
registry produces at least one non-handle `DrawingPart` from minimum points,
that the output moves when an anchor moves, that no coordinate is `NaN`, that
`Many`-arity kinds work at 2/10/30 points, and that every `DrawingOptions`
field is read by at least one geometry arm.

Reuse over reimplementation: `FixedRangeVP` and `AnchoredVP` delegate to
`analytics_core::volume_profile::calculate_volume_profile_from_candles`;
`AnchoredVWAP` computes running VWAP bar-by-bar (Σ(tp×vol)/Σ(vol));
`Regression` uses least-squares over the candle closes within the anchor span.

