# Drawing-tool parity implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** take the chart's hand-placed drawing tools from 17 kinds to 80 — TradingView's documented object set, grouped the way TradingView and LuxAlgo/Vela group it — so a trader leaving those platforms finds every tool they already know.

**Architecture:** replace the three named anchors with a point list whose arity the kind declares, split the `shapes` match into one geometry module per tool group, keep `tool_registry()` over the ABI as the only vocabulary the toolbar, storage and AI read from, and add a completeness gate that refuses a registry kind whose geometry is a stub.

**Tech stack:** Rust (`chart-engine` cdylib → wasm, `db`, `api-gateway`, `ai-agent`), vanilla-JS shell (`frontend/app/app.js`), Postgres migrations, Node harnesses under `tools/`.

**Spec:** [docs/superpowers/specs/2026-10-30-drawing-tools-parity-design.md](../specs/2026-10-30-drawing-tools-parity-design.md) — the catalog, the citations and the eight decisions (D1-D7) it argues for. This plan executes those decisions; it does not re-litigate them.

## Global constraints

- Work directly on `main` (the local gateway at :8080 serves this working tree). No worktree.
- Do **not** run `cargo fmt --all` — the repo is not rustfmt-clean and it rewrites 100+ unrelated files.
- Do **not** run `tools/guard_check.py` mid-session (it mutates `frontend/app/app.js`).
- Every gate runs with `NODE_PATH="/Users/macbook/.workbuddy-node/node_modules"`: `tools/shell_check.mjs`, `tools/ui_check.mjs`, `tools/wasm_abi_check.mjs`, `tools/component_check.mjs`, `tools/check_builder.mjs`, `tools/concept_visual_audit_check.mjs`.
- The wasm is checked in. After any Rust change to `chart-engine`: `cargo xtask build-frontend` (builds wasm32-unknown-unknown release and copies to `frontend/app/chart_engine.wasm`), then re-run the wasm-dependent gates. Commit the rebuilt wasm with the change that needed it.
- One commit per task; never push except at the final task (the product owner authorised the push for this task alone).
- No colour literal outside `tokens.css`; no new group trigger without a sprite symbol (`ui/icons.js`).
- Every new kind must be judged against a cited reference behaviour in `reports/drawing-tools/ledger/drawing_tools.json` — the same discipline as the concept audit: an uncited row is this project's opinion wearing a citation.

## File map

| File | Responsibility |
| --- | --- |
| `frontend/chart-engine/src/drawing.rs` | the object vocabulary: `DrawingKind`, `AnchorCount`, `ToolGroup`, `ToolSpec`/`REGISTRY`, `Drawing`, `DrawingOptions`, `DrawingPart`, `Anchor` |
| `frontend/chart-engine/src/draw/mod.rs` | the exhaustive `match kind` that dispatches to a group module — the compiler's guarantee that no kind is unimplemented |
| `frontend/chart-engine/src/draw/{lines,channels,pitchforks,fibonacci,gann,patterns,elliott,cyclic,measurement,shapes,annotation}.rs` | one group's geometry: resolved points + `Frame` + `DrawingOptions` → `Vec<DrawingPart>` |
| `frontend/chart-engine/src/scene.rs` | keeps `place`, `resolve`, the `Frame`, snapping and the handle emission; `shapes()` moves out to `draw/mod.rs` |
| `crates/db/migrations/0018_drawing_points.sql` + `crates/db/src/drawings.rs` | one `points` JSONB column, milliseconds at the boundary, `KINDS` and the single arity rule |
| `crates/api-gateway/src/drawing_routes.rs` | the HTTP door: validate with the engine's own rule |
| `crates/ai-agent/src/tools.rs` + `skills/tool/chart-drawing.yaml` | the agent's drawing vocabulary, which is the registry |
| `frontend/app/app.js` | toolbar built from the registry, the placement machine, per-kind colours, hit-testing |
| `frontend/app/index.html` + `styles/*` | the five-button fallback, group triggers, flyout CSS |
| `tools/shell_check.mjs` + `tools/wasm_abi_check.mjs` | the DOM/ABI contract, derived from the registry rather than literal |
| `reports/drawing-tools/` | the ledger (`drawing_tools.json`), its gate, and the per-group evidence PNGs |

---

## Task 1: Derive the harness from the registry (do this before any new kind)

**Files:** modify `tools/shell_check.mjs` (`:1679` counts 18 `data-tool` buttons, `:1688-1690` lists all 17 names, `:1697` counts 17 flyout buttons, `:2242` counts 4 group triggers), `tools/wasm_abi_check.mjs` (`:658-770`).

**Interfaces:**
- Consumes: `tool_registry()` over the ABI — the shell already calls it once at engine load (`app.js:5063 buildToolbarFromRegistry`).
- Produces: a harness that asserts *DOM equals registry* for any size, so Tasks 3-11 add kinds without editing the harness again.

- [ ] **Step 1:** in `wasm_abi_check.mjs`, assert `tool_registry()` parses as an array whose rows each carry `kind, label, title, group, group_label, anchors`, and that `anchors` is one of the shapes Task 2 defines (`Fixed(n)` / `Many{min}` — during this task the current integer is accepted, and Task 2 updates the assertion in the same commit it changes the wire).
- [ ] **Step 2:** in `shell_check.mjs`, replace the four magic numbers with: the set of `data-tool` values equals `registry.map(t => t.kind)` plus `cursor`; each flyout's button list equals its group's rows in registry order; the group-trigger list equals the registry's distinct `group` values in order.
- [ ] **Step 3:** add the assertion Task 1's gap analysis says is missing — *every registry kind has a colour key in `CHART_PALETTE_DARK` and `CHART_PALETTE_LIGHT`* (read the two objects out of `app.js`; today a missing key silently falls back at `app.js:2819`).
- [ ] **Step 4:** run both harnesses against the **unmodified** wasm and confirm green: `NODE_PATH=/Users/macbook/.workbuddy-node/node_modules node tools/shell_check.mjs` and `… tools/wasm_abi_check.mjs`. Expected: green, because the derivation must be behaviour-preserving.
- [ ] **Step 5:** commit `test: the drawing harness reads the registry, not a list of names` (plus the docs/21 paragraph saying why the literals went away).

## Task 2: The N-point object model, end to end

The one task that changes every interface. Do not add a kind here.

**Files:** `drawing.rs`, `scene.rs`, `lib.rs`, `crates/db/src/drawings.rs`, `crates/db/migrations/0018_drawing_points.sql`, `crates/api-gateway/src/drawing_routes.rs`, `crates/ai-agent/src/tools.rs`, `skills/tool/chart-drawing.yaml`, `app.js`, `index.html` (fallback buttons only if the fallback list is stale), both harnesses.

**Interfaces:**
- Produces:
  - `pub enum AnchorCount { Fixed(u8), Many { min: u8 } }` with `fn points_for(&self) -> Option<usize>` and `fn accepts(&self, n: usize) -> bool`.
  - `DrawingKind::anchors() -> AnchorCount` replacing `needs_second_anchor`/`needs_third_anchor` (**delete** both, engine and storage).
  - `struct Drawing { id, kind, points: Vec<Anchor>, label, selected, options: DrawingOptions }` — `a1/a2/a3` are gone from the wire.
  - `SceneDrawing { …, points: Vec<ScenePoint> }` where `ScenePoint { absolute: {time, price}, fraction: {x, y} }`; `drawing_parts`/`place`/`resolve` take the list.
  - `ToolSpec.anchors: AnchorCount`.
  - `db::drawings::KINDS` stays a closed list; `point_count(kind) -> AnchorCount` is the new mirror, pinned to the engine by the existing gateway test.
  - HTTP body: `{ kind, points: [{time, price}, …], label, options }`; `create_drawing`'s agent argument the same.
- Consumes: nothing new.

- [ ] **Step 1 — the failing tests first.** In `drawing.rs`'s test module, write: `a_kind_takes_exactly_the_points_it_declares` (a 2-point kind given 1 or 3 is refused with a message naming the count), `a_many_kind_accepts_two_five_and_forty`, `the_arity_rule_is_the_only_one` (assert `DrawingKind::ALL.iter().all(|k| k.anchors().accepts(declared))` against `REGISTRY` — engine and registry cannot disagree), and `validate_names_the_problem` (a `NaN` point, a mixed absolute/fraction list). Run `cargo test -p chart-engine` and watch them fail to compile — that is the red this task starts from.
- [ ] **Step 2:** implement `AnchorCount`, `Drawing::points`, `Drawing::validate_anchors`, `DrawingKind::anchors()`, `ToolSpec.anchors`, and `DrawingOptions` (the seven fields of D4, `#[serde(default)]`, defaults cited in the doc comments with the URL that states them).
- [ ] **Step 3:** move `shapes()` out of `scene.rs` into `draw/mod.rs` with the same exhaustive match and one arm calling a group function per kind; create `draw/{lines,channels,pitchforks,fibonacci,gann,patterns,elliott,cyclic,measurement,shapes,annotation}.rs` where the four groups with existing kinds get their real arms moved verbatim (lines: trendline/ray/extended/hline/vline/angle; shapes: rect/triangle/circle/arc; measurement: measure/dateprice_range/fib/fib_extension — keep fib where the current code lives but under `fibonacci.rs`) and the empty groups get a module whose functions return `vec![]` **and which no registry row points at yet**. Update `scene.rs`'s call site (`:2007`), `place` (`:3467`), `resolve` (`:3550`) and the handle emission (`:4050-4079`) to iterate the list. `lib.rs`'s re-exports and `tool_registry()` serialise the new shapes.
- [ ] **Step 4:** the migration. `0018_drawing_points.sql`, one transaction: `ADD COLUMN points JSONB NOT NULL DEFAULT '[]'`, backfill in declaration order (`a_time/a_price` then `a2_*` if not null then `a3_*`), then `DROP COLUMN` the six and both `CHECK` constraints. Add a Rust test that runs the backfill SQL against a fixture of a 1-, 2- and 3-anchor row and asserts the JSON list length and order. Milliseconds stay the unit, and the header comment in `db/src/drawings.rs` gets its "two anchors -- sometimes three" prose rewritten to the point list with the same 2^53 reasoning kept.
- [ ] **Step 5:** the doors. `drawing_routes.rs`'s `DrawingBody` takes `points`, validates with `Drawing::validate_anchors` (unchanged delegation), `kind_from_name` still iterates `DrawingKind::ALL`. `ai-agent/src/tools.rs:1307`'s free-text kind list becomes generated from the registry at startup rather than re-typed, because a 80-item list typed twice is a list that drifts.
- [ ] **Step 6:** the shell. `placing` → `{ id, kind, fixed: [], pending }`; `anchorsNeeded()` reads `toolRegistry`; `startPlacing` fixes a point per click until `accepts(fixed.length)`; Enter and a double-click finish a `Many` kind; `fractionsOf`/`shapeKey`/`anchorPrice`/`resolvedDrawing` read `points`; the PUT body sends `points`. Escape still cancels; single-shot arming unchanged.
- [ ] **Step 7:** rebuild and re-verify: `cargo test -p chart-engine && cargo test -p db && cargo test -p api-gateway --lib drawing_routes && cargo xtask build-frontend`, then all six Node gates with `NODE_PATH`. `wasm_abi_check.mjs`'s `anchors` assertion (Task 1 Step 1) is updated here.
- [ ] **Step 8:** commit `feat: a drawing is a list of points, and the kind says how many` with the rebuilt wasm and the migration.

## Task 3: Lines — the six trend tools

**Kinds:** `hray`, `crossline`, `infoline`, `regression`, `arrow`, `arrow_mark_up`, `arrow_mark_down` (7 new; `angle` stays where it is).
**Files:** `drawing.rs` (variants + `ALL` + `name()` + registry rows in group `lines`), `draw/lines.rs`, `db::drawings::KINDS`, `app.js` palettes, `reports/drawing-tools/ledger/drawing_tools.json`, `draw/lines.rs` tests.
**Cited geometry:** TV [horizontal ray](https://www.tradingview.com/support/solutions/43000518121-horizontal-ray-drawing-tool/) — one anchor, extends left→right only; [crossline](https://www.tradingview.com/support/solutions/43000477747-crossline-drawing-tool/) — one anchor, two perpendicular full-plot lines, *"split the chart into quadrants in one click"*; [info line](https://www.tradingview.com/support/solutions/43000791601-info-line-drawing-tool/) — a level plus a floating box with both points' price and bar; [regression trend](https://www.tradingview.com/support/solutions/43000518108-regression-trend-drawing-tool/) — a least-squares line over the bars between two anchors, with the *Pearson's R* readout; [arrow](https://www.tradingview.com/support/solutions/43000518134-arrow-drawing-tool/) — a segment with an arrowhead at the end (and *Middle Point*/*Price labels*); [arrow marks](https://www.tradingview.com/support/solutions/43000518087-arrow-marks-drawing-tools/) — a fixed-size glyph at one point, *"size … is fixed and cannot be changed"*, green up / red down.
**Tests:** each kind on a fixed `Frame` — the regression line's endpoints come from the candles between the anchors (assert the slope on a rising ramp and `R = 1`), the arrowhead is a polygon whose apex is at the second anchor, `hray` clips at the plot's right edge and not the left, `crossline` emits exactly two `segment`s plus the four quadrant edges inside the plot, the marks' pixel size does not change with the price scale.

## Task 4: Channels and pitchforks

**Kinds:** `disjoint_channel`, `flat_top_bottom`, `pitchfork`, `schiff_pitchfork`, `modified_schiff_pitchfork`, `inside_pitchfork`; new groups `channels` and `pitchforks`.
**Cited geometry:** [disjoint channel](https://www.tradingview.com/support/solutions/43000791604-disjoint-channel-drawing-tool/) — two parallel lines whose segments start and end independently; [flat top/bottom](https://www.tradingview.com/support/solutions/43000791603-flat-top-bottom-drawing-tool/) — *"click a third time to place the flat, horizontal boundary"*, filled between; [pitchfork](https://www.tradingview.com/support/solutions/43000518141-pitchfork-drawing-tool/) — Andrews' median line from three pivots (median from A through the midpoint of BC, two parallel sides through B and C); [Schiff](https://www.tradingview.com/support/solutions/43000518140-schiff-pitchfork-drawing-tool/) — median shifted to 50% of B–C; [modified Schiff](https://www.tradingview.com/support/solutions/43000791609-modified-schiff-pitchfork-drawing-tool/) — 50% of A–C; [inside](https://www.tradingview.com/support/solutions/43000518146-inside-pitchfork-drawing-tool/) — the inverted template.
**Tests:** the four pitchforks share one function and differ only by the median's origin — assert four *different* median y-values on one fixed triangle, which is the test that catches a fork quietly drawn as another; the disjoint channel's two segments have different x-ranges; flat top/bottom fills the region its three points define.

## Task 5: Fibonacci — ten more levels, and the ladder the audit already judged

**Kinds:** `trend_fib_extension`, `trend_fib_time`, `fib_time_zone`, `fib_channel`, `fib_fan`, `fib_arcs`, `fib_circles`, `fib_spiral`, `fib_wedge`, `pitchfan`; new group `fibonacci` (moving `fib` and `fib_extension` rows into it).
**Cited geometry:** TV [fib retracement](https://www.tradingview.com/support/solutions/43000518158-fibonacci-retracement-drawing-tool/) (*Prices*, *Levels*, *Reverse*, *Extend Lines Left/Right*, *log scale*), [trend-based extension](https://www.tradingview.com/support/solutions/43000518137-trend-based-fib-extension-drawing-tool/) (3 points, levels measured along the price/time slope), [trend-based fib time](https://www.tradingview.com/support/solutions/43000518136-trend-based-fib-time-drawing-tool/), [fib channel](https://www.tradingview.com/support/solutions/43000791610-fibonacci-channel-drawing-tool/) (*"first two points … the third to define the width"*), [fib time zone](https://www.tradingview.com/support/solutions/43000518155-fib-time-zone-drawing-tool/), [speed resistance fan](https://www.tradingview.com/support/solutions/43000518156-fib-speed-resistance-fan-drawing-tool/) and [arcs](https://www.tradingview.com/support/solutions/43000518157-fib-speed-resistance-arcs-drawing-tool/) — the price/time pair, *"Levels"* as values or percents, [circles](https://www.tradingview.com/support/solutions/43000518159-fib-circles-drawing-tool/) (*"drawn at 11 different levels"*), [spiral](https://www.tradingview.com/support/solutions/43000791611-fibonacci-spiral-drawing-tool/) (logarithmic, `Counterclockwise`), [wedge](https://www.tradingview.com/support/solutions/43000518153-fib-wedge-drawing-tool/), [pitchfan](https://www.tradingview.com/support/solutions/43000518143-pitchfan-drawing-tool/) (rays with fib coefficients from a pivot).
**Also here:** the `level_profile` option (`classic`/`extension`/`ote`/`time`) — which is pass 5's finding applied to the hand tool: the ratio set the trader expects, labelled with its ratio and its price, not an anonymous dash.
**Tests:** one ratio table, asserted against the reference numbers; a fan's three lines have the 45°-family slopes; the arcs and circles are `ellipse` parts at the fib radii; the spiral is a polyline whose radius grows by φ per quarter turn; the time tools put their levels on the **x** axis (a test that fails if a time tool is drawn as a price, which is the defect the audit found in `fib.time_zones`).

## Task 6: Gann — four tools that need a price/time ratio stated out loud

**Kinds:** `gann_box`, `gann_fan`, `gann_square`, `gann_square_fixed`; new group `gann`.
**Cited geometry:** [gann box](https://www.tradingview.com/support/solutions/43000518152-gann-box-drawing-tool/) — price×time partitions with corner angle rays and *Reverse*; [gann fan](https://www.tradingview.com/support/solutions/43000518151-gann-fan-drawing-tool/) — nine angles 1×1 … 16×1; [gann square](https://www.tradingview.com/support/solutions/43000518149-gann-square-drawing-tool/) — *Levels, Fan, Arcs* plus the *Price/Bar Ratio*; [gann square fixed](https://www.tradingview.com/support/solutions/43000791675-gann-square-fixed-drawing-tool/) — levels 0 through 5, the second anchor *"sets the scale"*. Both pages insist on *"a proper scale ratio, so that a unit of price would correspond a unit of time"* — which is why the engine, not the shell, computes these: `Frame` is the only thing holding both scales.
**Tests:** the 1×1 line is the one whose price delta per bar equals the box's declared ratio; the nine fan angles are nine distinct slopes; the square's arcs are `ellipse` parts centred on the anchor; a fixed square's five levels are evenly spaced in the *time* dimension it documents.

## Task 7: Patterns, Elliott and cyclic

**Kinds:** `xabcd` (with `preset`), `abcd`, `cypher`, `head_shoulders`, `triangle_pattern`, `three_drives`, `elliott_impulse`, `elliott_correction`, `elliott_triangle`, `elliott_double_combo`, `elliott_triple_combo`, `cyclic_lines`, `time_cycles`, `sine_line`; new groups `patterns`, `elliott`, `cyclic`.
**Cited geometry:** [XABCD](https://www.tradingview.com/support/solutions/43000569909-xabcd-pattern-drawing-tool/) with its published leg ratios (Gartley `AB = 61.8% XA`, `BC = 61.8-78.6% AB`, `CD = 127-161.8% BC`; Bat `AB = 0.382/0.5 XA`, `CD = 1.618/2.618 BC`; Crab; [Cypher](https://www.tradingview.com/support/solutions/43000791680-cypher-harmonic-pattern-drawing-tool/) `B = 0.382-0.618 XA`, `C = 1.13-1.414 XA`, `D = 0.786 XC`) — plus Vela's ✓/✗ leg badge; [ABCD](https://www.tradingview.com/support/solutions/43000570202-abcd-pattern-drawing-tool/); [triangle pattern](https://www.tradingview.com/support/solutions/43000570208-triangle-pattern-drawing-tool/) (4 points); [three drives](https://www.tradingview.com/support/solutions/43000570172-three-drives-pattern-drawing-tool/) (TV's Coordinates list runs to Price 7); [head and shoulders](https://www.tradingview.com/support/solutions/43000570149-head-and-shoulders-drawing-tool/) + neckline; the five [Elliott](https://www.tradingview.com/support/solutions/43000653212-elliott-wave-theory-and-tools/) tools; [cyclic lines](https://www.tradingview.com/support/solutions/43000518161-cyclic-lines-drawing-tool/) (*"set two vertical lines a specified distance apart"*, repeated into the future), [time cycles](https://www.tradingview.com/support/solutions/43000791682-time-cycles-drawing-tool/) (alternating shaded bands), [sine line](https://www.tradingview.com/support/solutions/43000791685-sine-line-drawing-tool/) (second anchor sets cycle length *and* amplitude).
**Tests:** a Gartley drawn on its ideal ratios badges every leg ✓, and one nudged leg badges ✗ (this is the test that proves the leg maths is real); each Elliott kind labels its own wave names — impulse `1..5`, correction `A..C`, triangle `A..E`, double `W,X,Y`, triple `W,X,Y,X,Z` — and none of them labels another's sequence; cyclic lines repeat to the plot's right edge and stop there; the sine's amplitude equals the second anchor's price delta.

## Task 8: Forecast and measurement — the tools that carry data

**Kinds:** `price_range`, `date_range`, `forecast`, `sector`, `fixed_range_vp`, `anchored_vp`, `anchored_vwap`; the group `measurement` absorbs `measure` and `dateprice_range`.
**Cited geometry:** [date and price range](https://www.tradingview.com/support/solutions/43000516996-date-and-price-range-drawing-tools/) (three tools under one article, with the *Stats* checklist); [position forecast](https://www.tradingview.com/support/solutions/43000517004-position-forecast-drawing-tool/) (source → target, success/failure styling that resolves as time passes); [sector](https://www.tradingview.com/support/solutions/43000516995-sector-drawing-tool/) (three points, a projection cone); [fixed range volume profile](https://www.tradingview.com/support/solutions/43000707985-fixed-range-volume-profile-drawing-tool/) (*Rows*, *Volume*, *Value area volume*, *Width (% of the box)*, *POC/VAH/VAL*, *Developing POC*); [anchored VP](https://www.tradingview.com/support/solutions/43000707989-anchored-volume-profile-drawing-tool/) (one anchor, runs to the last bar); [anchored VWAP](https://www.tradingview.com/support/solutions/43000669764-anchored-vwap-drawing-tool/) (*"Click on the chart to select the point from which you'd like to start"*).
**Reuse, do not re-implement:** `analytics_core::volume_profile::calculate_volume_profile_from_candles` (volume_profile.rs:217) and `analytics_core::vwap::calculate_anchored_vwap` (vwap.rs:40) already do this maths for the concept engine; a drawing tool that recomputes them differently is the same bug the concept audit kept finding, in a second surface. The scene already holds the candles, so the call is in `draw/measurement.rs`.
**Tests:** the profile's POC is the densest row and VAH/VAL bracket the documented 70% value area on a fixture histogram; the anchored VP and the fixed-range VP over the same bars agree row for row; AVWAP equals `Σ(price·vol)/Σ(vol)` from the anchor bar and ignores bars before it; the forecast switches to its failure styling once the price has passed the target.

## Task 9: Shapes — the seven the toolbar is missing

**Kinds:** `rotated_rect`, `ellipse`, `polyline` (`Many`, `closed` option), `curve`, `double_curve`, `brush` (`Many`, freehand), `highlighter`; existing `rect`/`triangle`/`circle`/`arc` move into the `shapes` module unchanged.
**Cited geometry:** [rotated rectangle](https://www.tradingview.com/support/solutions/43000791738-rotated-rectangle-drawing-tool/) — *"first corner … a second time … a third time"*; [ellipse](https://www.tradingview.com/support/solutions/43000516988-ellipse-drawing-tool/) — two corners of the bounding box, Shift makes it a circle; [polyline](https://www.tradingview.com/support/solutions/43000516986-polyline-drawing-tool/) — unlimited points, closed when the last meets the first, finished by double-click/right-click/Esc; [curve and double curve](https://www.tradingview.com/support/solutions/43000791740-curve-and-double-curve-drawing-tools/) — one and two control points (quadratic Bézier, sampled); [brush](https://www.tradingview.com/support/solutions/43000516987-brush-drawing-tool/) and [highlighter](https://www.tradingview.com/support/solutions/43000791735-highlighter-drawing-tool/) — freehand capture.
**Tests:** the rotated rect's four corners come from three clicks (the fourth is the vector sum — assert it); a curve through the same three points as a quadratic Bézier hits the control-point-weighted midpoint; `Many` kinds accept 2, 5 and 40 points and the renderer never emits a `NaN`; the highlighter is the brush's geometry with its own width and alpha.

## Task 10: Annotation — the six marks and labels

**Kinds:** `text`, `note`, `callout`, `price_label`, `flag_mark`, `icon`.
**Cited geometry:** [text](https://www.tradingview.com/support/solutions/43000516983-text-drawing-tool/), [note](https://www.tradingview.com/support/solutions/43000737571-note-drawing-tool/) (*"anchored to a price"*), [callout](https://www.tradingview.com/support/solutions/43000516978-callout-drawing-tool/) (two points: the box and its target), [price label](https://www.tradingview.com/support/solutions/43000518083-price-label-drawing-tool/) (*"always contains the price"*, auto-rendered), [flag mark](https://www.tradingview.com/support/solutions/43000518085-flag-mark-drawing-tool/) and [icons/emoji](https://www.tradingview.com/support/solutions/43000662396-how-to-chart-with-emojis/).
**Also here:** the merged rows from the spec (`comment`, `price_note`, `signpost`, `pin`, `arrow_marker`, `path`) get their ledger entries with `merged_into` naming the kind that carries them, so the catalog is honest about 80 vs 87.
**Tests:** the price label's text is the engine's resolved price and changes when the anchor moves — the one annotation kind whose text is data, not the user's words; `text`/`note`/`callout` render the `label` field, which already exists on `Drawing`, instead of inventing a second text store.

## Task 11: The completeness gate and the catalog

**Files:** `tools/drawing_tools_check.mjs`, `reports/drawing-tools/ledger/drawing_tools.json`, `draw/mod.rs` test module, `docs/21-CHART-OBJECT-ENGINE.md`, `docs/14-FRONTEND-CHART-ENGINE.md`.
- [ ] `every_registry_kind_draws_something` in Rust: build each kind from distinct points, assert ≥1 non-handle part, assert the parts move when an anchor moves, assert no `NaN`, and (for `Many`) 2/5/40 points.
- [ ] `every_options_field_is_read`: a field `DrawingOptions` declares but no `draw/` arm consults is deleted.
- [ ] The ledger + gate: one row per TradingView and Vela object — `reference`, `url`, `ours` (kind | `merged_into` | `out_of_scope` with a reason), `verified` date. `node tools/drawing_tools_check.mjs` fails on a reference row with no disposition, and the gate joins `tools/check_builder.mjs`'s sweep.
- [ ] docs/21: the parity section rewritten for the point model (it still says *"Eight kinds"* at :31 and "sixteen kinds" in the 2026-09 section); docs/14's tool table (`:400-430`) regenerated from the registry.

## Task 12: Verify in the browser, then push

- [ ] `cargo test -p chart-engine -p db -p api-gateway -p ai-agent`, `cargo xtask build-frontend`, all six Node gates.
- [ ] Gateway restart (needed for the Rust and migration changes; this is the moment the product owner asked to be able to test on their return), then a browser pass with the throwaway account: arm **every** group, place one tool from each, drag a handle of a 5-point pattern, finish a polyline with Enter, draw a brush stroke, delete and undo it. One PNG per group in `reports/drawing-tools/evidence/`.
- [ ] Check the toolbar itself at 1440, 1024 and 768 with 12 flyouts, and in light theme; the 80-button toolbar is the UI risk of this change and it is judged from a screenshot, not from CSS.
- [ ] Confirm the 17 pre-existing drawings still load (the migration's backfill is the risk: draw one of each arity before the migration in the dev DB, and check it renders after).
- [ ] Commit, then `git push origin main` — authorised for this task only.
