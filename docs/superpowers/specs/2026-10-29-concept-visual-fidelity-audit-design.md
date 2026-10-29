# HRZ Platform UI — Spec 5: Concept Visual Fidelity Audit

**Status:** approved 2026-10-29, amended the same day for venue tiers and for
concepts that are series rather than events
**Program:** platform UI redesign (Specs 1–4 delivered the frame, the panel
system and the Indicators face); this spec delivers the drawing contract for the
182 registered concepts
**Quality bar:** a trader who has used TradingView, LuxAlgo or MetaTrader should
need zero explanation to read a pattern on our chart

## Purpose

Our concepts are correct as definitions and wrong as pictures. A break of
structure draws as a word floating where a line should be — the event never
carries the swing's bar, so there is no segment to run rightward until price
returns to it, which is what every platform a trader has used draws. A triangle
draws as a flat band, when the shape a trader
looks for is two converging lines. A session draws as a price band, when a
session is a *when*, not a *where*. An open fair value gap draws as a one-bar
sliver, and a filled one as a band across a third of the chart. And a concept whose whole value is a number — a deviation of +2.7σ and
its historical percentile — is routed to the same text mark as a doji.

The reason is not styling. It is that all 182 concepts are forced through three
drawing shapes, and most of what the references draw cannot be said in three
shapes. This spec produces the contract that fixes that: for every concept, how
it is drawn and which bars it fires on in the wider world, cited; how we draw it
and which bars we fire on today, rendered from our own engine; and the verdict
that connects the two.

The audit is also the prerequisite for the platform's stated goal — a trader
adjusting a concept's rule and combining concepts into an indicator. That goal
needs each concept's rule written down as data rather than compiled as a Rust
function, which is exactly what this audit produces as a by-product. The
architecture for adjusted and combined concepts is deliberately **not** designed
here; see [Out of scope](#out-of-scope).

## What this is, and what it is not

| This spec does | This spec does not |
| --- | --- |
| Establish the canonical rendering of every concept family, with a citation per claim | Change any detector's rule |
| Extend the concept drawing vocabulary so the canonical rendering is expressible | Design the concept-state or composition architecture |
| Record, per concept, its detection rule stated as data | Redesign the Studio/Pine-lite scripting path |
| Record semantic mismatches as findings with a named owner | Fix the mismatches — that is a later, separate decision per family |
| Say which venue tier each concept honestly needs, as its rule's named inputs | Build the derived variant, its provenance field, or the channels it would need |
| Render our current output honestly, defects included | Fix the defects before photographing them |

## The finding that shapes everything

`ConceptShape` — the vocabulary the shell uses to tell the engine what a concept
drew — has exactly three members ([scene.rs:2847](../../../frontend/chart-engine/src/scene.rs),
built by `conceptShapes()` at
[app.js:6050](../../../frontend/app/app.js)):

- **`Zone`** — a rectangle from `from_ns` to `to_ns` between `low` and `high`.
- **`Segment`** — a horizontal level from `from_ns` to `to_ns` at `price`.
- **`CandleMark`** — a word at a bar, above or below it.

Every one of the 182 concepts is routed into one of those three, and both of the
spatial ones are bounded by their own two bars. The engine's own documentation
asserts the opposite of what the code turns out to mean:

> "This is a price band over a span of time, which is the shape every one of
> those concepts shares, and the reason a new concept needs no new drawing code."

The claim was right about bands and wrong about concepts. What the references
draw, and what we cannot express today:

| Needed by | Concept count | What the reference draws | What we draw |
| --- | --- | --- | --- |
| Structure and liquidity levels | 36 (SMC, Supply & Demand) | The broken level running **rightward until mitigated**, dashed, labelled at the break | A 1.3px stub and a floating word: `bars` holds the break bar twice, so there is no swing to start from |
| Imbalance and block zones | included above | A box that **lives until filled**, with mitigation shading | A live box ending on its own creation bar; a filled one stretched over a median of 177 bars |
| Classical patterns | 15 (Chart Patterns) | An **outline traced through the pivots** plus a dashed neckline and a projected target | A flat band between two prices |
| Harmonics | 9 | The **XABCD polyline**, legs named, often filled | A band, or a mark |
| Sessions | 14 | A **full-height vertical box** over a time range | A price band |
| Fibonacci | 6 | A **ladder of named ratios** across the impulse, extended | Six anonymous segments |
| Candlestick patterns | 34 | A **marker at the bar** — arrow, dot — with the name optional | A text label |
| Volume profile | 9 | POC line, value-area band, node bands, all labelled | Close, by accident |
| Economic releases | 6 | A vertical event marker with the figure on it | A mark |

The encouraging half: the engine already draws far more than the concept path can
ask for. `ScriptDraw` — the vocabulary the scripting path uses — has
`Line { extend: "right" | "left" | "both" }`, `Polyline`, `Polygon`, `Band`,
`Table` and more
([script.rs:67](../../../frontend/chart-engine/src/script.rs)). Most of the work
is wiring the concept path to shapes the engine already knows how to draw, not
writing a new renderer.

## The four kinds of thing a concept can be

The rows of the table above are not all the same kind of object, and `ConceptShape`
answers only *where on the chart* — never what the concept is. The vocabulary
chapter therefore fixes a second axis, and every ledger row carries it:

| `kind` | What the concept is | Drawn as | Families |
| --- | --- | --- | --- |
| `event` | Fires on a set of bars | A mark, a segment, a box | SMC, Price Action, Candlesticks, Economic Releases |
| `series` | One value per bar, over a scale | A line, a band — on the price scale or its own | Trend, Momentum, Volatility, Volume, GARCH, Kalman |
| `interval` | A state held across time, whatever price does | A full-height box with a label | Sessions, HMM regime, changepoint |
| `readout` | A number the trader reads | The figure, and the reference it is unusual against | `indicators.zscore` today; every score the engines beside the chart will want |

Without it, pass 11 does not audit ten statistical concepts, it forces them into an
event-shaped schema and calls that an answer. Three of the ledger's fields are
malformed for anything but an event: `fires` has no honest value for a series —
every bar has a number — `target_shape` cannot tell a session box from a regime
box when both are "a box over time", and `draw` cannot judge a quantity that is
not geometry at all.

Two consequences, both cheap and both load-bearing later:

- **The kinds reuse each other, which is a third reason the passes are ordered as
  they are.** A regime interval is exactly the pass-4 session shape, and a
  percentile band is a pass-5 ladder with two rungs named. The audit gets those
  for free if the shapes are defined by their kind rather than by the family that
  first asked for them.
- **A readout is arithmetic, and arithmetic stays in Rust.** The scene already
  formats its own tooltips for this reason — `SceneRegion.label` arrives reading
  `"bullish gap (79% mitigated)"`, and `docs/14` forbids arithmetic over market
  data in the shell outright
  ([scene.rs:786](../../../frontend/chart-engine/src/scene.rs),
  [14-FRONTEND-CHART-ENGINE.md:53](../../../docs/14-FRONTEND-CHART-ENGINE.md)).
  So a z-score concept must ship `+2.7σ` and `99.1st percentile` already formed,
  with the baseline window that produced the percentile named in the row — not
  have the shell derive either from a number it was handed.

## Ground truth: three concept systems that do not know about each other

This is a finding, not a task. It is recorded here because the audit's
rule-as-data column is the raw material for resolving it, and because a reader
of the audit will otherwise ask why parameters exist but do nothing.

1. **The registry** — `ConceptSpec` in
   [concepts.rs:97](../../../crates/market-data/src/concepts.rs). 182 entries,
   each mapping to engine `EventKind`s. Detection is
   `analytics_core::events::detect_events`, a Rust function. This is what the
   Indicators panel lists and the chart draws.
2. **The band DSL** — `Concept` in
   [analytics-core/src/concepts.rs:423](../../../crates/analytics-core/src/concepts.rs):
   a 2-to-8-bar candle window with selector comparisons, shape `Band | Trendline`.
   Consumed by the chart engine's indicator path, `indicator_preview` and the
   strategy runtime. Shares no ids and no definition with the registry.
3. **The composition** — `Composition` in
   [composer.rs:26](../../../crates/market-data/src/composer.rs): a real
   condition tree (`All`/`Any`/`Not`/`Sequence`/`Retrace`/`PriceVs`) over registry
   concepts with per-leaf symbol, timeframe, `lookback_bars` and
   `min_bars_since`. Persisted, backtested, bot-runnable, AI-generatable. It
   emits `Signal { bar, long, stop_level, evidence }` — a trade, not a drawing.

**Parameters are declared, not honoured.** 156 of 182 concepts declare no
parameters at all; the entire catalog contains two parameter names
(`swing_lookback`, `spike_multiple`). `/concept-events/scan` constructs
`EventConfig::default()` and its query struct has no parameter field, so **no
request a trader can make changes what a concept detects on the chart**. The one
wired path flattens per-concept parameters into one global config, so two
concepts asking for different lookbacks collapse last-write-wins. The knob that
actually gates FVG detection, `fvg_min_fraction`, is honoured by
`event_config_for` but declared by no concept, so `validate()` rejects it. The
catalog's declared default for `swing_lookback` is 10.0; the engine's real
default is 3.

## Venue tiers: one concept, and the honest weaker input

The mechanism for "this instrument cannot support this concept" already exists, and
the audit must not fight it. `DataChannels` is a bitset whose stated rule is
*"availability is set intersection, never a guess"*
([broker.rs:25](../../../crates/market-data/src/broker.rs)), every `ConceptSpec`
declares `required_channels` ([concepts.rs:111](../../../crates/market-data/src/concepts.rs)),
and the products say what they can serve — Deriv synthetics declare candles and tick
quotes and nothing else, with the comment naming the exact category error the
taxonomy exists to prevent: no trades, no aggressor side, no book, no macro
([broker.rs:193](../../../crates/market-data/src/broker.rs)).

Three rules follow, and they are the whole of what this spec says about venues.

**The drawing is tier-independent.** Absorption measured from aggressor-tagged prints
and absorption inferred from tick pressure draw *identically* — same mark, same box,
same label position. If a shape in the vocabulary chapter ever needs to know which
tier fed it, the vocabulary is wrong. What differs between tiers is the provenance
beside the drawing, and the strength a trader should lend it, which is a claim about
the evidence rather than about the geometry.

**Do not split the id.** A per-venue `absorption.binance` beside
`absorption.deriv` makes two kinds of evidence out of one, and the composition engine
that consumes concepts by id ([composer.rs:26](../../../crates/market-data/src/composer.rs))
would then break the moment a trader moves venue — a methodology written as
"absorption plus a fair value gap" would stop meaning anything. One concept, one id,
its input resolved per product.

**The audit stays single-tier, and says so.** A tier is recorded at *family* level, and
a family earns a second `fires` row only where the weaker input changes **which bars
fire** — order flow and volume are the two that will. The reason is honest rather than
lazy: the Deriv data plane is declared for a later phase, so a per-venue visual audit
today would photograph a venue that serves no data at all.

**Not this spec's, and recorded so the next one inherits it.** The third state between
*offered* and *hidden* — the derived variant itself, the source and quality that must
ride on every event so inferred data is never read as measured (no such field exists
on the event path today), the options-chain and positioning channels that GEX, DEX,
Vanna and COT would need, and the twelve analytical engines their own roadmap names —
is the concept-state design. This audit's contribution to it is only that its
`rule_as_data` column names each rule's inputs, because a rule that does not say what
data it consumes is not yet data.

## Deliverables

Three artifacts, produced together, reviewed as one.

**1. The audit document** —
`docs/superpowers/specs/2026-10-29-concept-visual-fidelity-audit.md`. One
vocabulary chapter followed by twelve family passes (the order below). This is
the human-readable contract.

**2. The ledger** — `reports/concept-visual-audit/ledger/<family>.json`, one shard
per family and one row per concept, *seeded* from `GET /concepts` rather than
typed: every judgement field starts empty, and the gate fails a row that is
still empty. That is what makes completeness checkable rather than assertable --
a concept registered tomorrow appears as a row nobody has judged, so a pass
cannot quietly end with a family unexamined. Committed.

**3. The renders** — `reports/concept-visual-audit/<family>/<concept-id>.html`, one
page per concept, from the real wasm and the app's own painter (the page inlines
`concept_shapes.js` from disk rather than reimplementing it, so the tool cannot
draw a concept better than the chart can). Our *actual current output*, so every
section of the audit shows what a trader sees now next to what the reference says
it should look like. A written description of a wrong picture is arguable; the
picture is not. Each page embeds a real 500-bar scene and weighs ~250 KB, so the
pages are working artifacts and are gitignored like `/test-shots/`: what a picture
showed is carried into the committed record as the row's `ours.today` and the
section's measurements, and `node tools/concept_visual_audit.mjs render <family>`
rebuilds any of them on demand.

### Prerequisite: the preview tool cannot see the concept path

`tools/chart_preview.mjs` calls the real wasm and writes a self-contained page
that draws a scene with `app.js`'s own rules — but its request sends
`zones: true` and one built-in `concepts` document. The real chart sends
`concept_shapes` ([app.js:3418](../../../frontend/app/app.js)). Until the tool
accepts that field, the audit can describe our rendering but cannot photograph
it. Extending it is the first task, and it is also the tool the build phase uses
to verify itself.

## Method: twelve passes, ordered by shared drawing need

Not by family size. Each pass is placed where the vocabulary it forces will be
reused by the passes after it.

| # | Families | Concepts | Forces |
| --- | --- | --- | --- |
| 1 | Smart Money Concepts | 32 | Levels and boxes that run until mitigated — the largest gap, and the one most families after it reuses |
| 2 | Supply & Demand | 4 | Same vocabulary as pass 1; nearly free, and it tests whether pass 1 generalised instead of special-casing SMC |
| 3 | Price Action | 11 | Breaks and retests; validates the SMC shapes outside the SMC family |
| 4 | Sessions | 14 | The full-height vertical time box — a shape we do not have at all |
| 5 | Fibonacci | 6 | The named ladder drawn across an impulse |
| 6 | Chart Patterns | 15 | Polyline outlines traced through pivots, dashed neckline, projected target — the hardest geometry |
| 7 | Harmonics | 9 | XABCD outline; reuses pass 6's machinery |
| 8 | Candlestick Patterns | 34 | Markers rather than words; largest count, smallest per-concept work |
| 9 | Volume Profile | 9 | POC/VAH/VAL/HVN/LVN — the engine already has `Level` and `ProfileBar` |
| 10 | Order Flow, Trend, Momentum, Volatility, Volume | 32 | The overlay and series path, checked against TradingView's built-ins |
| 11 | Statistical | 10 | No external convention exists; house style under the bar below |
| 12 | Economic Releases | 6 | Vertical event markers, against the earnings-marker convention |

Pass 11 has a dependency the table's order hides. Its concepts are all
`series` or `readout`, so its vocabulary is pass 10's — and its rows, unlike every
other pass's, **cannot be built when they are written**: GARCH without ω, α and β and
a z-score without the window that made its percentile are not the concepts their
names claim. Pass 11's rows are therefore complete on the day they are written and
gated on the parameter path being honoured, which the audit records but does not fix.

### Inside a pass

1. Establish the reference for the family and **cite it** — the platform's own
   documentation, the named book, or the convention's author.
2. Render our current output for every concept in the family with the extended
   preview tool.
3. Walk the concepts one at a time: `draw` verdict, `fires` verdict, rule stated
   as data, target shape, detector named.
4. Record the shape-vocabulary additions the pass required, and whether they are
   new rendering or new wiring.
5. Append the ledger rows. A pass is not done until its rows are complete.

## The ledger row

Every one of the 182 concepts gets one row. The fields, and why each exists:

| Field | Content |
| --- | --- |
| `concept_id`, `family`, `version` | Identity, straight from the registry |
| `kind` | `event` / `series` / `interval` / `readout` — which of the four the concept is, before anyone asks how it draws |
| `reference` | `{ source, url, quote }` — the cited convention. Required, or `no_convention` with a reason |
| `draw` | `{ today, canonical, verdict }` where verdict is `matches` / `differs_minor` / `differs_major` / `impossible_in_vocabulary` |
| `fires` | `{ our_rule, their_rule, verdict }` where verdict is `same_bars` / `different_rule` / `no_external_rule` / `continuous` — `continuous` is the honest answer for a series, and is *not* available for a row whose `kind` is `event` |
| `rule_as_data` | The rule stated declaratively, in whichever of the two forms the concept actually is: a **predicate** (the swing definition, the confirmation, the lifecycle) or a **recursion** (the estimator, its named inputs, its warm-up). Both name their **inputs**, and an input drawn from the venue rather than computed from candles cites its `DataChannels` bit — `TRADES_AGGRESSOR`, not "the tape". The raw material for adjusted and combined concepts |
| `target_shape` | Which vocabulary item the canonical drawing needs, once the vocabulary is extended. For a `series` it must also say whether that item belongs on the price scale or its own pane, and that answer is **read from the registry's existing `pane` and `overlay` booleans** ([concepts.rs:115](../../../crates/market-data/src/concepts.rs)) rather than invented in the audit — where the two disagree, the row records the disagreement as a defect |
| `warmup` | The bars the rule needs before it can honestly say anything, and what draws before then. The reference convention is nothing, and a concept that plots a confident wrong value for its first twenty bars teaches a trader to distrust it |
| `detector` | The Rust function whose output would have to change if `fires` is not `same_bars` |
| `parameters` | `{ declared, honoured }` — so the parameter gap is counted per concept instead of being asserted in prose |
| `render` | Path to the HTML that shows our current output for this concept |

Two verdicts, not one, because a concept can look exactly right and mark the
wrong bars, or mark the right bars in a shape nobody recognises. Both failures
confuse a new trader, and they are fixed in different places.

## Citation standard

Every claim about how a pattern should look carries a source. The reference set,
in the order a conflict between them resolves:

- **TradingView** built-in indicators and their documentation — the screen most
  of our users have in their head.
- **LuxAlgo**, whose Smart Money Concepts indicator is the de-facto rendering
  standard for structure, liquidity and imbalance.
- **Named literature** where the platforms are silent or inconsistent: Edwards
  & Magee and Bulkowski for classical patterns, Scott Carney for harmonics,
  Steidlmayer and the CBT convention for volume profile, the ICT corpus for
  smart-money terminology.
- **Order-flow terminals** — Bookmap, Sierra Chart, ATAS — for the footprint and
  delta families.

A citation is a source a trader can open and check. "Commonly drawn as" is not a
citation. Where two authorities disagree, the row records both and the audit
picks the one a TradingView user would expect, because the goal is recognition
rather than correctness in the abstract.

## Where no convention exists

Roughly eight concepts — the Kalman filter, HMM regime, changepoint, entropy,
Bayesian drift, GARCH, AR forecast, pair spread — have no charting platform to copy,
because what they estimate is a hidden state rather than a price. Do not read that as
*all* of pass 11 being orphaned: a deviation expressed in standard deviations, and the
percentile of that deviation, have long conventions in statistics and a familiar home
on a chart — a line on its **own** scale, and the number beside it. The house style
governs how something is presented; it does not license inventing a fifth `kind`.

For the orphans the audit defines a house style under a stated bar:
**legible, smooth, low visual noise, and quiet by default.** The rule for the
house style is that it must be *explainable in one line on the card* — a
concept whose drawing cannot be described to a trader in the sentence the card
already has is a concept that will not be understood, however correct it is.

These rows carry `fires: no_external_rule` and a `reference` of `house_style`
with the reasoning, so they are visibly a different kind of decision rather than
smuggled in beside the cited ones.

## Coupling: the panel's card previews

The Indicators face draws a live miniature of the pane's last 96 bars on every
card, with the concept's marks on it (Spec 1 §23). That preview is a second
renderer for the same shapes, written in `app.js` (`previewCandles`,
`overlayPrices`, `eventPrices`). **Any change to how a concept draws on the chart
must land in the card preview too**, or the panel will show a picture that
contradicts the chart it is describing. Each pass therefore has two render
targets, and the ledger's `render` field records both.

## Verification

- **Ledger completeness gate** — a tool that fails if any of the 182 rows is
  missing a `kind`, a reference, a `draw` verdict, a `fires` verdict, a
  `rule_as_data` statement, a `warmup` answer or a render path — and fails a row
  whose `fires` is `continuous` while its `kind` is `event`, because that is the
  audit answering the wrong question. This is what makes "every concept is cited" a
  checked claim rather than a promise.
- **Render diff** — each pass's HTML is re-rendered after the build phase and
  compared against the reference description in the same section.
- **Existing gates** — `tools/wasm_abi_check.mjs`, `tools/shell_check.mjs`,
  `tools/ui_check.mjs`, `tools/component_check.mjs` and `cargo test` for
  `analytics-core` and the gateway must pass at the end of every pass.
- **The chart itself** — a pass is not signed off from a JSON payload. The
  gateway runs on :8080 and every family is looked at on a real chart before its
  rows are called done.

## Known defects carried deliberately

These are found, not invented, and the audit photographs them before any of them
is fixed.

- [app.js:6066](../../../frontend/app/app.js) reads
  `const end = e.status === "forming" || e.status === "confirmed" ? toNs : toNs`
  — both branches identical. An open zone therefore ends on the bar that created
  it. Fixing it first would let the audit photograph its own correction instead
  of what a trader sees now, so it is fixed in pass 1's build, not before the
  audit.
- **Measured on the first render, and worse than the bullet above reads.** On
  `BTCUSDT 15m` over 501 bars, `smc.fvg` answers 50 events: the 7 still `forming`
  do draw as the one-bar sliver this bullet predicts (median span 1 bar), but the
  43 `expired` ones span a median of **177 bars** and stack into a wall of
  full-width bands over the right third of the chart. The dead branch is therefore
  not one defect but two directions of one mistake: a live zone that should reach
  the newest bar stops at its own, and a closed zone that should stop at its
  mitigation bar is drawn from its origin all the way to it. Neither is the box a
  trader reads as an imbalance. The same shape appears in `smc.ifvg` (median 120
  bars) and `smc.order_block` (29), so it is a lifecycle rule, not an FVG bug.
- **A structure segment has no start, so it draws as a dot.** `smc.bos`, `smc.choch`
  and `smc.mss` carry `bars: [t, t]` — every one of the 18, 19 and 14 events in the
  window has `bars[0] == event_time_ns`. The router reads `bars[0]` as the swing and
  `event_time_ns` as the break, so both are the break bar, and the engine's
  zero-length floor (`bar_w * 0.6`) draws a 1.3px stub at 15m. The photograph of
  `smc.bos` is the word "bos" printed in the air with no line under it. The earlier
  claim in this spec — that a break "draws as a line that stops at the bar which
  broke it" — was too generous: there is no line to stop anywhere, because the
  detector never puts the swing's bar in the event. Fixing the picture needs the
  swing timestamp on the event, which is a detector change, not a rendering one, and
  is why `fires` is a separate verdict from `draw` in the ledger.
- The registry already says which scale a series concept belongs to — `pane: true` is a
  sub-pane below the chart, `overlay: true` is a line on the price frame
  ([concepts.rs:115](../../../crates/market-data/src/concepts.rs)) — and the concept
  drawing path reads **neither**. `conceptShapes()` routes every `indicators.*` id into
  a text mark on the price pane ([app.js:6076](../../../frontend/app/app.js)) while a
  separate series path exists beside it. So a concept whose whole meaning is a number on
  its own scale arrives as a word sitting on a candle. The audit's `target_shape` column
  is where that contradiction gets written down per row.
- The catalog's declared parameter defaults disagree with the engine's real
  ones, so the panel can show a default that nothing honours.
- 156 of 182 concepts declare no parameters, so "adjust this concept" is not yet
  a question the catalog can answer for most of the library.

## Out of scope

- **The concept-state architecture.** Initial, adjusted and combined states need
  one concept definition that the registry, a variant and a composition all read
  as data. That is a design of its own, and designing it before 182 rules are
  written down would be designing a container without knowing what has to fit in
  it. This spec supplies the contents; the next one designs the container.
- **The analytical engines beside the chart.** The volatility, regime, tail-risk,
  positioning and options engines the platform's roadmap names are consumers of these
  drawings, not more drawings: they are computed from the same candle and tick data and
  reported as evidence to a reasoning layer. Two of their needs are visible from here and
  deliberately left to that design — a provenance and quality field on the event, so a
  derived figure is never read as a measured one, and the data channels an options chain
  or a positioning report would require, which no product declares today. What this audit
  owes them is one thing only: a vocabulary chapter that names no shape assuming a book,
  a tape or a chain, since those would be unusable wherever the tier is derived.
- **Changing detection semantics.** The `fires` verdict records where we and the
  reference disagree about which bars a pattern is on. Acting on that record is a
  per-family decision with backtest consequences, not a rendering fix.
- **The Studio and Pine-lite scripting path**, except where the vocabulary work
  gives it shapes for free.
- **Community-authored indicators**, except that a published indicator will
  eventually want this same drawing contract.

## Definition of done

1. `tools/chart_preview.mjs` renders `concept_shapes`, so our current output is
   photographable.
2. The audit document has a vocabulary chapter stating the four kinds, the rule that
   a shape never learns which data tier fed it, and the warm-up contract — followed by
   twelve family passes, each with its reference cited, our render attached, and every
   concept in it given a kind and both verdicts.
3. The ledger has 182 complete rows and the completeness gate passes, including the
   `continuous`-on-`event` guard.
4. The shape-vocabulary additions required by the audit are specified well enough
   to implement, and the ones that are pure wiring rather than new rendering are
   marked as such.
5. The three-systems finding and the parameter gap are written down with their
   evidence, and left alone.

## Risks

| Risk | Mitigation |
| --- | --- |
| Twelve passes is a long runway before anything looks better to a user | Pass 1 is first, and the build slice cut from its verdicts — the run-until-mitigated level and box — is the one that most affects the chart; the audit does not have to finish before the build starts |
| Citations inflate the work — a doji mark does not need a bibliography | The citation standard is per *claim*, not per word: one reference per family plus a citation wherever a row contradicts the family default |
| The reference platforms change their rendering | Every row records the date it was checked and the exact URL |
| Rule-as-data drifts from the Rust detector it describes | Each row names the detector function, so a later diff can find the pair |
| The audit becomes a document nobody implements | Every row's `target_shape` is a work item with a verdict attached, and the completeness gate fails on gaps |
| The four kinds grow a fifth and sixth by pass 12, because a hard concept makes a tempting exception | The kinds are declared once, in the vocabulary chapter, and the completeness gate rejects a value it was not told about — so a new kind is a decision made in the open rather than a row quietly typed |
