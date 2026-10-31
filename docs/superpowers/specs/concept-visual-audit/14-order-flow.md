# Pass 10d — Order Flow (1 row)

Judged 2026-10-31 against `main`, on the window the gateway serves today (501 bars,
2026-10-26T06:30 to 2026-10-31T11:30 UTC). The machine-readable verdict is the single row of
[`ledger/order-flow.json`](../../../../reports/concept-visual-audit/ledger/order-flow.json);
`node tools/concept_visual_audit.mjs check --family order-flow` reports **1 row, 0 incomplete**.

**The one order-flow concept in the registry draws a volume delta as a price level, twice, with a
dashed line and a word that still has its underscore.** 42 events, 42 horizontal segments, 42
labels reading `delta_shift` — not `delta shift`: the segment branch of the router keeps the
concept id intact ([`concept_shapes.js:99`](../../../../frontend/app/concept_shapes.js#L99)), which
is how the family's only row also carries the pass's only raw-underscore label. The segment's y is
the shift bar's close, so the drawing says *"something happened at this price"* about a quantity
that is not a price at all: the difference between aggressive buys and sells in that bar.

**This is the pass's only event concept with neither `pane` nor `overlay`, and the registry is the
defect.** `order_flow.delta_shift` declares `pane: false, overlay: false`
([`concepts.rs:1844-1855`](../../../../crates/market-data/src/concepts.rs#L1844)) — the only one of
the pass's 32 concepts that answers neither. Every other row in the five families has a scale
because the registry names one. Delta has a scale too, and it is neither: it is a signed quantity
around zero, per bar, which is the definition of an own-pane histogram. The pass records the flag
as the defect and the target shape as `series_histogram, own_pane`.

## How it was measured

`node tools/concept_visual_audit.mjs render order-flow` on 2026-10-31 rebuilt the page over the
501-bar window above. The concept supplied **42 events, 84 draws and 42 labels** — every event
lowered to a dashed Line plus a Label on the price frame. Every number below comes from parsing
the page's embedded scene and from the scan's own payload.

| concept | registry | events | label text | segment y span (484px plot) | what the drawing is | `draw` |
| --- | --- | --- | --- | --- | --- | --- |
| `order_flow.delta_shift` | neither | 42 | `delta_shift` | 46.9-449.5, 75 distinct y | 42 dashed one-bar segments at each shift bar's close | `differs_major` |

**42 is a detection count, not the route cap.** Unlike the profile and oscillator rows that sit
exactly at 50, the delta shift fired on 42 of 480 evaluable bars (8.8%) at ratios from the
threshold upward. The events are genuine; the geometry is what fails.

## What the reference set says

- **Volume delta is per-bar, signed, and best read as a histogram.** TradingView Help Center,
  [Volume Delta](https://www.tradingview.com/support/solutions/43000725057-volume-delta/): *"The
  Volume Delta indicator uses intrabar volume and price fluctuations to estimate the difference
  (delta) between buying and selling pressure within each chart bar, providing insight into an
  instrument's sentiment and market dynamics."* The bar is the unit, the pane is the scale, and
  the zero line is the reference.
- **There is no external 2.5x rule.** TradingView plots delta on every bar and marks no
  threshold event; our `spike_multiple` is a house reading of the same data. That is why the row's
  `fires` verdict is `no_external_rule`, not `different_rule`: there is nothing to agree or
  disagree with.

## What we draw today

**A price mark for a non-price quantity.** The 42 segments are 1.3px wide, dashed, 1px, drawn from
the event bar to itself at the bar's close ([`app.js:14945-14953`](../../../../frontend/app/app.js#L14945)
is the card's identical geometry; the chart's is `position_concept_shape`'s same floor). The
magnitude — the ratio of the bar's delta to the 20-bar mean, the thing that made the event fire —
arrives as `evidence` on the wire and is read by neither renderer. What remains is the word.

**The underscore is a second, smaller defect with the same root.** `candle_mark` labels replace
`_` with a space ([`concept_shapes.js:89`](../../../../frontend/app/concept_shapes.js#L89)); the
`segment` branch does not. A reader sees `delta_shift` where every other concept in the pass reads
like English. It is a one-character defect and it is in the picture, so the pass records it.

**The parameter is honoured — the audit's only fully wired parameter in the pass.**
`spike_multiple` is declared on the concept ([`concepts.rs:1849`](../../../../crates/market-data/src/concepts.rs#L1849))
and reaches the engine through `event_config_for`, which maps it onto `config.delta_multiplier`
([`composer.rs:738-741`](../../../../crates/market-data/src/composer.rs#L738)). The scan's default
run uses `EventConfig::default()` (2.5), so the catalog and the engine agree by construction and
by value. This row is the counterexample to the pass's general finding that declared parameters
are not honoured — which makes it the model the other 31 families do not follow.

## The card agrees with the chart, and both are wrong

The card is not a pane/overlay concept, so it takes the events path and lowers a `price_level`
event to a dashed segment from the first bar to the event bar
([`previewEvents`, app.js:14945-14953](../../../../frontend/app/app.js#L14945)). For a one-bar
event that is the same 1.3px stub the chart paints; the card omits the label, exactly as it does
for Pass 9's `volume.spike`. So `preview_agrees: agrees` — the two renderers agree about the
defect, which is what this verdict reports. It does **not** mean either one is right.

## Parameters: one declared, one honoured

`spike_multiple` (default 2.5) is declared and honoured. Nothing else moves the drawing because
there is no drawing to move: the constant that *would* move it is the delta's 20-bar mean window,
which is a Rust constant with no registry entry. The reference exposes the delta's source and
intrabar resolution as settings; this catalog exposes only the threshold.

## The row, and the shape it needs

| Target shape | Row | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `series_histogram` (1) | `delta_shift` | **wiring** | `SeriesBars` inside `SubPaneSpec::Series` already paints zero-anchored bars on a pane scale ([scene.rs:427-429](../../../../frontend/chart-engine/src/scene.rs#L427)); what is missing is the **host**: `pane: true` in the registry, a delta series builder in `indicator_series.rs`, and a dispatch arm beside the other pane ids ([indicator_series.rs:133](../../../../crates/analytics-core/src/indicator_series.rs#L133)). A marker on the exceptional bar rides in the same spec ([scene.rs:431-435](../../../../frontend/chart-engine/src/scene.rs#L431)). No painter is missing |

`draw.verdict`: **1 `differs_major`, 0 `matches`**. `fires.verdict`: **1 `no_external_rule`** —
the reference has no threshold event to compare against. `preview_agrees`: **1 `agrees`** (agreeing
on the defect).

## What this pass asks of the vocabulary

`series_histogram` is settled in
[`10-trend.md`](10-trend.md#what-this-pass-asks-of-the-vocabulary) as a pane shape with the
existing `SeriesBars` arm. This row is the shape's clearest case **for the scale rule**: a signed
quantity around zero cannot be drawn on the price frame, and a concept that declares neither pane
nor overlay has no scale at all. The registry defect is recorded here; the fix belongs to the
registry, not to the vocabulary.

The row also confirms the pass-wide rule that **status follows the painter, not the plumbing**.
The histogram painter exists, so the row is `wiring` even though its registry flag, its builder and
its dispatch arm are all absent — the same class of gap Pass 9 recorded on `price_band` and Pass 10a
on `series_line`. `new_rendering` is reserved for shapes the engine cannot paint at all.

## Fixes this pass asks for

1. **Give `order_flow.delta_shift` a pane.** `pane: true, overlay: false` in the registry, a
   delta series builder beside the other pane builders, and a zero line. The historical per-bar
   delta is already computed in `detect_events`; it needs to be emitted as a series, not only
   scanned for threshold events.
2. **Emphasise the event bar in the pane, and carry the multiple.** A marker on the shift bar and
   the ratio (the `evidence.magnitude` already on the wire) as its label — not the concept's id.
3. **Normalise the label text in the segment branch.** `delta_shift` should read `delta shift`;
   the `candle_mark` branch already does this substitution and the segment branch does not.
