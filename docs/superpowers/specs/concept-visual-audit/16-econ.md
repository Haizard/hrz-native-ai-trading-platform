# Pass 12 — Economic Releases (6 rows)

Judged 2026-10-31 against `main`, on the window the gateway serves today (501 bars,
2026-10-26T06:30 to 2026-10-31T11:30 UTC). The machine-readable verdicts are the six rows of
[`ledger/econ.json`](../../../../reports/concept-visual-audit/ledger/econ.json);
`node tools/concept_visual_audit.mjs check --family econ` reports **6 rows, 0 incomplete**.

**The econ family is the only family in the audit whose concepts draw nothing at all — and here
that is not a painter failure, it is the event path having no econ entry.** The six registry rows
declare `event_kinds: &[]`, `pane: false`, `overlay: false`
([`concepts.rs:440-511`](../../../../crates/market-data/src/concepts.rs#L440)), so the scan emits
nothing for them and all six pages are empty. `EventKind` has no economic variant
([`events.rs:49`](../../../../crates/analytics-core/src/events.rs#L49)); a release is an external,
time-keyed fact, and the concept path has no way to carry one. What the engine does have is honest
and off-chart: a countdown chip that arms you before the print and an alert box that reports the
number against forecast after it — never a word at a price
([`app.js:6150-6214`](../../../../frontend/app/app.js#L6150)). The shape the family needs is
`time_line`, Pass 5's vertical rule, already `wiring`; the label carries the figure and the
surprise.

**The pass's sharpest small defect: `gdp` has no countdown.** `/econ/next` returns five kinds —
claims, nfp, cpi, fomc, pce — and omits gdp
([`econ_routes.rs:576-583`](../../../../crates/api-gateway/src/econ_routes.rs#L576)). Five concepts
have an off-chart surface; the sixth has only `/econ/latest?kind=gdp` after the FRED refresh stores
a print.

## How it was measured

`node tools/concept_visual_audit.mjs render econ` on 2026-10-31 rebuilt all six pages over the
501-bar window above. The shared event scan answered **4,080 events across the library; not one was
an econ event**, and each of the six pages drew **0 events, 0 shapes, 0 draws**. The empty page is
the measurement, not a failed fetch: the concepts are in the registry, the pages build, and the
router has nothing to route.

| concept | registry | events | what the chart draws | off-chart surface | `draw` |
| --- | --- | --- | --- | --- | --- |
| `econ.nfp` | neither | 0 | nothing | countdown + alert; first Friday 08:30 ET | `differs_major` |
| `econ.cpi` | neither | 0 | nothing | countdown + alert; approximate NFP + 5d | `differs_major` |
| `econ.fomc` | neither | 0 | nothing | countdown + alert; published table, 14:00 ET | `differs_major` |
| `econ.claims` | neither | 0 | nothing | countdown + alert; every Thursday 08:30 ET | `differs_major` |
| `econ.pce` | neither | 0 | nothing | countdown + alert; approximate NFP + 21d | `differs_major` |
| `econ.gdp` | neither | 0 | nothing | **none — omitted from `/econ/next`** | `differs_major` |

**The registry's `channels` field is under-reported by the catalog.** Each row declares
`required_channels: DataChannels::MACRO`
([`broker.rs:49`](../../../../crates/market-data/src/broker.rs#L49)), but `/concepts` serializes
through `DataChannels::names()`
([`concept_routes.rs:75`](../../../../crates/api-gateway/src/concept_routes.rs#L75)), and `names()`
has no macro row ([`broker.rs:65-82`](../../../../crates/market-data/src/broker.rs#L65)). The live
catalog therefore reports `required_channels: []` for all six — the ledger records the registry's
`["macro"]` and this paragraph records the serializer's gap. It is the spec's catalog-versus-engine
defect in its quietest form: the one channel these concepts need is the one the catalog cannot say.

## What the reference set says

- **An event is a vertical mark on the time axis, not a price level.** The citable form is
  TradingView's Time Marker script: *"Time Marker draws vertical lines and optional background
  bands at user-defined times on the chart."*
  ([script](https://www.tradingview.com/script/IHgfEyaU-Time-Marker/), checked 2026-10-31.) The
  platform's own events overlay applies the same geometry to earnings and economic releases; the
  source of the timestamps differs, the drawing does not. A release has a timestamp, not a price.
- **The figure rides in the label.** A calendar marker without the actual-vs-forecast figure is a
  line the trader has to look up somewhere else. The reference's whole value is the instant plus
  the number; ours already computes both (`/econ/latest`: `actual`, `forecast`,
  `forecast_source`, `surprise`, `macro_bias`, [`econ_routes.rs:469-509`](../../../../crates/api-gateway/src/econ_routes.rs#L469)).
- **Our own honesty rule, stated in the code.** The schedule endpoint's note:
  *"the countdown arms you at T-0; the number itself lands when the source publishes it, and the
  engine reads it the moment the API has it. No API front-runs the wire."*
  ([`econ_routes.rs:583`](../../../../crates/api-gateway/src/econ_routes.rs#L583).) The engine's
  doctrine at the top of the file is the same sentence one step further: *"an economic event is an
  information catalyst, not a signal -- and the market's own reaction is the confirmation, never
  the headline"* ([`econ_routes.rs:3-5`](../../../../crates/api-gateway/src/econ_routes.rs#L3)).
  The alert box ends with it: *"the reaction confirms, not the headline — the chart is the
  confirmation"* ([`app.js:6205`](../../../../frontend/app/app.js#L6205)).

That doctrine settles the drawing's colour and direction: a `time_line` is neutral. The surprise
and the bias belong in the label and the card, never in an arrow on price.

## What we draw today

**Nothing on the chart.** The six concepts are registry rows with no event kinds and no builder;
the card asks the shared event scan
([`app.js:14523-14529`](../../../../frontend/app/app.js#L14523)) and gets an empty list, so it
paints the "nothing to draw" hairline. That is the family's `preview_agrees: not_rendered` verdict:
neither renderer attempts a mark, and there is nothing to compare.

**The off-chart surface is a chip and an alert, and both are deliberately ephemeral.**

- The chip polls `/econ/next` every tick, shows the soonest release as `kind T-<time>`, goes hot
  inside the last hour, and titles itself with the schedule rule
  ([`app.js:6169-6183`](../../../../frontend/app/app.js#L6169)). It is a header element, not a
  chart layer ([`index.html:1704`](../../../../frontend/app/index.html#L1704)).
- The alert box appears only within fifteen minutes of the release instant, compares
  `latest.release.id` against the last one it showed, and renders actual vs forecast, the forecast's
  source, the surprise in sigmas and the bias label
  ([`app.js:6186-6206`](../../../../frontend/app/app.js#L6186)). It auto-hides after sixty seconds
  and is dismissed by its close button ([`index.html:1710`](../../../../frontend/app/index.html#L1710)).
- The countdown's CPI and PCE instants are approximations — NFP + 5 days and NFP + 21 days,
  labelled approximate in the response itself ([`econ_routes.rs:564-581`](../../../../crates/api-gateway/src/econ_routes.rs#L564)).
  The stored release's own `release_ns` is exact, and a chart marker must use that, not the
  schedule's T-minus estimate. The chip is allowed to approximate because it says so; a mark on a
  bar cannot be five days early.
- `gdp` is absent from the schedule array entirely, so a GDP release gets no countdown and no
  alert: the alert path is driven by the same `next.kind` the chip shows.

**The missing piece is a feed, not a painter.** `ScriptDraw::Line` already contains the vertical
rule: two anchors with `x1 == x2` is a segment that does not extend
([`script.rs:69`](../../../../frontend/chart-engine/src/script.rs#L69)), and
`scene_overlay_tints` already paints full plot height from a zero-border box
([`script.rs:785`](../../../../frontend/chart-engine/src/script.rs#L785)). Pass 5 recorded
`time_line` as `wiring` for exactly this reason. What is missing is a `ConceptShape` variant and a
bridge from `EconStore` into the concept event stream; the release fact, the figure, the surprise
and the timestamp all already exist on the server.

## The 6 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `time_line` | all six | **6 wiring** | Pass 5's vertical rule (the existing `ScriptDraw::Line` arm at `x1 == x2`), a `ConceptShape` variant, and a macro-release feed into the concept path. The label is a `readout` hung on the line: figure, forecast, surprise. `gdp` additionally needs its schedule entry |

`draw.verdict`: **6 `differs_major`**. `fires.verdict`: **6 `different_rule`** — the reference
marks the release instant, our engine has no chart rule at all and keeps the fact off-chart.
`preview_agrees`: **6 `not_rendered`**.

## What this pass asks of the vocabulary

**`time_line` is the family's whole answer, and its label makes it a readout host.** The
vocabulary already defines the shape as *"a vertical rule at a bar, spanning the plot's full
height, labelled with its count and carrying no price at all"* and already records it as `wiring`
from Pass 5. Economic releases add the one thing Pass 5's Fibonacci case did not need: the label
carries a measured figure — actual, forecast, and the surprise between them — so the row's shape is
`time_line` with a `readout` member, not a new shape. Task 18 should record the label contract
there: the draw is time-only, the figure rides the label, and neither ever touches a price.

**Scale is `time_only`, and the registry's `pane`/`overlay` flags do not apply to an event.** The
six rows declare neither, correctly: the concept is an instant on the clock, not a pane or an
overlay. The plan's Task 17 says `interval`-adjacent but time-only; the vocabulary's own
distinction is sharper — a `time_box` spans two edges, a `time_line` has one. An event is the one
case where the line is the whole drawing. The shape is implementable without reading this pass:
*vertical rule at `release_ns`, full height, label = kind + actual vs forecast + surprise, neutral
colour*.

**`rule_as_data` names the macro channel, and the release timestamp is the event's identity.** All
six rows carry the same predicate — `release(kind, release_ns) from the macro store => time_line at
the containing bar` — with `inputs: [{ release, macro }]`. That is what makes the shape
venue-agnostic in the Quant Brain sense: the rule reads a release channel, not a candle pattern.
Warm-up is zero bars by definition; an event does not need history, it needs a clock.

## Fixes this pass asks for

1. **Feed releases into the concept path as time-keyed events.** Add the econ event variant (or an
   equivalent release channel the router can carry), emit `time_line` at `release_ns` with the
   figure and the surprise in the label. The shape and painter exist; the bridge does not.
2. **Add `gdp` to `/econ/next`.** Five kinds have a countdown and one does not
   ([`econ_routes.rs:576-583`](../../../../crates/api-gateway/src/econ_routes.rs#L576)); a GDP
   release currently has neither a countdown nor an alert.
3. **Serialize `MACRO` in `DataChannels::names()`.** The catalog reports `required_channels: []`
   for all six econ rows while the registry declares `MACRO`
   ([`broker.rs:65-82`](../../../../crates/market-data/src/broker.rs#L65)); the one channel the
   family needs is the one the catalog drops.
4. **Mark the stored instant, not the schedule approximation.** CPI's `T-` rides NFP + 5 days and
   PCE's rides NFP + 21 days, both documented as approximate; the release's `release_ns` is the
   fact. A marker five days early is worse than no marker.
5. **Keep the doctrine in the drawing.** Neutral rule, figure in the label, reaction stays the
   confirmation — no arrow, no surprise colour on price. The alert box's closing line is the
   contract, and the chart mark should obey it.
