# Pass 6 — Chart Patterns (15 rows)

Judged 2026-10-31 against `main`, on the window the gateway serves today (which ends
2026-10-29T06:45 UTC). The machine-readable verdicts are the 15 rows of
[`ledger/chart.json`](../../../../reports/concept-visual-audit/ledger/chart.json);
`node tools/concept_visual_audit.mjs check --family chart` reports **15 rows, 0 incomplete**.

**Headline: the pattern engine computes the outline and the wire collapses it to one number.**
Every concept in this family is detected from named pivots — two equal peaks and the valley
between, five H&S swings with a neckline joining the armpits, two converging coil lines, a cup's
rim, bottom and handle — and `PatternMatch` carries anchors, `entry_level`, `target` and
`invalidation` (patterns.rs:96-117), exactly the reference's outline, neck and measured move. The
event boundary keeps one scalar from that geometry (`price = m.entry_level`,
[events.rs:1120](../../../../crates/analytics-core/src/events.rs#L1120)), and the concept path —
which sends every `chart.*` id to the `price_level != null` segment branch because not one of
them is in `CONCEPT_ZONE_IDS` ([concept_shapes.js:27](../../../../frontend/app/concept_shapes.js#L27),
[92-102](../../../../frontend/app/concept_shapes.js#L92)) — paints it as a horizontal line at the
break with the pattern's snake_case id printed above it. The reference draws the shape; we draw
one number from the shape. Measured on the 15 render pages: 494 lowered draws, of which 0 are `Box`, 0 are `Polyline`, and
every one of the 247 `Line`s has `dy == 0`.

**Nothing that distinguishes the 15 concepts survives the drawing.** A double top, a triangle and
a cup arrive on screen as the same object — a flat dash and a word — differing only in how many
copies of the word there are. The one place the pattern's *extent* still shows is the span: the
adapters put the first pivot's timestamp in `formed_at` and
[`bars: (formed_at, bar_time)`](../../../../crates/market-data/src/concepts.rs#L2459) makes the
segment run from the pattern's start to its break (5-167 bars measured). That is the shape's
*x*-history; its *y*-shape — peaks, slopes, arcs, neck, target — is gone.

## How it was measured

Same window and route as passes 1-5:
`GET /concept-events/scan?symbol=BTCUSDT&timeframe=15m&limit=500` — 501 bars
(2026-10-24T01:45 to 2026-10-29T06:45 UTC), the same three caps
([concept_routes.rs:215-266](../../../../crates/api-gateway/src/concept_routes.rs#L215)):
replay from bar 22, at most 50 events per concept keeping the newest, `EventConfig::default()`
(`structure_lookback: 3`, [events.rs:926](../../../../crates/analytics-core/src/events.rs#L926)).
`node tools/concept_visual_audit.mjs render chart` photographs the fifteen; every number below
comes from that fetch and those pages:

| concept | events | uniq bars | uniq levels | span min/med/max (bars) | zone_low/high populated | evidence keys |
| --- | --- | --- | --- | --- | --- | --- |
| `chart.cup_and_handle` | 2 | 2 | 2 | 5 / 12 / 12 | yes | close |
| `chart.double_top_bottom` | 50 | 35 | 8 | 5 / 13 / 20 | yes | close, magnitude (confidence) |
| `chart.flag` | 12 | 12 | 2 | 11 / 17 / 22 | yes | close, magnitude |
| `chart.head_shoulders` | 2 | 2 | 1 | 23 / 24 / 24 | yes | close, magnitude |
| `chart.inverse_cup_handle` | 24 | 24 | 5 | 18 / 60 / 167 | yes | close |
| `chart.pennant` | 5 | 5 | 5 | 16 / 16 / 16 | yes | close |
| `chart.rectangle` | 4 | 4 | 4 | 28 / 60 / 68 | yes | close |
| `chart.rounding_bottom_top` | 44 | 44 | 10 | 4 / 59 / 167 | yes | close |
| `chart.trendline` | 4 | 4 | 4 | 11 / 13 / 26 | yes | close |
| `chart.triangle` | 50 | 50 | 8 | 9 / 42 / 53 | yes | close, magnitude |
| `chart.wedge` | 50 | 50 | 7 | 12 / 32 / 56 | yes | close, magnitude |
| `chart.triple_top_bottom` | 0 | — | — | — | — | silent this window |
| `chart.trend_channel` | 0 | — | — | — | — | silent this window |
| `chart.diamond` | 0 | — | — | — | — | silent this window |
| `chart.broadening` | 0 | — | — | — | — | silent this window |

"Silent" here is the render page's second empty kind — no events in the window, not events that
routed to nothing — and the four silent concepts lower to exactly zero shapes for it. Three of
the eleven live concepts sit at the 50-event cap (`double_top_bottom`, `triangle`, `wedge`), which
is the cap, not the count: the classical pass re-announces a pattern on **every bar the geometry
stays the newest**, because the adapters emit whatever `detect_patterns` returns against the
trailing window at each replay bar ([events.rs:1090-1128](../../../../crates/analytics-core/src/events.rs#L1090)).
The render pages are working artifacts and stay gitignored; per the plan's step 5, this pass did
not open a browser — the render pages under `reports/concept-visual-audit/chart/` are the
photograph, and the live-chart comparison is folded into Task 19's close-out.

**The two routing facts that decide the picture.** All 15 ids answer `pane: false, overlay: false`
and read only CANDLES ([concepts.rs:836-1015](../../../../crates/market-data/src/concepts.rs#L836));
none is in `CONCEPT_ZONE_IDS`, and all carry a non-null `price_level`, so every one takes the
segment branch — `Line` + `Label`, never `Box`, never a `Polyline`, never a dashed neck — and the
label is `concept_id.split(".")[1]`, which the segment branch does not even un-spaces
([concept_shapes.js:63](../../../../frontend/app/concept_shapes.js#L63),
[99](../../../../frontend/app/concept_shapes.js#L99)).

## What the reference set says

Bulkowski's Encyclopedia pages at thepatternsite.com carry the shape for every one of the 15; the
row citations are verbatim from them, read 2026-10-31. TradingView's own Chart Patterns help
articles corroborate the platform half; two of their bodies were retrieved as printed text
([the folder index](https://www.tradingview.com/support/folders/43000587407-chart-patterns/)), and
the `Auto chart patterns` page itself is JS-rendered and did not yield body text — noted as a
limit of this set, not a finding.

1. **Double top — the break of the valley is the pattern's event.** "The double top confirms as a
   true double top once price closes below the valley."
   ([Adam & Adam Double Tops](https://thepatternsite.com/aadt.html)). Our classical pass has no
   close-below clause at all; it fires on the two swings alone.
2. **Head & Shoulders — the neckline is a *trendline*, not a number.** "A neckline is a trendline
   that connects the two armpits of a head-and-shoulders pattern."
   ([HST Explained](https://thepatternsite.com/HSTExplained.html)); the shape is "a three-peak
   pattern with the middle peak above the others" ([HST](https://thepatternsite.com/hst.html)).
3. **Symmetrical triangle — two sloped lines through the swings.** "Two trendlines bound prices;
   the bottom trendline slopes up and the top one slopes down."
   ([Symmetrical Triangles](https://thepatternsite.com/st.html)).
4. **Falling wedge — both lines slope the same way and converge.** "Price follows two down-sloping
   and converging trendlines." ([Falling Wedges](https://thepatternsite.com/fallwedge.html)).
5. **Flag — the pole is half the pattern.** "Flags appear as small rectangles usually tilted
   against the prevailing price trend and mounted at the end of a flagpole."
   ([Flags](https://thepatternsite.com/flags.html)).
6. **Pennant — the same pole, a converging triangle instead of a parallelogram.** "Looks like a
   short symmetrical triangle." ([Pennants](https://thepatternsite.com/pennants.html));
   TradingView's article is the same shape with the break as its confirming event: "It forms after
   a sharp price decline, known as the flagpole, followed by a brief consolidation period where
   price moves within converging trendlines."
   ([Chart Pattern Bearish Pennant](https://www.tradingview.com/support/solutions/43000697937-chart-pattern-bearish-pennant/)).
7. **Bearish flag, as the platform describes it — and how it is traded.** "Identify the flagpole,
   which is the initial steep drop, followed by a rectangular channel of lower highs and lower
   lows. The breakout below the flag's support line confirms the pattern."
   ([Chart Pattern Bearish Flag](https://www.tradingview.com/support/solutions/43000697936-chart-pattern-bearish-flag/)).
   Both TradingView articles describe the pattern as lines bounding price and name the confirming
   *break*; neither describes a flat level at a single price.
8. **Triple top — three peaks, then the lowest valley decides.** "The pattern becomes valid when
   price closes below the lowest valley in the pattern."
   ([Triple Tops](https://thepatternsite.com/tt.html)); the "three peaks near the same price" line
   on the same page is the outline.
9. **Rectangle — two flat lines, and the close outside one of them.** "Prices have flat tops and
   flat bottoms, crossing the pattern from side to side following two parallel trendlines."
   ([Rectangle Bottoms](https://thepatternsite.com/rectbots.html)); same page: "wait for price to
   close outside the trendline before taking a position".
10. **Cup with handle — a curve, and the handle lives in the upper half.** "A rounded turn that
    looks like a cup with a handle on the right of the cup."
    ([Cup with Handle](https://thepatternsite.com/cup.html)).
11. **Inverse cup — the mirror, confirmed at the lip.** "The pattern confirms as valid when price
    closes below the right cup lip."
    ([Inverted Cup with Handle](https://thepatternsite.com/icup.html)).
12. **Rounding bottom — slow is the shape.** "Look for a rounded bowl shape, usually over many
    months and usually after an upward price trend."
    ([Rounding Bottoms](https://thepatternsite.com/roundb.html)); same page: "I use a close above
    the left peak" as the confirmation.
13. **Broadening top — diverging lines through five touches.** "Higher peaks and lower valleys --
    a megaphone shape." ([Broadening Tops](https://thepatternsite.com/bt.html)); same page: "At
    least five touches total, three peaks or three valleys should touch the associated trend line
    with two or more touches of the other trendline."
14. **Diamond — broadening then contracting, tilted.** "Looks like a diamond, but one usually
    tilted to the side." ([Diamond Tops](https://thepatternsite.com/diamondt.html)); same page:
    "Prices form higher peaks and lower valleys... then price action narrows with lower peaks and
    higher valleys."
15. **Channel and trendline — parallel lines, or one line with a validated third touch.** "When
    price trends upward or downward and fits between two parallel trendlines, the chart pattern is
    called a channel." ([Channels](https://thepatternsite.com/channels.html)); and
    "Draw an up-sloping trendline along the price valleys."
    ([Up-Sloping Trendlines](https://thepatternsite.com/uptrendlines.html)).

Where the two sets disagree, they disagree about strictness, not about geometry: Bulkowski gates a
double top on a "valley drop … at least 10%" and peaks "usually less than 3%" apart, while
TradingView's articles give the looser shape a scanner can auto-draw. Both halves of every
reference describe the pattern as **lines through pivots plus a confirming break** — never as one
flat level at the break price. That is the sentence all 15 rows are judged against.

## What we draw today

**1. Every live concept is a flat dash with a repeated word.** Lowered draws measured on the
render pages: `double_top_bottom` 50 lines + 50 labels (widths 11-43px), `triangle` 50 + 50
(19-114px), `wedge` 50 + 50 (26-120px), `rounding_bottom_top` 44 + 44 (9-358px),
`inverse_cup_handle` 24 + 24 (39-358px), `flag` 12 + 12 (24-47px), `pennant` 5 lines all 34px,
`rectangle` 4 (60-146px), `trendline` 4 (24-56px), `cup_and_handle` 2 (11-26px),
`head_shoulders` 2 (49-51px). `Box`: 0 across the family. `Polyline`: 0. Nonzero `dy`: 0 — the
pattern's own pivots, the neckline slope, the coil's convergence and the measured-move target are
all invisible *by construction*: the segment branch emits one y for both ends of the line.

**2. The cap and the re-emission decide how much pattern you see.** The three classical concepts
that cap at 50 are 50 copies of 7-8 distinct levels: `double_top_bottom`'s 50 events occupy 35
bars and 8 levels; `triangle`'s occupy 50 bars and 8 levels; `wedge`'s 50 bars and 7. The right
edge is worse than fib's: the 50 `double_top_bottom` lines sit at x 887-1082 of a 1074px plot —
the rightmost 18% — because each span is (first pivot → newest bar) and only the newest ~13-20
bars of history survive the cap. `head_shoulders` and `cup_and_handle`, whose geometry is rare,
fire twice each — so the same family shows 50 words of one pattern and 2 of another, and neither
number is the pattern count.

**3. Colour says a direction the drawing cannot support.** All 50 `triangle` events are
`direction: "neutral"` — the coil branch hard-codes `PatternDirection::Neutral`
([patterns.rs:371](../../../../crates/analytics-core/src/patterns.rs#L371)) —
so every triangle line is slate even after the break that resolves it. `wedge` is 37 teal / 13
red and `double_top_bottom` 29/21 — side colours on a level that shows neither the peaks nor the
neck whose break gives them meaning.

**4. The zone the detector computed is on the wire and off the chart.** All 11 live concepts
populate `zone_low`/`zone_high` — the rectangle's channel floor and ceiling (76566.06 / 77377.68
on the sample), the cup's rim and bowl bottom, the H&S's target and head top (79000.99 / 80527.02)
— and `as_concept_event` ships both. The concept path drops them because chart.* has no zone id;
the only renderer that uses them is the card (below). `chart.trendline` is the sharpest case: the
detector computes a *sloped* line and its break
([events.rs:1393-1422](../../../../crates/analytics-core/src/events.rs#L1393)), and what reaches
the screen is a horizontal segment at the break bar's projection of the line — the same
*when-answered-as-where* failure pass 4 recorded for the session clock and pass 5 for fib time,
in a third family.

**5. The four silent concepts would draw the same flat thing.** `triple_top_bottom`,
`trend_channel`, `diamond` and `broadening` produced 0 events in the 501-bar window, so their pages
route zero shapes and print the page's "no events" empty label. Had one fired, the router would
have given it the identical treatment: one horizontal `Line` at its `price_level` across
(first anchor → break) and a `Label` with its snake_case id — which is precisely the wrong picture
for two diverging lines, two parallel lines, an eight-pivot diamond or a third flat peak. They are
silent on *this* window, not unreachable: the rules are live (each is exercised by the engine's
own replay per bar) and `detect_chart_gaps` fires its siblings freely; the binding constraints are
listed below.

## Where our rule disagrees

**The classical six swing detectors never wait for the break.** The registry says
"confirmed through the neckline" ([concepts.rs:841,853](../../../../crates/market-data/src/concepts.rs#L841))
and so does every reference; `detect_double` / `detect_head_and_shoulders` / `detect_coil` /
`detect_flag` return a match from swing geometry alone, with no close-through-the-neckline clause,
and the adapter emits it at every bar the geometry stays newest. That is why three concepts sit at
the cap, why the levels repeat, and why a "confirmed" pattern marks bars where nothing was
confirmed. The gap-family detectors *do* demand the break (`last.close < neckline`, `> rim`,
`beyond either side`) — the family is split down the middle on its own definition.

**Two swing lookbacks, neither honourable.** `detect_chart_patterns` hard-codes
`StructureConfig { lookback: 2 }` ([events.rs:1083](../../../../crates/analytics-core/src/events.rs#L1083))
with the comment "finer than the structure detector's" — and ignores `config.structure_lookback`
(default 3) even though all six classical-concept rows declare `swing_lookback`. `detect_chart_gaps`
reads `config.structure_lookback` ([events.rs:1161](../../../../crates/analytics-core/src/events.rs#L1161))
but there is no per-concept override path, so nothing reaches it either. `parameters.honoured: []`
for the sixth consecutive family.

**Tolerances are family-internal folklore, not the reference's numbers.** The classical pass uses
`tolerance_pct: 0.004` and a 0.5/0.55-confidence floor with no threshold applied
([patterns.rs:133-137](../../../../crates/analytics-core/src/patterns.rs#L133));
the gaps pass uses `TOP_TOLERANCE: 0.003`, `HANDLE_MAX_DEPTH: 0.5`, a 1% minimum band height, a
0.15% third-touch window and a 50%-relative parallelism test
([events.rs:1157-1158,1244,1276,1405,1477](../../../../crates/analytics-core/src/events.rs#L1157)).
None is exposed; Bulkowski's double top asks for a ≥10% valley drop, which on this window's
80,000 BTC would be 8,000 points, and our detector accepts any confirmed valley.

**"Alive until violated" is what makes a channel's second edge exist — and it is why the silent
four are silent.** The gap pass counts a pivot only if no later bar violates it beyond the 0.3%
band ([events.rs:1196-1222](../../../../crates/analytics-core/src/events.rs#L1196)). Consequences
measured: `triple_top_bottom` needs three mutually-flat-and-alive highs *and* the confirming close
on the newest bar — no window in these 480 replay bars satisfied it; `broadening` needs three
strictly ascending newest highs *and* three strictly descending newest lows — BTC 15m trended, so
one side always violated; `diamond` adds the widen-then-narrow span chain on four pair-spans,
stricter still; `trend_channel` fails for a code reason, not a data one — the branch demands the
three newest **highs ascending as well as the three lows**
([events.rs:1469-1472](../../../../crates/analytics-core/src/events.rs#L1469)), so a **descending
channel can never fire at all**: the rule is half of the reference. Banked as a defect.

**Two arithmetic facts the rows record.** All four `chart.trendline` events ship
`zone_low > zone_high` (e.g. 77608.02 above 77156.99) — the push pairs (line value at the anchor,
line value at bar n) unsorted ([events.rs:1408,1419](../../../../crates/analytics-core/src/events.rs#L1408));
the segment ignores them but the card inverts its box (2px stub). And
`chart.head_shoulders` also carries the *inverse* pattern: `InverseHeadAndShoulders` maps to the
same event kind with `Side::Buy` ([events.rs:1096](../../../../crates/analytics-core/src/events.rs#L1096)),
so one concept id means both tops and bottoms — fine per "one concept, one id", but the label
`head_shoulders` never says which, and this window drew only tops (2 bearish).

## The card is a second renderer, and it draws a different object

Because every chart event ships `zone_low` *and* `zone_high`, the Indicators-face card's
`previewEvents` takes its **zone branch** ([app.js:14931-14943](../../../../frontend/app/app.js#L14931))
while the chart takes the **segment branch**: same event, two different shapes. The card fills a
rectangle from the pattern's first bar (`bars[0]`) to the crop's right edge — every event is
`confirmed`, so `right = axis.bars - 1` — at `alpha 0.18`, spanning the zone; the chart draws a
flat line at `price_level` with the word. For the rectangle, cup and H&S the card's box is at
least the pattern's *extent* (bottom-to-rim, target-to-head-top) while the chart shows one edge of
it — the card is closer to canonical and still wrong (a box is not an outline, and the run past
the break claims a future the event never measured). For `trendline` the card draws an inverted,
clamp-flattened 2px stub. `preview_agrees`: **11 `differs`**, and the four silent concepts have no
card drawing at all — **4 `not_rendered`**.

## The vocabulary this pass needs

**`outline` settles — as `wiring`, for all 15 rows.** The candidate was opened by pass 1 for
exactly this family ("a polyline traced through named pivot bars, plus an optional dashed neck
level and a projected target") and named here as the evidence row. This pass confirms all three
members are already paintable by `ScriptDraw`: the pivot outline is
`ScriptDraw::Polyline` — listed in the vocabulary's own "already draws" sentence
([00-vocabulary.md:40-43](00-vocabulary.md), [script.rs:67](../../../../frontend/chart-engine/src/script.rs#L67));
the neckline is a `Line` between the two armpit points — `Line` takes `(x1, y1, x2, y2)`, so it is
already *sloped-capable and dashed-capable* (`style: "dashed"`, painted at
[concept_shapes.js:137-146](../../../../frontend/app/concept_shapes.js#L137)); the projected
target is another `Line`; the pattern name is a `Label`. Nothing in the 15 targets needs a painter
the engine lacks — the gap is one layer up: the concept path's three-member `ConceptShape`
vocabulary has no variant that could carry a point sequence, so `chart.*` is *routed* to
`segment` and the geometry dies at the event boundary. Status: `wiring`, scale `price` for all 15.

**No new shape name was needed, and none was invented.** `chart.trendline` and
`chart.trend_channel` wanted a *sloped run*, which `level_run` explicitly is not; both are covered
by `outline`'s definition as a degenerate case — two (or three) named pivot points joined, plus the
line's projected continuation — so Task 18 does not need a `sloped_level`; it needs `outline`'s
entry to name the degenerate members: 2-point polyline = trendline, 2 parallel lines = channel.
The one thing `outline`'s current definition does not cover is the *break marker* — the reference's
"price closes below the valley" event — which the existing `bar_marker` already names; the rows
keep it inside `outline`'s neck clause rather than inventing a third shape.

## Tally

- `draw.verdict`: **15 `differs_major`**, 0 `matches`, 0 `differs_minor`, 0
  `impossible_in_vocabulary` — the vocabulary *can* draw every one of these shapes today; it is
  the routing that cannot.
- `fires.verdict`: **7 `same_bars`** (triple_top_bottom, rectangle, cup_and_handle,
  inverse_cup_handle, rounding_bottom_top, broadening, trendline — the gap-family members whose
  confirming close matches the reference's confirming close) / **8 `different_rule`** (all six
  classical concepts, which fire on geometry without a break, plus pennant, which fires on shape
  alone, and trend_channel, whose both-sides-ascending clause is not the reference's rule) / 0
  `no_external_rule`.
- `preview_agrees`: 11 `differs`, 4 `not_rendered`, 0 `agrees`.
- `target_shape`: 15 × `outline`, 15 × `wiring`, 15 × `price`. `parameters.honoured`: 15 × `[]`.
- The audit has now judged 82 rows across six families (smc 32, sd 4, pa 11, session 14, fib 6,
  chart 15) and still has **no `matches`**.

## Fixes this pass asks for

1. **Route chart.* to an outline.** Carry `PatternMatch.anchors` (or the gap pass's
   `(index, price)` pivot pairs) across the event boundary and lower a chart event to a
   `Polyline` through the pivots plus a dashed neck `Line` plus a projected target `Line` — all
   existing `ScriptDraw` arms. This one change separates a double top from a triangle from a cup.
2. **Make the classical six wait for the break.** The registry already promises "confirmed
   through the neckline"; add the close-through-the-neckline clause the gap family has, and the
   per-bar re-emission that put 50 words on the chart collapses to one event per real confirmation.
3. **Honour `swing_lookback` in the classical pass, or drop the declaration.** Six concepts
   declare it; `events.rs:1083` hard-codes 2. And give `detect_chart_gaps` a per-concept override
   path for the six that declare nothing.
4. **Fix the trend_channel asymmetry and the trendline zone.** Accept descending triples mirrored
   (events.rs:1469-1472), and sort the `zone_low`/`zone_high` pair before pushing
   (events.rs:1408, 1419) — all four trendline events ship them inverted.
5. **Say the pattern's name, and once.** `short.replace(/_/g, " ")` already exists for candle
   marks; the chart should print "double top" once at the outline's start — and, when
   `evidence.magnitude` is a confidence, print that too instead of nothing. The `InverseHead…` →
   `chart.head_shoulders` fold means the label should also carry top-or-bottom from `direction`.
6. **Give the triangle its break direction.** The coil is neutral until it isn't; slate forever
   tells the trader the detector cannot tell.
7. **Stop the card from re-deriving its own shape.** The second renderer uses fields the chart
   ignores; when the outline lands, both must consume the same lowering, per the plan's coupling
   rule.
8. **Expose the tolerances.** 0.004/0.003 valley/flat bands, the 10% Bulkowski gate, the handle
   50% depth, the third-touch 0.15% window: five today-constants that are six reference-set
   parameters.
