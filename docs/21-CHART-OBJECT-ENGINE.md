# 21 — Chart Object Engine

The drawing system as a subsystem, not a pile of buttons. Every visual thing on
the chart is a structured object with a machine-readable representation, so the
same vocabulary serves the user's toolbar, the storage layer, and the AI —
instead of three lists that drift.

## The rule

> **Every visual thing on the chart has a structured representation.**

A trendline is not pixels; it is `{ kind: "trendline", a1: {time, price},
a2: {time, price} }`. The canvas is a *projection* of that object, never the
object itself. Consequences:

- The shell never converts a pixel into a price. A placement or drag writes a
  `fraction` anchor into the engine request; the engine's scene answers with
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

- `DrawingKind`: `trendline, hline, vline, ray, extended, rect, fib, measure`.
  Eight kinds, two anchors each (`hline`/`vline` need only one).
- `Anchor`: `absolute {time, price}` (what is stored) or `fraction {x, y}`
  (what a pointer gives). `Absolute` uses milliseconds — f64 is exact there and
  loses precision past 2^53, so epoch-ms is the boundary unit everywhere.
- `ToolGroup`: `lines, shapes, measurement` — a vocabulary, not free text.
- `ToolSpec` / `REGISTRY`: the one declaration of each tool — kind, label,
  tooltip, group, group label, anchor count.
- The storage rule (`db::drawings::KINDS` + `needs_second_anchor`) is pinned
  against `DrawingKind::ALL` by a test, so engine and database cannot disagree
  silently.

### Tool registry over the ABI — `chart-engine::lib`

`tool_registry() -> i32` serialises `REGISTRY` into the scene result buffer
(read via `scene_ptr`/`scene_len`). The shell calls it once at engine load and
rebuilds every pane's toolbar. `wasm_abi_check.mjs` covers it.

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

In registry order, the natural next kinds — each is one variant + one `shapes`
arm + one row:

- **Fibonacci family**: extension, fan, arcs, time zones (retracement shipped).
- **Channels & pitchforks**: parallel channel, regression, Schiff family.
- **Positions**: long/short with entry/stop/target — this wants a third
  anchor-class (levels as fractions of the risk distance), so design the
  object first.
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
(one anchor per vertex), `position_long`/`position_short` (entry plus a 1:1
profit/stop band), and `dateprice_range` (a measure box that also counts bars).
Each is one `DrawingKind` variant, one `shapes` arm, one registry row, one
`KINDS` entry — the same one-each arithmetic phase 1 recorded, still holding.

- **Three anchors, named `a3`, whole or absent.** `0011_drawing_third_anchor.sql`
  adds `a3_time`/`a3_price` behind the same whole-or-absent CHECK the second
  pair has. `needs_third_anchor` exists in both the engine (`DrawingKind`) and
  storage (`db::drawings`), and a gateway test pins the two to each other —
  the same cross-crate pin `needs_second_anchor` has, because a `channel` the
  route believes stops at two is a channel the chart cannot restore. A circle
  is centre + radius, deliberately **two** anchors: the radius is the distance
  between them, and a third would be a second way to say it that could
  disagree with the first.
- **The gesture grew a third click.** Placing a channel, arc or triangle is
  click, move, click, move, click — between clicks the pending anchor follows
  the pointer on *hover* (no button held), because a placement the user cannot
  aim is not a placement. The pending anchor is an explicit field
  (`placing.pending`), not inferred from `null` vs `undefined`: the engine
  refuses an `a3` on a kind that does not take one, so a mis-encoded drag
  would store its second anchor into `a3` and the shape would vanish with a
  note. Storage, the routes, the agent's drawing tools and the wire all carry
  `a3` the way they carry `a2`.
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
storing one drawing with three absolute anchors, and a two-anchor drag
sending no third anchor at all.
