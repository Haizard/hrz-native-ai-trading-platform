# Pass 10b — Volatility (4 rows)

Judged 2026-10-31 against `main`, on the window the gateway serves today (501 bars,
2026-10-26T06:30 to 2026-10-31T11:30 UTC). The machine-readable verdicts are the four rows of
[`ledger/volatility.json`](../../../../reports/concept-visual-audit/ledger/volatility.json);
`node tools/concept_visual_audit.mjs check --family volatility` reports **4 rows, 0 incomplete**.

**Four concepts, four correct series, 64 words on top of them.** Bollinger's three lines are drawn
over the candles and `bollinger` is printed 50 times along the price axis beside them. ATR, the ATR
family and %B are drawn in panes with their own scales, and each pane's concept name is stamped on
the price frame at the signal bar's close. The row that matters most here is not the duplication —
it is `bollinger_percent`, where **the pane itself is on the wrong scale**: `%B` (bounded near 0..1
around its 0/1 levels) and bandwidth (unbounded, in price units) share one `SeriesScale::Auto`
([`indicator_series.rs:452-465`](../../../../crates/analytics-core/src/indicator_series.rs#L452)),
so the reference levels sit somewhere on a scale neither trace fills. This is the family where the
series route is not automatically the correct drawing: two of the four panes need their scale fixed
before the words are worth removing.

## How it was measured

`node tools/concept_visual_audit.mjs render volatility` on 2026-10-31 rebuilt all four pages over
the 501-bar window above. The family supplied **64 events, 64 draws and 64 labels** — every event
lowered to a Label on the price frame. Every number below comes from parsing the pages' embedded
scenes and from the scan's own payloads.

| concept | registry | events | label text | label y span (484px plot) | what the series route draws | `draw` |
| --- | --- | --- | --- | --- | --- | --- |
| `indicators.bollinger_percent` | pane | 10 | `bollinger percent` | 93.6-419.2 | %B + bandwidth on **one auto scale**, levels 1 and 0 | `differs_major` |
| `indicators.atr_family` | pane | 2 | `atr family` | 52.4-136.9 | 23 ATR lines, 2..24, auto-scaled | `differs_major` |
| `indicators.atr` | pane | 2 | `atr` | 52.4-234.5 | one ATR(14) line, auto-scaled | `differs_major` |
| `indicators.bollinger` | overlay | 50 | `bollinger` | 67.9-486.1 | upper/middle/lower, no fill | `differs_major` |

**The labels are at price in every row, including the two whose panes answer in other units.** ATR
is measured in the instrument's own price units, so its label's height is at least dimensionally a
price — but the line is in the pane and the label is on the candles, so it says nothing about the
ATR value. `%B` is a dimensionless ratio: its labels at y 93.6-419.2 are nine distinct prices for a
quantity bounded near 0..1. The mapping is the same one every `indicators.*` event takes
([`concept_shapes.js:84-90`](../../../../frontend/app/concept_shapes.js#L84)): `price_level` or
`zone_low` through the price frame.

**The `%B` pane's scale defect is measurable from the builder, not from taste.** The reference
draws %B against 1 (upper band) and 0 (lower band). Our builder puts `levels: vec![1.0, 0.0]` on a
pane whose auto scale is set by bandwidth — a number like 0.02 or 0.2 for this instrument, not 1.0.
The two reference levels therefore lie outside (or at the very edge of) the plotted range, and the
%B trace, which oscillates around 0.5, is a nearly flat line at the bottom of a pane scaled to
bandwidth. The builder's own label says it: *"Bollinger %B + bandwidth"*
([`indicator_series.rs:453`](../../../../crates/analytics-core/src/indicator_series.rs#L453)).

## What the reference set says

- **%B quantifies price against the band, on a scale where 0 and 1 are the bands.** TradingView:
  *"What the %b indicator does is quantify or display where price is in relation to the bands."*
  The 1 and 0 levels are the bands; a %B above 1 or below 0 means price is outside them. The
  percentage bandwidth is a **separate** study on its own scale — TradingView's "Bollinger Bands
  Width" — because it is in price units.
- **ATR is a value in price units, read for its level.** TradingView: *"The way to interpret the
  Average True Range is that the higher the ATR value, then the higher the level of volatility."*
  One line in its own pane; the reading is the height of the line, and no signal is printed at the
  bar.
- **Bollinger Bands are three lines over the candles.** TradingView: *"Bollinger Bands consist of a
  band of three lines: a middle simple moving average, and upper and lower bands set two standard
  deviations away from the middle line."* The reference's page shows the bands filled — the space
  between them is the object — and marks no bar.

## What we draw today

**`bollinger` is the pass's clearest double draw: the correct band and 50 words lying on it.** The
overlay route paints upper/middle/lower over the candles
([`bollinger_overlay`, indicator_series.rs:1118](../../../../crates/analytics-core/src/indicator_series.rs#L1118)),
which is the reference's drawing minus the fill. The concept route then routes the same concept's
breakout events — close outside the band, the reference's everyday reading — through `candle_mark`
and prints `bollinger` 50 times (the route cap) across 45 distinct prices between y 67.9 and 486.1.
Fifty of the labels sit **on** the band they name.

**`atr` and `atr_family` announce an expansion at a price.** Both panes are correct and need no
change (the family ribbon is our composition of the reference's single line; its expansion is a
legitimate house event, and the row's `fires` verdict is `continuous` because the reference marks
nothing). The labels add nothing to the panes: 2 events each, each one a word at the close of the
expansion bar, describing a movement the pane already shows.

**`bollinger_percent` needs its pane fixed before its label goes.** %B and bandwidth share one auto
scale; the 1/0 levels are drawn against a scale bandwidth sets. The reference's own shape is %B
alone against 0/1 — or %B and bandwidth as two stacked panes. This is the only row in the four
where "wire the existing series" is not sufficient: the series exists and is wrong.

**The Bollinger band is unfilled.** The reference calls it a band; our three lines have no body,
and `bollinger_overlay` returns `fills: Vec::new()`
([`indicator_series.rs:1140`](../../../../crates/analytics-core/src/indicator_series.rs#L1140)).
The overlay fill arm is already live for Ichimoku, so this is the same one-line wiring Pass 10a
asks for on Keltner and Donchian.

## The card is a second renderer, and here it is the honest one

For all four rows the card asks `/indicator-series` and paints the returned spec
([`conceptPreviewPayload`, app.js:14522-14527](../../../../frontend/app/app.js#L14522)). So all four
rows are `preview_agrees: differs` for the same reason as the trend family: the card draws the
series, the chart draws the series **and** the concept's word. On `bollinger_percent` the two
renderers agree about the pane's scale — both draw the wrong one — which is exactly the kind of
agreement the pass does not count as a match.

## Parameters: four concepts, none declared, none honoured

Every row carries `parameters: []` and `honoured: []`. The numbers that move these drawings are
constants: Bollinger (20, 2.0), ATR(14), the ATR family's periods 2..=24, the %B periods. The
reference exposes every one as a setting; TradingView's Bollinger page is the archetype — the
period, the standard-deviation multiplier and the source are user inputs. This catalog cannot
adjust any of them.

## The 4 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `series_line` (4) | `bollinger_percent`, `atr_family`, `atr`, `bollinger` | **4 wiring**, plus a scale fix on `bollinger_percent` | the existing `SubPaneSpec::Series` / `OverlaySeriesSpec` arms. `bollinger_percent` additionally needs %B on its own 0..1 fixed scale with bandwidth split to a second pane or dropped; `bollinger` needs its band filled through `OverlayFillSpec` |

`draw.verdict`: **4 `differs_major`, 0 `matches`**. `fires.verdict`: **4 `continuous`** — every
volatility concept has a value on every bar and the reference marks no signal. `preview_agrees`:
**4 `differs`**.

## What this pass asks of the vocabulary

The series vocabulary settled in
[`10-trend.md`](10-trend.md#what-this-pass-asks-of-the-vocabulary) covers all four rows unchanged —
`series_line` on a pane or on the price frame, with `levels` as a member. The one thing this family
adds is a constraint on the shape's data, not a new shape:

**A `series_line` on a fixed scale must not share its pane with a trace on another scale.**
`bollinger_percent` is the proof by counterexample. `SeriesScale::Fixed { 0, 1 }` versus
`SeriesScale::Auto` is a per-pane choice, and a builder that needs both must emit two panes. This
is a builder rule, not a renderer rule; the vocabulary records it because Task 18's
"implementable without reading a pass" test has to tell the implementer which scale to pick.

## Fixes this pass asks for

1. **Split %B and bandwidth into two panes, or drop bandwidth.** Give %B its own fixed 0..1 scale
   (with the 0 and 1 levels) and put bandwidth in a second pane if it is drawn at all. Today the two
   traces fight over one auto scale and the reference levels are meaningless.
2. **Fill the Bollinger band** through the existing `OverlayFillSpec` arm — same wiring as Keltner
   and Donchian in Pass 10a.
3. **Stop routing `indicators.*` events into `candle_mark`** — the pass-wide fix stated once in
   [`10-trend.md`](10-trend.md#fixes-this-pass-asks-for). For this family it removes 64 words,
   including 50 lying on top of the band they name.
