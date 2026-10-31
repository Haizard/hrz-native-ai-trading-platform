# Pass 10a — Trend (12 rows)

Judged 2026-10-31 against `main`, on the window the gateway serves today (501 bars,
2026-10-26T06:30 to 2026-10-31T11:30 UTC). The machine-readable verdicts are the twelve rows of
[`ledger/trend.json`](../../../../reports/concept-visual-audit/ledger/trend.json);
`node tools/concept_visual_audit.mjs check --family trend` reports **12 rows, 0 incomplete**.

**This family is not missing a renderer. It has two, and one of them is a word.** Every one of the
twelve concepts has a complete `/indicator-series` specification — lines, reference levels, bars,
markers, fills — built by `indicator_series()` in
[`indicator_series.rs:133`](../../../../crates/analytics-core/src/indicator_series.rs#L133) and
forwarded by the chart into `request.sub_panes`, `request.overlay_series` and
`request.overlay_fills` ([`app.js:3555`](../../../../frontend/app/app.js#L3555), fed by
[`seriesPaneSpecs`, app.js:6073](../../../../frontend/app/app.js#L6073)). At the same time the
concept path turns the **same concept's scan events** into `candle_mark` — the concept's short name,
lower-case, attached to a candle on the price frame
([`concept_shapes.js:80-90`](../../../../frontend/app/concept_shapes.js#L80)). So an enabled ADX
draws ADX correctly below the chart *and* prints `adx` 29 times along the price axis. The pass
photographs the second drawing; the first is what the reference draws.

**287 labels, no series mark.** The twelve rows produced 287 events on the window, and every one
became a Label — text on the price frame, never a line at its own scale. The counts split into
three regimes: the fast channels and oscillators fire constantly (`macd` 50, `bollinger` is the
volatility pass, `psar` 38, `chandelier` 39, `macd_family` 37), the slow crossers are nearly silent
(`ema` 3, `sma` 1), and one concept cannot fire at all (`donchian` 0). The series route draws all
twelve regardless, which is the point: a trend indicator is a line on every bar, not an event on
some.

**Donchian is the only `matches` in the pass, and it is a rule defect, not a drawing one.**
`donchian()` builds the 20-bar channel from `candles[i + 1 - period..=i]`
([`trend.rs:79-95`](../../../../crates/analytics-core/src/indicators/trend.rs#L79)), so the current
bar is inside its own channel and `close > upper` / `close < lower` are unsatisfiable. The detector
emits nothing, the concept path draws nothing, and the three-line overlay the series route draws is
exactly the reference's banded indicator. The picture is right; the concept's signal does not exist.

## How it was measured

`node tools/concept_visual_audit.mjs render trend` on 2026-10-31 rebuilt all twelve pages over the
501-bar window above. The scan route answers roughly 4,100 events across 138 concepts on this
window (4,096 across 137 on a re-read while this section was written, as the window rolls); this
family supplied **287 events, 287 draws** and **287 labels**. Every number below comes from parsing
the pages' embedded scenes and from the scan's own payloads.

| concept | registry | events | label text | label y span (484px plot) | what the series route draws | `draw` |
| --- | --- | --- | --- | --- | --- | --- |
| `indicators.adx` | pane | 29 | `adx` | 67.9-429.6 | ADX, +DI, -DI, fixed 0-100, level 25 | `differs_major` |
| `indicators.keltner` | overlay | 16 | `keltner` | 67.9-398.6 | three lines (EMA20 +/- 2 ATR10), no fill | `differs_major` |
| `indicators.donchian` | overlay | 0 | — | — | three lines, 20-bar channel | **`matches`** |
| `indicators.supertrend` | overlay | 13 | `supertrend` | 67.9-416.3 | one line, one colour, side dropped | `differs_major` |
| `indicators.psar` | overlay | 38 | `psar` | 58-454.6 | one connected line, no dots | `differs_major` |
| `indicators.ichimoku` | overlay | 50 | `ichimoku` | 63-438.1 | tenkan/kijun/chikou + kumo fill; no senkou strokes | `differs_major` |
| `indicators.chandelier` | overlay | 39 | `chandelier` | 55.3-431.3 | both stops at once | `differs_major` |
| `indicators.macd_family` | pane | 37 | `macd family` | 89.8-409.5 | 23 MACD lines, zero line | `differs_major` |
| `indicators.ema_family` | overlay | 11 | `ema family` | 94.4-410.7 | 12 GMMA lines | `differs_major` |
| `indicators.macd` | pane | 50 | `macd` | 55.3-447.3 | MACD + signal + histogram, zero line | `differs_major` |
| `indicators.ema` | overlay | 3 | `ema` | 63-234.5 | EMA 20/50/200 | `differs_major` |
| `indicators.sma` | overlay | 1 | `sma` | 179.1 | SMA 20/50/200 | `differs_major` |

**The label's y is the price at the signal bar, not the indicator's value.** For a pane concept this
is the whole category error in one number: the label sits between 67.9 and 429.6 in the price plot,
while the value it claims to report is bounded 0-100 (ADX, RSI) or lives around zero (MACD family).
The engine maps the event's `price_level`/`zone_low`/`zone_high` through the price frame
([`concept_shapes.js:84-90`](../../../../frontend/app/concept_shapes.js#L84)); an indicator value
never enters that mapping.

## What the reference set says

- **ADX is a pane, and the pane is the drawing.** TradingView: *"The Average Directional Index (ADX)
  is a line that plots in its own pane below the price chart."* The three traces are read against
  the 25 level; no signal is printed at price.
- **Keltner is a band over the candles.** TradingView: *"The Keltner Channels (KC) indicator is a
  banded indicator similar to Bollinger Bands and Moving Average Envelopes."* A band is three lines
  with a body, not a word at a bar.
- **Donchian is a banded indicator with three lines.** TradingView: *"It is a banded indicator
  consisting of an upper band, lower band and middle line."* The channel is the whole drawing.
- **Supertrend is one line whose colour carries the side.** TradingView: *"The calculation of its
  single line combines trend detection and volatility."* The line sits below price while long and
  above while short; the flip is visible because the line changes sides, not because a label
  appears.
- **Parabolic SAR is dots.** TradingView: *"The indicator is plotted as dots above or below the
  price."* A connected line is a different object: it draws a segment across every side change that
  the reference never draws.
- **Ichimoku is a package, and the cloud is a fill.** TradingView: *"The Ichimoku Cloud is a package
  of multiple technical indicators that signal support, resistance, market trend, and market
  momentum."* The kumo is the space between senkou A and senkou B; both spans have edges.
- **The Chandelier Exit is one active stop.** TradingView: *"The Chandelier Exit, created by Charles
  Le Beau and popularized by Alexander Elder, is a volatility-based indicator that identifies
  stop-loss exit points based on local price extremes and Average True Range (ATR)."* An exit is the
  stop on the side you are on, not both sides at once.
- **MACD is two lines and a histogram in their own pane.** TradingView: *"The indicator plots the
  MACD and signal values as lines, and the histogram values as color-coded columns."*
- **GMMA is two groups of averages.** TradingView's GMMA script: *"The GMMA plots two groups of
  Exponential Moving Averages: short-term and long-term."*
- **Moving averages are continuous lines.** TradingView, on the EMA: *"Exponential Moving Average is
  very similar to (and is a type of) WMA. The major difference with the EMA is that old data points
  never leave the average."* On the SMA: *"The Simple Moving Average (SMA) is calculated by adding
  up the prices of a security over a set period and then dividing that total by the number of
  periods."* Neither page describes a cross marker; the averages cross, and the crossing is read off
  the lines.

## What we draw today

**The concept path stamps the indicator's name where the price is.** The measured pattern is the same
on all twelve rows: N Labels whose text is the concept's short name (underscores become spaces,
[`concept_shapes.js:89`](../../../../frontend/app/concept_shapes.js#L89)), coloured by direction,
placed at the signal bar's price, below the bar for a bullish event and above for a bearish one.
There is no value, no level and no line in any of them.

Three consequences follow from the measurement rather than from taste:

- **A pane concept prints its word on the price axis.** `adx` (fixed 0-100), `macd` (around zero),
  `macd_family` (23 lines around zero) all label at prices between 55 and 449 — the words are
  literally at the wrong scale, not merely in the wrong style.
- **An overlay concept prints its word on top of its own line.** `keltner`, `supertrend`,
  `ichimoku`, `chandelier`, `ema_family`, `ema`, `sma` already draw the correct overlay; the label
  is a second mark for the same detection, and for `sma` it is the only event in 501 bars.
- **The label count is a detection count, not a signal count.** 50 for `macd` is the route's
  newest-50 cap ([`concept_routes.rs:240`](../../../../crates/api-gateway/src/concept_routes.rs#L240)),
  not 50 MACD signals; the same is true for `stochastic`, `cmf`, `roc`, `williams_r`, `cci`,
  `bollinger` and `anchored_vwap` in the next four sections.

**The series route's own gaps, read against the same references:**

- **Supertrend loses the side.** `supertrend_overlay` keeps the value and drops the boolean
  ([`indicator_series.rs:518-539`](../../../../crates/analytics-core/src/indicator_series.rs#L518)),
  so the single line is cyan for both trends. The reference's colour change is the reading.
- **PSAR is a line, not dots.** `psar_overlay` returns one pink polyline
  ([`indicator_series.rs:541-559`](../../../../crates/analytics-core/src/indicator_series.rs#L541)).
  Nothing in the overlay vocabulary draws a dot per bar; this is `series_dots`, a new painter.
  It is why the row is the trend family's only `new_rendering`, and one of two in the whole pass
  beside the pane band that Pass 11 will need.
- **Ichimoku's cloud has no edges.** The kumo fill is present (`fills`, painted through
  `overlay_fills`), but senkou A and senkou B are not in `lines`
  ([`indicator_series.rs:561-595`](../../../../crates/analytics-core/src/indicator_series.rs#L561)).
  The reference strokes both spans and fills between them; we fill between two invisible curves.
- **Chandelier draws both stops.** `chandelier_overlay` always emits `long_stop` and `short_stop`
  ([`indicator_series.rs:736-756`](../../../../crates/analytics-core/src/indicator_series.rs#L736)).
  The canonical chart shows one active stop; two stops bracket every bar in a band the indicator
  does not claim.
- **The bands are unfilled.** Keltner, Donchian and Bollinger all return `fills: Vec::new()`. The
  reference calls Keltner and Bollinger banded indicators; the space between the bands is part of
  the object. The overlay fill arm exists and already paints the Ichimoku cloud, so this is wiring,
  not a new renderer — but no row in this family asks for it as its primary shape.

## The card is a second renderer, and on this family the card is the honest one

For a pane or overlay concept the card asks `/indicator-series` and paints the returned spec
([`conceptPreviewPayload`, app.js:14522-14527](../../../../frontend/app/app.js#L14522);
[`previewSeries`, app.js:14833](../../../../frontend/app/app.js#L14833)). The chart asks the same
route *and* routes the concept's events through `ConceptShapes.from`. So on eleven of the twelve
rows the two renderers **differ** for one reason: the card draws the indicator, the chart draws the
indicator and a word. The card is not a different opinion about the drawing; it is the drawing
without the defect.

`donchian` is the exception: the card paints the three overlay lines, the chart paints the three
overlay lines and nothing else (0 events), so `preview_agrees: agrees` — and both are right. It is
the only row in the whole pass where the two renderers agree *and* the reference is satisfied.

## Parameters: twelve concepts, none declared, none honoured

Every row in this family carries `parameters: []` and `honoured: []`. The numbers that move the
drawings are Rust constants inside the builders: ADX 14, Keltner (20, 10, 2), Donchian 20,
Supertrend (10, 3), PSAR (0.02, 0.2), Ichimoku (9, 26, 52), Chandelier (22, 3), MACD (12, 26, 9),
GMMA (3-15, 30-60), EMA/SMA (20, 50, 200). The reference exposes every one of these as a setting.
The audit records the gap; it does not fix it.

## The 12 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `series_line` (10) | `adx`, `keltner`, `donchian`, `supertrend`, `chandelier`, `macd_family`, `ema_family`, `macd`, `ema`, `sma` | **10 wiring** | the existing `SubPaneSpec::Series` / `OverlaySeriesSpec` arms, already painted by `series_pane` ([scene.rs:2385](../../../../frontend/chart-engine/src/scene.rs#L2385)). The work is to stop painting the event label, not to build a line |
| `series_band` (1) | `ichimoku` | **1 wiring** | one `OverlayFillSpec` per cloud, which the engine already lowers to `ScriptDraw::Band` ([scene.rs:2211](../../../../frontend/chart-engine/src/scene.rs#L2211)); add senkou A/B to `lines` so the band has edges |
| `series_dots` (1) | `psar` | **1 new_rendering** | a per-bar dot trace on the price frame. No arm exists: `SeriesMarker` is a point mark inside a pane, and the overlay vocabulary is lines and fills only |

`draw.verdict`: **11 `differs_major`, 1 `matches`** (`donchian`). `fires.verdict`: **12
`continuous`** — every trend indicator has a value on every bar, and the reference marks no signal
at all. `preview_agrees`: **11 `differs`, 1 `agrees`**.

## What this pass asks of the vocabulary

The series vocabulary is settled once for the whole pass here, and the other four family sections
use it.

| shape | meaning | rows in Pass 10 | status | existing arm |
| --- | --- | --- | --- | --- |
| `series_line` | a connected per-bar trace on its own pane scale or the price scale, with optional reference levels, segments and point marks | 28 | **wiring** | `SubPaneSpec::Series` ([scene.rs:400-440](../../../../frontend/chart-engine/src/scene.rs#L400)) and `OverlaySeriesSpec` ([scene.rs:370-400](../../../../frontend/chart-engine/src/scene.rs#L370)); painter `series_pane` ([scene.rs:2385](../../../../frontend/chart-engine/src/scene.rs#L2385)) |
| `series_histogram` | zero-anchored bars on a pane scale | 2 | **wiring** | `SeriesBars` inside `SubPaneSpec::Series` ([scene.rs:418-421](../../../../frontend/chart-engine/src/scene.rs#L418)) |
| `series_band` | a fill between two traces | 1 | **overlay wiring; pane `new_rendering`** | `OverlayFillSpec` -> `ScriptDraw::Band` ([scene.rs:2211](../../../../frontend/chart-engine/src/scene.rs#L2211)); `SubPaneSpec::Series` has no fills field, and [app.js:6103](../../../../frontend/app/app.js#L6103) drops `spec.fills` for pane targets |
| `series_dots` | disconnected per-bar points | 1 | **new_rendering** | none |
| `series_marks` | a segment or point mark riding an existing trace | 0 as a primary shape | **folded into `series_line`** | `SeriesSegment` / `SeriesMarker` inside `SubPaneSpec::Series` ([scene.rs:431-438](../../../../frontend/chart-engine/src/scene.rs#L431)) |

**`series_marks` is not a fifth shape.** Three rows in this pass draw marks on a trace —
`rsi_divergence` and `rsi_pca` in momentum, and OBV/A-D divergences in volume — and in every one the
mark's y position is only meaningful relative to the line underneath it. A `series_marks` shape with
no trace would have to carry the trace anyway, so the mark is a member of `series_line`, exactly as
`levels` already is. Pass 10 settles that; Task 18 can record it without inventing a shape whose
first user would immediately need the line.

**`series_band` splits by host.** On an overlay the band is wiring today: the Ichimoku kumo is a
real `OverlayFillSpec`, the branch at
[app.js:6094-6100](../../../../frontend/app/app.js#L6094) maps it, and the engine paints it. On a
pane it is not: `SubPaneSpec::Series` has no `fills` field, and the pane branch at
[app.js:6101-6106](../../../../frontend/app/app.js#L6101) omits fills on purpose because there is
nothing to send them in. Pass 11's `indicators.bayesian_drift` needs a pane band, so the status is
recorded per host rather than per shape.

**The scale rule is the registry's, not the pass's.** `pane: true` means `own_pane`; `overlay: true`
with `pane: false` means `price`. Every row here follows it. The one event concept in the pass with
neither flag is `order_flow.delta_shift`, and its row records the registry as the defect. A z-score
of 2.7 belongs to neither the price axis nor a word at price; it belongs to a pane, and Pass 11
judges it there.

## Fixes this pass asks for

1. **Stop routing `indicators.*` events into `candle_mark`.** The series route is the drawing; the
   event label is a second draw at the wrong scale. The smallest correct change is at the router:
   an id in the `indicators.` namespace should not take the candle-mark branch
   ([`concept_shapes.js:80-90`](../../../../frontend/app/concept_shapes.js#L80)) while a series spec
   exists for it.
2. **Fix the Donchian detector or retire its event.** Build the channel from the prior 20 bars
   (`..i`, not `..=i`) and compare the current close, or mark the concept as a pure channel with no
   breakout event. Today it can never fire, and its `matches` verdict is the proof that the drawing
   was never the problem.
3. **Draw PSAR as dots** — one point per bar, side-coloured. `series_dots`, new painter.
4. **Colour Supertrend by side.** Two colours on one line, or two line records; the boolean is
   already in the builder's input and is being discarded.
5. **Stroke senkou A and senkou B.** The cloud already has a fill; it needs edges.
6. **Draw one Chandelier stop, the active one.** Both stops at once is a band the reference does not
   have.
7. **Fill the Keltner and Donchian bands** through the existing overlay fill arm (Bollinger, the
   other unfilled band, belongs to the volatility section).
8. **Give the pane a fill field** (`SubPaneSpec::Series`) before any pane band is wired; today a
   pane band cannot reach the engine even if the gateway sends one.
