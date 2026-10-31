# Pass 7 — Harmonics (9 rows)

Judged 2026-10-31 against `main`, on the window the gateway serves today (501 bars,
2026-10-26T04:00 to 2026-10-31T09:15 UTC). The machine-readable verdicts are the 9 rows of
[`ledger/harmonics.json`](../../../../reports/concept-visual-audit/ledger/harmonics.json);
`node tools/concept_visual_audit.mjs check --family harmonics` reports **9 rows, 0 incomplete**.

**Headline: the XABCD geometry is computed in Rust and destroyed at the event boundary.**
`detect_harmonics` reads five named pivots X-A-B-C-D, measures all four leg ratios against them,
and then emits one number from the figure: `price: d.price`, with `formed_at: Some(x.time)` as the
only other survivor of the five swings
([harmonics.rs:217-254](../../../../crates/analytics-core/src/harmonics.rs#L217)).
A, B and C never leave the crate. The router, which has no zone id for any of the nine
([concept_shapes.js:27](../../../../frontend/app/concept_shapes.js#L27)), sends what did arrive to
the flat-level branch, so the drawing is a 1px dashed horizontal line at D's price plus the word
`abcd` — and all 51 lines on the nine render pages have `dy == 0`. The pattern is five points; we
draw one of them, flat. Measured across the 9 pages: 102 lowered draws, of which 0 are `Box`, 0 are
`Polyline`, 0 are `CandleMark`, and 51 are `Line` + 51 their `Label`.

**Seven of the nine concepts are silent on this window, and two of the seven can never fire at
all.** `detect_harmonics` iterates six kinds
([harmonics.rs:229-236](../../../../crates/analytics-core/src/harmonics.rs#L229)) and
`windows_for` has no arm for `HarmonicCypher` or `HarmonicThreeDrives` — they hit the `_ => None`
catch-all and are skipped on every bar of every instrument
([harmonics.rs:75](../../../../crates/analytics-core/src/harmonics.rs#L75)) — while both kinds are
still in `EventKind::ALL`, which is why the catalog's integrity test passes and their cards paint a
"nothing to draw" hairline. The other five are not dead code, just strict: over this window's 71
distinct five-pivot chains no chain cleared all four of any named pattern's windows, and the Bat's
AD/XA window was cleared by **0 of 71**.

**And what does fire fires twice, under two names.** The 29 `abcd` events come from 4 chains and
the 22 `wolfe_wave` events from 4 wave origins, and two of those origins are the same five pivots:
chain 76-82-99-103-107 is announced as `harmonics.abcd` on bars 110-113 and, from the same `formed_at`
and at the same price 79,558.49, as `harmonics.wolfe_wave` on bars 110-113; chain 111-121-123-128-131
does the same on bars 134-138 at 80,497.31. On the rendered scenes the two concepts' labels land on
the *same pixel rows* — y 215.5 four times and y 140.9 five times in both pages. 18 of the family's
51 events are two names on one geometry.

## How it was measured

Same window and route as passes 1-6:
`GET /concept-events/scan?symbol=BTCUSDT&timeframe=15m&limit=500` — 501 bars, 4,113 events across
138 concepts, the same three caps
([concept_routes.rs:215-266](../../../../crates/api-gateway/src/concept_routes.rs#L215)): replay
from bar 22, at most 50 events per concept keeping the newest, `EventConfig::default()`. Harmonics
supplied 51 of those 4,113 events — 1.2% — from 2 of its 9 concepts.
`node tools/concept_visual_audit.mjs render harmonics` photographed the nine; every number below
comes from that fetch and those pages:

| concept | events | shapes | draws | span min/med/max (bars) | distinct levels | distinct origins | label |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `harmonics.gartley` | 0 | 0 | 0 | — | — | — | — |
| `harmonics.bat` | 0 | 0 | 0 | — | — | — | — |
| `harmonics.butterfly` | 0 | 0 | 0 | — | — | — | — |
| `harmonics.crab` | 0 | 0 | 0 | — | — | — | — |
| `harmonics.shark` | 0 | 0 | 0 | — | — | — | — |
| `harmonics.cypher` | 0 | 0 | 0 | — | — | — | unreachable by construction |
| `harmonics.abcd` | 29 | 29 | 58 | 23 / 37 / 55 (49.3-117.9px) | 4 | 4 | `abcd` × 29 |
| `harmonics.three_drives` | 0 | 0 | 0 | — | — | — | unreachable by construction |
| `harmonics.wolfe_wave` | 22 | 22 | 44 | 21 / 26 / 37 (45.0-79.3px) | 5 | 4 | `wolfe_wave` × 22 |

"9 rendered, 0 fired-but-routed-nothing, 7 silent on this window — 5 of them strictly rule-bound,
2 of them structurally unreachable." The two that fired drew at x 117.2-1047.6 (AB=CD, across the
whole plot) and x 172.9-340.1 (Wolfe, the left fifth of 1,074px) of a 1,074×484 plot, at widths of
exactly 1px, `style: "dashed"`, `extend: none`. The render pages are the photograph; the
live-chart comparison is folded into Task 19's close-out.

**The structure was there.** Re-deriving the detector's own swing engine over the same 501 bars
(lookback-3 strict fractals, newest-five-alternating, per replay bar as the gateway does):
**465 of the 479 replay bars held an alternating five-pivot chain**, 83 raw pivots existed on the
final replay, and 71 distinct chains were evaluated. Per-window pass counts over those 71:

| kind | AB/XA | BC/AB | CD/BC | AD/XA | all four |
| --- | --- | --- | --- | --- | --- |
| Gartley (0.55-0.70 / 0.382-0.886 / 1.13-1.618 / 0.74-0.84) | 10 | 28 | 11 | 4 | **0** |
| Bat (0.382-0.50 / 0.382-0.886 / 1.618-2.618 / 0.84-0.93) | 7 | 28 | 4 | **0** | **0** |
| Butterfly (0.74-0.84 / 0.382-0.886 / 1.618-2.618 / 1.27-1.618) | 4 | 28 | 4 | 10 | **0** |
| Crab (0.382-0.618 / 0.382-0.886 / 2.24-3.618 / 1.55-1.65) | 14 | 28 | 5 | 4 | **0** |
| Shark (1.13-1.618 / 1.618-2.24 / 0.382-0.886 / 0.886-1.13) | 11 | 2 | 32 | 4 | **0** |
| AB=CD (unconstrained / 0.382-0.886 / 1.13-2.618 / unconstrained) | 71 | 28 | 15 | 71 | **4** |

The four chains that passed, with the events each bought (a chain stays announced on every bar
until a newer chain displaces it):

| pivots (bar indices) | AB/XA | BC/AB | CD/BC | AD/XA | XA (pts) | D | events |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 50-53-67-71-76 | 0.956 | 0.528 | 1.262 | 1.088 | 758 | 79,728.57 | 6 |
| 76-82-99-103-107 | 1.134 | 0.545 | 1.200 | 1.258 | 660 | 79,558.49 | 4 |
| 111-121-123-128-131 | 1.261 | 0.486 | 2.049 | 1.904 | 521 | 80,497.31 | 5 |
| 429-443-448-452-468 | 0.483 | 0.652 | 1.828 | 0.745 | 1,784 | 77,654.44 | 14 |

Closest misses, in the detector's own units: chain 429-… was **0.095 off the Bat's AD/XA window
— 170 points of a 1,784-point XA leg**; chain 76-… was **0.012 off the Butterfly's AD/XA window —
8 points of a 660-point leg**, and those same five pivots were reported as `abcd`.

## What the reference set says

Nine LuxAlgo Library concept pages carry this family, one per concept, plus Bulkowski's Crab page.
All were read on 2026-10-31; every sentence below is verbatim. Scott Carney's own material was
unreachable from this machine (`harmonictrading.com` fails to fetch, and Wikipedia has no
*Harmonic (technical analysis)* article — 404), and no TradingView support page for a harmonic
drawing tool could be found: the support id the pass brief cites (43000518156) resolves to the
*Fibonacci speed resistance fan* page. The ratios below therefore come from the Library pages,
which state Carney's grids.

1. **Gartley** — "The Gartley is a five-point XABCD retracement pattern… B retraces 0.618 of XA,
   C retraces 0.382 to 0.886 of AB, and D completes at the 0.786 retracement of XA"
   ([Gartley](https://www.luxalgo.com/library/concept/gartley/)).
2. **Bat** — "B at 0.382 to 0.50 of XA … C at 0.382 to 0.886 of AB, and D at the 0.886 retracement
   of XA, confluent with a BC extension between 1.618 and 2.618"
   ([Bat](https://www.luxalgo.com/library/concept/bat/)). *Four* numbers, and ours are these four
   to the digit.
3. **Butterfly** — "The common grid: B at 0.786 of XA, C between 0.382 and 0.886 of AB, and D at a
   1.27 extension of XA with 1.618 as the outer limit"
   ([Butterfly](https://www.luxalgo.com/library/concept/butterfly/)).
4. **Crab** — "B retraces 0.382 to 0.618 of the XA leg, C retraces 0.382 to 0.886 of AB, and D
   completes well beyond the X extreme at a 1.618 extension of XA, typically confluent with a
   2.618 to 3.618 projection of BC"
   ([Crab](https://www.luxalgo.com/library/concept/crab/)); and on what the pattern *is* on the
   chart, "The bullish crab can resemble a double top chart pattern except that the turns are
   located using Fibonacci ratios"
   ([Bulkowski, Bullish Crab](https://www.thepatternsite.com/CrabB.html)) — the turns, not a body.
5. **Shark** — "The AB leg extends 1.13 to 1.618 of XA ... completion at C lands between the 0.886
   retracement and 1.13 extension ... confluent with a 1.618 to 2.24 extension", on pivots the page
   names **0, X, A, B, C** ([Shark](https://www.luxalgo.com/library/concept/shark/)). Five pivots,
   but not named X-A-B-C-D — the vertex names are per pattern, not per family.
6. **Cypher** — "B retraces 0.382 to 0.618 of XA, C extends to 1.272 to 1.414 of XA beyond the A
   point, and D completes at the 0.786 retracement of the full XC swing"
   ([Cypher](https://www.luxalgo.com/library/concept/cypher/)). The terminal ratio is measured from
   **XC**, a leg our window struct has no field for.
7. **AB=CD** — "Project the final leg CD from C in the direction of AB with abs(D - C) = abs(B - A),
   the AB = CD condition"; "Price travels from A to B, corrects to C (commonly 0.618 to 0.786 of
   AB), then travels from C to D until CD equals AB in price distance"
   ([ABCD](https://www.luxalgo.com/library/concept/abcd/)).
8. **Three Drives** — "The Three Drives is a harmonic exhaustion pattern: three successive,
   symmetrical pushes to new extremes, each separated by a corrective pullback of similar depth"
   ([Three Drives](https://www.luxalgo.com/library/concept/three-drives/)). Four pivots, three
   drives — not an XABCD at all.
9. **Wolfe Wave** — "Wolfe Waves are a five-point reversal pattern developed by Bill Wolfe, built
   from swing points labeled 1 through 5 inside a converging channel"; "The target is not a fixed
   level but the EPA line (estimated price at arrival), drawn from point 1 through point 4 and
   extended forward"; and, decisively for the `fires` column, "Wolfe Waves are drawn rather than
   detected from a single bar: the construction is the identification"
   ([Wolfe Waves](https://www.luxalgo.com/library/concept/wolfe-waves/)).

The set agrees on one structural point the render contradicts on all nine rows: **a harmonic is the
path through the pivots, with the ratios written on its legs, plus one zone at its end.** Not a
level at the last pivot.

## What we draw today

**1. Four of five anchor prices are dropped at the event boundary.** `MarketEvent` has one `price`
slot and it is filled with D
([harmonics.rs:248](../../../../crates/analytics-core/src/harmonics.rs#L248)); `formed_at` carries
X's time ([253](../../../../crates/analytics-core/src/harmonics.rs#L253)); A, B and C — the three
pivots the pattern's *name* is made of — are consumed by the ratio test and discarded. The contract
then has nothing to send: `as_concept_event` puts `price_level: Some(event.price)` and
`bars: (formed_at, bar_time)` on the wire
([concepts.rs:2450-2459](../../../../crates/market-data/src/concepts.rs#L2450)), which is exactly
what the 29 AB=CD events carry. This is pass 6's finding one layer further along: the chart engine
collapsed a `PatternMatch`'s anchors to `entry_level`; harmonics never had the anchors in the event
at all.

**2. The PRZ is computed, shipped, and thrown away by the router.** `PRZ_FRACTION = 0.01` gives D ±
1% ([harmonics.rs:196, 241, 250-251](../../../../crates/analytics-core/src/harmonics.rs#L196)),
which is a genuine zone and does reach the browser as `zone_low`/`zone_high`. Because none of the
nine ids is in `CONCEPT_ZONE_IDS`, the router tests `price_level` first and lowers a `Segment`
([concept_shapes.js:92-102](../../../../frontend/app/concept_shapes.js#L92)), so the zone is dropped
at the branch. Measured: the four AB=CD chains carry bands of 1,553-1,610 points = **123.3-127.8px,
25.5-26.4% of the 484px plot**. The single harmonic feature every reference names — the reversal
zone at D — is on the wire, sized, and unpainted.

**3. Cypher and Three Drives are unreachable, not unlucky.** Both kinds are outside the iterated
array and both fall through `windows_for` to `None`
([harmonics.rs:229-237](../../../../crates/analytics-core/src/harmonics.rs#L229)), so their cards,
chips and registry entries are all live against a detector that will never emit them. Worse, the
`Windows` struct could not state them even if they were added: it holds `ab_xa`, `bc_ab`, `cd_bc`,
`ad_xa` ([24-29](../../../../crates/analytics-core/src/harmonics.rs#L24)) — Cypher's completion is
0.786 of **XC**, and Three Drives is three extensions across **four** pivots. Two of the nine rows
are therefore not a tuning problem but a missing rule and a missing field.

**4. The AB=CD concept is announced on a rule that does not contain AB=CD.** Its windows are
`bc_ab: (0.382, 0.886)` and `cd_bc: (1.13, 2.618)`
([harmonics.rs:69-74](../../../../crates/analytics-core/src/harmonics.rs#L69)): CD is measured
against BC, never against AB, and AB/XA and AD/XA are left at `0.0..f64::MAX`. Recomputed from the
window's own pivots, the four announced chains have CD/AB = 0.666, 0.654, 0.996 and 1.192 — **one
of four satisfies the pattern's namesake equality**, and one of four sits in the page's 0.618-0.786
correction band. The catalog sentence is even further away: "the CD leg mirrors the AB leg in price
*and time*" ([concepts.rs:1093](../../../../crates/market-data/src/concepts.rs#L1093)) — nothing in
the detector measures time.

**5. The Bat's grid is the reference's and still never fires.** All four windows match the page's
numbers to the digit, yet 0 of 71 chains cleared AD/XA, and the 28 chains that cleared BC/AB collapse
to 4 at CD/BC. The grid is right; the swing engine is the constraint: a strict lookback-3 fractal
leg on 15m data is 1-3% of price, and a 0.84-0.93 AD/XA completion on such a chain is rare. That is
worth recording because the module's own doc is explicit about the trade
([harmonics.rs:4-11](../../../../crates/analytics-core/src/harmonics.rs#L4)) — the reference engine
"completed nothing on three years of real data", so the windows were kept and the swings were
swapped. On this window the swap produced five silent concepts and a family that emits 1.2% of the
scan's events.

**6. Wolfe's target is a per-bar projection, not the line the pattern is named for.** `line_at`
evaluates the 1-4 line at `n + 10` where `n = candles.len() - 1` — ten bars past the **emitting**
bar — and the bar indices are re-derived with `candles.iter().position(...)` because `Pivot` carries
a time, not an index ([harmonics.rs:136-140, 153, 168](../../../../crates/analytics-core/src/harmonics.rs#L136)).
So one wave re-announces a *different* target every bar: the bearish wave whose five pivots sit at
bars 123-128-131-136-141 (p5 = 80,704.86) moved its target **146.70 points across its own 8
events**; the other four waves moved 35.49, 32.21, 43.17 and 41.91.
The reference's target is a fixed line the trader extends and reads where price crosses it. The
distance does reach the browser — as `evidence.magnitude` (690.65 on the first event) — and neither
renderer reads it.

**7. Wolfe is five points drawn as one level, with the band discarded.** `price = p5`,
`zone = [p5, target]` on a bullish wave and `[target, p5]` on a bearish one
([harmonics.rs:162-163, 177-178](../../../../crates/analytics-core/src/harmonics.rs#L162)); points
2, 3 and 4 never leave Rust, the 1-3 and 2-4 channel lines are never computed, and the router drops
the target band (55-122px tall, 690-1,533 points). What ships is the one thing the page says the
pattern is not.

**8. Labels: the id, printed once per event, underscores intact.** `short = concept_id.split(".")[1]`
and the segment branch writes it raw
([concept_shapes.js:63, 92-102](../../../../frontend/app/concept_shapes.js#L63)) — unlike the
candle-mark branch, which does apply `replace(/_/g, " ")` (line 89 vs 99). So `wolfe_wave` is
printed 22 times and `abcd` 29 times, and 14 of the AB=CD labels share one pixel row (y 366.6)
while the family's own names — Gartley, Bat, Butterfly, Crab, Shark, Cypher, Three Drives — are what
the reference cites and what the chart never shows. The pattern name belongs at the figure's start,
once; the ratios belong on the legs.

**9. Every row is a price-frame overlay and the registry says neither pane nor overlay.** All nine
answer `pane: false, overlay: false` with `parameters: &[]`
([concepts.rs:1016-1123](../../../../crates/market-data/src/concepts.rs#L1016)) — the sixth
consecutive family. Placement is decided by `CONCEPT_ZONE_IDS`, which lists none of them, so the
answer to "where does an XABCD live" is currently "on the price axis, by accident".

**10. `detect_harmonics` writes debug noise to stderr on every bar.** Three unconditional
`eprintln!` calls — the whole pivot vector, the "no alternating five" case, and the resolved chain
([harmonics.rs:206-211](../../../../crates/analytics-core/src/harmonics.rs#L206)) — fire once per
replay bar, so one 500-bar scan request prints ~1,400 lines including 83 pivots each. It is not a
drawing defect, but it is this family's cost per request and it is visible in the gateway's log.

## The card is a second renderer, and on this family it is the better one

`previewEvents` tests `zone_low != null && zone_high != null` **before** `price_level`
([app.js:14929-14946](../../../../frontend/app/app.js#L14929)), and every harmonic event carries
both. So the card draws the PRZ the chart discards:

| | the chart | the card (newest 96 bars of the same window) |
| --- | --- | --- |
| `harmonics.abcd` | 29 dashed 1px levels at 4 heights + `abcd` × 29 | **14 of the 29** events, each as a filled band (α 0.18) whose right edge is the crop's last bar, no text |
| `harmonics.wolfe_wave` | 22 dashed levels + `wolfe_wave` × 22 | **0 of 22** — every emission is on 2026-10-27, older than the crop's bar 405, so `indexByTime` misses and the card paints its empty hairline |
| the 7 silent concepts | 0 shapes | the same empty hairline |

Three disagreements, all measured. The band-vs-level split is the router's, not the data's; the run
to the crop's right edge is pass 4's defect again, since every event is `confirmed` and the
`forming || confirmed` test is always true
([app.js:14932-14933](../../../../frontend/app/app.js#L14933)); and the 14-of-29 / 0-of-22 gap is
the 96-bar crop deciding which of a 501-bar family's history a trader is shown. For the record: the
card is closer to canonical here than the chart is — a band at D is a PRZ — and it is still wrong,
because a box is not a path and it extends past its own measurement. `preview_agrees`: **2
`differs`, 7 `not_rendered`**.

## No parameter is honoured

All nine declare `parameters: []` and honour `[]` — the sixth consecutive family. Harmonics has the
most to lose from that, because every one of its windows is a *convention with named substitutes*:
the Bat page gives B as a range 0.382-0.50, the Crab page gives 2.618-3.618 as a projection band,
the AB=CD page gives the correction as "commonly 0.618 to 0.786" where the code says 0.382-0.886,
and Carney's own practice is to trade the PRZ's width. Ours hard-codes 44 finite window bounds in
`windows_for` (six patterns × four pairs, AB/XA and AD/XA sentinelled for AB=CD) plus the ±1% zone
in `PRZ_FRACTION`
([harmonics.rs:37-77, 196](../../../../crates/analytics-core/src/harmonics.rs#L37)), and the swing
engine's lookback (3) is not reachable either
([harmonics.rs:195](../../../../crates/analytics-core/src/harmonics.rs#L195)) — so the one dial a
harmonic trader would turn first, how finely swings are resolved, is welded. Given seven concepts
that never fire on a window full of structure — five rule-bound, two unreachable — an adjustable
window set is the difference between a silent tool and a usable one.

## The 9 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `legged_path` (9) | all nine | **9 wiring** | the pivots joined in order as one stroked path (optionally filled at a wash), each leg carrying its ratio as its own label, one pattern name at the figure, and a terminal band where the projections overlap |

`draw.verdict`: **9 `differs_major`**, no `matches`, no `differs_minor`, and — unlike pass 5 — no
`impossible_in_vocabulary`: every row in this family is expressible in the shape vocabulary the
audit already has. `fires.verdict`: **9 `different_rule`**, no `same_bars`. `warmup.bars`: **14** on
the eight XABCD rows (`if candles.len() < 14 { return out }`,
[harmonics.rs:201](../../../../crates/analytics-core/src/harmonics.rs#L201)) and **11** on Wolfe,
which carries *no* length guard at all — its floor is arithmetic
([harmonics.rs:131-135](../../../../crates/analytics-core/src/harmonics.rs#L131)). Both are moot on
the audit route, which starts its replay at bar 22.

Wolfe's own note, per its row: it needs **five points, not four**, and three of them exist only to
form the channel — so it is `legged_path` with vertices named 1-5 rather than XABCD. Shark names its
five vertices 0-X-A-B-C, which is why vertex names are per-pattern data, not a constant.

## What this pass asks of the vocabulary

**No new shape name.** `legged_path` is already listed
([00-vocabulary.md:85](00-vocabulary.md)), opened by this pass, and nine rows need it, so it is not
one family's private invention. What the definition is missing, and what Task 18 should grow before
Phase C:

- **Vertex count is per concept** — 5 for the XABCD set, 5 again for Shark under different names,
  4 for AB=CD (A-B-C-D), 4 for Three Drives (a start plus three drive ends), 5 for Wolfe under 1-5;
  the shape must take an ordered point list, not a fixed quintuple.
- **A projected target ray** — the Wolfe 1-4 (EPA) line extended forward, which is a sloped `Line`,
  not a level, and is the pattern's stated target.
- **A terminal zone band** — the PRZ at D, which the detector already computed and the router already
  drops; today it would be a second shape on the same path, so the shape should own it.
- **Per-pattern vertex names and per-leg ratio labels** — X-A-B-C-D, 0-X-A-B-C, 1-2-3-4-5.

Two corrections to its current row: its evidence column says "Harmonics draw as a band or a mark",
and measured, they draw **neither** — all 51 lines on this family's pages are flat dashed levels
(`dy == 0` on every one), and the only band that exists is the one the router discards. And it is
`open`, while `outline` — pass 1's candidate, settled as `wiring` by pass 6 for 15 rows — is
described as "a polyline traced through named pivot bars, plus an optional dashed neck level and a
projected target". These are one lowering with different label payloads; Task 18 should say so
explicitly, or the ledger will ship two shapes that both compile to `ScriptDraw::Polyline` + `Line`
+ `Label`.

## Pass 6's machinery, not reused

Task 12's brief expects pass 7 to build on pass 6 ("Reuses pass 6's machinery: if it does not,
record why"). It does not, and the gap is concrete:

- Pass 6's patterns go through the shared engine — `detect_chart_patterns` calls
  `market_structure::detect_market_structure` with `StructureConfig { lookback: 2 }` and hands the
  resulting `MarketStructure` to `crate::patterns::detect_patterns`
  ([events.rs:1078-1085](../../../../crates/analytics-core/src/events.rs#L1078)). Harmonics calls
  neither. It has its own `pivots(candles, 3)`
  ([harmonics.rs:88-101](../../../../crates/analytics-core/src/harmonics.rs#L88)), which is the
  same strict both-sides comparison `market_structure::find_swings` already implements — and
  `find_swings` is **public**
  ([market_structure.rs:185](../../../../crates/analytics-core/src/market_structure.rs#L185)).
- Its `last_five_alternating` same-side merge
  ([harmonics.rs:106-125](../../../../crates/analytics-core/src/harmonics.rs#L106)) is a second
  copy of what `collapse_edges` does in the chart engine
  ([events.rs:1133-1146](../../../../crates/analytics-core/src/events.rs#L1133)) — keep the more
  extreme of two same-side swings.
- Harmonics' `Pivot` carries `time`, not `index`
  ([harmonics.rs:80-85](../../../../crates/analytics-core/src/harmonics.rs#L80)), while
  `SwingPoint` carries `index` and `timestamp`. That single choice is why `detect_wolfe` must
  re-derive bar positions with `candles.iter().position(...)` — two linear scans of the window per
  event, and the off-by-one source of the moving target in finding 6.
- The lookbacks disagree with the rest of the platform: harmonics 3, chart patterns 2,
  `EventConfig::default().structure_lookback` 3
  ([events.rs:926](../../../../crates/analytics-core/src/events.rs#L926)). The module doc claims
  lookback 3 "matching the rest of the structure machinery"
  ([harmonics.rs:186-187](../../../../crates/analytics-core/src/harmonics.rs#L186)); the family
  adjacent to it in the same crate uses 2.

The honest reason is stated in the file: harmonics was written as a replacement for an external
engine, keeping that engine's windows and swapping its swing source, and the platform's own swing
source was re-implemented at the boundary instead of imported. Two swing engines, two collapse
rules, and two lookbacks now decide which concepts can fire at all.

## Corrections to the brief

The pass brief names the detector as `analytics_core::events::detect_harmonics` in `events.rs`. It is
`analytics_core::harmonics::detect_harmonics`
([harmonics.rs:194](../../../../crates/analytics-core/src/harmonics.rs#L194)), dispatched from
[events.rs:3565](../../../../crates/analytics-core/src/events.rs#L3565); Wolfe is
`detect_wolfe_waves` → `detect_wolfe`
([harmonics.rs:188, 131](../../../../crates/analytics-core/src/harmonics.rs#L188)) from
[events.rs:3551](../../../../crates/analytics-core/src/events.rs#L3551). The rows carry the real
paths. The TradingView harmonic-drawing-tool URL in the brief resolves to a different tool's page,
and Wikipedia's harmonic article is a 404, so this pass cites the Library pages and Bulkowski and
records that no TradingView harmonic page could be verified rather than quoting one it did not read.

## Fixes this pass asks for

1. **Put the anchors on the wire.** Carry all five pivots (price *and* bar index) on the event and
   route the nine ids to one `legged_path` lowering — `ScriptDraw::Polyline` for the path, a `Line`
   per leg label, a `Box` for the PRZ. The engine paints all three today
   ([script.rs:67-165](../../../../frontend/chart-engine/src/script.rs#L67)); the path is exactly
   what the module doc calls "the harmonic wing".
2. **Paint the PRZ that already exists.** D ± 1% is measured, sized and discarded; the id list in
   `CONCEPT_ZONE_IDS` is the one-line reason. Until the path lands, a band at D is closer to the
   reference than the current level.
3. **Write the ratios on the legs, and the name once.** The label is the ratio the reference names
   (0.618 XA at B, 0.786 XA at D), the figure carries one pattern name, and `wolfe_wave` gets its
   underscore either by the branch fix (line 89 vs 99) or by a real per-concept display name.
4. **Decide what Cypher and Three Drives are.** Either implement them — which needs a fifth window
   term (`xc` for Cypher) and a four-pivot/three-extension rule shape for Three Drives — or remove
   them from the catalog. Shipping a chip, a card and a registry row for a kind no detector can emit
   is the worst of the three options, and it is currently indistinguishable from "no pattern
   completed" on the chart.
5. **Give AB=CD its namesake test.** Measure CD against AB, add the correction band the page states
   (0.618-0.786), or rename the concept. Today the concept that promises symmetry cannot detect it:
   1 of 4 announced chains has CD ≈ AB.
6. **Make the Wolfe target a fixed line.** Project the 1-4 line from the wave's own geometry and
   carry it as a target ray with an explicit arrival bar, not `emitting_index + 10` recomputed each
   bar; emit once per wave, and draw the 1-3 / 2-4 channel the pattern is defined by.
7. **Emit once per completion, not once per bar.** 4 chains bought 29 events and 4 wave origins
   bought 22; the re-announcement cadence is what makes the family's visible history a function of
   the 50-event cap. Same finding as pass 5's state-as-events.
8. **Do not announce one chain as two concepts.** The dedupe rule belongs at the detector: when the
   same five pivots satisfy AB=CD and Wolfe, the row with the specific rule wins.
9. **Reuse `find_swings`, one lookback, and name it in the row.** Delete `pivots` and
   `last_five_alternating` in favour of `market_structure::find_swings` + `collapse_edges`, pick 2
   or 3 and state it, and give `Pivot` an index so nothing has to be re-derived by scanning candles.
10. **Expose the windows.** The 44 finite bounds, `PRZ_FRACTION` and the swing lookback are the
   whole rule set; seven of the nine rows are silent on a window that held an alternating
   five-pivot chain on 465 of 479 bars, so the parameters are the fix, not a nicety.
11. **Silence the `eprintln!`** at [harmonics.rs:206-211](../../../../crates/analytics-core/src/harmonics.rs#L206)
   or move it behind the debug flag; ~1,400 stderr lines per scan request is not a log, and it is
   the family's only unconditional output.
12. **Mark these ids `overlay` in the registry** and stop deriving placement from `CONCEPT_ZONE_IDS`
    — the sixth consecutive pass to ask.
