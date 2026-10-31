# Pass 8 — Candlestick patterns (34 rows)

Judged 2026-10-31 against `main`, on the window the gateway serves today (501 bars,
2026-10-26T04:00 to 2026-10-31T09:15 UTC). The machine-readable verdicts are the 34 rows of
[`ledger/candles.json`](../../../../reports/concept-visual-audit/ledger/candles.json);
`node tools/concept_visual_audit.mjs check --family candles` reports **34 rows, 0 incomplete**.

**Headline: this is the closest the platform comes to the reference, and the distance left is one
glyph wide — but the family's ink is a word, and on a quarter of the bars it is the wrong word.**
Every one of the 25 concepts that fires places its mark on the correct bar and the correct side:
measured across all 25 render pages, **every label's `x` equals the marked candle's `x`** (the
nearest-bar distance is under 0.6px on all 560 marks) and **every label sits exactly 10px off the
bar's own extreme on the right side** — under the low when the event is bullish, over the high
when it is bearish or neutral, which is what the reference's own pictures do. Nothing in this
family draws at the wrong price, the way passes 5, 6 and 7 do. What differs is that the reference
marks a candle with a glyph and lets the name appear on hover, while we print the name permanently
in lowercase at 9px: *"If the indicator finds a candlestick pattern, a special label will appear on
the chart: blue for Bullish indicators, red for Bearish indicators, or gray for indicators that can
show both bullish and bearish signals. If you'd like to learn more about the specific candlestick
pattern featured, simply point to the placemark and read the popup information listed in the
tooltip"*
([TradingView: Automatic candlestick pattern detection](https://www.tradingview.com/support/solutions/43000584462-automatic-candlestick-pattern-detection/)).
Bulkowski, whose candlestick index is the dictionary this family is built from, marks his
examples with a *shape* and an arrow — *"the black arrow on the figure shows which way price
usually moves after the candlestick pattern ends"*
([Visual Candlestick Index](https://thepatternsite.com/CandleVisual.html)) — and never with the
pattern's name written across the candles.

**The word is the problem, and it is measurable.** A 15m BTCUSDT candle in this scene is
**1.50px wide on a 2.14px pitch**; the shortest label in the family (`doji`, 4 characters) is
about 19px and the longest (`three black crows`, 17) about 82px, so a mark is 13–54× the bar it
marks. Consecutive marks therefore overlap by design: **`tweezer` has 49 of its 49 label pairs
closer than 21px, `belt_hold` 46, `engulfing` 44, `spinning_top` 43, `harami` 41, `three_inside`
36, `hikkake` 27, `three_outside` 23, `marubozu` 20, `doji` 19**. The painter sets no clip and no
background ([render paint, `phase === "over"`: `ctx.font = "600 9px ui-sans-serif"; ctx.fillText(lb.text, lb.x, lb.y)`]),
so the newest label — which starts at **x 1081.9** of a plot that ends at **1084** — runs 20–80px
into the price gutter, over the axis numbers.

**Six of the 25 numbers in the table below are the cap, not the count.** `belt_hold`, `engulfing`,
`harami`, `spinning_top`, `three_inside` and `tweezer` all answer exactly 50, the route's
newest-50-per-concept limit
([concept_routes.rs:215-269](../../../../crates/api-gateway/src/concept_routes.rs#L215)). Re-running
both detectors' own clauses over the same 501 bars, uncapped, gives the real density: **`tweezer`
378 hits (76.4% of the 495 evaluated bars), `belt_hold` 123 (24.8%), `spinning_top` 110 (22.2%),
`harami` 98, `engulfing` 94, `three_inside` 65, `hikkake` 51.** Union across the family:
**478 of 495 bars (96.6%) carry at least one candle mark, averaging 2.40 concepts per marked bar,
with a maximum of 7** — bar 168 (1793138400000000000) is simultaneously `doji`, `harami`,
`long_legged_doji`, `tweezer`, `matching_low`, `on_neck` and `hikkake`, i.e. seven words stacked
on 1.5px of candle. The scan's capped view still spends 560 of its 4,113 events (13.6%) on 319
distinct bars (63.7% of the window).

**Nine concepts are silent, and none of the nine is silent by luck.** All nine need a *gap*, and
this feed has none: over the 501 bars, **0 of the 500 adjacent pairs put one bar entirely above or
entirely below its neighbour**, 275 opens are exactly the prior close, and the largest deviation of
an open from the prior close is +0.68 / −1.21 USD (≤ 8.5 ppm). `kicker`, `abandoned_baby`,
`tri_star`, `tasuki_gap`, `upside_gap_two_crows`, `stick_sandwich`, `three_methods`,
`ladder_bottom` and `three_white_soldiers` return 0 events under the gateway's cap **and** 0 events
when their clauses are re-run uncapped — see the funnel table below for where each one dies.

## How it was measured

Same window and route as passes 1-7:
`GET /concept-events/scan?symbol=BTCUSDT&timeframe=15m&limit=500` — 501 bars, 4,113 events across
138 concepts, the same three caps (replay from bar 22, newest-50 per concept,
`EventConfig::default()`). Candles supplied **560 of those events from 25 of its 34 concepts**.
`node tools/concept_visual_audit.mjs render candles` photographed the 34; every number below comes
from that fetch and those pages. The render pages are the photograph; per the plan's step 5 this
pass did not open a browser, and the live-chart comparison is folded into Task 19's close-out.

| concept | events | bars | shapes | label | min pitch | pairs < 21px | uncapped fires (share of 495) | Bulkowski freq rank /103 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `hammer` | 8 | 8 | 8 Label | `hammer` × 8 | 2.14px | 1 | 8 (1.6%) | 36 |
| `hanging_man` | 3 | 3 | 3 Label | `hanging man` × 3 | 147.92px | 0 | 3 (0.6%) | 16 |
| `shooting_star` | 8 | 8 | 8 Label | `shooting star` × 8 | 15.01px | 2 | 8 (1.6%) | 37 |
| `inverted_hammer` | 4 | 4 | 4 Label | `inverted hammer` × 4 | 120.05px | 0 | 4 (0.8%) | 61 |
| `doji` | 37 | 37 | 37 Label | `doji` × 37 | 2.14px | 19 | 39 (7.9%) | 53 |
| `marubozu` | 38 | 38 | 38 Label | `marubozu` × 38 | 2.14px | 20 | 41 (8.3%) | 27 |
| `engulfing` | 50 | 50 | 50 Label | `engulfing` × 50 | 2.14px | 44 | 94 (19.0%) | 12 |
| `harami` | 50 | 50 | 50 Label | `harami` × 50 | 2.14px | 41 | 98 (19.8%) | 25 |
| `morning_star` | 6 | 6 | 6 Label | `morning star` × 6 | 6.43px | 1 | 7 (1.4%) | 66 |
| `evening_star` | 6 | 6 | 6 Label | `evening star` × 6 | 23.58px | 0 | 7 (1.4%) | 71 |
| `piercing_line` | 10 | 10 | 10 Label | `piercing line` × 10 | 25.72px | 0 | 10 (2.0%) | 40 |
| `dark_cloud_cover` | 5 | 5 | 5 Label | `dark cloud cover` × 5 | 12.86px | 1 | 5 (1.0%) | 46 |
| `three_white_soldiers` | 0 | 0 | 0 | — | — | — | 0 | 67 |
| `three_black_crows` | 2 | 2 | 2 Label | `three black crows` × 2 | 493.05px | 0 | 2 (0.4%) | 60 |
| `dragonfly_doji` | 9 | 9 | 9 Label | `dragonfly doji` × 9 | 2.14px | 2 | 9 (1.8%) | 44 |
| `gravestone_doji` | 2 | 2 | 2 Label | `gravestone doji` × 2 | 285.11px | 0 | 3 (0.6%) | 42 |
| `long_legged_doji` | 15 | 15 | 15 Label | `long legged doji` × 15 | 8.57px | 2 | 16 (3.2%) | 41 |
| `spinning_top` | 50 | 50 | 50 Label | `spinning top` × 50 | 2.14px | 43 | 110 (22.2%) | **2** |
| `tweezer` | 50 | 50 | 50 Label | `tweezer` × 50 | 2.14px | 49 | **378 (76.4%)** | 39 |
| `kicker` | 0 | 0 | 0 | — | — | — | 0 | 100 |
| `matching_low` | 10 | 10 | 10 Label | `matching low` × 10 | 6.43px | 3 | 12 (2.4%) | 58 |
| `on_neck` | 5 | 5 | 5 Label | `on neck` × 5 | 6.43px | 1 | 6 (1.2%) | 70 |
| `three_inside` | 50 | 50 | 50 Label | `three inside` × 50 | 4.29px | 36 | 65 (13.1%) | 31 |
| `three_outside` | 42 | 42 | 42 Label | `three outside` × 42 | 4.29px | 23 | 44 (8.9%) | 24 |
| `three_methods` | 0 | 0 | 0 | — | — | — | 0 | 88 |
| `abandoned_baby` | 0 | 0 | 0 | — | — | — | 0 | 92 |
| `tri_star` | 0 | 0 | 0 | — | — | — | 0 | 79 |
| `stick_sandwich` | 0 | 0 | 0 | — | — | — | 0 | 59 |
| `mat_hold` | 1 | 1 | 1 Label | `mat hold` × 1 | — | 0 | 2 (0.4%) | 93 |
| `tasuki_gap` | 0 | 0 | 0 | — | — | — | 0 | 74 |
| `upside_gap_two_crows` | 0 | 0 | 0 | — | — | — | 0 | 75 |
| `ladder_bottom` | 0 | 0 | 0 | — | — | — | 0 | 80 |
| `belt_hold` | 50 | 50 | 50 Label | `belt hold` × 50 | 2.14px | 46 | 123 (24.8%) | 22 |
| `hikkake` | 49 | 49 | 49 Label | `hikkake` × 49 | 4.29px | 27 | 51 (10.3%) | 16 |

"34 rendered, 0 fired-but-routed-nothing, 9 silent on this window — all 9 unreachable by clause on
a gapless feed." Every event in the family answers `status: "confirmed"` and `bars: [t, t]`
(`formed_at` is `None` in both detectors, so
[`as_concept_event`](../../../../crates/market-data/src/concepts.rs#L2423) spans the mark from the
bar's own time to itself); every event carries a unique bar and a unique price.

**The two routing facts that decide the picture.** All 34 ids answer `pane: false`,
`overlay: false`, `parameters: []`, `required_channels: CANDLES`
([concepts.rs:1856-2260](../../../../crates/market-data/src/concepts.rs#L1856)), and every one of
them reaches the same branch of the router — `concept_id.startsWith("candles.")` builds a
`candle_mark` at `zone_low` if bullish else `zone_high`, with `below: bullish` and
`label: short.replace(/_/g, " ")`
([concept_shapes.js:80-91](../../../../frontend/app/concept_shapes.js#L80)). That label is the
second half of the concept id with underscores turned into spaces, so what prints is
`three white soldiers`, `dark cloud cover`, `long legged doji` — not the snake_case the brief
expected, and not the pattern's proper name either. `CandleMark` then lowers to exactly one
`ScriptDraw::Label`, nudged 10px off the anchor
([scene.rs:2877-2973](../../../../frontend/chart-engine/src/scene.rs#L2877)), and the router keeps
only `out.slice(-200)` of the whole shape list
([concept_shapes.js:108](../../../../frontend/app/concept_shapes.js#L108)) — the family alone
supplies 560 marks on this window, so on a fully-populated chart the family cannot show its own
history.

## What the reference set says

Thirty-four Bulkowski pattern pages from *The Encyclopedia of Candlestick Charting Patterns* (via
thepatternsite.com), four LuxAlgo Library concept pages and one TradingView help page, all read on
2026-10-31. Every quote in the rows is verbatim; the structural points are these four.

1. **The mark is a glyph whose name is a hover, and its colour is the direction.** The TradingView
   sentence quoted above is the whole rendering contract for this family: label appears, colour
   encodes bullish/bearish/both, *the reading happens in the tooltip*. Three colours, not thirty-four
   words.
2. **Bulkowski's own figures mark patterns with a circle and an arrow, and name nothing.** "the
   black arrow on the figure shows which way price usually moves after the candlestick pattern ends"
   ([Visual Candlestick Index](https://thepatternsite.com/CandleVisual.html)); his per-pattern pages
   describe the pattern's example as "Circled in red is the three white soldiers candlestick pattern"
   ([Three White Soldiers](https://thepatternsite.com/ThreeWhiteSoldiers.html)). The name is the
   page title, never the pixel on the chart.
3. **Every definition is a configuration *plus a trend*, and the trend is the first clause.**
   "Look for the hammer to appear in a downward price trend and have a long lower shadow at least two
   or three times the height of the body with little or no upper shadow"
   ([Hammer](https://thepatternsite.com/Hammer.html)); "Look for two candles in a downward price
   trend" ([Bullish Engulfing](https://thepatternsite.com/BullEngulfing.html)); "Look for a small
   bodied candle atop a long lower shadow in an uptrend"
   ([Hanging Man](https://thepatternsite.com/HangingMan.html)). Each page carries a
   `Price trend leading to the pattern` field, and four of this family's concepts answer it with
   `None` or `None required` — `doji`-family (`dragonfly`, `gravestone`, `long-legged`), `marubozu`,
   `spinning top`, `kicker`, `hikkake` — which is the row-level evidence for the direction claims
   below.
4. **Frequency is published, so a hit rate can be checked rather than argued.** The same pages print
   `Frequency rank: N/103`: spinning top **2/103**, engulfing 12, belt hold 22, harami 25, marubozu
   27, three inside 31, hammer 36, shooting star 37, tweezer 39, piercing 40, long-legged doji 41,
   gravestone 42, dragonfly 44, dark cloud 46, doji-star 53, matching low 58, stick sandwich 59,
   three black crows 60, inverted hammer 61, morning star 66, soldiers 67, on-neck 70, evening star
   71, tasuki 74, upside gap two crows 75, tri-star 79, ladder bottom 80, three methods 88,
   abandoned baby 92, mat hold 93, kicker **100**. Two conclusions come straight out of this list:
   a rule that marks 22% of bars (`spinning_top`, rank 2) is *plausible*, while a rule that marks
   76% of bars for a rank-39 pattern (`tweezer`) and 24.8% for a rank-22 pattern (`belt_hold`) is
   not a busy chart — it is a broken clause.

## What we draw today

**1. A word, always, at the right bar.** 560 labels, one per event, all placed correctly: `x` on
the marked candle (all 560 within 0.6px), `y` = the marked candle's own low + 10px when the event
is bullish and its high − 10px otherwise, colour teal `0xCC26A69A` / red `0xCCEEF5350` / slate
`0xCC90A4AE` from the router's direction test. There is no glyph, no arrow, no dot, no hover, and
no tooltip path on the concept side at all — so the reference's *reading* step ("point to the
placemark and read the popup information") does not exist here, and the reference's *quietness*
step does not either: the name is the drawing.

**2. The word is 13–54× the bar and the marks collide by construction.** At a 2.14px pitch, any
label wider than ~14px (three characters) overlaps its neighbour's candle. The family's shortest
word is `doji` (~19px) and its longest `three black crows` (~82px), and 8 of the 25 live concepts
have a minimum pitch of 2.14–6.43px. On `tweezer`, whose 50 marks sit on essentially consecutive
bars, the words print over each other into a band; the pass's own collision counts are in the table
above. No label is clipped to the plot, so the newest one on every dense concept crosses the axis.

**3. `tweezer` is not a marker drawn wrong, it is a marker on bars that are not tweezers.** The
clause is `|low_n − low_{n−1}| ≤ 0.001 × price`, else the same on highs, with no trend test
([candlesticks.rs:101-107](../../../../crates/analytics-core/src/candlesticks.rs#L101)). On this
window 0.1% of price is **79.94 USD**, which is **0.48 of the median 15m bar range (166.33 USD)** —
so "the same low" means "within half a candle of each other", and 378 of 495 bars qualify. Bars
whose lows match to the cent: **3**; to the cent on highs: **0**. Bulkowski's sentence is "Look for
two candles sharing the same low price" and his rank for it is 39/103. This row is `differs_major`
on the bar, not on the glyph.

**4. Three concepts assert a direction the reference refuses to assert.** `long_legged_doji` is
hardwired `Side::Buy` ([candlesticks.rs:80-81](../../../../crates/analytics-core/src/candlesticks.rs#L80)),
so all 15 marks print teal *under* the low of an indecision candle whose own reference page answers
`Price trend leading to the pattern: None`; TradingView's rule for such a pattern is grey.
`matching_low` is likewise hardcoded Buy while Bulkowski's page reports matching lows that "act as
continuations most often" and requires *both* candles black — our clause tests the parent's colour
only. `hikkake` marks the **false break** as the signal: our rule is "an inside bar, then a bar
breaking both of its extremes", and the reference's own page adds a confirmation clause we do not
implement — "Price must rise above the high of the inside day — the second candle of the pattern —
in three days or less, after the candle ends"
([Bullish Hikkake](https://thepatternsite.com/HikkakeBull.html)) — so 29 of the 49 teal marks are
placed on the bar the reference says is not yet a pattern. These three rows carry the family's
other `differs_major` verdicts among the live concepts.

**5. Every multi-bar pattern arrives as one bar.** Both detectors set `formed_at: None`
([events.rs:3107](../../../../crates/analytics-core/src/events.rs#L3107),
[candlesticks.rs:67](../../../../crates/analytics-core/src/candlesticks.rs#L67)), so
`as_concept_event` emits `bars: [bar_time, bar_time]` and the wire carries no span. All 34 events
also carry `evidence: {close}` — one number, the close of the confirming bar. The consequence for
the drawing: a three-bar reversal (`morning_star`, `three_inside`, `tri_star`), a two-bar reversal
(`engulfing`, `harami`, `tweezer`) and a five-bar continuation (`three_methods`, `mat_hold`,
`ladder_bottom`) all paint the identical figure — one word at the confirming candle. No renderer
could draw the pattern's shape from this payload, which is the same transport finding passes 6 and
7 recorded in a different form. `warmup.bars` in the rows records the gate each detector actually
applies: 7 for the fourteen `events.rs` kinds (`if candles.len() < 7 { return out; }`,
[six trend bars + the pattern bar](../../../../crates/analytics-core/src/events.rs#L3070)), 2 for
the single- and two-bar `candlesticks.rs` kinds, 3 for its eight three-bar kinds
([early return at :132](../../../../crates/analytics-core/src/candlesticks.rs#L132)) and 5 for its
three five-bar kinds ([guard at :200](../../../../crates/analytics-core/src/candlesticks.rs#L200)).

**6. Two thresholds for one word, in one tree.** `events.rs:3119` calls a bar a marubozu at
`body ≥ 0.9 × range`; `candlesticks.rs:40` calls it one at `body ≥ 0.95 × range`, and that is the
version the `kicker` test reads. Doji agrees with itself (0.1 in both places), but note that a
`candles.marubozu` mark and a `candles.kicker` candidate therefore disagree about 5% of range on
the same candle. `spinning_top` shows the same looseness against its reference: our clause accepts
any bar with `0.1 < body/range ≤ 0.3` and shadows `> 0`, while the reference requires each shadow
*longer than the body* — 112 bars pass ours, 91 pass the reference's, so 21 of our tops are
notched by a tick.

**7. `hikkake` and `belt_hold` are the two rules whose base rate is the finding.** `belt_hold`
fires on 24.8% of bars because its clause is `body ≥ 0.5 × range` plus `|open − extreme| ≤ 0.1 ×
range`: no trend, and a tenth-of-range allowance where the reference allows **no** shadow ("a white
candle with no lower shadow, but closing near the high", rank 22/103). `hikkake` fires on 10.3%
because "inside bar then break both extremes" is a common bar sequence; the reference's rank-16
pattern is common too, but only after the false-break-and-confirm structure is applied, which ours
never tests. Both rows are `differs_major`, and both are fixable in the detector rather than in
the painter.

**8. The registry says none of this is drawn anywhere.** All 34 rows answer `pane: false,
overlay: false` — the eighth consecutive family, and here it is 34 of 182 rows, more than any
other family contributes. The marks are price-frame objects by definition, the router's placement
comes from a string prefix match on the concept id, and gate rule 5 correctly forbids calling any
of them a `series`.

## The nine silent concepts, and the clause that kills each one

Every one of the nine is silent in the scan **and** silent under an uncapped re-run of its own
clauses over the same 501 bars, so none is an artefact of the newest-50 cap. Seven die on a gap
clause; two die on a multi-bar configuration this feed never produces. The gap algebra first:
**0 of 500 adjacent bar pairs trade entirely above or entirely below their neighbour, 275 of 500
opens equal the prior close exactly, and the largest body-to-body gap in the window is 0.01 USD**.
Binance spot 15m candles are continuous, so a Nison gap is not a rare event here, it is an
unreachable one.

| concept | clause funnel over 501 bars | first clause that never passes |
| --- | --- | --- |
| `three_white_soldiers` | 50 rising bullish triples → 3 with all three bodies ≥ 60% of range → **0** also closing within 20% of their highs | `close ≥ high − 0.2 × range` on all three bars ([events.rs:3187-3193](../../../../crates/analytics-core/src/events.rs#L3187)) |
| `kicker` | 22 bars with `body ≥ 0.95 × range` → **0** adjacent such pairs → 0 with a gap | two consecutive marubozus *and* `last.low > prev.high` ([candlesticks.rs:109-115](../../../../crates/analytics-core/src/candlesticks.rs#L109)) |
| `three_methods` | 105 tall-white first bars → 20 keep three mid bodies ≤ half of it → 11 make all three mids black → 9 hold every mid high under bar 1's high → **0** pass all five mid clauses (2 windows in the series keep three bars wholly inside bar 1's range) | the pullback-inside-the-first-body box ([candlesticks.rs:206-220](../../../../crates/analytics-core/src/candlesticks.rs#L206)) |
| `abandoned_baby` | 258 windows with a near-flat middle bar (`\|close − open\| ≤ 0.001 × price`) → **0** where that bar's high sits below the prior bar's low | both gaps of the island ([candlesticks.rs:172-178](../../../../crates/analytics-core/src/candlesticks.rs#L172)) |
| `tri_star` | 40 doji bars → **0** windows of three consecutive dojis (and the middle body must clear both neighbours) | three adjacent dojis ([candlesticks.rs:163-168](../../../../crates/analytics-core/src/candlesticks.rs#L163)) |
| `stick_sandwich` | 69 down-up-down triples → 1 with the middle low above the first close → **0** closing within 10% of the first bar's range of the first close | the two-matching-closes pair ([candlesticks.rs:182-186](../../../../crates/analytics-core/src/candlesticks.rs#L182)) |
| `tasuki_gap` | 243 same-direction adjacent pairs → 60 where the second opens beyond the first close → **0** complete the third bar's partial fill | the third bar inside the gap ([candlesticks.rs:189-197](../../../../crates/analytics-core/src/candlesticks.rs#L189)) |
| `upside_gap_two_crows` | 105 tall-white first bars → 50 followed by black → 9 with the second's low or open above the first close → **0** complete the engulfing third bar | the surviving gap ([candlesticks.rs:262-280](../../../../crates/analytics-core/src/candlesticks.rs#L262)) |
| `ladder_bottom` | 72 windows of three consecutive black bars → 72 with descending closes → **0** ending in a white bar that opens above the prior bar's *open* | the gap-up fifth day, already relaxed to `open > prev.open` ([candlesticks.rs:235-242](../../../../crates/analytics-core/src/candlesticks.rs#L235)) |

Bulkowski's ranks corroborate the split: the gap-bound patterns are his rarest — kicker 100/103,
mat hold 93, abandoned baby 92, three methods 88, ladder bottom 80, tri-star 79, upside gap two
crows 75 — so silence is *expected* for several of them. The two that should not be silent are
`three_white_soldiers` (67/103, "Bullish reversal 82% of the time") and `stick_sandwich` (59/103),
where our own clauses, not the market, are the wall. Two further consequences of the same fact: the
`abandoned_baby` doji test is an absolute (`0.001 × price` ≈ 79.9 USD) rather than a body/range
ratio, so 258 of 501 bars pass it; and because gaps cannot occur, `morning_star` and `evening_star`
— whose references *define* them by two gaps — can only fire at all because our clause drops the
gap test (7 and 7 hits).

## The card is a second renderer, and it draws a completely different figure

`previewEvents` branches on payload, not on family: because **every** candle event ships both
`zone_low` and `zone_high` (the pattern bar's own extremes —
[events.rs:3104-3105](../../../../crates/analytics-core/src/events.rs#L3104),
[candlesticks.rs:64-65](../../../../crates/analytics-core/src/candlesticks.rs#L64)), all 560
events take the first branch and none can reach the third, which is the branch the function's own
doc comment reserves for "anything that names a candle rather than measures a price"
([app.js:14921-14961](../../../../frontend/app/app.js#L14921)).

| | the chart | the card |
| --- | --- | --- |
| what one event draws | one `Label`: the pattern's spaced lowercase name, 10px off the bar's extreme, coloured by direction | a filled (α 0.18) *and* stroked (α 0.75) rectangle from `scale(zone_high)` to `scale(zone_low)` |
| how long it is | one word at one x | `right = axis.bars − 1` for every `confirmed` event, i.e. the band **runs to the card's right edge** ([app.js:14933-14934](../../../../frontend/app/app.js#L14933)) — the bar's whole range, smeared across the 96-bar crop (`PREVIEW_BARS`, [app.js:14564](../../../../frontend/app/app.js#L14564)) |
| what it says | the pattern's name, always | **nothing** — the card draws no text for any shape |

So the two renderers disagree in both directions: on the chart a hammer is a word and no shape; on
the card a hammer is the candle's own range as a translucent ribbon to the right edge and no name.
Neither one is the reference's glyph-with-hover, and a trader who reads the card concludes the
pattern is a *price zone* that persists indefinitely. `preview_agrees`: **25 `differs`, 9
`not_rendered`**, 0 `agrees` — the largest disagreement surface of any family audited so far,
because the card's shape is derived from the payload rather than from the concept.

## No parameter is honoured

All 34 declare `parameters: []`
([concepts.rs:1856-2260](../../../../crates/market-data/src/concepts.rs#L1856)) and honour the same
empty list. This family has more numbers than any audited family before it and none of them are
exposed: the doji body ratio (0.1), the marubozu ratios (0.9 and 0.95 in two places), the
hammer/shooting-star shadow multiple (2.0), the upper-shadow allowance (`max(body, 0.05 × range)`),
the trend gate's step fraction (0.6 of 6 closes), the engulfing/harami body comparisons, the
star's first-bar strength (0.5 × range) and middle-bar size (0.4 × parent body), the soldiers' and
crows' `strong()` pair (0.6 and 0.2), the spinning top's band (0.1–0.3), the belt hold's 0.5 and
0.1, the tweezer's 0.001, the matching low's 0.05 × mean range, the on-neck's and stick sandwich's
0.1 × range, the hikkake's inside-bar definition, the three-methods and mat-hold mid-body ceiling
(0.5 × b1 body), and the route's own newest-50 cap. TradingView exposes the *choice of pattern set*
as the indicator, and Bulkowski's identification is stated as ratios precisely so a reader can
tighten them; ours is thirty hard-coded constants with a single default. The one parameter this
family most needs is the tweezer tolerance: at 0.1% of price it is half a bar range on BTCUSDT 15m
and would be a rounding error on a $5 equity — an absolute that silently changes meaning across
instruments, which the audit's own arithmetic-over-market-data constraint exists to catch.

## The 34 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `bar_marker` (34) | every concept in this family | **34 wiring** | a glyph at the bar — arrow, dot, placemark — with the name optional and available on hover. The scripting path already lowers `CandleMark` to a `ScriptDraw::Label` and paints it ([scene.rs:2877-2973](../../../../frontend/chart-engine/src/scene.rs#L2877)), so the painter exists one layer over; what the family needs is a glyph in place of (or ahead of) the word, a clip to the plot, and a hover/tooltip path on the concept side |

`draw.verdict`: **21 `differs_minor`, 13 `differs_major`, 0 `matches`, 0
`impossible_in_vocabulary`**. The 13 majors are the 9 silent concepts plus `tweezer`, `belt_hold`,
`hikkake` and `long_legged_doji` — four live rows marked at the wrong bars or with a direction the
reference refuses to claim. The 21 minors are all one defect: a permanent lowercase word where the
reference puts a glyph whose name appears on hover; in every one of them the bar, the price and
(besides `long_legged_doji`) the side are the reference's own. `fires.verdict`: **2 `same_bars`
(`doji`, `marubozu`) and 32 `different_rule`** — the two `same_bars` rows are the only concepts in
the family whose reference states no numeric threshold at all, so a body/range cut *is* the
reference read faithfully; the other 32 lose a clause (trend, gap, confirmation, second bar's
colour, which close/band to clear) or invent one (hardcoded direction, an absolute tolerance).
All 34 rows carry a citation — 30 Bulkowski pages, 4 LuxAlgo pages (doji, marubozu, inverted
hammer, spinning top), plus the TradingView help page quoted above for the rendering convention.

This pass settles nothing new in [the vocabulary](00-vocabulary.md): all 34 rows claim `bar_marker`,
which pass 3 already claimed as `wiring`, and the audit's first `matches` is still not earned — this
family is the closest any pass has come, and the gap it records is one glyph wide.

## Fixes this pass asks for

1. **Draw a glyph and keep the name for the hover.** An arrow under the low (bullish) or over the
   high (bearish), grey for a directionless read, with the pattern name in a tooltip — TradingView's
   contract, quoted above, and Bulkowski's arrow, quoted above, agree on it. This single change
   turns 21 `differs_minor` rows into `matches` and retires the collision and overflow findings.
2. **Clip concept text to the plot, and cap the label density.** The painter sets no clip, so the
   newest mark of six concepts crosses x 1084 into the price gutter. On a chart with all concepts
   on, `out.slice(-200)` already throws away more candle marks than it draws
   ([concept_shapes.js:108](../../../../frontend/app/concept_shapes.js#L108)).
3. **Fix `tweezer`: match the extreme, or say so.** 378 of 495 bars is not a tweezer set. Either
   compare to the tick the instrument prints at, or keep a relative tolerance and expose it — and
   add the trend clause the reference's own configuration line opens with.
4. **Stop inventing directions.** `long_legged_doji` and `matching_low` should emit `Side::None`
   (grey), and `hikkake` should not emit at all until the confirmation bar arrives within three
   bars, per the reference's own page.
5. **Give `belt_hold` and `spinning_top` their shadow clauses.** No shadow at the open (not
   10% of range) for the belt hold; each shadow *longer than the body* for the top. That is the
   difference between 24.8% of bars marked and a rank-22 pattern, and between 112 tops and 91.
6. **Put the pattern's span on the wire.** Both detectors should set `formed_at` to the pattern's
   first bar so `bars` arrives as `[first, confirming]`; every two-, three- and five-bar row in
   this family is currently drawn as one bar, and the card cannot even do that honestly.
7. **Reconcile the marubozu threshold** (0.9 in `events.rs:3119`, 0.95 in `candlesticks.rs:40`) and
   replace the two absolute doji/baby tolerances (`0.001 × price`) with body/range ratios, which are
   the ratio-stated form the reference uses and do not change meaning between a $79,940 asset and a
   $5 one.
8. **Say what the gap-bound concepts are for.** Nine of 34 concepts can never fire on a continuous
   spot-crypto feed. They should either be marked venue-conditional (an island/futures variant) or
   documented as unreachable on this feed — `three_white_soldiers` and `stick_sandwich` are the
   exceptions: their silence is our clause, not the market, and the funnel table above localises
   both.
9. **Mark these ids `overlay` in the registry** and stop deriving placement from a `startsWith` on
   the concept id — the eighth consecutive family to ask, and the largest single block of rows
   (34 of 182) affected.
10. **Give the family's thresholds to the registry.** Thirty constants today; the ratio-based ones
   (doji body, shadow multiple, trend step fraction, mid-body ceiling) are the parameters a candle
   library exposes, and the ones that decide hit rate (tweezer tolerance, belt-hold shadow
   allowance) are the ones a trader needs to tighten.
