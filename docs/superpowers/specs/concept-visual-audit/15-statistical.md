# Pass 11 — Statistical (10 rows)

Judged 2026-10-31 against `main`, on the window the gateway serves today (501 bars,
2026-10-26T06:30 to 2026-10-31T11:30 UTC). The machine-readable verdicts are the ten rows of
[`ledger/statistical.json`](../../../../reports/concept-visual-audit/ledger/statistical.json);
`node tools/concept_visual_audit.mjs check --family statistical` reports **10 rows, 0 incomplete**.

**This is the family where the drawings are all correct and three of them are missing.** Seven
concepts have a complete series spec in `indicator_series.rs` and draw it in a pane or on the
price frame; the concept path then stamps a word on the price axis for each event — 205 words on
this window. Three concepts draw nothing at all: `changepoint` and `bayesian_drift` produced 0
events (their events are rare by construction and did not occur on this window), and `pairs`
cannot draw without a second instrument. But the family's two real defects are not the labels:

- **`hv` puts a volatility and its percentile rank on one scale.** HV is a return deviation of
  order 0.003; rank is 0..1. Both share `SeriesScale::Auto`
  ([`indicator_series.rs:763-776`](../../../../crates/analytics-core/src/indicator_series.rs#L763)),
  so the rank line sets the pane and HV flattens onto the axis. This is the `%B`/bandwidth defect
  from Pass 10b, in the family where the measurement is the headline.
- **`bayesian_drift` loses its band.** The builder emits a `SeriesFill` for the 95% credible
  interval ([`indicator_series.rs:874-879`](../../../../crates/analytics-core/src/indicator_series.rs#L874));
  `SubPaneSpec::Series` has no `fills` field, and the chart's pane mapping drops it
  ([app.js:6101-6119](../../../../frontend/app/app.js#L6101)). The band is the concept — *"the trend
  AND its uncertainty, which is the whole Bayesian point"* — and it reaches the card
  ([`previewSeries` paints `spec.fills`, app.js:14836](../../../../frontend/app/app.js#L14836)) but
  not the chart. It is the pane-band gap Pass 10a predicted, with a concrete victim.

## How it was measured

`node tools/concept_visual_audit.mjs render statistical` on 2026-10-31 rebuilt all ten pages over
the 501-bar window above; the scan route answered 4,096 events across 137 concepts. This family
supplied **205 events, 205 draws and 205 labels** — every event lowered to a Label on the price
frame; 7 pages drew, 3 were silent. Every number below comes from parsing the pages' embedded
scenes and from the builders' and detectors' source.

| concept | registry | events | label text | label y span (484px plot) | what the series route draws | `draw` |
| --- | --- | --- | --- | --- | --- | --- |
| `indicators.hv` | pane | 12 | `hv` | 32.4-397.6 | HV **+ rank on one auto scale** | `differs_major` |
| `indicators.zscore` | pane | 28 | `zscore` | 55.3-477.3 | z line, +2/-2/0 levels | `differs_major` |
| `indicators.kalman` | overlay | 50 | `kalman` | 55.3-453.7 | one smooth line on price | `differs_major` |
| `indicators.changepoint` | pane | 0 | — | — | CUSUM line, +/-2.5 gates | **`matches`** |
| `indicators.entropy` | pane | 6 | `entropy` | 86.3-364.9 | line, fixed 0..1, 0.85/0.35 | `differs_major` |
| `indicators.bayesian_drift` | pane | 0 | — | — | drift line, **band dropped** | `differs_major` |
| `indicators.ar_forecast` | pane | 50 | `ar forecast` | 33-451.1 | line, zero level | `differs_major` |
| `indicators.garch` | pane | 9 | `garch` | 45.7-336.3 | sigma line, auto | `differs_major` |
| `indicators.hmm` | pane | 50 | `hmm` | 251.8-419.6 | posterior line, fixed 0..1, 0.5 | `differs_major` |
| `indicators.pairs` | pane | 0 | — | — | pane only when a pair is named | `differs_major` |

**Three rows answer exactly 50** — `kalman`, `ar_forecast`, `hmm` — the route's newest-50 cap, not
a detection count. The slow estimators that did not reach the cap are honest counts: `hv` 12,
`zscore` 28, `garch` 9, `entropy` 6. The window's price range (roughly 78k-82k) is irrelevant to
every label's y, which is the point: the y is the signal bar's price, and none of these quantities
is a price except Kalman's.

## What the reference set says

- **HV is a return-dispersion study.** TradingView: *"Historical volatility is a statistical
  measure used to analyze the general dispersion of security or market index returns for a
  specified period of time."* One line in a pane; the percentile rank is a separate study, not a
  second trace on the same scale.
- **A z-score is a count of sigmas.** Wikipedia: *"In statistics, the standard score or z-score is
  the number of standard deviations by which the value of a raw score (i.e., an observed value or
  data point) is above or below the mean value of what is being observed or measured."* The ±2
  bands are the convention; the line crossing them is the reading.
- **CUSUM is a control chart.** NIST: *"CUSUM charts, while not as intuitive and simple to operate
  as Shewhart charts, have been shown to be more efficient in detecting small shifts in the mean of
  a process."* A statistic line with a decision interval in a pane.
- **A pairs study is a spread z-score.** TradingView's Statistical Pairs Trading script: *"It
  calculates the Z-Score of the log-price spread between any two symbols you choose, providing both
  long-term and short-term Z-Score signals."* A pane, ±2 levels, entry markers.
- **The other six have no chart convention.** Kalman, entropy, Bayesian drift, AR(2), GARCH and
  the HMM are estimators with canonical *formulas* but no canonical *drawing*: no platform ships a
  reference geometry for them, and the ones that draw them disagree on scale and decoration. Those
  rows are `house_style` under the plan's bar — legible, smooth, low visual noise, quiet by
  default, explainable in one line on the card.

## What we draw today

**The label pattern is the pass-wide defect, at its most absurd here.** The y span of the entropy
labels (86.3-364.9) is a price range for a quantity bounded 0..1. The HMM labels sit between
251.8 and 419.6 — all 50 in the lower third of the price plot — because the volatile regime fired
on the newest 50 bars and those bars happened to be cheap. The label is a detection count, not a
measurement.

**The two scale defects are the family's own, and they are different in kind.**

- `hv`: two traces in one pane, one measuring returns and one measuring a percentile. The rank
  line is the one you can see; HV is the flat line at the bottom. The fix is not a new renderer —
  two panes, or a fixed 0..1 scale for the rank with HV kept on its own.
- `bayesian_drift`: one trace and one band in one pane, and the band is dropped by the wire. The
  chart draws the drift line alone, which is exactly the object the registry's description warns
  against: *"the trend AND its uncertainty, which is the whole Bayesian point"*. The card draws
  the band; the chart does not. This is the only row in the family where the card is more right
  than the chart.

**`hmm` is drawn as a per-bar probability when a regime is an interval.** The posterior is a
reading — 0..1, one value per bar — but the thing the concept names is a *state that holds across
bars*. The plan calls it: a regime is an `interval`, and Pass 4's `time_box` is the shape. Today
the same 50 events print a word at price; the pane draws a wiggly 0..1 line that crosses 0.5 forty
times on the window. The reference has no HMM drawing, but the house bar (quiet by default, low
visual noise) points the same way: shade the volatile intervals, name the state once.

**`pairs` is unreachable by default.** The registry declares no event kinds; the scan emits
nothing; the only renderer is `/indicator-series` with `symbol2`, served by
[`concept_routes.rs:346-390`](../../../../crates/api-gateway/src/concept_routes.rs#L346) and built
by `pair_series`. When a second instrument is typed, the pane is the reference's drawing (±2
levels, entry markers, the gate in the line's name). On the audit chart, with no pair typed, the
concept draws nothing. The geometry is right; the reachability is not.

**`changepoint` is the one row where the drawing is already the reference's.** The pane plots the
signed CUSUM statistic with ±2.5 gates and a zero line; 0 events fired, so no label appears. The
control chart is there. What is ours is the statistic (a normalized maximum excursion, not the
classic cumulative sum) and the fixed 2.5 gate (the reference's decision interval is an ARL
setting).

## The card is a second renderer, and on `bayesian_drift` it is the better one

For pane and overlay concepts the card asks `/indicator-series` and paints the returned spec
([`conceptPreviewPayload`, app.js:14522-14527](../../../../frontend/app/app.js#L14522)). Eight
rows are `preview_agrees: differs` because the card draws the series and the chart draws the
series **and** the word. `changepoint` is `agrees` — both draw the CUSUM pane and nothing else,
which is the same precedent as `donchian` in Pass 10a. `pairs` is `not_rendered` — neither side
draws without a second instrument.

The interesting one is `bayesian_drift`: the card's `previewSeries` paints `spec.fills`
([app.js:14836-14855](../../../../frontend/app/app.js#L14836)) while the chart's pane mapping
never forwards them. The card draws the posterior mean **and** its 95% band; the chart draws the
mean alone. This is a preview disagreement that is not the usual "card omits the defect" — the
card is the only renderer showing the concept as specified.

## Parameters: ten concepts, none declared, none honoured

Every row carries `parameters: []` and `honoured: []`. The constants that move the drawings are
all in Rust: HV (20, 126), z-score 20 and ±2, Kalman (0.01, 0.5), CUSUM (60, 2.5), entropy (60, 6
bins, 0.85/0.35), Bayesian prior 0.01, AR(2) 120, GARCH (60, 0.1, 0.85) and its 120-rank, HMM
(120, 0.5), pairs (60, ±2, 80 shared bars). The reference exposes the periods and thresholds as
settings; the pair's second instrument is the one parameter this family *does* expose — a text
field on the card ([app.js:14203-14220](../../../../frontend/app/app.js#L14203)) — and it is not
in the registry at all.

## The 10 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `series_line` (8) | `hv`, `zscore`, `kalman`, `changepoint`, `entropy`, `ar_forecast`, `garch`, `pairs` | **8 wiring** | the existing `SubPaneSpec::Series` / `OverlaySeriesSpec` arms. `hv` additionally needs its two traces split across scales; `pairs` needs the second instrument to be reachable (it already is, via the card's field) |
| `series_band` (1) | `bayesian_drift` | **1 new_rendering** | a `fills` field on `SubPaneSpec::Series` and a painter for it; the overlay arm already has one (`OverlayFillSpec` → `ScriptDraw::Band`) |
| `time_box` (1) | `hmm` | **1 wiring** | Pass 4's full-height shade (`scene_overlay_tints` → `ScriptDraw::Box`, [script.rs:785](../../../../frontend/chart-engine/src/script.rs#L785)); the work is to emit regime *intervals* from the posterior instead of per-bar events |

`draw.verdict`: **9 `differs_major`, 1 `matches`** (`changepoint`). `fires.verdict`: **8
`continuous`, 1 `different_rule`** (`changepoint`), **1 `no_external_rule`** (`hmm`).
`preview_agrees`: **8 `differs`, 1 `agrees`** (`changepoint`), **1 `not_rendered`** (`pairs`).

## What this pass asks of the vocabulary

**`series_band` needs a pane arm, and this is its first concrete user.** Pass 10a recorded the
status per host: overlay wiring, pane `new_rendering`. `bayesian_drift` is the row that proves the
gap: the builder already produces the fill, the card paints it, and the chart cannot. Task 18
should carry that as a pane-band item, not as a new shape.

**A regime is an interval, and the registry's `pane` flag is about its companion line.**
`indicators.hmm` declares `pane: true` because the engine computes a posterior series, and that is
a legitimate pane drawing. But the concept *as a regime* is a state over time, and the plan's rule
is explicit: a regime is an `interval`, Pass 4's shape reused, scale `time_only`. The row records
both facts: the registry's pane for the posterior, the target `time_box` for the regime. This is
not a registry defect — it is two drawings of one concept, and only one of them is the concept.

**The scale rule holds for the series rows and exposes two builders.** `pane: true` → `own_pane`,
`overlay: true` → `price`; every row follows it. The failures are inside the panes: `hv` mixing
units and `bayesian_drift` losing a fill. Task 18's "implementable without reading a pass" test
should include the constraint Pass 10b named — a fixed-scale trace must not share a pane with an
auto-scale one — because `hv` is the second instance of that mistake in two families.

## Fixes this pass asks for

1. **Split HV and its rank across scales.** Two panes, or a fixed 0..1 scale for the rank with HV
   on its own scale. The reference calls them two studies; the pane currently shows one of them.
2. **Give the pane a fill field and paint the Bayesian band.** `SubPaneSpec::Series` gains
   `fills`; the chart's pane mapping forwards them; the engine paints them. The card already knows
   how.
3. **Draw the HMM as regime intervals, not per-bar labels.** Emit the volatile state's intervals
   and shade them full-height, name the state once; keep the posterior pane as the companion.
4. **Make `pairs` reachable before it is enabled.** The card's pair field exists; the registry
   does not name a second instrument and the concept cannot draw without one. At minimum the
   catalog should show the concept as needing a pair, the way it shows data-channel needs today.
5. **Normalise the `changepoint` stat or record it as a variant.** The pane is the control chart;
   the statistic is a normalized maximum excursion rather than the classic cumulative sum, and the
   2.5 gate is fixed where the reference's decision interval is an ARL setting. The drawing is
   right; the rule should be named as ours.
6. **Stop routing `indicators.*` events into `candle_mark`** — the pass-wide fix stated once in
   [`10-trend.md`](10-trend.md#fixes-this-pass-asks-for). This family's 205 words come off.

## The fires decisions (2026-10-31)

Ten rows: 8 `continuous`, 1 `different_rule` (`changepoint`), 1 `no_external_rule` (`hmm`). This is the
family where `adopt` and `keep_ours` needed separating first, because most of its estimators have **no
external convention to adopt** — a Kalman local level, a Bayesian drift, an AR(2) fit and a fitted HMM
are not things TradingView defines. Decided: **2 adopt, 6 keep_ours, 1 defer, 1 nothing-to-adopt.** The
two adopts are the only two rows with a citable convention behind them.

| concept_id | fires verdict | decision | the one-line reason |
| --- | --- | --- | --- |
| `indicators.hv` | continuous | adopt | the 126-bar percentile decile event retires — the reference plots HV and "the percentile rank is a separate study"; the guard `n > 150` is honest and stays |
| `indicators.zscore` | continuous | adopt | the ±2 crossing is read off the bands, not marked; the number a trader wants is the percentile the label should carry, not a word at a price |
| `indicators.kalman` | continuous | keep_ours | no external rule; the cross is our reading, and the fixed (q=0.01, r=0.5) is a house tuning nobody can change ([indicator_signals.rs:424](../../../../crates/analytics-core/src/indicator_signals.rs#L424)) |
| `indicators.entropy` | continuous | keep_ours | no external rule; the line is normalised by ln(bins) so 0.85/0.35 are on a [0,1] scale, which is ours to defend and not the reference's |
| `indicators.bayesian_drift` | continuous | keep_ours | no external rule; "the 95% band first excludes zero" is the honest statement of a normal-normal posterior we chose |
| `indicators.ar_forecast` | continuous | keep_ours | no external rule; AR(2) by OLS over 120 bars is a house specification, and the zero-cross is its reading |
| `indicators.garch` | continuous | keep_ours | the recursion is Bollerslev's, but α=0.1, β=0.85 are **assumed**, not estimated ([indicator_signals.rs:491](../../../../crates/analytics-core/src/indicator_signals.rs#L491)) — so the top-decile event is ours and the card must stop implying a fit |
| `indicators.pairs` | continuous | keep_ours | the reference marks its own ±2 spread crossings and ours use the same convention — this is the family's one row where the event is the reading |
| `indicators.changepoint` | different_rule | defer | our gate is 2.5 on a σ√n-normalised maximum excursion; the classic CUSUM's h is chosen for a target average run length, so the two are different statistics — the count decides |
| `indicators.hmm` | no_external_rule | nothing to adopt | the state is latent, the 0.5 posterior split is ours, and there is no external rule to agree or disagree with |

### What each decision means

**The two adopts** (`hv`, `zscore`). Both rows' `their_rule` is the same sentence as the volatility and
volume families: the reference plots the line and marks nothing. HV's event fires when the 126-bar
percentile rank crosses into the top or bottom decile
([indicator_signals.rs:101-110](../../../../crates/analytics-core/src/indicator_signals.rs#L101)), which
is a *separate study* on the reference side and a price-frame word on ours; the `n > 150` floor — 20 for
the deviation, 126 for the rank, plus the guard — is one of the most honest warm-ups in the audit and
stays. The z-score's ±2 cross
([indicator_signals.rs:409-419](../../../../crates/analytics-core/src/indicator_signals.rs#L409)) is
read off its own bands. Adopted for both: the condition survives as a state of the series, and the label
becomes the **percentile** — "+2.7σ / 99.1th percentile", the platform's stated convention for expressing
a z-score — which is information the number already has and the current mark throws away.

**The six keep_ours.** Each of these is a model, not an indicator, and the audit's citation standard
does not let us invent an external convention to conform to. The right answer for a house model is to
keep it, label it, and make its tuning reachable — which is exactly what the roadmap's
Evidence/Inference/Confidence triad asks of it. Concretely: the Kalman's (0.01, 0.5), the Bayesian
prior's 0.01, the entropy's 6 bins and its 0.85/0.35 gates, the AR fit's 120 bars and the GARCH's (α, β)
are **parameters that do not exist as parameters** — they are literals at the call sites
([indicator_signals.rs:424, 461, 448, 478, 491](../../../../crates/analytics-core/src/indicator_signals.rs#L424)).
The one real correction inside these rows is GARCH: the recursion at
[indicator_signals.rs:491](../../../../crates/analytics-core/src/indicator_signals.rs#L491) is called as
`garch(&closes2, 60, 0.1, 0.85)` — ω seeded from the 60-bar variance, α and β assumed. That is a
legitimate variance filter and a poor GARCH *estimate*; the concept is sold as the latter. Adopted
wording, no code change beyond the card: it is a fixed-parameter variance recursion until a fit lands,
and α+β = 0.95 (stationary) should be stated where a trader can read it. `pairs` is the exception that
proves the rule: its ±2 markers match the reference's own convention, so the row keeps the event.

**The defer** (`indicators.changepoint` — the family's only `different_rule`). `cusum_inner` computes
the running cumulative deviation of the window's returns from their own mean, takes the **largest
absolute excursion**, and normalises it by σ·√n
([quant.rs:74-110](../../../../crates/analytics-core/src/indicators/quant.rs#L74)); the detector then
gates |stat| at 2.5 ([indicator_signals.rs:436-441](../../../../crates/analytics-core/src/indicator_signals.rs#L436)).
That is a range-type statistic (KPSS-adjacent), not a CUSUM with a decision interval h chosen against a
target average run length — and the module's own doc comment calls the gate "the classic 4-sigma-ish
gate" while the call site uses 2.5, which is the row's disagreement in miniature. Two things follow and
neither can be settled from a page: the statistic's name, and whether 2.5 is a tight or a meaningless
gate on a normalised max excursion. Measurement: over the audited BTCUSDT 15m 501 bars plus ETHUSDT 15m
and BTCUSDT 1h, emit the crossing rate at gate ∈ {2.0, 2.5, 3.0, 4.0} and the mean bars between crossings.
If 2.5 crosses more than ~10 times in 500 bars, the gate is noise and the classic CUSUM with h sized from
the measured run length is adopted; if it crosses 0-2 times, keep the statistic, fix the doc comment and
the card's name.

**`indicators.hmm` — nothing to adopt.** The row's verdict is `no_external_rule` and it is the truest
statement of that verdict in the audit: a two-state Gaussian HMM EM-fit over 120 bars has no TradingView
convention to photograph. Adopted here means only this: the concept stays as a house model with its
0.5 posterior split declared as ours, its `n > 125` floor respected, and the EM window's two assumptions
(2 states, 120 bars) registered as parameters like the rest of the family's.

This section authorises, now: the HV and z-score events demoted to series states with percentile labels,
the parameterisation of every literal the house models run on, and the GARCH card's correction. It keeps
the six house readings and the pairs ±2 convention. It defers the changepoint gate and its name behind
the crossing-rate count at four thresholds. It authorises nothing about drawing — the 205 labels and the
pane routing are the shape work above.
