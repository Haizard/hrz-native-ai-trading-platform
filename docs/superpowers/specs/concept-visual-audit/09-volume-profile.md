# Pass 9 — Volume profile (9 rows)

Judged 2026-10-31 against `main`, on the window the gateway serves today (501 bars,
2026-10-26T06:30 to 2026-10-31T11:30 UTC). The machine-readable verdicts are the nine rows of
[`ledger/volume-profile.json`](../../../../reports/concept-visual-audit/ledger/volume-profile.json);
`node tools/concept_visual_audit.mjs check --family volume-profile` reports **9 rows, 0 incomplete**.

**The arithmetic in this family is the best in the audit so far, and the drawing is turned ninety
degrees.** LuxAlgo defines the object this family is meant to draw in one sentence: *"A volume
profile is a histogram of traded volume organized by price instead of time"*
([LuxAlgo Library, Volume Profile](https://www.luxalgo.com/library/concept/volume-profile/)). The
engine builds exactly that object and gets its prices right — the POC (80,225.98), VAL (78,625.98)
and VAH (81,625.98) bracket each other, and the value area's 70% is the reference's own convention
([`DEFAULT_VALUE_AREA_PCT = 0.70`](../../../../crates/analytics-core/src/volume_profile.rs#L28)).
What the painter receives is a *bar*: the router sees `price_level` and draws a Segment — a 1.29px
stub plus the concept's id — so the profile's price axis survives and its **time axis collapses to
one bar**. The value area, which is a horizontal band across the period it summarises, renders as a
**106 × 245px translucent curtain pasted over the newest 50 bars**; the point of control, which is
one line across that period, renders as **50 back-to-back ticks**, one per bar, with the word `poc`
printed 50 times on them.

**Four concepts produce 200 events that carry eight distinct price measurements.** The profile is
recomputed over the entire trailing window on every replay bar
([`detect_events` per bar, then the newest-50 cap](../../../../crates/api-gateway/src/concept_routes.rs#L240)),
so `volume.poc` emits the same price on all 501 bars (50 kept, **one** distinct y), `volume.value_area`
the same band (**two** heights, one per VAL revision), `volume.hvn` **three** lower edges and
`volume.lvn` **two**. Every one of the four hits exactly 50 — the route's cap, not a detection count.
The remaining five are genuine event families (spike 19, initial balance 38, single prints and
acceptance also capped at 50, poor extreme 0). **319 of the family's 357 events are exactly 1.29px
wide** — one bar; the 38 that are not are all `volume.initial_balance`, and they are long only
because their `formed_at` is the chart's first bar.

**Three of the nine concepts are not profiles at all, and one of them is silent.** `volume.spike`
is a property of a bar's volume, `volume.initial_balance` is a session range, and `volume.poor_extreme`
is an unfinished-auction extreme. The family is the registry's `Library::Volume` bucket, not a
drawing family, and the pass judges each against its own reference rather than against the profile
convention. `poor_extreme` produced **0 events on 501 bars**: its rule wants an edge cell carrying
1.5× the profile's mean TPO count, while the reference's bar is *more than one block* — a far
lighter, far more common condition
([TradingView, TPO indicator](https://www.tradingview.com/support/solutions/43000713306-time-price-opportunity-tpo-indicator/):
*"Poor highs and lows are extreme levels with more than one TPO block."*).

## How it was measured

`node tools/concept_visual_audit.mjs render volume-profile` on 2026-10-31 rebuilt all nine pages over
the 501-bar window above. The scan route answered **4,099 events** across 138 concepts; this family
supplied **357 events, 714 draws** (the zone and segment router lowers every event to a Line or Box
plus a Label) and **357 labels**. Every number below comes from parsing the pages' embedded scene and
from the scan's own payloads; the render command reports the same totals it printed while building
them (`19/38/50/50/0/50/50/50/50` events).

| concept | events | distinct price levels | draw | label text | label count | x extent |
| --- | --- | --- | --- | --- | --- | --- |
| `volume.spike` | 19 | 19 | `Line` 19 + `Label` 19 | `spike` | 19 | x 65.7-1041.1 |
| `volume.initial_balance` | 38 | 2 | `Line` 38 + `Label` 38 | `initial_balance` | 38 | x 12.1-747.4 |
| `volume.single_prints` | 50 | 2 | `Line` 50 + `Label` 50 | `single_prints` | 50 | x 1030.4-1083.1 |
| `volume.acceptance` | 50 | 23 | `Line` 50 + `Label` 50 | `acceptance` | 50 | x 976.8-1083.1 |
| `volume.poor_extreme` | 0 | — | — | — | 0 | — |
| `volume.poc` | 50 | 1 | `Line` 50 + `Label` 50 | `poc` | 50 | x 976.8-1083.1 |
| `volume.value_area` | 50 | 2 | `Box` 50 + `Label` 50 | `value_area` | 50 | x 976.8-1083.1 |
| `volume.hvn` | 50 | 3 | `Box` 50 + `Label` 50 | `hvn` | 50 | x 1030.4-1083.1 |
| `volume.lvn` | 50 | 2 | `Box` 50 + `Label` 50 | `lvn` | 50 | x 1030.4-1083.1 |

Two numbers in that table are the cap rather than the density. `volume.single_prints`,
`volume.acceptance`, `volume.poc`, `volume.value_area`, `volume.hvn` and `volume.lvn` all answer
exactly 50, the route's newest-50-per-concept limit; re-running the detectors' own clauses over the
window shows why: the profile concepts emit on **every** bar (501 fires, newest 50 kept), and the
single-print and acceptance clauses are true on long runs of consecutive bars. `volume.spike` is the
only row in the family whose 19 is a count: 19 bars of 480 evaluated (4.0%) traded at 2.52× to 9.45×
the prior 20-bar average.

## What the reference set says

- **The profile is organized by price, and its rows are price bands over a period.** LuxAlgo:
  *"A volume profile is a histogram of traded volume organized by price instead of time."* The POC
  is the histogram's heaviest row, the HVN/LVN are its bulges and thin shelves; none of them is a
  property of a single bar.
- **The value area is a band, 70% by convention, and the percentage is a setting.** LuxAlgo:
  *"The value area is the price band containing a set share of a period's trade, 70% by convention."*
  The audit's own plan names the gap this creates: the percentage "is a parameter this catalog does
  not honour".
- **A value-above-band edge is not a fixed threshold.** LuxAlgo's value-area page: *"None of these
  boundaries are guaranteed to hold; they mark where acceptance ended last time, not where it must
  end next time."* Our acceptance rule — two consecutive closes beyond the edge — is one
  implementation of that idea; the platforms' better-known test is the 80% rule's two consecutive
  **30-minute** periods, a time measure rather than a close count.
- **Poor extremes are common, not rare.** TradingView: *"Poor highs and lows are extreme levels with
  more than one TPO block."* One extra block at the extreme is enough. Our 1.5×-of-mean bar is a
  much heavier condition, and on this window it is never met.
- **The initial balance is a session range.** TradingView's Initial Balance Suite: *"The Initial
  Balance is the price range established during the opening period of a session — traditionally the
  first hour of RTH trading."* The band belongs to the session; it resets when the session does.
- **A single print is a row of the profile.** Senzou Kria: *"A single print is a price row in a
  Market Profile that was visited during only one time bracket, so it carries a single TPO letter."*
  The unit is a price row, not a bar.
- **A volume spike is the bar's volume.** LuxAlgo: *"A volume spike is a bar that trades dramatically
  more volume than its recent norm."* The bar is the host; the volume is the figure.

## What we draw today

**The profile's four concepts draw its price axis and throw away its time axis.** Each of the four
takes the zone-or-segment branch in the router, so the price is right and the x extent is the event
bar — one bar wide — while the *number of events* is the number of bars that re-emitted the same
snapshot. The result, measured on the pages:

- `volume.poc` — **50 lines, one price** (y 166.46; `price_level` 80,225.98 on every event), each
  1.29px long and tiled one bar apart: a 105px dotted line made of fifty ticks, labelled `poc` 50
  times. The reference draws one line at that price across the profile's period.
- `volume.value_area` — **50 boxes with the same top edge** (y 55.32) and one of two heights,
  254.04px (39) or 238.16px (11), each 1.29px wide at fill 0.08: a **106 × 245px curtain** over the
  newest 50 bars. The band's prices (VAL 78,625.98, VAH 81,625.98) are never printed; `value_area`
  is, fifty times.
- `volume.hvn` — 50 boxes **1.29px wide × 15.88px tall** (one bucket) at three lower edges; the node
  is a vertical tick, not a price band.
- `volume.lvn` — the same, two lower edges.

**The auction events are bars too, including the one that is supposed to be a band.**
`volume.initial_balance` emits **38 segments at only two prices** — the band's high (y 187.73, 20
segments) and low (y 165.72, 18) — every one starting at x 12.14, the chart's first bar, and ending
at its breakout or reversal bar; lengths run 64.3px to 735.3px. The band between the two levels is
never filled, so the "first-hour range" is two horizontal lines 22px apart with no body. The reason
is in the detector: `initial_balance(candles, 4)` takes the first four bars of the **loaded window**
([tpo.rs:100-107](../../../../crates/analytics-core/src/tpo.rs#L100)), not of the current session,
and stamps the event's `formed_at` with `candles[0].open_time` — so the band is the first hour of the
500-bar load, fixed for the whole chart, and the breakout events are crossings of a five-day-old
range. `volume.single_prints` draws **two prices** (y 474.17 below, y 31.18 above) on the newest 50
bars; `volume.acceptance` **23 prices** inside a 35px strip. Both are 1.29px ticks plus a snake_case
word.

**`volume.spike` puts a volume property on the price axis.** 19 dashed 1px lines, every one 1.29px
long, at the spike bar's close — 19 distinct prices, 19 copies of `spike`, and no number saying how
big the spike was. The magnitude (2.52×-9.45×) reaches the browser as `evidence.magnitude` and
neither renderer reads it. The reference's drawing is the volume column itself; with no pane in the
registry, the closest honest overlay is a flag on the bar carrying the multiple. The current mark is
a price level that does not exist.

**`volume.poor_extreme` has nothing to draw**: 0 events on 501 bars, so its page is the family's only
empty one and its card paints the "no data" hairline. If it fired it would draw what the detector
already computes — a one-bucket `zone_low`/`zone_high` at the poor extreme
([tpo.rs:186-199](../../../../crates/analytics-core/src/tpo.rs#L186)) — as another 1.3px dash plus
the word, because `volume.poor_extreme` is not in `CONCEPT_ZONE_IDS`
([concept_shapes.js:27](../../../../frontend/app/concept_shapes.js#L27)).

## The card is a second renderer, and on three rows it draws the band the chart refuses

Five rows **agree**: the card lowers a `price_level` event to the same segment from `bars[0]` to the
event bar at the same price, and for a one-bar event that is the same 1.3px stub the chart paints
([`previewEvents`, app.js:14945-14953](../../../../frontend/app/app.js#L14945)). `volume.poor_extreme`
is **not rendered** (0 events; the card shows its hairline).

The three zone rows **differ**, and the card is the better of the two. For a `confirmed` zone the
card stretches the box to the chart's right edge —
`right = event.status === "forming" || event.status === "confirmed" ? axis.bars - 1 : at`
([app.js:14933-14943](../../../../frontend/app/app.js#L14933)) — so it paints the value area and the
nodes as bands crossing time, which is the reference's own geometry, while the chart stops each box
at its event bar. Neither is right: the card's band runs from each of 50 event bars to the edge
(fifty stacked rectangles), the chart's runs one bar. The finding is that the two renderers disagree
about what a profile is, and the card is the one pointing at the answer.

## One parameter is declared, and none is honoured

The family declares **one** parameter, `spike_multiple` (default 2.5, range 1.2-20.0,
[concepts.rs:199-205](../../../../crates/market-data/src/concepts.rs#L199)), and honours **none**:
the scan calls `detect_events` with `EventConfig::default()` and passes no per-concept parameter, so
the catalog's knob and the engine's constant merely happen to agree at 2.5. Everything else that
moves in this family is a constant with no registry entry at all: the value-area percentage (0.70),
the node lookback, the bucket count (48 rows), the initial-balance bar count (4), the single-print
bucket (`last.close * 0.01` rounded to 50 rows), the spike average window (20 bars) and the route's
newest-50 cap. The reference exposes the value-area percentage as a setting on every platform that
draws a profile; this catalog cannot.

## The 9 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `price_band` (5) | `initial_balance`, `single_prints`, `value_area`, `hvn`, `lvn` | **5 wiring** | a box between two prices whose **two x edges are the period the measurement was computed over**, with no lifecycle and no removals, labelled once at its right edge with the measured prices. The `Box` + `Label` painter already exists (`Zone` lowers to it, [scene.rs:2847](../../../../frontend/chart-engine/src/scene.rs#L2847)); what is missing is a `ConceptShape` variant whose time edges come from the profile's window rather than from the event's own bar — the same class of gap pass 4 found on `time_box` |
| `level_run` (2) | `poc`, `poor_extreme` | **2 new_rendering** | a horizontal level at one price with a visible run across the profile's period, labelled once with its price. Pass 1's shape, unchanged; the run, not the level, is what the painter lacks |
| `bar_marker` (2) | `spike`, `acceptance` | **2 wiring** | a glyph at the bar — the volume multiple for a spike, a direction for acceptance — with the name optional and the value on hover. Pass 3's shape, unchanged |

`draw.verdict`: **9 `differs_major`, 0 `matches`**. Every row draws the right price (where it has
one) at the wrong extent — one bar where the reference draws a period — or draws a price where the
reference draws volume. `fires.verdict`: **9 `different_rule`**, for three distinct reasons: the
profile rows re-fire one snapshot on every bar (the reference computes it once per period); the
initial balance is the load's first hour, not the session's; and the spike, acceptance and poor
extreme thresholds are the platform's own numbers rather than the reference's. `preview_agrees`:
**5 `agrees`, 3 `differs`, 1 `not_rendered`**.

## What this pass asks of the vocabulary

**`price_band` is a new candidate, and it is not `lifecycle_box` or `time_box`.** A
`lifecycle_box`'s right edge is an *event* (mitigation, expiration) and its box is removed when the
lifecycle closes; a `price_band`'s two edges are the **measurement window** and the band persists as
long as that window does. A `time_box` has no price extent at all; a `price_band` has both price and
time. The five rows above want exactly this: two prices from the data, two times from the profile's
period, one label carrying the numbers. Task 18 decides whether it stays its own shape or becomes a
variant of `lifecycle_box` with a null lifecycle; the pass records the distinction rather than
folding a measurement into a lifecycle.

The family also **reuses** `level_run` for the two single-price rows and `bar_marker` for the two
bar-level rows, so the audit's vocabulary grows by one candidate for nine rows — and the plan's
expectation that "most rows here are `wiring`" holds: seven of nine.

## Fixes this pass asks for

1. **Compute each profile once per period, and emit it when the period closes.** Today
   `detect_volume_profile` recomputes the whole-window profile on every replay bar and the route keeps
   the newest 50 — 200 events for eight distinct measurements. One emission per profile is the
   reference's contract and removes the 50× label stacking on four concepts at once.
2. **Give the profile a time axis.** POC as one line, VAH/VAL as one band, HVN/LVN as price bands, the
   single print as one bucket-tall row, each across the period they summarise. That is the five-row
   `price_band` work above, plus `level_run` for the POC.
3. **Print the measurement, once.** `value_area` should carry VAH/VAL (or the percentage), the POC its
   price, a node its price — not the concept's snake_case id 50 times. The numbers are already on the
   wire; neither renderer reads them.
4. **Reset the initial balance at the session boundary.** `initial_balance(candles, 4)` must take the
   first four bars *of the session the event belongs to*, and `formed_at` must be the session open,
   not the load's first bar. Measured today: 38 segments from x 12.14, i.e. every one anchored to the
   chart's first bar.
5. **Give `poor_extreme` the reference's threshold, or record that it is a different rule.** "More
   than one TPO block" is the platform convention; 1.5× the mean cell count is a much heavier
   condition and was silent for all 501 bars. A concept that cannot fire is not a concept with a
   drawing problem — it is a rule problem the pass photographs and does not fix.
6. **Mark a spike as volume, not as a level.** A flag or volume column at the spike bar carrying the
   multiple (2.52-9.45 on this window), not a 1.3px dash at the close.
7. **Expose the four numbers that move the drawings**: the value-area percentage (70), the node
   lookback, the initial-balance bar count (4) and the spike multiple (declared, never read). Today the
   family declares one parameter and honours none, while the reference's profile page is a settings
   panel.
