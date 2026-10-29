# HRZ Platform UI — Spec 5: Concept Visual Fidelity Audit

**Status:** design approved; awaiting spec review
**Program:** platform UI redesign (Specs 1–4 delivered the frame, the panel
system and the Indicators face); this spec delivers the drawing contract for the
182 registered concepts
**Quality bar:** a trader who has used TradingView, LuxAlgo or MetaTrader should
need zero explanation to read a pattern on our chart

## Purpose

Our concepts are correct as definitions and wrong as pictures. A break of
structure draws as a line that stops at the bar which broke it, when every
platform a trader has used draws the broken level running to the right until
price returns to it. A triangle draws as a flat band, when the shape a trader
looks for is two converging lines. A session draws as a price band, when a
session is a *when*, not a *where*. An open fair value gap draws as a one-bar
sliver.

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
| Structure and liquidity levels | 36 (SMC, Supply & Demand) | The broken level running **rightward until mitigated**, dashed, labelled at the break | A segment that stops at the break bar |
| Imbalance and block zones | included above | A box that **lives until filled**, with mitigation shading | A box ending on its own creation bar |
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

## Deliverables

Three artifacts, produced together, reviewed as one.

**1. The audit document** —
`docs/superpowers/specs/2026-10-29-concept-visual-fidelity-audit.md`. One
vocabulary chapter followed by twelve family passes (the order below). This is
the human-readable contract.

**2. The ledger** — `reports/concept-visual-audit/ledger.json`. One row per
concept, machine-readable, appended as each pass lands. This is what makes
completeness checkable rather than assertable.

**3. The renders** — `reports/concept-visual-audit/<family>.html`. Our *actual
current output* per family, produced by the real engine, so every section of the
audit shows what a trader sees now next to what the reference says it should
look like. A written description of a wrong picture is arguable; the picture is
not.

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
| `reference` | `{ source, url, quote }` — the cited convention. Required, or `no_convention` with a reason |
| `draw` | `{ today, canonical, verdict }` where verdict is `matches` / `differs_minor` / `differs_major` / `impossible_in_vocabulary` |
| `fires` | `{ our_rule, their_rule, verdict }` where verdict is `same_bars` / `different_rule` / `no_external_rule` |
| `rule_as_data` | The detection rule stated declaratively — the swing definition, the confirmation, the lifecycle. The raw material for adjusted and combined concepts |
| `target_shape` | Which vocabulary item the canonical drawing needs, once the vocabulary is extended |
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

Roughly ten concepts — the Kalman filter, HMM regime, changepoint, entropy,
Bayesian drift, GARCH, AR forecast, pair spread z-score — have no charting
platform to copy. For those the audit defines a house style under a stated bar:
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
  missing a reference, a `draw` verdict, a `fires` verdict, a `rule_as_data`
  statement or a render path. This is what makes "every concept is cited" a
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
2. The audit document has a vocabulary chapter and twelve family passes, each
   with its reference cited, our render attached, and every concept in it given
   both verdicts.
3. The ledger has 182 complete rows and the completeness gate passes.
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
