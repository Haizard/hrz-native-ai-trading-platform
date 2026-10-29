# The shape vocabulary

Status: **levels, boxes and readouts are settled; the rest is open.** Pass 1 (Smart Money
Concepts, 32 rows) fixed what `level_run` and `lifecycle_box` mean, because nine later
passes draw against them. Pass 2 (Supply & Demand, 4 rows) tested that generalisation and
it held — three of its four rows needed no new shape — and claimed `readout`, which until
then was a candidate on pass 11's say-so. Pass 3 (Price Action, 11 rows) tested it a third
time and it held again: ten of its eleven rows asked for `level_run`, `lifecycle_box` or
`time_box` unchanged, and the eleventh claimed `bar_marker` as *wiring* rather than as a new
shape, which is the first time a pass has asked for a form the app can already paint.
Plan Task 18 finalises the file.
Its purpose is to be the one place the vocabulary is declared, so that a pass which
needs a fifth kind or a new shape has to say so here, in front of the gate, instead of
typing a value into a ledger row and hoping the count still adds up.

The gate enforces exactly this file's four kinds:
[`concept_visual_audit.mjs`](../../../tools/concept_visual_audit.mjs) refuses a `kind`
it was not told about, and `check` fails if any kind stops being named here.

## The four kinds of thing a concept can be

| `kind` | What the concept is | Drawn as | Families |
| --- | --- | --- | --- |
| `event` | Fires on a set of bars | A mark, a segment, a box | SMC, Price Action, Candlesticks, Economic Releases |
| `series` | One value per bar, over a scale | A line, a band — on the price scale or its own | Trend, Momentum, Volatility, Volume, GARCH, Kalman |
| `interval` | A state held across time, whatever price does | A full-height box with a label | Sessions, HMM regime, changepoint |
| `readout` | A number the trader reads | The figure, and the reference it is unusual against | `indicators.zscore` today; every score the engines beside the chart will want |

`kind` is the axis the drawing question is asked on. `ConceptShape` answers only *where
on the chart*; it never says what the concept is, and that is how a `+2.7σ` deviation and
a doji ended up with the same text mark.

Three rules the passes do not get to renegotiate:

- **A shape does not know which venue fed it.** What differs between a Binance tier and a
  derived tier is the rule's inputs and the provenance on the signal, never the geometry.
- **Wiring before rendering.** The scripting path already draws `Line { extend }`,
  `Polyline`, `Polygon`, `Band` and `Table`
  ([script.rs:67](../../../../frontend/chart-engine/src/script.rs)); a vocabulary item the
  engine can already paint is `wiring`, and only a shape it cannot paint at all is
  `new_rendering`. The ledger's `target_shape.status` is where that distinction is recorded.
- **A series' scale comes from the registry,** not from the pass: `pane` true means its own
  scale, `overlay` true means the price frame, and the gate rejects a row that disagrees
  ([rule 5](../../../../docs/superpowers/plans/2026-10-29-concept-visual-fidelity-audit.md)).

The rule is enforced by reading, not by code, so it is worth recording that pass 1 found
two of its own rows breaking it and fixed them: `smc.trendline_sweep` is not a `segment`
(a horizontal level cannot carry a projected slope, so it is `outline`) and
`smc.smt_divergence` is not a `segment` either (it needs a second series, so it is
`pair_link`). A name the engine can already paint is never tagged `new_rendering`.

## What the concept path can draw today

Three members, and both spatial ones are bounded by their own two bars
([scene.rs:2847](../../../../frontend/chart-engine/src/scene.rs)):

- `Zone` — a rectangle from `from_ns` to `to_ns` between `low` and `high`. Lowers to a
  Box plus a Label, two draws.
- `Segment` — a horizontal level from `from_ns` to `to_ns` at `price`. Lowers to a Line
  plus a Label, two draws.
- `CandleMark` — a word at a bar, above or below it. One Label.

`position_concept_shape` then floors a zero-length segment at `bar_w * 0.6`, which is why
the audit's most common `draw.today` reads "a 1.3px stub and a floating word": 10 of the
29 SMC concepts that fired at all in a 501-bar window answer `bars: [t, t]` for **every**
event they produce, so every one of them draws that stub.

## Vocabulary the family passes have asked for

Each row below is a *candidate*, named by the gap the audit measured rather than by the
family that first needs it, so pass 4's session box and pass 1's model boxes are one shape
and not two. A pass that renames or refuses one does it here, with the reason.

| Candidate shape | Kind | Settled? | What it must express | Claimed by | Evidence |
| --- | --- | --- | --- | --- | --- |
| `level_run` | event | **yes — pass 1** | A horizontal level at one price with a *run*: it starts at the bar that made it and extends right across the following bars, dashed or solid by class, labelled once at its left end. Never a point. | pass 1 (9 rows) | `smc.bos` answers `bars: [t, t]` for every event in a 501-bar window, so today it lowers to a 1.3px stub and a floating word |
| `lifecycle_box` | event | **yes — pass 1, confirmed by pass 2** | A box between two prices whose right edge is *time, not the bar that made it*: unmitigated boxes reach forward, mitigated ones stop at the mitigation bar and are then removed. Same class carries the polarity (a failed one is redrawn in the opposite colour with a non-solid fill). | pass 1 (12 rows), pass 2 (3) | `smc.fvg`'s 50 measured events span 1 to 221 bars (median 174); `smc.ifvg` 2-219, `smc.order_block` 5-178 — the detector already knows a lifecycle, and the renderer treats it as a fixed two-bar box. Pass 2 found the same gap one layer closer to the source: `sd.zone_tested`'s band has both edges right and is 1-4 bars long, because the event boundary drops the `to` and `mitigated_at` its own detector already computed ([regions.rs:150-171](../../../../crates/analytics-core/src/regions.rs#L150)) |
| `time_box` | interval | open | A full-height region over a time range, labelled, with no price extent at all | pass 1 (4), pass 4 | `smc.judas_swing`, `smc.power_of_three`, `smc.model2022` and `smc.market_maker_model` are states of the clock that the router sends to the price-zone branch, so they draw as a price band — a *where* answering a *when* |
| `outline` | event | open | A polyline traced through named pivot bars, plus an optional dashed neck level and a projected target | pass 1, pass 6 | Chart patterns draw as a flat band between two prices; `smc.trendline_sweep` computes a slope through three touches and then throws it away, drawing a horizontal level at the last one |
| `pair_link` | event | open | A line joining a pivot on this instrument's pane to the matching pivot on a second series' pane | pass 1 | `smc.smt_divergence` emits nothing on the scan route at all: the gateway's `/concept-events/smt` lane has no caller and the card paints its "nothing to draw" hairline |
| `legged_path` | event | open | An XABCD polyline with each leg labelled, optionally filled | pass 7 | Harmonics draw as a band or a mark |
| `ladder` | event | open | A set of named ratios across one impulse, each with its own level and label | pass 5 | Fibonacci draws as six anonymous segments |
| `bar_marker` | event | open — claimed as `wiring` by pass 3 | A glyph at a bar — arrow, dot — with the name optional | pass 3, pass 8 | Candlestick patterns draw as a text label, which is the noise a trader scans past. Pass 3 found the same gap one layer closer to the source: `pa.higher_highs` knows which swings stepped up but pushes *prices* into its swing vectors ([events.rs:3014](../../../../crates/analytics-core/src/events.rs#L3014)), so the pivot's bar never reaches the painter that already exists — the row is `wiring` because the missing piece is an index, not a shape |
| `readout` | readout | **yes — pass 2** | A formed figure, the window it was computed over named in the row, and — because a score is about something — the drawing it hangs on named too: a bar for a per-bar figure, a zone or a band for a grade. Never a word that happens to contain a number. | pass 2 (`sd.zone_strength`), pass 11 | A z-score concept currently routes to a `CandleMark`; the arithmetic stays in Rust and the shell never derives it. Pass 2 found the number already on the wire (`evidence.magnitude` reaches the browser on 1,169 of 4,247 events) and read by neither renderer, and that `ConceptShape`'s only text member prints a *name* taken from `concept_id`, never a measured value — so the shape needs a painter, and `new_rendering` is the honest tag |

## A readout is attached to something

Pass 1 asked where an *interval* is supposed to live and found the registry has no answer
(`pane: false, overlay: false` on all nine). Pass 2 found the same question on the
`readout` side, in a sharper form: `sd.zone_strength` is a 0-100 grade, and the registry
puts it on the price frame with no pane and no overlay, so the only scale available to it
is a price it is not. The vocabulary's answer is that a readout names its **host** — the
bar, zone or band the figure is about — and the host's own shape carries the geometry while
the readout carries the number and the window it was measured over. A row whose host is a
`lifecycle_box` therefore inherits that box's status; a per-bar figure like
`indicators.zscore` hangs on a bar and needs no box at all.

## How far a level or a box runs — the question pass 1 had to answer

The spec predicted "a broken level runs rightward until mitigated". **That sentence is not
in the reference set**, and the audit says so rather than drawing it. What is documented,
verbatim, on
[the toolkit's settings page](https://docs.luxalgo.com/platform/algos/price-action-concepts/settings):

> `Extend Imbalance` — Numerical, default: 10 — *Extend the imbalance area to the right by
> the selected amount of bars.*

and on the same page a `Mitigation Method` per concept family (`Close`, `Wick`, `Average`),
with the imbalance areas removed once mitigated, and for order blocks
`Show Last` default 5 — *how many of the most recent volumetric order blocks to display on
the chart*. The four concept pages (market-structures, imbalances, order-blocks, liquidity)
contain no use of the word "extend" at all.

So the two settled shapes take their right edge from different places, and this is the rule
the next nine passes inherit:

- **`lifecycle_box` — the edge is a lifecycle.** For the gap and block family the reference
  does document both halves: the area extends right by a count and is removed when
  mitigated. Our `forming` / `mitigated` / `invalidated` statuses already carry the same
  information; only the renderer ignores them, and `concept_shapes.js`'s
  `status === "forming" || status === "confirmed" ? toNs : toNs` is the dead branch that
  proves it. The count is a parameter, not a shape property.
- **`level_run` — the edge is a run, and its length is a policy.** A level must have
  visible length, because a level drawn as a point is not read as a level. Nothing in the
  reference set fixes where a structure or liquidity line ends, so the shape specifies the
  run and the pass leaves its length to a parameter with a default — the same choice
  LuxAlgo exposes as `Extend Imbalance`. A row whose canonical says "until swept" without a
  citation is a row that was corrected; pass 1 corrected seven.

One more convention pass 1 found and did **not** copy: the reference toolkit optionally
colours the candles themselves by the structure read — *Bullish BOS: Bright Green, Bearish
BOS: Bright Red, Bullish CHoCH: Dark Green, Bearish CHoCH: Dark Red* — behind a
`Color Candles` toggle. It is a toggle, not the drawing, so no `level_run` row asks for it;
if the platform ever wants it, it is a candle colour, not a concept shape.
