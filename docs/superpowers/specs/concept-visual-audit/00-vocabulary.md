# The shape vocabulary

Status: **final — Task 18, 2026-10-31.** Judged over **182 rows** in sixteen shards
([`ledger/`](../../../../reports/concept-visual-audit/ledger)) and the sixteen family sections
([`01-smc.md`](01-smc.md) through [`16-econ.md`](16-econ.md)). This chapter is the contract the
build reads; the gate enforces it. Where a pass's row and this chapter disagree, the row keeps what
the pass measured and this chapter records the painter reality — the split is named under *Status
is per row where the host changes the draw*, never smoothed over.

The audit's shape numbers after twelve passes:

| | rows |
| --- | --- |
| kinds | **124 `event`, 40 `series`, 14 `interval`, 4 `readout`** |
| statuses | **126 `wiring`, 56 `new_rendering`** |
| `draw.verdict` | 133 `differs_major`, 25 `differs_minor`, 22 `impossible_in_vocabulary`, 2 `matches` |
| `fires.verdict` | 115 `different_rule`, 39 `continuous`, 21 `same_bars`, 7 `no_external_rule` |
| `preview_agrees` | 132 `differs`, 38 `not_rendered`, 12 `agrees` |
| `rule_as_data.form` | 176 `predicate`, 6 `recursion` |

The two `matches` are `indicators.donchian` and `indicators.changepoint`; the twenty-two
`impossible_in_vocabulary` rows are SMC concepts whose draw needs a shape the three-member concept
path cannot express at all. The ceiling the audit measured is the vocabulary in this file, not the
engine: the scripting path already paints every primitive named below.

## The four kinds of thing a concept can be

| `kind` | What the concept is | Drawn as | Rows | Families |
| --- | --- | --- | --- | --- |
| `event` | Fires on a set of bars or at an instant | A mark, a segment, a path, a box, or a vertical rule | 124 | SMC 23, Candlesticks 34, Chart Patterns 15, Harmonics 9, Price Action 10, Supply & Demand 3, Sessions 8, Fibonacci 6, Volume Profile 9, Order Flow 1, Economic Releases 6 |
| `series` | One value per bar, over a scale | A line, a histogram, a band, or dots — on the price scale or its own | 40 | Trend 12, Momentum 10, Volatility 4, Volume 5, Statistical 9 |
| `interval` | A state held across time, whatever price does | A full-height box with a label | 14 | SMC 9, Sessions 3, Price Action 1, Statistical 1 |
| `readout` | A number the trader reads | The figure, the window it was measured over, and the host it hangs on | 4 | Supply & Demand 1, Sessions 3 |

`kind` is the axis the drawing question is asked on. `ConceptShape` answers only *where on the
chart*; it never says what the concept is, and that is how a `+2.7σ` deviation and a doji ended up
with the same text mark.

## The nine gate rules

The gate is [`tools/concept_visual_audit.mjs`](../../../tools/concept_visual_audit.mjs); `check`
with no flag sweeps the union. Every row passes all nine:

| Rule | What it enforces |
| --- | --- |
| 1 | Exactly one row per registry concept, in the shard `familyOf(id)` puts it in, at the registry's version |
| 2 | Nothing left at its seeded value; `kind`, both verdicts, `form`, `status` and `scale` are values from this chapter's vocabulary |
| 3 | A citation — `source`, `url`, one quoted sentence, and a `YYYY-MM-DD` checked date — or `no_convention: true` with a reason that is more than "none found" |
| 4 | An `event` cannot fire `continuous`; a rule that fires every bar is a series wearing an event's row |
| 5 | A `series`' scale is the registry's: `pane: true` → `own_pane`, `overlay` only → `price`; a series concept with neither flag is a recorded defect, not a row to route around |
| 6 | `rule_as_data.inputs` is non-empty, and each input is `{ name, channel }` where `channel` is a `DataChannels` constant — the lowercase name or the bit name; `macro` is legal because the channel constants (not the catalog's `names()`) are the source |
| 7 | The render the row was judged from exists on disk |
| 8 | `preview_agrees` is `agrees`, `differs`, or `not_rendered` — the card in the Indicators face is a second renderer and must be judged |
| 9 | `parameters.declared` equals the registry's parameter list and `parameters.honoured` is an array drawn from it; `[]` is the honest answer, an absent list is an unjudged row |

The rules are enforced by reading the ledger, not by re-running the engine: each row names the
detector function it measured, so a later diff can find the pair.

## What `wiring` and `new_rendering` mean

- **`wiring`** — the shape can be lowered to a painter that already exists; the entry below names
  the arm. What remains is plumbing: a `ConceptShape` member, a wire field, a registry flag, a
  dispatch arm. None of those is a painter, and the audit counts them as wiring on purpose.
- **`new_rendering`** — no existing lowering paints the shape. Three cases qualify, and the entry
  says which one applies: **no painter exists** (pane dots, pane fills), **the shape's data contract
  is not on the wire** (a run's start, a lifecycle's edge, the slope `smc.trendline_sweep` computes
  and discards), or **the host does not exist** (a second pane for `pair_link`, a value-carrying
  label for `readout`).
- The rule "a name the engine can already paint is never tagged `new_rendering`" still holds: every
  `new_rendering` entry names the missing painter, data or host — never merely a missing variant.

### Status is per row where the host changes the draw

Two shapes split by host and one splits by data. The chapter records both halves; it does not
average them:

- **`time_box`** — the full-height host is `wiring` (the `Box` tint arm); the price-extent clock
  states (four SMC rows, `pa.regime`) are `new_rendering` because the state's span is not on the
  wire. The geometry for the latter is `price_band`'s box; the missing piece is the span.
- **`series_band`** — the overlay host is `wiring` (`OverlayFillSpec` → `ScriptDraw::Band`); the
  pane host is `new_rendering` because `SubPaneSpec::Series` has no fills field and the pane branch
  of the mapper drops them.
- **`outline`** — fifteen Chart Pattern rows are `wiring` (`Polyline`); `smc.trendline_sweep` is
  `new_rendering` because the detector computes the slope through three touches and then discards
  it, so no arm can draw the line the row asks for.

## The three rules that never bend

- **A shape does not know which venue fed it.** What differs between a Binance tier and a derived
  tier is the rule's inputs and the provenance on the signal, never the geometry.
- **Wiring before rendering.** The scripting path already paints `Line`, `Polyline`, `Polygon`,
  `Band`, `Table`, `Label` and `Box` ([`script.rs:69-150`](../../../../frontend/chart-engine/src/script.rs#L69));
  a shape expressible as one of those is wiring, and the missing work is named as plumbing.
- **A series' scale comes from the registry,** not from the pass: `pane` true means its own scale,
  `overlay` true means the price frame, and the gate rejects a row that disagrees (rule 5). Event
  and interval shapes take their scale from the shape, never from a guess — see *Scale*.

## The shape contract

Every shape the audit asked for, with the status its rows carry and the arm to reuse. `Rows` counts
all 182; a shape with a host split names both statuses.

| Shape | Kind | Rows | Status | Existing arm, or what is missing |
| --- | --- | --- | --- | --- |
| `bar_marker` | event | 37 | **wiring** | `CandleMark` lowers to `ScriptDraw::Label` ([scene.rs:2955](../../../../frontend/chart-engine/src/scene.rs#L2955), [script.rs:139](../../../../frontend/chart-engine/src/script.rs#L139)); the glyph, the plot clip and the hover path are the work |
| `series_line` | series | 36 | **wiring** | `SubPaneSpec::Series` ([scene.rs:400-445](../../../../frontend/chart-engine/src/scene.rs#L400), painter [2408](../../../../frontend/chart-engine/src/scene.rs#L2408)) and `OverlaySeriesSpec` ([scene.rs:370-400](../../../../frontend/chart-engine/src/scene.rs#L370)); `series_marks` is folded in (below) |
| `level_run` | event | 24 | **new_rendering** | The primitives are `Line` + `Label`; the missing piece is the run's two edges — the detector answers `bars: [t, t]` and eight of eleven PA detectors pass `None` for a start |
| `lifecycle_box` | event | 19 | **new_rendering** | The primitives are `Box` + `Label` (Zone lowering, [scene.rs:2897](../../../../frontend/chart-engine/src/scene.rs#L2897)); the missing piece is the status-driven edge and the removal, plus the `to`/`mitigated_at` the event boundary drops ([regions.rs:150-171](../../../../crates/analytics-core/src/regions.rs#L150)) |
| `outline` | event | 16 | **15 wiring, 1 new** | `ScriptDraw::Polyline` for the pivots ([script.rs:91](../../../../frontend/chart-engine/src/script.rs#L91)) plus a dashed neck `Line` and a projected target `Line`; `smc.trendline_sweep` adds the missing slope to the wire first |
| `legged_path` | event | 9 | **wiring** | `Polyline` ([script.rs:91](../../../../frontend/chart-engine/src/script.rs#L91)) + a `Label` per leg ([139](../../../../frontend/chart-engine/src/script.rs#L139)) + an optional `Polygon` wash ([105](../../../../frontend/chart-engine/src/script.rs#L105)); one name at the figure, a terminal PRZ band where projections overlap |
| `time_box` | interval | 9 | **4 wiring, 5 new** | Wiring host: the full-height `Box` from `scene_overlay_tints` ([script.rs:785](../../../../frontend/chart-engine/src/script.rs#L785)); new host: a price-extent state span, whose geometry is `price_band`'s and whose start/end is missing from the wire |
| `time_line` | event | 7 | **wiring** | `ScriptDraw::Line` with `x1 == x2` is a vertical segment that does not extend ([script.rs:69](../../../../frontend/chart-engine/src/script.rs#L69)); the label is a `Label` ([139](../../../../frontend/chart-engine/src/script.rs#L139)) and carries the figure |
| `ladder` | event | 5 | **wiring** | `Line` per rung ([69](../../../../frontend/chart-engine/src/script.rs#L69)) + `Label` for ratio and price ([139](../../../../frontend/chart-engine/src/script.rs#L139)), the two anchors joined, one name for the study; the pocket is a `Polygon` ([105](../../../../frontend/chart-engine/src/script.rs#L105)) or `Band` ([115](../../../../frontend/chart-engine/src/script.rs#L115)) |
| `price_band` | event | 5 | **wiring** | `Box` + `Label` through the Zone lowering ([scene.rs:2897](../../../../frontend/chart-engine/src/scene.rs#L2897)); the two x edges are the measurement window, there is no lifecycle and no removal, the label sits once at the right edge |
| `readout` | readout | 4 | **new_rendering** | `Label` prints the concept name today ([script.rs:139](../../../../frontend/chart-engine/src/script.rs#L139)); the missing piece is a label that carries the measured figure and its window, attached to its host |
| `candle_mark` | event | 3 | **wiring** | The existing `ConceptShape::CandleMark` ([scene.rs:2955](../../../../frontend/chart-engine/src/scene.rs#L2955)); the work is routing and a display name, not a painter |
| `series_histogram` | series | 2 | **wiring** | `SeriesBars` inside `SubPaneSpec::Series` ([scene.rs:429](../../../../frontend/chart-engine/src/scene.rs#L429), struct [493](../../../../frontend/chart-engine/src/scene.rs#L493)), painted by `series_pane`; zero-anchored, signed |
| `segment` | event | 2 | **wiring** | The existing `ConceptShape::Segment` ([scene.rs:2928](../../../../frontend/chart-engine/src/scene.rs#L2928)) → `Line` + `Label`; the run direction and the display name are the work |
| `series_band` | series | 2 | **1 wiring, 1 new** | Overlay: `OverlayFillSpec` → `ScriptDraw::Band` ([scene.rs:468](../../../../frontend/chart-engine/src/scene.rs#L468), [2232](../../../../frontend/chart-engine/src/scene.rs#L2232)). Pane: `SubPaneSpec::Series` has no fills field and the pane mapper drops `spec.fills` ([app.js:6101-6106](../../../../frontend/app/app.js#L6101)) |
| `pair_link` | event | 1 | **new_rendering** | `Line` can join two points, but the second pane/series host does not exist on the chart; `/concept-events/smt` exists and has no caller |
| `series_dots` | series | 1 | **new_rendering** | No dot-series painter: `series_pane` draws connected traces; a per-bar point series, side-coloured, must be built |

### The entries that need more than a row

**`level_run` is one shape, not a segment with a parameter.** A `segment` says *a level exists
between these two bars*; a `level_run` says *this level exists from the bar that made it and runs
right by policy*. The visible length is part of the meaning — a level drawn as a 1.3px point is not
read as a level — so the run's start and its length policy are shape fields, not a cosmetic
setting. The reference documents the policy as a count (`Extend Imbalance`, default 10 bars), which
is why the run length is a parameter with a default; a row whose canonical says "until swept"
without a citation was corrected by Pass 1.

**`lifecycle_box` is one shape, not a static zone.** Its right edge is a lifecycle: `forming` and
`confirmed` reach forward, `mitigated` stops at the mitigation bar and is removed, and an
invalidated box flips polarity. The information already exists in the detector's statuses; the
renderer's two-bar box ignores it. The count of boxes shown last is a parameter, not a shape
property.

**`price_band` is not `lifecycle_box` and not `time_box`.** A lifecycle box's right edge is an
event; a price band's two x edges are the **measurement window** and the band persists as long as
that window does; a time box has no price extent at all. A price band has both price and time
extent and neither lifecycle nor removal.

**`time_box` and `time_line` are the clock's two shapes.** A time box says *this span matters* and
needs two edges, drawn full height so it never claims a price. A time line says *this bar is the
count* (or the release) and has one. An instant is never a zero-width box: the label would sit
inside a rectangle that is not there.

**`ladder` is one measurement, not five levels.** A run says *a level exists here*; a ladder says
*these levels are one measurement*, so it carries the rig a run does not — the anchor pair, the
ratio on each line, the price beside it, and the shaded slice between two rungs. Five independent
runs make a fib grid nine unrelated parallel lines, which is exactly what Pass 5 measured.

**`outline` covers the sloped pattern and the trendline.** The pivot path is a polyline, the neck is
a dashed line, the target is a projected line. One degenerate case is two or three named points
joined plus the projection; it does not need a `sloped_level` shape. `smc.trendline_sweep` is the
one row that cannot use it yet because the slope is discarded before the wire.

**`series_marks` is folded into `series_line`, not a fifth series shape.** Three rows draw marks on
a trace — `rsi_divergence`, `rsi_pca`, and the OBV/A-D divergences — and in every one the mark's y
is meaningful only relative to the line underneath it. `series_line` already carries `segments` and
`markers` members; a mark without a line would have to carry the line anyway.

## Scale

The scale is a property of the shape and its host, recorded per row and never invented:

- **`series`** — the registry decides: `pane: true` → `own_pane`; `overlay` true with `pane` false
  → `price`. A series concept with neither flag is a registry defect (Order Flow's `delta_shift`),
  and the row records the defect rather than choosing a scale for it.
- **`event` / `interval`** — the shape decides. Anything with a y carries `price` (marks, runs,
  boxes, paths, price bands). A full-height time box and a time line carry `time_only`, because
  they claim no price. `pair_link` carries `own_pane`: it joins two panes.
- Six shapes live on both frames by host: price bands and lifecycle boxes on price, full-height
  time boxes on time. The row's `scale` says which host that row uses; the entry says what to
  build.

## The warm-up contract

`warmup.bars` is the first bar index at which the detector can emit on the 501-bar scan, and
`warmup.before` says what is missing before it. The audit's rows span **0 to 248 bars**:

- **External-time concepts need no bars.** The six Economic Releases rows carry `bars: 0`: the mark
  is keyed to a release timestamp, and it needs only the bar whose `open_time` contains it.
- **Event concepts warm up on their lookback.** Candlestick patterns need 2-7 bars; chart patterns
  11-25; harmonics 11-14; SMC 2-60; Price Action 2-34; Fibonacci 12-26.
- **Series concepts warm up on their longest window.** Trend 12-201, Momentum 14-248, Statistical
  22-150, Volatility 21-60, Volume 16-40.
- **The shape is not drawn before warm-up is complete.** A renderer that seeds the first N bars
  with a neutral value is drawing a claim the detector has not made. The first N bars carry no
  shape, and a row with an empty `warmup.before` is an incomplete row.

## A readout is attached to something

A readout names its **host** — the bar, zone or band the figure is about — and the host's own shape
carries the geometry while the readout carries the number and the window it was measured over. A
row whose host is a `lifecycle_box` inherits that box's status; a per-bar figure like
`indicators.zscore` hangs on a bar and needs no box at all. The host decides the scale: Sessions'
`daily_bias` and `weekly_bias` hang on the period open and carry `price`, while
`htf_candle_bias` hangs on the completed higher-timeframe candle and carries `time_only` — one
shape, two hosts, no second shape invented to carry them.

**The `time_line` label is a readout member.** Pass 5's Fibonacci row labels the rule with its
count; Pass 12's six Economic Releases rows label it with the release figure, the forecast and the
surprise between them. The line is time-only; the figure rides the label; neither ever touches a
price. That is the same host rule one step further: the line is the host, and the number is the
readout.

## What this chapter refuses

- **No fifth kind.** The four kinds are declared once, here; the gate rejects a value it was not
  told about, so a hard concept cannot quietly become a new kind in a ledger row.
- **No `series_marks`.** Folded into `series_line`; a mark on nothing is not a shape.
- **No `sloped_level`.** `outline` covers the two-pivot line and its projection.
- **No zero-width `time_box`.** An instant is a `time_line`.
- **No price extent on a `time_box`, no lifecycle on a `price_band`.** The three boxes mean three
  different things, and conflating them is what made the sessions draw a *where* for a *when*.
- **No candle-colouring convenience.** The reference's `Color Candles` toggle behind a BOS/CHoCH
  read is a candle colour, not a concept shape; it is recorded in Pass 1 and left alone.
- **No shape that knows its venue.** Provenance is on the signal; the geometry is shared.

## Definition of done — the close-out, 2026-10-31

`node tools/concept_visual_audit.mjs check` sweeps the union and prints
`ok 182 row(s) complete across 16 families`. The spec's five items, one line at a time:

1. **Our output is photographable — met by substitution, not by the named file.**
   `tools/chart_preview.mjs` still renders a band-DSL `concepts` document, not `concept_shapes`.
   The photograph Task 4 built lives in `tools/concept_visual_audit.mjs render`: it sends
   `concept_shapes` through the real wasm ([mjs:331](../../../tools/concept_visual_audit.mjs#L331)),
   inlines the page's own `frontend/app/concept_shapes.js`
   ([mjs:315](../../../tools/concept_visual_audit.mjs#L315)), and paints it under and over in the
   app's order. The plan's wording — "the same shape `chart_preview.mjs` writes" — made the
   preview tool the model, not the renderer. This is the one DoD clause that is not literally
   true; the guarantee it existed for, that the audit cannot draw a concept better than the app,
   is.
2. **The chapter and the passes — met.** The four kinds, the tier rule (*a shape does not know
   which venue fed it*), and the warm-up contract are above. Twelve passes are written as sixteen
   family sections — Pass 10 covered five families — and every row carries a citation or a
   `no_convention` reason, a render on disk, a kind, and both verdicts.
3. **182 complete rows and a passing gate — met.** Zero incomplete; rule 4's
   `continuous`-on-`event` guard ([mjs:445](../../../tools/concept_visual_audit.mjs#L445)) is
   enforced and covered by the fixtures. Nothing is left at a seeded value.
4. **The shape additions are implementable and their status is honest — met.** Seventeen shapes,
   each with its status and, where wired, the arm that paints it: 126 rows are `wiring`, 56 are
   `new_rendering` and name what is missing — a painter, a field on the wire, or a host.
5. **The three systems and the parameter gap are recorded and untouched — met.** The spec's
   *Ground truth* names the registry, the band DSL and the composition; the ledger records
   156 of 182 rows with `parameters.declared: []`; and [01-smc](01-smc.md) photographs the
   catalog's `swing_lookback: 10` against the engine's real 3. No production file changed during
   the passes. The one code move — `concept_shapes.js` out of `app.js` — was Task 2, before
   pass 1, so the photograph uses the page's own painter.

The plan's four defects, each with its photograph:

| Defect | Where it is photographed |
| --- | --- |
| The concept path ignores `pane`/`overlay` | The scale rule here; [05-fib](05-fib.md), [06-chart](06-chart.md), [07-harmonics](07-harmonics.md), [10-trend](10-trend.md), [14-order-flow](14-order-flow.md), [15-statistical](15-statistical.md) |
| `app.js:6066` — the `indicators.*` route | The cited line predates the extraction. Today the route is [concept_shapes.js:80](../../../../frontend/app/concept_shapes.js#L80) (`indicators.*` becomes a `candle_mark` on the price pane), and the series path beside it is [app.js:6073](../../../../frontend/app/app.js#L6073) |
| Catalog default versus engine default | [01-smc](01-smc.md): declared 10, replayed at 3; [06-chart](06-chart.md) for the gap detector's own 3 |
| 156 concepts with no parameters | The count above, read from the ledger; rule 9 makes `[]` the honest declaration; the spec's *Ground truth* argues what it means |

What could not be met, and why: DoD 1's literal file name, as above. Nothing else failed. What
this chapter does not do — design the concept-state container, act on a `fires` disagreement, or
give the three systems one definition — the spec put out of scope before the first pass, and the
rows record the evidence those designs will need.
