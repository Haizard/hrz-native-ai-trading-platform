# Concept state: one definition, three states, provenance on every event

Status: design. Decides the container the visual-fidelity audit deliberately left out
([2026-10-29 audit design, "Not this spec's, and recorded so the next one inherits it"](2026-10-29-concept-visual-fidelity-audit-design.md)).
Nothing here is implemented; §9 is the increment order, and §10 is what a reviewer
should push back on.

## 1. What this decides, and what it does not

| In scope | Out of scope |
| --- | --- |
| One concept definition that the registry, a variant and a composition all read as data | Any new rendering shape — the vocabulary is closed at 17 ([`00-vocabulary.md`](concept-visual-audit/00-vocabulary.md)) |
| The three states a concept can be in: **initial**, **adjusted**, **combined** | Per-venue concept ids. The audit's rule stands: *do not split the id* |
| `DataProvenance` on every event, so derived input is never read as measured | The options/positioning data plane itself (GEX, DEX, Vanna, COT), which needs channels that do not exist yet |
| Honouring parameters on the lane that does not honour them today: the indicator series | A rule interpreter. `rule_as_data` is a *declaration*, not a program |
| How a composition's leaf resolves the same definition the chart resolves | Confidence/backtest statistics — §8 says where they will hang, not what they will say |

**The one-sentence thesis.** A concept is a rule over named inputs; the registry, the
trader's knobs, the venue's data tier and the composition's leaf are four ways of
*resolving* that rule, and every event it produces must carry the answer to "what
actually fed you".

## 2. The three states

**Initial** — the concept as registered: id, version, its rule, its named inputs, its
declared parameters at their defaults, its presentation. This is the only state that
exists today, and it exists twice over in two unrelated shapes (§3).

**Adjusted** — the same id, resolved against something. Two independent kinds of
adjustment, which must not be conflated because they have different authors:

- *Trader-adjusted*: the knobs the trader turned (`{"smc.bos": {"swing_lookback": 5}}`).
  Already real for the event lane: `DetectionPlan` groups concepts by the config each
  one's own parameter map asks for
  ([`composer.rs:713`](../../../crates/market-data/src/composer.rs)), and `/concept-events/scan`
  parses and validates the overrides against the catalog's declarations
  ([`concept_routes.rs:218`](../../../crates/api-gateway/src/concept_routes.rs)).
- *Venue-adjusted*: the input the product can actually serve — aggressor-tagged prints
  where they exist, tick-derived pressure where they do not. Not real at all today,
  and this is the state the roadmap turns on.

**Combined** — a `Composition`: a condition tree over ids with per-leaf symbol,
timeframe, lookback and delay
([`composer.rs:26`](../../../crates/market-data/src/composer.rs)). It emits `Signal { bar,
long, stop_level, evidence }` ([`composer.rs:863`](../../../crates/market-data/src/composer.rs))
— and `evidence` is a `Vec<String>` of concept ids. A trade justified by a list of
names, with no levels, no directions, no bar times, and no answer to what fed each
name.

The three states are a chain, not three systems: `initial → adjusted → combined`, each
link reading the same definition and each link recording what it changed.

## 3. What exists today

Five pieces, none of which knows the others:

1. **The registry**: `ConceptSpec` — id, version, library, name, description,
   `parameters`, `required_channels`, `pane`, `overlay`, `event_kinds`
   ([`concepts.rs:97`](../../../crates/market-data/src/concepts.rs)). 182 entries. The
   rule itself is Rust inside `analytics_core::events::detect_events`; the spec can
   name the knobs but cannot state the rule.
2. **The band DSL**: `Concept` — a 2-to-8-bar candle window with `Selector` comparisons
   ([`analytics-core/src/concepts.rs:423`](../../../crates/analytics-core/src/concepts.rs),
   selectors at `:104`). Consumed by the engine's indicator path and the strategy
   runtime. Shares no ids and no definition with the registry.
3. **The composition** — §2. Resolves per-leaf parameters through `ParameterMap` and
   `validate_parameters` ([`composer.rs:703`](../../../crates/market-data/src/composer.rs),
   `:826`), which is the right machinery in the right place: it just reads the
   registry, not a definition.
4. **The event contract**: `ConceptEvent` — levels, zone, `evidence` JSON, `bars`
   ([`concepts.rs:156`](../../../crates/market-data/src/concepts.rs)) and, upstream,
   `MarketEvent` with `geometry`
   ([`events.rs:899`](../../../crates/analytics-core/src/events.rs)). Neither has a
   provenance field. The two-instrument case works around it by inventing an evidence
   key: `correlated_symbol` ([`concept_routes.rs:183`](../../../crates/api-gateway/src/concept_routes.rs)).
   The macro case works around it by merging a second lane into the candle replay
   ([`concept_routes.rs:288`](../../../crates/api-gateway/src/concept_routes.rs)) — an
   event whose input is a BLS/FRED release, indistinguishable on the wire from one
   whose input was the tape.
5. **The series lane**: `indicator_series(concept, candles)`
   ([`indicator_series.rs:133`](../../../crates/analytics-core/src/indicator_series.rs))
   — a `&str` match with every period and every colour literal inside
   (`RSI_BLUE` at `:129`, the ribbons' `period` loops from `:179`). No `EventConfig`,
   no `ParameterMap`, no channels, and the route's own query struct has no parameter
   field to read
   ([`concept_routes.rs:318`](../../../crates/api-gateway/src/concept_routes.rs)). So
   pane and overlay concepts — every indicator in the catalog — are **initial-state
   only**: the trader cannot adjust them, and the venue cannot resolve them, because
   nothing reads a declaration for them.

The shape vocabulary and the engine are not part of this problem and must stay out of
it: `shapeOf(id)` is the single routing table
([`concept_shapes.js:115`](../../../frontend/app/concept_shapes.js)) and the engine
lowers each shape once ([`scene.rs:3226`](../../../frontend/chart-engine/src/scene.rs)).
The audit's finding that a shape must never know its tier is a constraint on this
design, not a suggestion.

## 4. The container: `ConceptDefinition`

One record per concept id, read by all five consumers. Declared in Rust (a const
table beside the registry, not a runtime-loaded asset) for the same reason the
registry is: `validate()` and the audit gate can check a const at compile time and in
CI, and a JSON file drifts from the code it describes.

```rust
pub struct ConceptDefinition {
    pub identity: ConceptIdentity,      // id, version, library, name, description
    pub rule: ConceptRule,              // the audit's rule_as_data, typed
    pub inputs: Vec<InputRequirement>,  // named inputs, each with its fallback ladder
    pub parameters: &'static [ConceptParam],
    pub presentation: Presentation,     // target shape, pane/overlay, warm-up bars
    pub detection: DetectionRef,        // which Rust entry point runs this rule
}
```

**`ConceptRule`** mirrors the audit's `rule_as_data`, which already has two forms
across 182 rows:

```rust
pub enum ConceptRule {
    /// A condition over bars: swings, breaks, lifecycles. (event concepts)
    Predicate { states: &'static str, holds_when: &'static str },
    /// An estimator with a recursion and a warm-up. (series concepts)
    Recursion { estimator: &'static str, warm_up_bars: usize },
}
```

The strings are prose the audit already wrote — `rule_as_data.rule` in each ledger row
is the seed content, so the 182 rows become this table rather than being re-derived.
Prose in a declaration is honest as long as nothing executes it: §6 makes the prose
checkable by pairing it with the `DetectionRef` that implements it, and §7 makes a
mismatch a gate failure rather than a rumour.

**`InputRequirement`** is the piece that makes venue adjustment real:

```rust
pub struct InputRequirement {
    pub name: &'static str,                  // "closes", "tape", "book_depth"
    /// The preferred channel, and the ladder of acceptable stand-ins, best first.
    pub ladder: &'static [InputSource],
}

pub enum InputSource {
    FromChannel(DataChannels),               // CANDLES, TRADES_AGGRESSOR, DEPTH, ...
    FromSeries { concept: &'static str },    // a second instrument, or a derived series
    FromProvider { source: &'static str },   // "bls", "fred" — macro
}
```

The ladder is ordered and the resolution is the existing rule, not a new one:
availability is set intersection, never a guess
([`broker.rs:25`](../../../crates/market-data/src/broker.rs)). A concept with no
ladder entries beyond `CANDLES` — the overwhelming majority — resolves identically on
every product, which is what "one concept, one id, its input resolved per product"
means in code.

**`DetectionRef`** keeps the rule and the detector named together:

```rust
pub enum DetectionRef {
    Events(EventKind),                                    // the registry's existing mapping
    Series(fn(&[Candle], &SeriesConfig) -> Option<IndicatorSeries>),
    Provider,                                             // macro: the store answers, not a formula
    Pair(fn(&[Candle], &[Candle]) -> Vec<MarketEvent>),   // two instruments, SMT
    Composite(&'static str),                              // a composition id — §8
}
```

`Composite` is the only variant that makes a concept out of other concepts, and it is
the reason the combined state needs no fourth system: a saved composition can be
registered as a definition, its leaves resolved through the same table, and its
detection is the existing evaluator.

## 5. Provenance: the field that makes adjustment safe

Venue adjustment is only honest if the event says which rung of the ladder answered.
So every event carries it, from the first rung to the last:

```rust
pub struct DataProvenance {
    /// The source that actually fed this detection.
    pub source: InputSourceKind,     // Measured | Derived | Estimated | Provider
    /// What it was: "trades_aggressor", "tick_quotes", "candles", "bls".
    pub input: &'static str,
    /// A one-line note a trader can read: "not exchange order-flow delta".
    pub note: Option<&'static str>,
}
```

Rules, in order of how much they matter:

1. **The drawing never changes with provenance.** Same mark, same box, same label
   position. If a shape needs to know its provenance, the vocabulary is wrong — the
   audit's stated rule, and this spec's binding constraint.
2. **Every event carries it, or it says it does not know.** Absent provenance is a
   bug, not a state. `#[serde(default)]` on the wire keeps old clients parsing while
   the lanes fill in one at a time.
3. **The weakest rung in a multi-input rule is the event's rung.** An absorption that
   uses closes *and* the tape is `Derived` when the tape is missing, because the
   number a trader reads came from the weaker input.
4. **Provenance gates decisions, not pixels.** A bot's entry, the AI's evidence, and
   the confidence the roadmap wants may refuse `Estimated`; the chart always draws.
   This is where `quality` differs from a hidden feature — the user's stated rule is
   that a missing input yields *a variant that works on the data that exists*, never
   silence and never fabrication.
5. **Adjustment is provenance too.** A detection under the trader's knobs must record
   the resolved parameters, so a backtest and a chart agree about which BOS is which.
   `Provenance` therefore travels with a resolved `DetectionPlan`, and the plan is
   derivable from `(definition, ParameterMap, DataChannels)`.

Where it attaches: `MarketEvent` gains `provenance` beside `geometry`
([`events.rs:899`](../../../crates/analytics-core/src/events.rs)); `as_concept_event`
copies it onto `ConceptEvent` as a first-class field rather than an evidence key
([`concepts.rs:156`](../../../crates/market-data/src/concepts.rs)); the series response
gains it on `IndicatorSeries`
([`indicator_series.rs:106`](../../../crates/analytics-core/src/indicator_series.rs)).
`evidence` stays what it is: measured facts, kind-specific.

## 6. Making the definition the only source of truth

The registry, the scan route, the panel and the composer each currently hold a copy of
some answer. The container does not delete those copies; it makes them derived:

| Consumer | Today | After |
| --- | --- | --- |
| `concept_registry()` | the definition, incomplete | a projection of definitions; `ConceptSpec`'s fields become reads |
| `/concepts` | hand-copied fields ([`concept_routes.rs:42`](../../../crates/api-gateway/src/concept_routes.rs)) | serialized from the definition, incl. inputs and ladder |
| `/concept-events/scan` | `DetectionPlan::for_catalog` | `DetectionPlan::resolve(definitions, params, channels)` — same grouping trick, one more key |
| `/indicator-series` | fixed literals | `SeriesConfig` built from `parameters` + resolved inputs, then the same dispatcher |
| `Composer` leaves | registry defaults | the leaf's own `parameters` override the document's, which override the definition's |
| Indicators panel | name, description, params | adds the resolved input note, so a trader sees *why* a concept is `derived` here |
| `ConceptShapes.shapeOf` | its own table | unchanged, checked against `presentation.target_shape` by the gate |

The series lane is the only place where honouring parameters means new plumbing: a
`SeriesConfig { parameters: BTreeMap<String, f64>, channels: DataChannels }` passed to
the dispatcher, and each arm reading its period from the map with the literal as the
default. Existing behaviour is byte-identical when no override arrives — the same
back-compatibility rule the event lane already follows.

## 7. The band DSL, resolved rather than merged

The 2-to-8-bar window DSL is a real, useful, *authored* concept form: a community
indicator's shape is a window with selectors, and rewriting it as a registry concept
would either lose the expressiveness or fake it. The decision is therefore explicit
rather than opportunistic:

- The DSL keeps its own evaluation path and its own authors' ids (a script, not a
  catalog concept).
- A DSL concept that is *also* a catalog concept must declare which one is the
  definition and which is the rendering; the gate refuses a catalog id whose rule is
  stated in two places with different words.
- `rule_as_data` is how that refusal is checkable: a predicate whose declared inputs
  include something its detector cannot read, or a warm-up that contradicts the
  estimator's own first non-null bar, fails a test rather than a review comment.

## 8. The combined state, and where confidence hangs

A composition's leaf today asks "did a matching event occur within lookback". Under
this container it asks the same question of a *resolved* detection, which means three
additions and no restructuring:

1. **Leaf-level adjustment.** `Condition::Concept` already carries `symbol`,
   `timeframe`, `lookback_bars`, `min_bars_since`; it gains nothing, because
   `Composition.parameters` is already per-concept
   ([`composer.rs:51`](../../../crates/market-data/src/composer.rs)). What changes is
   that the resolution path is shared with the chart, so a backtest cannot silently
   replay a different config than the picture the trader wrote the strategy from.
2. **Evidence with content.** `Signal.evidence: Vec<String>` becomes a list of the
   satisfying events (id, direction, bar, level, provenance). A signal a chart can
   draw and an agent can cite verbatim is the platform's stated event contract
   ([`concepts.rs:150`](../../../crates/market-data/src/concepts.rs)) — the composition
   is currently the one consumer that breaks it.
3. **A registered composition is a definition.** `DetectionRef::Composite` (§4) lets a
   saved document appear in the catalog as one id with one drawing, which is how a
   methodology ("absorption plus a fair value gap") becomes something the panel can
   switch on rather than something only the bot knows.

Confidence — the roadmap's third standing concept, after evidence and inference — is
the next layer and gets its anchor here so it lands in the right place: a per-(concept,
venue, resolution) statistic keyed by the *resolved* plan, not by the bare id. Without
§5's provenance, two variants of one id would share one confidence number, which is
exactly the lie the design exists to prevent.

## 9. Increments

Each is independently shippable, each keeps the wire parseable by an old client, and
none touches the 17 shapes.

1. **`SeriesConfig` on the series lane** — declared parameters honoured for pane and
   overlay concepts: `/indicator-series` gains the same `params` query the scan route
   already parses and validates, and the dispatcher reads periods from it with the
   literal it uses today as the default. Smallest change, closes the last
   "parameters do nothing" gap.
2. **`provenance` on `MarketEvent` / `ConceptEvent` / `IndicatorSeries`** — additive,
   `#[serde(default)]`; the candle-only lanes set `FromChannel(CANDLES)` mechanically.
3. **Input resolution for the multi-input concepts** — SMT's `correlated_symbol`
   becomes an `InputRequirement` (`FromSeries`) instead of an evidence key; macro
   becomes `FromProvider { "bls" | "fred" }`.
4. **`ConceptDefinition` as the source**, `ConceptSpec` and `/concepts` as projections,
   seeded from the audit's 182 `rule_as_data` rows.
5. **Provenance on the composition path**: `Signal.evidence` with content; `validate()`
   extended to definitions.
6. **`DetectionRef::Composite`** — a saved composition as a catalog entry.
7. **Variant ladders where the audit named a second tier** — the `no_external_rule`
   rows and the two families the audit expected it for (order flow, volume).

Steps 1–3 change behaviour traders can see; 4–7 change what the platform can assert
about a signal. Doing 4 first would be the classic mistake — a schema with no lane
that reads it.

## 10. Decisions, with the alternative each rejected

1. **Definition lives in Rust, not a runtime asset.** Alternative: JSON in the repo.
   Rejected because the registry's whole honesty today comes from `validate()` and the
   gates being able to see the declarations next to the code.
2. **`rule_as_data` stays prose + a typed form.** Alternative: an interpreter.
   Rejected: 182 rules in a DSL is a second analytics engine, and every rule would
   then need a benchmark against the Rust it replaced.
3. **Provenance is a field, not an evidence key.** Alternative: keep stuffing
   `evidence`. Rejected: evidence is per-kind and unreadable by contract, so a bot
   that wants "never trust an estimated input" would have to know 182 key spellings.
4. **No venue ids.** Restated from the audit because it is the constraint most likely
   to be revisited under pressure: a per-venue id is one extra line in a registry and
   breaks every composition written against the bare one.
5. **The band DSL keeps its own path.** Alternative: fold it in. Rejected — it is a
   different artifact (an authored shape) with a different author, and this spec's job
   is to stop the two from disagreeing, not to make them the same thing.
6. **Adjusted has two authors, one state.** Venue-adjusted and trader-adjusted are
   recorded in the same resolved plan so a chart, a backtest and an agent can be shown
   the identical statement of "what this concept was measured from".

## 11. Verification

- **Gate: definition coverage.** Every registry id has a definition whose rule names
  its inputs; every input names a `DataChannels` bit or a provider that exists.
  Same shape as the audit's `check` and the routes gate
  ([`tools/concept_visual_audit.mjs`](../../../tools/concept_visual_audit.mjs)), which
  is the pattern that proved a vocabulary can be held id-for-id.
- **Gate: no provenance-less event.** A scan over a fixed window asserts every emitted
  event carries a source, and every multi-input concept asserts the weakest rung won.
- **Gate: parameters reach both lanes.** For each declared parameter, a request with an
  out-of-default value must change either the emitted events or the emitted series —
  which is the test the catalog failed for 156 concepts before the audit photographed it.
- **Test: plan equality.** Chart request and composition replay over the same window,
  same parameters, same product: identical resolved `DetectionPlan`. This is the test
  that makes §6's "shared resolution" claim checkable rather than aspirational.
- **Wasm contract:** a definition's `presentation.target_shape` must be a shape the
  engine can place, and the engine must draw it — the completeness gate the audit
  already runs refuses a registry kind that draws nothing.
