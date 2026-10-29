# Pass 5 — Fibonacci (6 rows)

Judged 2026-10-29 against `main`. The machine-readable verdicts are the 6 rows of
[`ledger/fib.json`](../../../../reports/concept-visual-audit/ledger/fib.json);
`node tools/concept_visual_audit.mjs check --family fib` reports **6 rows, 0 incomplete**.

**Headline: the arithmetic is the reference's; the drawing is not.** Both retracement
generations on this window fit the candles to the cent — the bearish leg from 81380.01 (bar 473)
to 79710.00 (bar 484), R = 1670.01, puts its five levels at exactly `end + r × (origin − end)`,
and the bullish leg 79710.02 → 80527.02, R = 817, does the same — and the five ratios are
TradingView's and LuxAlgo's five. What the chart draws is 50 dashed 1px lines at 10 heights, all
of them inside the last 5.4% of the plot width, each labelled `retracement`, 50 times. No line
carries a ratio, no line carries a price, and no line is long enough to read as a level: the
longest is 49px, the shortest 28px. A trader who knows Fibonacci will not recognise the tool.

**Every fib concept is a state shipped as one-shot events, re-announced on every bar.** The
retracement grid belongs to the newest completed leg and exists for as long as that leg is the
newest one; the detector instead re-emits it at every bar, so the same five levels arrive five
times a bar. The consequences measure cleanly:

- The retracement's 50 events are **5 events a bar over 10 bars** (491-500) from **2 anchors**,
  so the chart shows ten bars of Fibonacci history, however far back the leg runs.
- The pocket and the OTE band emit **once a bar**, so their 50 events reach back **50 bars**
  across **9 distinct swings** — the same family, a 5× difference in visible history, decided by
  push rate rather than by time.
- The 0.4% confluence tolerance emits **up to five bands a bar** over 11 bars, from two swings.
- Time zones are the extreme: `if at <= bar_time` has no equality test, so each count
  re-fires on every bar after it is due, and the seven counts collapse into **3-5 events on the
  same bar**, all pinned at the emitting bar's close.

**And the one concept that is not a price at all is drawn as a price.** Fib time tools place
**vertical lines** at bar counts; `fib.time_zones` carries `price_level = last.close`, the router
has no zone id for it, and the segment branch draws a horizontal dashed line at the close of the
bar that emitted it. 50 events, 12 distinct heights, all in the last 5.4% of the width — a
`when` answered as a `where`, exactly the failure pass 4 recorded for the session clock, in a
second family. This is the row that cannot be drawn at all in today's vocabulary
(`impossible_in_vocabulary`) and the reason this pass adds `time_line` to it.

## How it was measured

Same window and route as passes 1-4: `GET /concept-events/scan?symbol=BTCUSDT&timeframe=15m&limit=500`
— 501 bars (2026-10-24T01:45 to 2026-10-29T06:45 UTC), 4,197 events across 135 concepts, the same
three caps ([concept_routes.rs:215-266](../../../../crates/api-gateway/src/concept_routes.rs#L215)):
replay from bar 22, at most 50 events per concept keeping the newest, `EventConfig::default()`.
`node tools/concept_visual_audit.mjs render fib` photographs the six; every number below comes
from that same fetch:

| concept | events | shapes | draws | span (bars) min/med/max | zero-span | distinct levels | longest run |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `fib.retracement` | 50 | 50 | 100 | 13 / 18.5 / 23 | 0 | 10 | 6 |
| `fib.golden_pocket` | 50 | 50 | 100 | 3 / 9.5 / 23 | 0 | 10 | 10 |
| `fib.extension` | 50 | 50 | 100 | 4 / 14 / 23 | 0 | 10 | 10 |
| `fib.confluence` | 50 | 50 | 100 | 9 / 17 / 23 | 0 | 14 | 10 |
| `fib.time_zones` | 50 | 50 | 100 | 13 / 18 / 23 | 0 | 12 | 1 |
| `fib.ote` | 50 | 50 | 100 | 3 / 9.5 / 23 | 0 | 10 | 10 |

"6 drawn, 0 fired-but-routed-nothing, 0 silent on this window." All 300 events answer
`status: "confirmed"`; every one is capped at exactly 50, which is the cap, not the count — the
detector produces a fib event on **every** bar from bar 12 onward, so the number this family
shows is always the cap. The render pages are working artifacts and stay gitignored;
[the six on the real chart](evidence/pass5-chart.png),
[the pile at the right edge](evidence/pass5-chart-right.png) and
[the six card previews](evidence/pass5-cards.png) are committed because they are the finding.

**One transport fact had to be cleared first: on the 1m chart nothing paints.** The first
photograph was blank with all six chips enabled. `scan?timeframe=1m&limit=500` returns
`{"symbol":"BTCUSDT","events":[]}` — an empty replay — while the same route at 1m with
`limit=50`, `200` or `1000` returns events, and at 15m/500 returns 4,197. The frontend omits
`limit` ([app.js:5841](../../../../frontend/app/app.js#L5841)), so a 1m chart asks for the one
window that comes back empty. That is a fetch defect, not a fib one; this pass switched the
chart to 15m, which is the timeframe the render and every other pass used, and records the 1m
gap here rather than in a row.

**Two facts about the rendering path decide everything below.** `fib.golden_pocket`,
`fib.confluence` and `fib.ote` are in `CONCEPT_ZONE_IDS` and lower to a Box plus a Label
([concept_shapes.js:27](../../../../frontend/app/concept_shapes.js#L27)); the other three are
not, so they lower to a horizontal `Segment` and its Label
([concept_shapes.js:67](../../../../frontend/app/concept_shapes.js#L67)). And every label is
`concept_id.split(".")[1]` — the snake_case half of the id, never a ratio
([concept_shapes.js:63](../../../../frontend/app/concept_shapes.js#L63)). Those two branches
produce the whole picture.

## What the reference set says

Six LuxAlgo Library concept pages and one TradingView help page carry this family. All were read
on 2026-10-29; every sentence below is verbatim. They agree on one structural point the render
table contradicts: **a fib study is a labelled grid drawn once per anchored leg, and its members
are slices of that grid, not separate tools.**

1. **A retracement is horizontal levels, labelled, with an anchor line.** "The vertical distance
   between those two points is then divided up vertically with horizontal lines placed at key
   levels of 23.6%, 38.2%, 61.8% and 100%"; the tool's own settings list a **Trend Line** toggle
   ("the visibility of the trend line"), **Prices** ("Toggles the visibility of the level's price
   beside the level"), **Levels** ("text displaying level values … an absolute or percent value
   to display beside the level") and **Extend Lines Left/Right** ("Extends levels' lines
   indefinitely to the left or right")
   ([TradingView: Fibonacci retracement drawing tool](https://www.tradingview.com/support/solutions/43000518158-fibonacci-retracement-drawing-tool/)).
   Four settings, and ours has none of the four.
2. **The grid is anchored once and redrawn only when the leg is replaced.** "Once price breaks
   the anchored extreme, the old grid is stale; redraw from the new completed leg"; "use the most
   recent completed impulse leg, keep one wick-or-body convention, and redraw once price sets a
   new extreme"; and the level arithmetic is stated exactly as ours computes it — "For an up leg,
   draw each retracement level at Level_r = H - r × R, measuring back down from the high"
   ([Fib Retracement](https://www.luxalgo.com/library/concept/fib-retracement/)).
3. **The pocket is a shaded slice of that grid, not a tool of its own.** "Draw the retracement
   from the leg's origin to its extreme, and mark the band between the 0.618 and 0.65 levels";
   "Shade the zone between the two levels; the pocket is watched for a reaction as the pullback
   trades into it"; "It is a zone rather than a single line, and it moves with the chosen swing
   anchors" ([Golden Pocket](https://www.luxalgo.com/library/concept/golden-pocket/)).
4. **An extension is the same leg read past 100%, and it is dormant until the leg is exceeded.**
   "Plot extension levels beyond the swing end: E_e = A + e × Range for each ratio e in {1.272,
   1.618, 2.0, 2.618}"; "Wait for the swing extreme to actually break. Extension levels are
   dormant until the 100% point gives way"; "Equivalently E_e = B + (e - 1) × Range"
   ([Fib Extension](https://www.luxalgo.com/library/concept/fib-extension/)). The second form is
   the one our detector implements — with two ratios out of four, and no dormancy.
5. **A cluster is a narrow band where independent draws agree, and width is the discipline.**
   "Mark each zone as the band from its lowest to its highest grouped level; more levels in a
   tighter band rank the zone stronger"; "Respect width discipline: the zone spans the
   overlapping levels only, and a band wide enough to catch everything proves nothing"; "group
   levels lying within a tolerance w of each other; a group qualifies as a confluence zone when
   it holds at least k levels from at least two different swings"
   ([Fib Clusters](https://www.luxalgo.com/library/concept/fib-clusters/)).
6. **Time tools draw vertical lines at the count bars — the x axis is the measurement.** "Draw
   vertical lines at t_0 and at t_0 + F_k for the Fibonacci numbers F_k = 1, 2, 3, 5, 8, 13, 21,
   34, and so on"; "Everything is counted in bars, so the same anchors mark different calendar
   dates on different timeframes"; "The first few zones sit close together and are usually
   disregarded" ([Fib Time Tools](https://www.luxalgo.com/library/concept/fib-time-tools/)).
   Note the sequence: it starts at 1 and stops at 34 where ours starts at 3 and runs to 55.
7. **The OTE is a band of the same grid with a marked midpoint.** "The OTE zone spans level(0.62)
   to level(0.79); the level(0.705) midpoint is the preferred entry price"; "Mark the band between
   the 0.62 and 0.79 retracement levels; many templates add 0.705 as the midline"
   ([Optimal Trade Entry](https://www.luxalgo.com/library/concept/optimal-trade-entry/)).
   Our band is 0.62-0.79 — the right slice — with no midline and no grid around it.

## What we draw today

**1. The ladder has no rungs.** 50 `Segment`s at 10 heights, every one inside x 1024-1082 of a
1074px plot — the right 5.4% — because each event's span is `(swing origin, emission bar)` and
every emission is within ten bars of the newest one. Widths run 28-49px. The drawing is
therefore a nest of dashes at the right edge, and the levels it encodes cannot be read: two of
the ten heights are 40px apart and the rest are 20-60px apart *inside a 49px-wide stub*. On
[the real chart](evidence/pass5-chart.png) the fib marks are the only drawings on screen that
carry no information at all, and [the 2× crop](evidence/pass5-chart-right.png) shows why: 30
overlapping `time_zones` labels and a heap of dashes under them.

**2. No label is a ratio, and every label repeats.** `short = concept_id.split(".")[1]` yields
`retracement`, `golden_pocket`, `extension`, `confluence`, `time_zones`, `ote` — printed 50
times each. The reference's labels are the *level values*: "an absolute or percent value to
display beside the level". This is the fourth pass to record the label defect (passes 1, 3 and 4
did), and the first where the correct label is not even the concept's name — it belongs to the
rung, not the study.

**3. The figures that should be translucent are effectively opaque.** Every zone box is drawn
with `fill_alpha ?? 0.08` ([concept_shapes.js:130](../../../../frontend/app/concept_shapes.js#L130)),
and the concepts emit 50 of them: 0.08 stacked 50 times composites to **0.985**. Measured on the
rendered scenes: the pocket covers 1.2% of the plot with a 13.8%-wide band, the OTE 6.3%, and
the confluence **13.3% of the entire plot**, its widest band 52px tall across the last bars. The
concept that should be the quietest — a "shade" — is the one that hides the candles.

**4. The confluence band is 12-18% of the leg.** Bands average 210.62 points and reach 301.72
against a 1670.01-point leg. The reference's own cluster definition allows this — a group spans
"from its lowest to its highest grouped level" — but it pairs that with width discipline and a
tolerance the user sets. Ours hard-codes `CONFLUENCE_TOL = 0.004`
([fibonacci.rs:21](../../../../crates/analytics-core/src/fibonacci.rs#L21)), which on an 80,000
price is a 324-point window: the tolerance *is* the band, and it is not exposed.

**5. The extension projects a fifth of its lines off the canvas.** 10 of the 50 lines — one whole
level, emitted ten times — sit at y 569.55 where the plot ends at 494. Nothing clamps a level to
the visible price range, so the concept spends a fifth of its ink drawing nothing. The two ratios
that remain are 1.272 and 1.618 because `EXTENSIONS: [f64; 2]` stops there
([fibonacci.rs:17](../../../../crates/analytics-core/src/fibonacci.rs#L17)), and the registry
declares no parameter, so a trader who wants 2.0 and 2.618 cannot ask.

**6. Time zones use the wrong axis, the wrong sequence, and the wrong predicate.** Three
separate divergences, and the smallest is the sequence: ours is `[3, 5, 8, 13, 21, 34, 55]`
([fibonacci.rs:19](../../../../crates/analytics-core/src/fibonacci.rs#L19)) against the
reference's `1, 2, 3, 5, 8, 13, 21, 34`. The predicate is worse: `if at <= bar_time` fires on
every bar *after* a count is due, so one count produces a run of events instead of one line, and
the event carries `price_level = last.close` — a price the concept never computed — which is what
the router then draws. Twelve distinct heights, no two meanings.

**7. Every row is a price-frame overlay the registry says is neither pane nor overlay.** All six
answer `pane: false, overlay: false` ([concepts.rs:1134-1194](../../../../crates/market-data/src/concepts.rs#L1134)),
the same silence passes 1-4 found. For this family it is the fifth consecutive instance and the
plainest: a fib grid is drawn on the price frame by definition, the router decides placement from
`CONCEPT_ZONE_IDS`, and gate rule 5 forbids calling any of them a `series` — which is correct, and
is why the fix is a registry field and a shape, not a kind.

**8. A state is shipped as one-shot events, and the delivery cap becomes the concept's history.**
This is the family-level version of finding 1: five fib concepts are functions of *the newest
swing pair*, which is a state that persists for tens of bars, and every one of them re-announces
that state at every bar. The retracement's 50-event allowance is spent in ten bars; the pocket's
in fifty. Nothing about the tool changes between those two numbers, and a trader comparing them
on the chart would conclude — wrongly — that pockets have five times the history of ladders.

## The card is a second renderer, and it tells a different story

The card's counts are the API's counts exactly — 50, 50, 50, 50, 50, 50 — so the two renderers
agree perfectly about *how much*. About *what*, they split two ways, and both are visible side by
side in [evidence/pass5-cards.png](evidence/pass5-cards.png):

| | concepts | the chart | the card |
| --- | --- | --- | --- |
| zone ids in `CONCEPT_ZONE_IDS` | golden_pocket, confluence, ote | a box ending at `to_ns`, labelled 50× | the same box **running to the 96-bar crop's right edge**, no label |
| `price_level`, no zone id | retracement, extension, time_zones | a 28-49px dashed stub, labelled 50× | a dashed line **with no label at all**, at 96 bars of zoom |

The zone row disagrees for the reason pass 4 measured: every event is `confirmed`, so
`previewEvents`' `forming || confirmed` test is always true and every box is extended to
`axis.bars - 1` ([app.js:14780-14781](../../../../frontend/app/app.js#L14780)) — a pocket drawn
to 2030 rather than to the bar that measured it. The level row is the preview's own blind spot:
`previewEvents` draws no text for any shape, so the card shows a fib retracement with no numbers
at all. The card is 239px wide and the prefix of the drawing that fits is the dashes, so the
preview makes the family look *cleaner* at 96 bars than it is at 500 — which is the opposite of a
preview's job. `preview_agrees`: **6 `differs`**.

## No parameter is honoured

All six declare `parameters: []`, the fifth consecutive family. This one has the most to lose,
because every reference in the set exposes its grid: TradingView's tool "allows you to set 24
different Fibonacci levels" with a checkbox per level, LuxAlgo's extension page lists a four-ratio
default and alternatives (1.414, 3.618, 4.236), the pocket page names 0.66 and 0.666 as
substitutes for its 0.65 edge, and the cluster page has both `w` (tolerance) and `k` (minimum
draws) as inputs. Ours hard-codes `RETRACEMENTS`, `EXTENSIONS`, `GOLDEN_POCKET`, `TIME_COUNTS`
and `CONFLUENCE_TOL` ([fibonacci.rs:13-21](../../../../crates/analytics-core/src/fibonacci.rs#L13)).
A trader who thins the grid to the two levels their strategy uses — the retracement page
recommends exactly that — cannot.

## The 6 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `ladder` (5) | retracement, golden_pocket, extension, confluence, ote | **5 wiring** | a named set of horizontal levels with one span, each rung a `Line` + a `Label` carrying its ratio and price, two rungs able to enclose a shaded band. The engine paints both draws already ([script.rs:69](../../../../frontend/chart-engine/src/script.rs#L69), [151](../../../../frontend/chart-engine/src/script.rs#L151)); what is missing is the `ConceptShape` variant and the ratio on the wire — the same kind of gap pass 3 found on `bar_marker` |
| `time_line` (1) | time_zones | **1 wiring** | a vertical rule at a bar: `ScriptDraw::Line` with `x1 == x2` spanning the plot's full height, the same full-height geometry `time_box` already lowers to ([script.rs:785](../../../../frontend/chart-engine/src/script.rs#L785)). The painter exists; the shape does not |

`draw.verdict`: **5 `differs_major`, 1 `impossible_in_vocabulary`**, no `matches`, no
`differs_minor`. `fires.verdict`: **6 `different_rule`**, no `same_bars` — and note that the
retracement and extension *formulas* are the reference's to the cent, so the verdict is about
cadence and dormancy, not arithmetic. Every row in this family had a cited page.

This pass settles `ladder` in [the vocabulary](00-vocabulary.md) — it is **wiring**, because a
rung is a Line and a Label and the engine paints both; the missing piece is a shape and a
payload, not a painter — and adds `time_line` as its second new candidate, also wiring.

## Fixes this pass asks for

1. **Draw the rung's own numbers.** Each level carries its ratio and its price, at the line's
   left end, and the study carries one name at the anchor. This single change turns six
   unrecognisable drawings into six recognisable ones.
2. **Emit the grid once per leg.** A fib grid is a state of the newest completed swing: emit when
   the anchor pair changes, and the 50-event cap stops deciding how much history a concept shows.
   The same fix retires the 5-events-per-bar retracement, the 50-bar pocket and the 0.4%
   re-emission of the confluence at once.
3. **Extend the levels, and draw the anchor line.** TradingView extends the grid left or right by
   toggle and joins the two anchors with a line; ours clips every level at the emission bar. The
   settled `level_run`'s run length is the same parameter here.
4. **Draw the pocket and the OTE as slices of the grid.** One band per leg, the grid's labels
   around it, 0.705 marked inside the OTE, and a fill that stays below the level's own ink —
   0.08 × 50 is 0.985, and the fix is emitting once, not lowering the alpha.
5. **Give confluence its width discipline.** Expose `w` and `k`, span the zone over the grouped
   levels rather than the whole tolerance window, and keep the band under the ~50px the reference
   implies when it says a band "wide enough to catch everything proves nothing".
6. **Fix time zones: vertical, and once per count.** `time_line` at `t0 + F_k` with the
   reference's sequence (1, 2, 3, 5, 8, 13, 21, 34), emitted at the count bar rather than on
   every bar after it — and carrying no price at all, so the router cannot draw it as a level.
7. **Expose the grid.** Ratios for the retracement and the extension, the pocket's outer edge,
   the time counts, and the confluence tolerance: five constants today, five parameters
   tomorrow ([fibonacci.rs:13-21](../../../../crates/analytics-core/src/fibonacci.rs#L13)).
8. **Make the extension dormant until the swing breaks**, and clamp a level that falls outside
   the visible price range instead of painting it off-canvas. Ten of fifty lines currently draw
   nothing at y 569.55.
9. **Mark these ids `overlay` in the registry** and stop deriving placement from
   `CONCEPT_ZONE_IDS` — the fourth pass to ask, and the first where all six rows are affected.
10. **Fix the 1m scan window.** `limit=500` at 1m returns an empty replay while 50, 200 and 1000
    all return events; a 1m chart currently renders no concept at all. This is a transport bug
    the family exposed, not a fib rule.
