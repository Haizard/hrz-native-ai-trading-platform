# Pass 10b — Momentum (10 rows)

Judged 2026-10-31 against `main`, on the window the gateway serves today (501 bars,
2026-10-26T06:30 to 2026-10-31T11:30 UTC). The machine-readable verdicts are the ten rows of
[`ledger/momentum.json`](../../../../reports/concept-visual-audit/ledger/momentum.json);
`node tools/concept_visual_audit.mjs check --family momentum` reports **10 rows, 0 incomplete**.

**The oscillators are the best-drawn family in the audit, and their labels are the worst-placed.**
All ten concepts have a correct pane or histogram in the series route: RSI at fixed 0-100 with
70/30, Stochastic at 0-100 with 80/20, CCI against +/-100, Williams at -100..0, ROC and TSI around
zero, the Awesome Oscillator as zero-anchored bars, the 23-line RSI family ribbon, and the RSI-PCA
prediction with its zero line. Every one is then contradicted by the concept path, which stamps the
oscillator's name at a **price** — `williams r` between y 48.2 and 477.3, `stochastic` between 324.4
and 486.1, `rsi` between 45.7 and 297.3. A bounded 0-100 number cannot be 477 pixels down a price
axis. The scale error is not a matter of styling; the words are on the wrong plane.

**287 events, 287 labels, and two of the ten are genuinely rare.** `rsi_divergence` produced 4
divergences in 501 bars and `rsi_pca` 7 signals after its 248-bar warm-up; those are detection
counts, and the reference draws both as marks on the oscillator. The rest are cap-or-frequency
counts: `stochastic`, `williams_r` and `roc` answer exactly 50 (the route's newest-50 limit,
[`concept_routes.rs:240`](../../../../crates/api-gateway/src/concept_routes.rs#L240)), `cci` 46,
`tsi` 39, `awesome` 24, `rsi` 9, `rsi_family` 8. None of them is a signal count in the sense the
reference uses, because the reference marks no signal at all: it plots the oscillator.

**RSI-PCA is the one concept with no external convention, and the row says so.** The prediction pane
is the house model, not an RSI pane with an extra line; its own header records the provenance
("The neurotrader RSI-PCA idea, in-house", [`rsi_pca.rs`](../../../../crates/analytics-core/src/rsi_pca.rs)).
The row carries `{ no_convention: true, reason }` and judges the pane against the shape the model
already has: one auto-scaled prediction line, a zero line, a dot at each signal.

## How it was measured

`node tools/concept_visual_audit.mjs render momentum` on 2026-10-31 rebuilt all ten pages over the
501-bar window above. The family supplied **287 events, 287 draws** and **287 labels**. Every number
below comes from parsing the pages' embedded scenes and from the scan's own payloads.

| concept | registry | events | label text | label y span (484px plot) | what the series route draws | `draw` |
| --- | --- | --- | --- | --- | --- | --- |
| `indicators.rsi_pca` | pane | 7 | `rsi pca` | 61.4-407.1 | prediction line, auto scale, zero line, signal dots | `differs_major` |
| `indicators.rsi` | pane | 9 | `rsi` | 45.7-297.3 | RSI(14), fixed 0-100, levels 70/30 | `differs_major` |
| `indicators.rsi_divergence` | pane | 4 | `rsi divergence` | 37.9-460.3 | the RSI pane plus divergence segments and dots | `differs_major` |
| `indicators.rsi_family` | pane | 8 | `rsi family` | 29.7-460.3 | 23 RSI lines, fixed 0-100, 70/30 | `differs_major` |
| `indicators.stochastic` | pane | 50 | `stochastic` | 324.4-486.1 | %K/%D, fixed 0-100, 80/20 | `differs_major` |
| `indicators.cci` | pane | 46 | `cci` | 71.2-455.3 | CCI(20), auto, levels +/-100 | `differs_major` |
| `indicators.williams_r` | pane | 50 | `williams r` | 48.2-477.3 | %R, fixed -100..0, levels -20/-80 | `differs_major` |
| `indicators.roc` | pane | 50 | `roc` | 60.1-447.1 | ROC(12), auto, zero line | `differs_major` |
| `indicators.tsi` | pane | 39 | `tsi` | 55.3-447.3 | TSI + signal, auto, zero line | `differs_major` |
| `indicators.awesome` | pane | 24 | `awesome` | 94.4-419.5 | zero-anchored histogram bars | `differs_major` |

Every label's y is the **price at the signal bar**, routed through the price frame by the
`candle_mark` branch ([`concept_shapes.js:84-90`](../../../../frontend/app/concept_shapes.js#L84)).
The `stochastic` labels are the clearest evidence: they sit in the bottom 102 pixels of the price
plot because the signals arrived near the low of the window, not because Stochastic was near 20.

## What the reference set says

- **RSI is a separate pane, 0-100, 30/70.** TradingView: *"It is plotted in a separate pane below
  the price chart, oscillating between 0 and 100 with overbought at 70 and oversold at 30."*
- **Divergence is drawn on the oscillator.** TradingView's RSI Divergence tool: *"The divergence
  indicator helps traders assess changes in the price trend and indicates when price will move with
  or against the direction of another indicator."* The line joins two RSI pivots; it does not put a
  word at price.
- **The 23-period RSI ribbon is our composition, not theirs.** TradingView's RSI page establishes
  the pane, the fixed scale and the 70/30 levels; the ribbon repeats that one trace across periods.
  The row cites the RSI page for the pane convention and says so.
- **Stochastic marks 80 and 20.** TradingView: *"Overbought levels are at 80 and oversold levels are
  at 20."*
- **CCI reads +/-100.** TradingView: *"Values above +100 indicate overbought conditions, while
  values below -100 signal oversold markets."*
- **Williams %R is a bounded momentum oscillator.** TradingView: *"Williams %R (%R) is a
  momentum-based oscillator used in technical analysis, primarily to identify overbought and
  oversold conditions."*
- **ROC is a percent-change oscillator.** TradingView: *"The Rate of Change indicator (ROC) is a
  momentum oscillator. It calculates the percent change in price between periods."*
- **TSI is a two-line oscillator.** TradingView: *"The True Strength Index indicator is a momentum
  oscillator designed to detect, confirm or visualize the strength of a trend."*
- **The Awesome Oscillator is a histogram around zero.** TradingView: *"The Awesome Oscillator is an
  indicator used to measure market momentum."* The reference plots it as colour-coded bars.
- **RSI-PCA has no external drawing.** The model is in-house; its published form is a repo, not a
  chart convention. The row's reference is `no_convention` with the reason, and the canonical is the
  pane the model already has.

## What we draw today

**Nine panes and one histogram, all correct, plus 287 words at price.** The series route's shapes
match the references line for line:

- `rsi` and `rsi_divergence` share `rsi_pane()`: fixed 0-100, levels 70/30, and — when divergences
  are on — a segment between the two RSI pivots and a dot at the confirming bar
  ([`indicator_series.rs:1146-1200`](../../../../crates/analytics-core/src/indicator_series.rs#L1146)).
  This is the one place in the pass where the concept's *event* has a correct drawing waiting for it:
  the divergence mark the detector found is already in the pane, and the price-frame word is a
  duplicate of it.
- `rsi_family` draws 23 lines on the fixed scale with 70/30; only the two edge periods are named
  ([`indicator_series.rs:186-227`](../../../../crates/analytics-core/src/indicator_series.rs#L186)).
- `rsi_pca` draws one auto-scaled prediction line, a zero line and a dot per signal
  ([`indicator_series.rs:1203-1250`](../../../../crates/analytics-core/src/indicator_series.rs#L1203)).
  The model refuses before 248 bars (48 warm-up + 200 training,
  [`rsi_pca.rs:237-241`](../../../../crates/analytics-core/src/rsi_pca.rs#L237)), so in a 501-bar
  window it has 253 predictions and produced 7 signals.
- `stochastic` draws %K/%D at 80/20, `cci` a line at +/-100, `williams_r` at -20/-80, `roc` a line
  across zero, `tsi` two lines across zero, `awesome` zero-anchored bars.

**What the concept path adds is always the same word.** The label's text is the id's short name with
underscores replaced by spaces — `rsi pca`, `williams r`, `rsi_divergence` becomes `rsi divergence`
([`concept_shapes.js:89`](../../../../frontend/app/concept_shapes.js#L89)) — printed below the bar for
a bullish detection and above for a bearish one. On a pane concept that placement is arbitrary: the
event's `price_level` is a price, so the word lands wherever price happened to be, once per signal.

**Two rows are nearly empty by design, and that is the argument for the series kind.** 4 RSI
divergences and 7 RSI-PCA signals in five days are reasonable detection rates for a mark that rides
a trace; they are a poor way to represent an indicator that has a value on every one of the 501 bars.
The reference draws the continuous trace and the mark together; we draw the mark alone on the price
and the trace alone in a pane.

## The card is a second renderer, and it is the one the trader should believe

For all ten rows the card asks `/indicator-series` and paints the pane or histogram
([`conceptPreviewPayload`, app.js:14522-14527](../../../../frontend/app/app.js#L14522)). The chart
asks the same route and also routes the concept's events through `ConceptShapes.from`. So all ten
rows are `preview_agrees: differs`, for the same reason as trend: the card draws the oscillator, the
chart draws the oscillator and a word. There is no row in this family where the card and the chart
disagree about the oscillator itself.

## Parameters: ten concepts, none declared, none honoured

Every row carries `parameters: []` and `honoured: []`. The numbers are constants in the builders:
RSI 14 and 70/30, the family 2..24 and 70/30, RSI-PCA (2..24 periods, 6-bar lookahead, 2 components,
24-bar refit, 200-bar training, 0.99/0.01 quantiles), Stochastic (14, 3, 80/20), CCI (20, +/-100),
Williams (14, -20/-80), ROC (12), TSI (13, 25, 7), AO (5, 34). The reference exposes the period and
the levels on every one of these panes. The audit records the gap; it does not fix it.

## The 10 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `series_line` (9) | `rsi_pca`, `rsi`, `rsi_divergence`, `rsi_family`, `stochastic`, `cci`, `williams_r`, `roc`, `tsi` | **9 wiring** | the existing `SubPaneSpec::Series` arm, already painted; the divergence row rides its segments and markers on the line (the `series_marks` member settled in [10-trend.md](10-trend.md#what-this-pass-asks-of-the-vocabulary)) |
| `series_histogram` (1) | `awesome` | **1 wiring** | the existing `SeriesBars` arm |

`draw.verdict`: **10 `differs_major`**. `fires.verdict`: **10 `continuous`**. `preview_agrees`:
**10 `differs`**.

## Fixes this pass asks for

1. **Stop routing `indicators.*` events into `candle_mark`.** On a pane concept the label's y is a
   price and the indicator's value is not; there is no version of this mark that is in the right
   place. The series route already draws every one of these ten indicators.
2. **Keep the divergence and PCA marks in the pane.** `rsi_divergence` already has the segment and
   the dot; `rsi_pca` already has the signal dots. The event path duplicates them at the wrong
   scale.
3. **Do not build a momentum "event" for a continuous indicator.** The 50-label rows are the route's
   cap, not a signal count; the reference marks no zone entry, cross or zero crossing on any of these
   panes. If a mark is wanted, it belongs to the pane (a level crossing), not to the price frame.
4. **Expose the periods and levels the references expose** — RSI 14 and 70/30, Stochastic (14, 3) and
   80/20, CCI 20 and +/-100, Williams 14 and -20/-80, ROC 12, TSI (13, 25, 7), AO (5, 34). Today the
   family declares none of them.
5. **Carry the RSI-PCA reason into the card copy.** The card currently shows a house model beside
   externally-defined indicators with no distinction; the row's `no_convention` is the distinction
   the panel should state.

## The fires decisions (2026-10-31)

Ten rows, all `continuous` — the same shape of disagreement the trend family answers: the reference
draws a line, we draw a line, and the question is whether ours is the same line. It is, for eight of
the ten. Decided: **2 adopt, 8 keep_ours.** Both adopts are warm-up defects, and both are the same
class of defect the audit keeps finding where an indicator fills its own undefined prefix: not a wrong
formula, but a confident number printed at exactly the bars where no number exists yet.

| concept_id | fires verdict | decision | the one-line reason |
| --- | --- | --- | --- |
| `indicators.tsi` | continuous | adopt | the double EMA fill-forwards its own undefined prefix and the signal is an EMA of a **zero-filled** line, so the first ~40 signal bars are a decaying zero |
| `indicators.stochastic` | continuous | adopt | ours is the **fast** stochastic; the pane a TradingView user has in mind is the slow one, %K = SMA(raw, 3) |
| `indicators.rsi` | continuous | keep_ours | Wilder smoothing, seeded by the plain mean of the first `period` changes, first value at `period` ([rsi.rs:17-59](../../../../crates/analytics-core/src/indicators/rsi.rs#L17)) |
| `indicators.rsi_divergence` | continuous | keep_ours | confirmed at the pivot's `strength` bar — i.e. at the bar where it is knowable, not before |
| `indicators.rsi_family` | continuous | keep_ours | a house ribbon with a real floor; the reference marks nothing here either |
| `indicators.rsi_pca` | continuous | keep_ours | `no_convention` is the row's own verdict; the code refuses below 248 bars rather than guessing ([rsi_pca.rs:236](../../../../crates/analytics-core/src/rsi_pca.rs#L236)) |
| `indicators.cci` | continuous | keep_ours | Lambert's 0.015 with the mean absolute deviation over N — `/N` is right here, so the sample/population question does not arise ([oscillators.rs:43-61](../../../../crates/analytics-core/src/indicators/oscillators.rs#L43)) |
| `indicators.williams_r` | continuous | keep_ours | Williams' own %R over 14 ([oscillators.rs:65-86](../../../../crates/analytics-core/src/indicators/oscillators.rs#L65)) |
| `indicators.roc` | continuous | keep_ours | the percentage change over `period` bars ([oscillators.rs:89-100](../../../../crates/analytics-core/src/indicators/oscillators.rs#L89)) |
| `indicators.awesome` | continuous | keep_ours | Williams' AO: the 5/34 difference of **midpoint** SMAs ([misc.rs:46-56](../../../../crates/analytics-core/src/indicators/misc.rs#L46)) |

### What each decision means

**TSI** (`indicators.tsi`). `tsi`
([misc.rs:13-43](../../../../crates/analytics-core/src/indicators/misc.rs#L13)) fill-forwards the
undefined prefix of each EMA at `:22` and `:24`, and then takes the signal EMA of a **zero-filled**
line at `:35`. The result is that the first ~40 signal bars are a decaying zero — a definite,
plausible-looking value at exactly the place the `warmup` column is about. Adopted: remove the fills
and seed the signal from the line's own defined tail, which is what
[macd.rs:51-52](../../../../crates/analytics-core/src/indicators/macd.rs#L51) already does for the
same shape of problem. Two notes on the row itself: its `warmup.before` ("TSI(25) exists from bar 25")
is wrong — Blau's double smoothing makes the line defined from `fast + slow - 1` = bar **37**
([misc.rs:31](../../../../crates/analytics-core/src/indicators/misc.rs#L31)); and there is no `0.5`
factor in Blau's TSI or in our code, so the ratio question the row raises has nothing to decide.

**Stochastic** (`indicators.stochastic`). The pane is labelled `Stochastic (14, 3)` and the ledger
reads "%K(14,3)", but `oscillators.rs:10-40` applies **no smoothing** to %K: %D is an SMA(3) of the raw
%K. So ours is Lane's *fast* stochastic — which the concept card names correctly — while the reference
pane a TradingView user pictures is the *slow* one, %K = SMA(raw, 3) with %D = SMA(%K, 3). The audit's
own tie-break settles it (design spec §Citation standard: pick what a TradingView user expects, for
recognition): adopted, the 3-smoothed %K, with %D kept as the SMA(3) **of that smoothed %K**. The
card's "Lane's" wording is corrected with it, because Lane's original is the fast form we are moving
away from. This is also why the row fires at the route's cap of 50 while the reference marks nothing:
raw %K crosses 80/20 constantly; smoothed %K does not.

**The eight we already match.** RSI is Wilder's with the standard seeding, and its first value lands at
index `period` — the row's warm-up claim is answered by the code, not argued; `atr` follows the same
discipline. CCI keeps Lambert's 0.015 and the mean absolute deviation over the window; the
sample-versus-population divisor debate that recurs in this audit does not arise for an MAD. Williams
%R, ROC and AO are single-formula indicators with no variants, and each of ours is that formula.
`rsi_divergence` confirms at the pivot's `strength` bar, which is when a divergence is knowable, not
when it is visible in hindsight — the honest end of the "repaint" argument. `rsi_family` is a house
ribbon with a real floor, and `rsi_pca` is a house model whose row verdict is `no_convention`: there is
no external rule to adopt, and the code's refusal to compute below 248 bars
([rsi_pca.rs:236](../../../../crates/analytics-core/src/rsi_pca.rs#L236)) is the correct behaviour for
a fitted estimator.

This section authorises, now: TSI's undefined prefix left undefined with the signal seeded off the
defined tail, and the slow (3-smoothed) %K with %D taken from it. It keeps the other eight estimators
unchanged, records the TSI warm-up floor as bar 37, and leaves `rsi_pca` as a house model whose card
copy must say so. It authorises nothing about drawing — the `candle_mark` routing, the pane marks and
the parameter exposure are the shape work above.
