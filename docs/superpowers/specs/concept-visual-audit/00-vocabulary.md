# The shape vocabulary

Status: **open.** The twelve family passes grow this file; plan Task 18 finalises it.
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

## What the concept path can draw today

Three members, and both spatial ones are bounded by their own two bars
([scene.rs:2847](../../../../frontend/chart-engine/src/scene.rs)):

- `Zone` — a rectangle from `from_ns` to `to_ns` between `low` and `high`. Lowers to a
  Box plus a Label, two draws.
- `Segment` — a horizontal level from `from_ns` to `to_ns` at `price`. Lowers to a Line
  plus a Label, two draws.
- `CandleMark` — a word at a bar, above or below it. One Label.

## Vocabulary the first photographs already asked for

Each row below is a *candidate*, named by the gap the audit measured rather than by the
family that first needs it, so pass 4's session box and pass 11's regime box are one shape
and not two. A pass that renames or refuses one does it here, with the reason.

| Candidate shape | Kind | What it must express | Claimed by | Evidence |
| --- | --- | --- | --- | --- |
| `level_run` | event | A price level that starts at a bar and runs right until a bar touches it, dashed, labelled at its start | pass 1 | `smc.bos` answers `bars: [t, t]` for every event in a 501-bar window, so today it lowers to a 1.3px stub and a floating word |
| `lifecycle_box` | event | A box whose right edge is its *mitigation* bar: open boxes reach the newest bar, closed ones stop where they were filled | pass 1 | `smc.fvg` draws 7 one-bar slivers and 43 bands with a median span of 177 bars; `smc.ifvg` 120, `smc.order_block` 29 — a lifecycle rule, not an FVG bug |
| `outline` | event | A polyline traced through named pivot bars, plus an optional dashed neck level and a projected target | pass 6 | Chart patterns draw as a flat band between two prices |
| `legged_path` | event | An XABCD polyline with each leg labelled, optionally filled | pass 7 | Harmonics draw as a band or a mark |
| `time_box` | interval | A full-height region over a time range, labelled, with no price extent at all | pass 4 | Sessions draw as a price band, which is a *where* answering a *when* |
| `ladder` | event | A set of named ratios across one impulse, each with its own level and label | pass 5 | Fibonacci draws as six anonymous segments |
| `bar_marker` | event | A glyph at a bar — arrow, dot — with the name optional | pass 8 | Candlestick patterns draw as a text label, which is the noise a trader scans past |
| `readout` | readout | A formed figure with the window it was computed over named in the row | pass 11 | A z-score concept currently routes to a `CandleMark`; the arithmetic stays in Rust and the shell never derives it |
