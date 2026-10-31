# Pass 10c — Volume (5 rows)

Judged 2026-10-31 against `main`, on the window the gateway serves today (501 bars,
2026-10-26T06:30 to 2026-10-31T11:30 UTC). The machine-readable verdicts are the five rows of
[`ledger/volume.json`](../../../../reports/concept-visual-audit/ledger/volume.json);
`node tools/concept_visual_audit.mjs check --family volume` reports **5 rows, 0 incomplete**.

**The volume family is where the label's y is most obviously meaningless, and where the pane is
missing the mark the concept is named for.** OBV, A/D, CMF and MFI are cumulative or bounded
volume quantities; their events are divergences, zero crossings and zone entries; their labels sit
at the signal bar's **price**, between 70 and 447 in a 484px plot, for values that are neither
prices nor anywhere near the price scale. Anchored VWAP is the one row whose value *is* a price,
and its double draw is the mildest: two already-correct lines with the words `anchored vwap`
printed over them 50 times. The family's real finding is narrower and sharper: **`obv` and
`ad_line` are the only two concept events in the pass whose mark exists in no renderer at all.**
Their panes draw the line and nothing else
([`indicator_series.rs:392-409`](../../../../crates/analytics-core/src/indicator_series.rs#L392),
[`644-657`](../../../../crates/analytics-core/src/indicator_series.rs#L644)); the divergence
segment and marker the detector reports appear nowhere except as a word at price.

## How it was measured

`node tools/concept_visual_audit.mjs render volume` on 2026-10-31 rebuilt all five pages over the
501-bar window above. The family supplied **148 events, 148 draws and 148 labels**, every event a
Label on the price frame. Every number below comes from parsing the pages' embedded scenes and from
the scan's own payloads.

| concept | registry | events | label text | label y span (484px plot) | what the series route draws | `draw` |
| --- | --- | --- | --- | --- | --- | --- |
| `indicators.obv` | pane | 11 | `obv` | 77.4-417.2 | OBV line only — **no divergence segment or marker** | `differs_major` |
| `indicators.ad_line` | pane | 20 | `ad line` | 77.4-416.6 | A/D line only — **no divergence segment or marker** | `differs_major` |
| `indicators.cmf` | pane | 50 | `cmf` | 80.2-447.3 | CMF line, zero level | `differs_major` |
| `indicators.mfi` | pane | 17 | `mfi` | 70.4-486.1 | MFI, fixed 0-100, levels 80/20 | `differs_major` |
| `indicators.anchored_vwap` | overlay | 50 | `anchored vwap` | 46.8-479.7 | two anchor lines (swing high, swing low), no fill | `differs_major` |

**`cmf`, `mfi` and `anchored_vwap` hit the route's newest-50 cap**, not a signal count. `obv` (11)
and `ad_line` (20) are real detection counts — divergences are sparse by construction. The
asymmetry is instructive: the two concepts whose marks are missing from the pane are the two whose
event count is honest.

## What the reference set says

- **OBV is a cumulative buying/selling-pressure line in its own pane.** TradingView: *"The On
  Balance Volume indicator (OBV) is used in technical analysis to measure buying and selling
  pressure."* The line is the drawing. TradingView plots no divergence unless the user adds a
  divergence script; when one is drawn it rides **on the line**.
- **A/D is the same object with an intrabar weighting.** TradingView: *"The Accumulation
  Distribution (ADL) indicator is a cumulative volume-based technical analysis tool that measures
  the flow of money into and out of a security."* A cumulative volume line, not a price, not a
  word at price.
- **CMF is a ratio around zero.** TradingView: *"Chaikin Money Flow (CMF) is a technical analysis
  indicator used to measure Money Flow Volume over a set period of time."* The line crosses zero;
  the crossing is read off the line.
- **MFI is bounded 0-100 with 80/20 levels.** TradingView: *"The Money Flow Index indicator (MFI)
  is a tool used in technical analysis for measuring buying and selling pressure."* Same geometry
  as RSI: one line, fixed scale, two levels.
- **Anchored VWAP starts where the user anchored it.** TradingView: *"Anchored VWAP displays the
  volume-weighted average price for a specific time period, starting from a user-selected point."*
  The anchor is the parameter; the line runs from it forward.

## What we draw today

**Two concepts' marks are drawn nowhere.** `oscillator_divergence` emits `ObvDivergence` and
`AdDivergence` events with segments and pivots
([`indicator_signals.rs:26-30, 630`](../../../../crates/analytics-core/src/indicator_signals.rs#L26)),
but `obv_pane` and `ad_pane` return `segments: Vec::new(), markers: Vec::new()`. The event reaches
the browser, `ConceptShapes.from` lowers it to a `candle_mark`, and the divergence becomes the
word `obv` or `ad line` at the pivot bar's price. A divergence is a relationship between two points
on a trace; with the trace's values absent from the label, **the drawing carries no information
about the divergence at all** — not its direction, not its two pivots, not its vertical distance.

**Three panes are correct and their labels are pure duplication.** `cmf` has its zero line
([`indicator_series.rs:660-676`](../../../../crates/analytics-core/src/indicator_series.rs#L660)),
`mfi` its fixed 0-100 scale and 80/20 levels
([`678-695`](../../../../crates/analytics-core/src/indicator_series.rs#L678)); both are the
reference's drawing. `anchored_vwap` puts two VWAP lines on the price frame — the right object —
but the anchors are chosen automatically as the newest 3-bar swing high and low
([`indicator_series.rs:697-714`](../../../../crates/analytics-core/src/indicator_series.rs#L697)),
not by the user. The reference makes the anchor an input; ours is an inference. That is a
parameters gap recorded below, not a drawing defect.

**Anchored VWAP is the family's one label at a legitimate scale.** Its 50 labels are at prices
between y 46.8 and 479.7 because the VWAP *is* a price — the same precedent as Pass 9's
`volume.spike` and Pass 9's `poc`: a label at price is not wrong because it is at price, but it
is still a second mark over a line that already shows the level. The other four rows have no such
defence: OBV, A/D and CMF cumulative values and the MFI index are not prices.

## The card is a second renderer, and here it draws the same thing the chart's series route does

For all five rows the card asks `/indicator-series` and paints the returned spec
([`conceptPreviewPayload`, app.js:14522-14527](../../../../frontend/app/app.js#L14522)). All five
are `preview_agrees: differs`, for the same reason as the trend and volatility families: the card
draws the series without the concept's word; the chart draws both. On `obv` and `ad_line` the card
and the chart's pane agree that the divergence is invisible — the card simply omits the word that
is the only trace of it on the chart.

## Parameters: five concepts, none declared, none honoured

Every row carries `parameters: []` and `honoured: []`. The constants behind the drawings are
OBV's (none), A/D's (none), CMF's period 20, MFI's period 14 and its 80/20 levels, and AVWAP's
3-bar swing anchor plus its two-anchor choice. **The AVWAP anchor is the reference's primary
input** — a user-selected point — and it is the one in this family that a user would reach for
first. The audit records the gap; it does not fix it.

## The 5 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `series_line` (5) | `obv`, `ad_line`, `cmf`, `mfi`, `anchored_vwap` | **5 wiring**, plus segments/markers on the two divergence rows | the existing `SubPaneSpec::Series` / `OverlaySeriesSpec` arms. `obv` and `ad_line` additionally need their divergence events emitted as `SeriesSegment` + `SeriesMarker` inside the same pane spec — the fields exist ([scene.rs:431-438](../../../frontend/chart-engine/src/scene.rs#L431)) and `rsi_pane` already fills them, so this is builder wiring, not a new shape |

`draw.verdict`: **5 `differs_major`, 0 `matches`**. `fires.verdict`: **5 `continuous`** — the
divergence and cross events are our readings of continuous traces and the reference marks nothing.
`preview_agrees`: **5 `differs`**.

## What this pass asks of the vocabulary

The series vocabulary settled in [`10-trend.md`](10-trend.md#what-this-pass-asks-of-the-vocabulary)
covers all five rows unchanged. This family is the concrete evidence for the decision that
**`series_marks` folds into `series_line`**: the OBV and A/D divergences are marks whose y is only
meaningful on the trace, and a mark without the trace would have to carry the trace anyway.
`rsi_pane` in the momentum family is the working example — it emits the trace *and* the divergence
segments and markers in one spec — and it is the precedent these two rows should follow.

## Fixes this pass asks for

1. **Draw the OBV and A/D divergences on their lines.** Fill `segments` and `markers` in
   `obv_pane` and `ad_pane` from the same events `rsi_pane` already uses, instead of leaving the
   divergence to a word at price. This is the family's most informative missing mark.
2. **Make the anchored VWAP's anchor a parameter.** Today it is inferred from the newest 3-bar
   swing high and low; the reference's anchor is user-selected. Even before the parameter path
   exists, the registry should name one.
3. **Stop routing `indicators.*` events into `candle_mark`** — the pass-wide fix stated once in
   [`10-trend.md`](10-trend.md#fixes-this-pass-asks-for). This family's 148 labels come off, and
   on `obv`/`ad_line` fix 1 is what replaces the information they were wrongly carrying.
