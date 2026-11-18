# 61 — Charting contract

- **Status:** Phase 4 audit and contract. Identity, anchors, ownership,
  lifecycle, caps, serialization and failure behavior are defined below; the
  exit gate's visual-acceptance items remain open (§8).
- **Date:** 18 November 2026
- **Companion:** [`docs/14`](./14-FRONTEND-CHART-ENGINE.md),
  [`docs/21`](./21-CHART-OBJECT-ENGINE.md), [`docs/23`](./23-PINE-LITE-SCRIPTING-LANGUAGE.md),
  [`docs/55`](./55-LIGHT-PINE-EVOLUTION-ROADMAP.md) Phase 4.

## 1. The pipeline

```
Pine source
   │  vet (parse, types, caps)
   ▼
pine_lite::run  ──► Output { plots, hlines, shapes, objects, fills, tints,
   │                         barcolors, tables, polylines, linefills,
   │                         simulation, notes, objects_truncated }
   ▼
chart-engine::script  ──► positioned wire types
   │                      ScriptPlot / ScriptLevel / ScriptShape / ScriptDraw
   │                      ScriptOverlay / SceneScriptPane / ScriptStrategyLayer
   ▼
Scene { script_panes, script_overlays, strategy_layers, equity_panes }
   ▼
shell paints; it never maps a bar or a price
```

Two rules hold the pipeline together:

1. **The script speaks bar index + price; the engine speaks canvas.** The VM
   resolves negative bar offsets and `*_time` anchors at run time, the engine
   maps them through the frame. The shell receives only canvas coordinates.
2. **A run is a pure projection.** Nothing on the script path is incrementally
   mutated between frames: each frame re-runs the script over its visible
   window and the engine rebuilds the positioned lists. There is no
   script-object diff and no retained handle on the shell side.

## 2. Identity

| Surface | Id | Stability |
|---|---|---|
| script pane | `script:{title}` | per scene, derived from the header |
| overlay | `script:{title}` | per scene |
| strategy layer | `script:{title}` | per scene |
| equity pane | `equity:{title}` | per scene |
| legend keys | plot `title=` or `plot N` | per pane |

A drawing object has no wire id, by design. Its identity inside one frame is
its index in creation order; the VM's numeric handle is an internal arena slot
and never reaches the wire. This is sufficient because (a) the shell repaints
the whole list per frame, and (b) lifecycle operations happen inside the run,
where the handle still exists.

**Resolved (2026-11-18):** titles may repeat; scene assembly disambiguates ids in
deterministic scene order — the first keeps `script:Same`, later ones become
`script:Same~2`, `script:Same~3`, and strategy/equity ids follow the same rule.
`two_scripts_with_the_same_title_get_distinct_ids` pins it. Titles themselves
are unchanged.

## 3. Coordinates and anchors

- **Plot / shape / drawing.** Bar index and price. A negative bar index counts
  back from the run's last bar; `line.new_time` / `box.new_time` /
  `label.new_time` take nanosecond timestamps instead and are mapped by time.
- **`xloc`.** Time-anchored objects are the `*_time` forms; the bar forms stay
  bar-indexed (docs/28).
- **Canvas.** `Point { x, y }` in CSS pixels within the pane's `Plot`
  rectangle. Extending lines (`extend.right|left|both`) are projected to the
  plot edge by the engine, except a vertical segment, which Pine does not
  extend.
- **User drawings are a different contract.** Interactive drawings store
  millisecond anchors (`docs/21`); the script path never round-trips through
  storage, so it stays in the scene's own units.

## 4. Pane ownership

| Script header | Where its output goes |
|---|---|
| `overlay=true` | price pane: plots, shapes and objects as `ScriptOverlay`; levels are mapped as price levels |
| `overlay=false` | its own `SceneScriptPane`, below the price plot, with `value_min`/`value_max` for the shell's ticks |
| strategy simulation | `strategy_layers` on the price pane (trades always live with candles) |
| strategy equity | its own `equity_panes` entry, regardless of `overlay` — account scale is never the candle scale |

Pane stacking is computed by `sub_pane_heights` from the declared sub-panes
plus one slot per script pane; the equity pane takes a slot too. An overlay
script does not claim a slot.

## 5. Lifecycle and update semantics

Inside one run:

- `line.new`, `box.new`, `label.new`, `polyline.new`, `linefill.new`,
  `table.new` return a handle.
- `*.set_*` mutates through that handle. A setter on `na`, an out-of-range
  slot, or a deleted object is a **run error**, not a silent no-op.
- `*.delete` clears the slot; the object disappears from `Output.objects`, and
  a `linefill` referencing a deleted line is dropped with it.
- Draw calls on a conditional bar simply do not run on other bars — no
  implicit persistence. Persistence is the script's `var` handle.

Between frames:

- The visible window changes; the script re-runs; the lists are rebuilt.
- A script that wants a stable object across frames keeps the handle in a
  `var` and updates it, exactly as in Pine.
- There is no wire-level patch/delete message, and none is needed while the
  render is a pure function of the frame.

## 6. Limits and failure

| Limit | Value | Behavior when hit |
|---|---|---|
| global object heap | `MAX_OBJECTS = 256` | further draws are no-ops; `objects_truncated = true`; a scene note is shown; the first 256 still render |
| per-kind cap (`max_lines_count`, `max_boxes_count`, `max_labels_count`) | 1–500, header-declared | the oldest of that kind is retired and its slot reused; the newest N render |
| statements / plots / fills / loop depth | vet-time | refused before execution |
| runtime fuel, call depth, array caps | run-time | run error, surfaced as a scene note |

Two behaviors are deliberate:

- **Caps truncate, they do not fail the script.** A chart that stops drawing
  is inspectable; one that refuses to render is not.
- **A failed run is a note, not a dead frame.** `script_pane` returns the
  error text for the scene note and the rest of the chart still paints.

Creation order is part of the contract: `Output.objects` and the wire list are
ordered by creation, not by arena slot, so "later objects paint on top" holds
even after a per-kind cap recycles the oldest slot. This was fixed and pinned
on 2026-11-18 (`per_kind_caps_rotate_the_oldest_and_keep_creation_order`).

## 7. Serialization

- All wire types are `Serialize + Deserialize` and live in
  `frontend/chart-engine/src/script.rs`.
- Colors are packed `0xAARRGGBB` (the VM's layout).
- Optional fields are omitted rather than defaulted on the wire where their
  absence has meaning: `border_color` absent = no border (pre-`docs/31`
  scenes deserialize unchanged); `last_value` omitted for an all-`na` series.
- `ScriptDraw` is an externally tagged enum. Adding a variant is backward
  compatible for readers that ignore unknown variants; renaming one is not.

## 8. Coverage and gaps

| Output | Language | VM | Wire | Engine mapping | Tests |
|---|---|---|---|---|---|
| `plot` | yes | `plots` | `ScriptPlot` | pane or price scale | pane, overlay, offset, `na` gaps |
| `hline` | yes | `hlines` | `ScriptLevel` | pane only | RSI pane |
| `plotshape` / `plotchar` / `plotarrow` | yes | `shapes` | `ScriptShape` | pane or price | marker, fresh-bar, text |
| `plotcandle` / `plotbar` | yes | `plots` (bar kind) | `ScriptPlot` | pane or price | candle/bar tests |
| `fill` | yes | `fills` | `ScriptDraw::Band` | pane | band tests |
| `bgcolor` / `barcolor` | yes | `tints` / `barcolors` | scene fields | chart background / candles | tint tests |
| `line` / `polyline` | yes | `objects` / `polylines` | `ScriptDraw::{Line,Polyline}` | pane or price | anchor, extend, delete |
| `box` / `label` | yes | `objects` | `ScriptDraw::{Box,Label}` | pane or price | border, time anchor, caps, delete |
| `linefill` | yes | `linefills` | `ScriptDraw::Polygon` | pane | fill-between-lines |
| `table` | yes | `tables` | `ScriptDraw::Table` | pane | cell/grid tests |
| strategy trades / equity | yes | `simulation` | `ScriptStrategyLayer` / `ScriptEquityPane` | price pane + own pane | S2 tests |

Not supported as script output, and not promised by any doc: order-book
heatmaps, footprint panels, and native volume-profile panes. `docs/36`'s
structure-anchored volume profile is a platform feature, not a Pine builtin.

Open items for the Phase 4 exit gate:

1. **Visual acceptance** of the `docs/23` Phase 10 (inputs UI, string
   semantics), Phase 11 (MTF history/final bucket) and Phase 16 (time-anchored
   drawings) outputs, and of the `prompt1`/`prompt2` fixtures, with a real
   provider-backed run. Projection tests are not visual acceptance.
2. ~~**Duplicate-title identity** (§2) — decide and test.~~ Resolved: ids are
   disambiguated with a deterministic `~N` suffix.
3. **Z-order between distinct script panes/overlays** is scene order
   (attach order); it is not configurable and not documented on the wire. If
   users can reorder layers, that needs an explicit field.
4. **Wire-level update/delete tests**: box/label deletion now has a VM test and
   a chart-engine mapping test; a shell-level integration test is still owed.
5. **`ScriptDraw` has no style/width on `Label`** beyond the VM's own knobs;
   confirm that is intentional before calling the label contract complete.
