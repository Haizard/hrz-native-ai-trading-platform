# Drawing-tool parity with TradingView, LuxAlgo/Vela and MT5 — design

Designed 2026-10-30, executed on `main`. Requested by the product owner as the follow-on to
the [concept visual-fidelity audit](concept-visual-fidelity-audit-design.md): that audit judged
how *automatically detected* concepts are drawn; this one judges the **tools a trader places by
hand**.

> **Brief (verbatim intent):** analyse all the chart drawing / analysis tools from TradingView
> and LuxAlgo versus ours, and implement the missing ones — the platform is new, so a trader
> must find the tools they already know, in the place and with the behaviour they expect.

## Headline

We ship **17** drawing kinds
([`drawing.rs:56`](../../../frontend/chart-engine/src/drawing.rs#L56), registry at
[`drawing.rs:254`](../../../frontend/chart-engine/src/drawing.rs#L254)). TradingView documents
**~81** manual drawing objects of its own, and LuxAlgo's Vela charts catalogue **72 tools across
9 groups**. The gap is not taste — it is a list, and most of the missing entries are things a
trader reaches for in the first five minutes: pitchforks, disjoint channels, fib fans, arcs,
circles and spirals, Gann boxes, harmonic and Elliott pattern tools, fixed-range volume profile,
anchored VWAP, forecasts, and every annotation object except a bare label.

The ceiling is structural, and there is exactly one ceiling: **the object model stops at three
anchors**. `Drawing` carries `a1`, `a2`, `a3`
([`drawing.rs:435`](../../../frontend/chart-engine/src/drawing.rs#L435)), storage has two
whole-or-absent `CHECK` constraints for those pairs
(`0011_drawing_third_anchor.sql`), and the shell's placement machine counts
`pending ∈ {2, 3}`. A pitchfork needs 3 (fits), an ABCD needs 4, an XABCD needs 5, a head and
shoulders needs 5, Elliott's triangle needs 5, three drives needs 7, and a polyline, path or
freehand brush needs *however many points the trader clicked*. So this design starts by
replacing the three named anchors with a declared-arity list of points, and every tool after
that is what the docs/21 contract already promises: one variant, one geometry arm, one registry
row.

What is *not* the ceiling: the primitive vocabulary. `DrawingPart` already speaks `segment`,
`rect`, `ellipse`, `polygon`, `text`, `handle`
([`drawing.rs:535`](../../../frontend/chart-engine/src/drawing.rs#L535)) and the shell paints
them all, so no new primitive is needed by any tool below except the arrowhead and the freehand
capture, which are a polygon and a dense polyline.

## The reference set

Every row of the catalog is cited. The three sources are used for different things: TradingView
for **which objects exist and what they draw** (it is what the product owner's customers use),
Vela for **group names, settings vocabulary and interaction** (it is the other platform the
owner benchmarks against, and it documents its own registry), MT5 for **the object-type
cross-check** (44 constants, the shortest list of the three, useful for confirming a tool is
canonical rather than a fad).

- TradingView — the master list *Drawing tools available on TradingView*
  (https://www.tradingview.com/support/solutions/43000703396-drawing-tools-available-on-tradingview/)
  and the per-object article, each at `…/<id>-<slug>-drawing-tool/`. Its own grouping is
  *Cursors, Trend tools, Fibonacci and Gann tools, Patterns (with Elliott and cyclic),
  Forecasting and measurement tools, Geometric shapes, Annotation tools, Chart management
  utilities*, and it says plainly of the first: *"It's not actually a drawing tool."*
- LuxAlgo/Vela — https://docs.luxalgo.com/platform/charts/drawings and
  https://docs.luxalgo.com/vela/user/drawing-tools (the header reads "67 tools across 9 groups"
  while its own table has 72 rows; both numbers are reported rather than one chosen), plus
  https://docs.luxalgo.com/vela/contributing/adding-a-drawing-tool for the tool schema and
  https://docs.luxalgo.com/vela/architecture/settings-rows for the settings rows. Anchoring,
  verbatim: *"Drawings are anchored in time and price, not in screen pixels"*; placement is
  *"click-then-move-then-click"*, `polyline` *"finishes on Enter/double-click"*, `freehand`
  captures a drag path, **Shift** locks to 45°, and **Magnet** has three states (`off → weak →
  strong`).
- MT5 — https://www.mql5.com/en/docs/constants/objectconstants/enum_object (44 object types;
  `OBJ_TREND`, `OBJ_PITCHFORK`, `OBJ_FIBO`, `OBJ_ELLIOTWAVE5`, `OBJ_GANNGRID`, …). MT5 has no
  risk/reward object, which is worth knowing: the Long/Short position is a TradingView-era
  expectation, not a universal one.

One distinction this design takes seriously because both vendors make it: **a drawing tool is
not an indicator**. TradingView's Visible-Range / Session / Periodic volume profiles and every
auto pattern detector are indicators; only *Fixed range VP*, *Anchored VP* and *Anchored VWAP*
have drawing-tool articles. LuxAlgo's PAC toolkit auto-computes and exposes inputs; only its
*Fixed Range Volume Tails* is described as interactive (*"starts an interactive two-click range
selection on the chart, and the anchor lines stay draggable afterwards"*). So the tools below
are the hand-placed half of what our concept engine already auto-detects — the two surfaces
meet at the same `analytics-core` maths.

## The target list

80 kinds: the 17 we have, 63 new. `anchors` is the declared arity (`many` = click as many points
as the tool takes, finished with Enter or a double-click, or a captured drag path). Cited
geometry goes in the plan, one line per group; the full per-tool table lives in
[`ledger/drawing_tools.json`](../../../reports/drawing-tools/ledger/README.md) so the completeness
gate can read it.

| Group | Kinds | New | Reference for the group |
| --- | --- | --- | --- |
| `lines` | trendline, ray, extended, hline, **hray**, vline, **crossline**, **infoline**, **regression**, **arrow**, **arrow_mark_up**, **arrow_mark_down**, angle | 6 | TV *Trend tools*: horizontal ray, crossline, info line, regression trend (with *Pearson's R*), arrow, arrow marks (up/down) |
| `channels` | channel, **disjoint_channel**, **flat_top_bottom** | 2 | TV: *"Disjoint channel"* (two parallel lines, different start times), *"Flat top/bottom"* (angled line + horizontal boundary, filled) |
| `pitchforks` | **pitchfork**, **schiff_pitchfork**, **modified_schiff_pitchfork**, **inside_pitchfork** | 4 | TV documents all four with Price 1/2/3; Vela groups them as its own flyout |
| `fibonacci` | fib, fib_extension, **trend_fib_extension**, **trend_fib_time**, **fib_time_zone**, **fib_channel**, **fib_fan**, **fib_arcs**, **fib_circles**, **fib_spiral**, **fib_wedge**, **pitchfan** | 10 | TV Fibonacci & Gann; *"Circles are drawn at 11 different levels"*; spiral is logarithmic with a *Counterclockwise* toggle |
| `gann` | **gann_box**, **gann_fan**, **gann_square**, **gann_square_fixed** | 4 | TV: box = price×time partitions with corner *Angles*; fan = nine angles 1×1…16×1; square = *Levels, Fan, Arcs* + *Price/Bar Ratio*; square fixed = levels 0-5 |
| `patterns` | **xabcd**, **abcd**, **cypher**, **head_shoulders**, **triangle_pattern**, **three_drives** | 6 | TV publishes the leg ratios per pattern (Gartley `BC 61.8-78.6% AB`, Bat, Crab, Cypher `B 0.382-0.618 XA`, `D 0.786 XC`); Vela's harmonics add a per-leg ✓/✗ badge |
| `elliott` | **elliott_impulse**, **elliott_correction**, **elliott_triangle**, **elliott_double_combo**, **elliott_triple_combo** | 5 | TV: impulse 1·2·3·4·5, correction A·B·C, triangle A·B·C·D·E, double combo W·X·Y, triple combo W·X·Y·X·Z |
| `cyclic` | **cyclic_lines**, **time_cycles**, **sine_line** | 3 | TV: cyclic lines repeat a bar distance *"into the future"*; time cycles shade alternating bands; sine's second anchor *"defines the wave's cycle length and amplitude"* |
| `measurement` | measure, **price_range**, **date_range**, dateprice_range, **vr_volume_profile** (fixed range), **anchored_volume_profile**, **anchored_vwap**, **forecast**, **sector** | 9 | TV Forecasting & measurement; VP inputs *Rows*, *Value area volume*, *Width %*, *POC/VAH/VAL*; forecast has *Source* and *Target* with success/failure styling |
| `shapes` | rect, **rotated_rect**, triangle, **ellipse**, circle, arc, **polyline**, **curve**, **double_curve**, **brush**, **highlighter** | 7 | TV *Geometric shapes*: rotated rectangle is 3 clicks; ellipse is 2; polyline is the verbatim name and closes on the first point; curve 2+1, double curve 2+2; brush and highlighter are freehand |
| `annotation` | **text**, **note**, **callout**, **price_label**, **flag_mark**, **icon** | 6 | TV *Annotation tools* + Vela's `stamps`; a price label *"always contains the price"*, auto-rendered at its anchor |

Merged on purpose, with the reason recorded rather than silently dropped:

- **`path` → `polyline`.** TradingView distinguishes an open *Path* from a closing *Polyline*.
  One kind with a `closed` option is the same object; two kinds would be two ways to say
  "a figure of N points".
- **`comment` / `price_note` / `signpost` / `pin` → `note` + `callout`.** All four are *an
  anchor, an offset text box, and a leader*. `note` is the box with a price link, `callout` is
  the box with a tail; a fifth, sixth, seventh and eighth kind would be styling.
- **`arrow_marker` → `arrow` + the existing `label`.** TradingView's arrow marker is an arrow
  with a text signature; `Drawing` already carries `label`.
- **Gartley / Bat / Butterfly / Crab / Shark / Cypher → `xabcd` + a preset option.** Vela ships
  one button per harmonic; TradingView ships XABCD and documents the ratios. A preset selects
  the ideal bands and the per-leg ✓/✗ badge, so one tool covers seven buttons and cannot drift
  from its own ratio table.

Deliberately **out of scope** for this design, each for a reason rather than by omission:

- **Image, X-posts/idea embeds** — not geometry; they need an upload path and an embed renderer,
  which the platform does not have.
- **Table** — `DrawingPart::Table` exists, but the tool's whole value is inline cell editing,
  which needs a text-grid editor; a read-only grid is not the tool the trader knows.
- **Magnifier** — a viewport feature (re-render the same market at a lower timeframe inside a
  rectangle), not a drawing object.
- **Ghost feed** — needs a projection model over a tick feed; the drawing is the least of it.
- **Bar pattern** — the object is a copy of a candle range, which the engine can draw, but its
  meaning is a *mode* (bar sticks vs line + price source) and a mirror toggle; it returns with
  the settings model rather than as a shape without its inputs.
- **Dedekind tessellation, Sonic / Supersonic / Golden Sonic / Golden Supersonic** — Vela's
  mach-cone set, undocumented by TradingView and MT5; adding four exotic kinds to a toolbar a
  new customer reads is the opposite of the brief.
- **Settings-model extras**: log-scale fib levels, named styles, per-timeframe visibility,
  sync-across-layouts, nested groups, lock-individual, extend-by-bars on channels. These are a
  settings UI, and the UI is the next spec; this one ships the tools and the seven `options`
  fields the geometry genuinely cannot do without.

## D1 — The object model is a point list with a declared arity

```rust
pub enum AnchorCount { Fixed(u8), Many { min: u8 } }   // trendline = Fixed(2), polyline = Many{min:2}
pub struct Drawing { id, kind, points: Vec<Anchor>, label, selected, options }
```

- `DrawingKind::anchors()` is the single arity rule; `needs_second_anchor` /
  `needs_third_anchor` are deleted, not kept as wrappers, because a rule with two spellings is
  the rule that eventually disagrees (that deletion is the one genuinely breaking move in this
  design, and the gateway test that pins engine to storage is what makes it safe).
- `validate_anchors` becomes: the list is exactly the arity, every point is finite, and a
  `Many` kind has at least `min`. The reason still goes into the scene's note with the id, as it
  does today.
- **Storage**: one migration, `0018_drawing_points.sql`, transactional: add `points JSONB NOT
  NULL DEFAULT '[]'`, backfill it from the existing `a_time/a_price`, `a2_*`, `a3_*` columns in
  declaration order, then drop those six columns and their two `CHECK`s. Milliseconds stay the
  wire unit and the reason does not change (past 2^53 a nanosecond is not an integer in JSON —
  `db/src/drawings.rs`'s header). One row per drawing, edited in place, stays true.
- **The scene echo** keeps its contract: the shell sends fractions, the engine resolves them and
  answers absolutes. `a1_fraction` / `a2_fraction` / `a3_fraction` become `points[]` with both
  forms per entry, so the shell still never turns a pixel into a price.
- **AI path**: `create_drawing`'s arguments become a `points` array; `kind` stays free text by
  the docs/21 decision, and an unknown kind still answers with the valid list.

Why not keep `a1/a2/a3` and add an `extra` list for 4+: because then a 5-point pattern would be
read from two different places by two different code paths, and the engine's `place`,
`resolve`, `hitTest` and the storage's whole-or-absent discipline would each need to know which
kinds use which. One list, one rule.

## D2 — Groups follow the references, not our count of buttons

`ToolGroup` grows from 4 to **10** (`lines, channels, pitchforks, fibonacci, gann, patterns,
elliott, cyclic, measurement, shapes, annotation, positions` — 12 with the two we already have;
the final list is in the plan). Both references name these groups; neither would make a trader
look for a pitchfork under "Lines" or a fib fan under "Measure". The toolbar keeps building
itself from `tool_registry()` over the ABI — that contract (docs/21: *"A tool the engine cannot
draw is a button the shell cannot grow"*) is what makes a 17→80 jump a registry change instead
of a UI rewrite.

## D3 — Geometry lives in one module per group

`scene.rs` is 7,513 lines and its `shapes` match already holds 17 arms; 63 more would make the
file unreadable, which the house rule about focused files forbids. So:

```
frontend/chart-engine/src/draw/mod.rs     // the exhaustive match over DrawingKind -> one call
frontend/chart-engine/src/draw/lines.rs   channels.rs   pitchforks.rs
frontend/chart-engine/src/draw/fibonacci.rs  gann.rs    patterns.rs
frontend/chart-engine/src/draw/elliott.rs   cyclic.rs   measurement.rs
frontend/chart-engine/src/draw/shapes.rs    annotation.rs
```

Each takes `(points: [(f64,f64); N], frame: &Frame, options: &DrawingOptions)` already resolved
to `(ms, price)` — the resolution stays in `scene.rs` because that is where the price scale
lives — and returns `Vec<DrawingPart>`. `draw/mod.rs` owns the exhaustive match, so the compiler
still refuses a kind nobody implemented; that guarantee is the reason the current design works
and it is kept verbatim.

**The completeness gate** (the audit's own pattern, applied here):
`every_registry_kind_draws_something` builds each kind from two clearly distinct points and
asserts it emits at least one non-`handle` part, that the parts move when the anchors move, and
that a `Many` kind tolerates 2, 5 and 40 points. A stubbed geometry fails it, so no tool can
ship as a button that draws nothing — the exact class of defect the concept audit kept finding.

## D4 — Options are typed, and only where the geometry needs them

`DrawingOptions` is a small struct with serde defaults, and the rule is
**a field exists only if a `draw/` arm reads it** — the test that enforces it is in the plan.
Seven fields, each cited:

| Field | Default | Whose geometry needs it |
| --- | --- | --- |
| `extend` (`left`/`right`/`both`/`none`) | per kind | TV's *Extend Left/Right* on lines, channels, fib |
| `reverse` | `false` | TV fib retracement *"Reverses the direction"*, Gann box's diagonal reflection |
| `level_profile` (`classic`/`extension`/`ote`/`time`) | `classic` | the fib ladder's ratio set — the audit's `ladder` finding, same gap here |
| `closed` | `true` for shapes | polyline open vs closed |
| `value_area_pct`, `rows`, `width_pct` | `70`, `24`, `30` | fixed-range / anchored VP — Vela's documented numbers |
| `preset` (`none`/`gartley`/`bat`/`crab`/`butterfly`/`shark`) | `none` | harmonic bands + ✓/✗ leg badges |
| `source` (`hlc3`/`ohlc4`/`close`) | `hlc3` | anchored VWAP, whose anchor maths is `analytics-core::vwap` |

Position kinds get their risk/reward from **a third point** (entry, target, stop) instead of an
option: three prices are data, one ratio would be a claim the drawing cannot show.

## D5 — Interaction: click-to-N, Enter/double-click to finish, drag for freehand

The shell's `placing` becomes `{ id, kind, fixed: [fraction], pending: fraction|null }`, and the
hard-coded `anchorsNeeded()` (`app.js:4466` — a copy of the engine's rule that has already
drifted into a third place) reads `toolRegistry.find(t => t.kind === tool).anchors`. Placement
stays a drag: `drag.mode = "place"` unchanged, pointer capture unchanged, `finishPlacing` still
awaits the save before recording the command (docs/21's "record after settle"), and undo/redo
still find a drawing by shape, not id.

`freehand` kinds capture every pointermove into `pending` — a point per frame, which is what
Vela's `placementMode() === 'freehand'` is — and single-shot arming stays: one drawing per
toolbar click, with Escape reverting, because *"the chart cannot be controlled"* is the report
the current comment already cites as the reason.

Magnet is unchanged and applies to every point, not just the first — today `snap_anchor` is
offered on placement and the harness covers `trendline`; the harness grows to cover a three- and
a five-point tool.

## D6 — Style comes from the registry, and a test says so

`app.js`'s two palettes are keyed by wire name and a missing key **silently falls back** to the
text colour (`app.js:2819`) — that is how a new tool arrives grey and stays grey. So: one
palette key per kind, added in the same commit as the kind, and `shell_check.mjs` gains
*every registry kind has a palette key in both themes*, which does not exist today. Group
triggers get icons from the sprite (`ui/icons.js`'s set); a new group needs one symbol, which is
the only markup change this design asks for.

## D7 — The harness stops hard-coding the vocabulary

`tools/shell_check.mjs` currently asserts **exactly 18** `data-tool` buttons, **exactly 17**
flyout names, **exactly 4** group triggers. At 80 kinds those numbers are a wall between us and
the work, and they test nothing: the right assertion is that the DOM equals `tool_registry()`
over the ABI. That change lands first, before any kind, so every later commit is verified
against the registry rather than a literal.

## Testing map

| Layer | Test |
| --- | --- |
| Arity is one rule | `cargo test -p chart-engine` — every kind × its arity, whole-or-absent refused |
| Geometry is not a stub | `every_registry_kind_draws_something` + per-group `draw/*.rs` unit tests on a fixed `Frame` |
| Engine ↔ storage vocabulary | `cargo test -p db`, `cargo test -p api-gateway --lib drawing_routes` (both anchor rules replaced by one arity pin) |
| Migration keeps old rows | `points` backfilled from `a1/a2/a3` in order — a checked-in fixture row per arity |
| ABI | `tools/wasm_abi_check.mjs` — `tool_registry()` row count and one place/move per new arity |
| Shell gestures | `node tools/shell_check.mjs` — toolbar equals registry, 1/2/3/5-point and freehand placement, magnet, undo/redo, palette keys |
| What a trader sees | browser pass over every group, one screenshot per group in `reports/drawing-tools/evidence/` |

## Sequencing

Twelve tasks in [`the plan`](../plans/2026-10-30-drawing-tools-parity.md): the object model and
the harness first (they gate everything), then the groups from the ones a day trader reaches for
(lines/channels/pitchforks, fibonacci) to the ones that carry data (measurement, then VP/AVWAP
on `analytics-core`'s existing maths), shapes and annotation last because they are the cheapest,
and the completeness gate + docs at the end. One commit per group; every group commit is green
against all six repository gates.
