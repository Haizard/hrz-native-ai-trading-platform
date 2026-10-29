# Pass 1 — Smart Money Concepts (32 rows)

Judged 2026-10-29 against `main`. The machine-readable verdicts are the 32 rows of
[`ledger/smc.json`](../../../../reports/concept-visual-audit/ledger/smc.json);
`node tools/concept_visual_audit.mjs check --family smc` reports **32 rows, 0 incomplete**.
This file is the argument those rows are built on, and the template the next eleven passes
follow.

**Headline: not one of the 32 draws the way a trader has already seen it 10,000 times.**
`draw.verdict` splits 18 `impossible_in_vocabulary` / 10 `differs_major` / 4 `differs_minor`
/ 0 `matches`. Even the four `differs_minor` rows, where the geometry is close, still print
the detector's wire name.

## How it was measured

One window, one instrument, the route the chart itself uses:
`GET /concept-events/scan?symbol=BTCUSDT&timeframe=15m&limit=500` — 501 bars, 4,254 events
across 138 concept ids, of which 29 of the 32 smc ids fire at all.
`smc.silver_bullet`, `smc.trendline_sweep` and `smc.smt_divergence` return nothing.

Three caps in that route shape every number below, and every later pass inherits them
([concept_routes.rs:215-266](../../../../crates/api-gateway/src/concept_routes.rs#L215)):

- the replay starts at bar 22, so the first 22 bars of the chart can never carry an event;
- at most **50 events per concept** survive, newest first — so `n = 50` in a row means
  "saturated", not "there were exactly 50";
- the replay uses `EventConfig::default()`
  ([concept_routes.rs:238](../../../../crates/api-gateway/src/concept_routes.rs#L238)),
  which sets `structure_lookback: 3`
  ([events.rs:926](../../../../crates/analytics-core/src/events.rs#L926)) and is never
  given the concept's own parameters.

`BINANCE:BTCUSDT` returns zero events; the broker-prefixed symbol is not what the candle
window is keyed on. Passes 2-12 use bare `BTCUSDT`.

The visual claims come from the per-concept pages `node tools/concept_visual_audit.mjs
render smc` builds with the real wasm and the real router. Those pages are working
artifacts — everything under `/reports/concept-visual-audit/*/` is ignored except `ledger/` —
and each row's `render` field points at its page so rule 7 can prove the row was judged from a
picture that exists. They were confirmed in the browser at
`http://127.0.0.1:8080/#/charts` (cache-busted) with six smc concepts switched on:
[the panel](evidence/pass1-panel.png), [the chart](evidence/pass1-chart.png) and
[one BOS beside one FVG](evidence/pass1-fvg-bos.png) are committed here, because they are
findings rather than artifacts.

## What the reference set says

Citations are per row in the ledger; the set is LuxAlgo's price-action-concept docs
(market-structures, imbalances, order-blocks, liquidity, pdzones, settings), the LuxAlgo
library pages, and the TradingView *Smart Money Concepts (SMC)* script page, which is the
de-facto rendering. Four rows state `no_convention` with a reason rather than borrow a
citation they do not have: `smc.rejection_block`, `smc.internal_range_liquidity`,
`smc.market_maker_model`, `smc.smt_divergence`.

Across that set, three things are consistent about how SMC is drawn:

1. **A level is a line, not a point.** BOS, CHoCH, MSS, inducement, equal highs/lows and
   the range's BSL/SSL are all drawn *at* a price, across the bars that follow, with one
   label. LuxAlgo additionally splits the line style by class — *"The toolkit highlights
   internal structure with dashed lines and labels with a smaller text size"* — so the
   picture encodes which structure broke, and the label is the short form (BOS, CHoCH, IDM,
   BSL, SSL, EQH/EQL), never the detector's name.
2. **A zone has a life.** *"[Order blocks] are automatically highlighted by the toolkit and
   will disappear once mitigated"*; for imbalances, *"Once price breaks a highlighted
   imbalance area it is said to have been 'mitigated' and will automatically disappear"*.
   A failed zone is redrawn as a different thing — a breaker is *"differentiated from
   Volumetric Order Blocks by their non-solid background color"*, an inverse FVG is
   *"essentially mitigated fair value gaps"* — so the chart tells the story of the zone that
   did not hold.
3. **The clock is drawn as the clock.** The Silver Bullet is *"an ICT setup defined by the
   clock: a specific one-hour window"*; killzones and the Asian range are full-height time
   regions. Nothing in the reference set draws a session as a price band.

### One correction to the spec

The spec asserted the canonical for a broken level is *"runs rightward until mitigated"*.
**That is not in the reference set, and this pass does not adopt it.** The four concept docs
pages contain no use of the word "extend" at all; the only documented right edge is on the
settings page — *"`Extend Imbalance` — Numerical, default: 10 — Extend the imbalance area to
the right by the selected amount of bars"* — which is a bar count, and is per-concept rather
than global. Mitigation is documented as a *removal* rule (`Mitigation Method`: Close / Wick
/ Average), and `Show Last` default 5 caps how many order blocks are on the chart at once.

So `level_run` requires the run but not its length, and the length is a parameter with a
default — the same shape LuxAlgo exposes. Seven rows were rewritten for asserting more than
the citation supports; the shape definitions in
[00-vocabulary.md](00-vocabulary.md) carry the corrected version.

## What we draw today

The engine's concept vocabulary has three members — `Zone`, `Segment`, `CandleMark`
([scene.rs:2847](../../../../frontend/chart-engine/src/scene.rs#L2847)) — and both spatial
ones are bounded by their own two bars, with a zero-length `Segment` floored at
`bar_w * 0.6` ([scene.rs:2932](../../../../frontend/chart-engine/src/scene.rs#L2932)).
Against the 501-bar window:

**1. Ten concepts never have a length at all.** `bos`, `choch`, `bpr`, `inducement`,
`displacement`, `unicorn`, `market_maker_model`, `mss`, `equilibrium`, `mtf_alignment`
answer `bars: [t, t]` for **every** event they emit. Each therefore draws as a 1.3px tick
and a floating word; the FVG beside it at least reaches its mitigation bar
([pass1-fvg-bos.png](evidence/pass1-fvg-bos.png)).
This is the single largest gap in the family: it is what a BOS looks like on this platform
today, and a BOS is the concept the whole family is named after.

**2. The boxes that do have length are frozen, not alive.** `smc.fvg`'s 50 events span 1 to
221 bars (median 174), `smc.ifvg` 2-219, `smc.order_block` 5-178 — the detector already
tracks a lifecycle and stamps `bars: (formed_at, bar_time)`. The router reads none of it:
`concept_shapes.js:70` is

```js
const end = e.status === "forming" || e.status === "confirmed" ? toNs : toNs;
```

both arms identical, a dead branch left mid-decision. And the *removal* half of the
lifecycle is missing entirely — 43 of `smc.fvg`'s 50 events are `expired` and every
`smc.ifvg` event is `invalidated`, yet all of them are painted as if current. A mitigated
zone that stays on the chart at full strength is worse than a stub: it says the level is
live when the reference set says it was erased.

**3. Four states of the clock are drawn as prices.** `judas_swing`, `power_of_three`,
`model2022` and `market_maker_model` are `kind: interval` rows whose whole meaning is a
*when*. Two of them are in the router's id set, so they paint a price band; the other two
paint a horizontal level at the model's midpoint. Neither is a time box — the shape says
where, and the concept says when. The 32 rows are
23 `event` + 9 `interval`, and every one of the nine has `pane: false, overlay: false` and
only the `candles` channel — the platform has never asked where an interval is supposed to
live.

**4. The label is the detector's name.** `concept_shapes.js:63` prints
`e.concept_id.split(".")[1]`, so the chart says `bos`, `liquidity_sweep`,
`premium_discount`, `mtf_zone_alignment`. Only `smc.equal_levels` is special-cased to
BSL/SSL, and only the `candle_mark` branch (line 89) even spaces the underscores out — the
zone and segment branches print the raw id tail. Traders reading a TradingView chart see BOS, CHoCH, IDM, BSL, SSL, OB, FVG — the
abbreviations *are* the recognition the audit was asked to protect.

**5. Six concepts on one chart is noise, not a picture.** `smc.premium_discount` emits one
box per bar, each spanning its own last 12 bars' range, so 50 boxes stack over the same
strip of plot; with six concepts switched on it alone covers roughly 40% of it in solid fill
and repeats the word `premium_discount` a dozen times
([pass1-chart.png](evidence/pass1-chart.png)).
`smc.mtf_zone_alignment` does the same thing to a single HTF band: its 50 events are 50
copies of one zone, spans 420 to 469 bars, because the detector re-emits the state every bar
and the router has no notion of "this is the same zone again". The reference toolkit caps
this by construction — 5 order blocks, one dealing range at a time. Our vocabulary cannot, so
every state ever replayed is drawn with the same weight.

## The card is a second renderer, and it disagrees

The Indicators face's miniature does not read the router; it branches on the payload's
fields ([`previewEvents`, app.js:14768](../../../../frontend/app/app.js#L14768)):
a box if `zone_low` and `zone_high` are both set, a dashed line otherwise. The chart
branches on an id list ([`CONCEPT_ZONE_IDS`, concept_shapes.js:27](../../../../frontend/app/concept_shapes.js#L27))
that holds 13 of these 32. The two therefore disagree for **10 concepts** — `displacement`,
`unicorn`, `model2022`, `market_maker_model`, `mss`, `equilibrium`,
`consequent_encroachment`, `liquidity_void_fill`, `mtf_alignment`, `mtf_zone_alignment` —
each of which carries a zone and shows its owner a **box in the card and a horizontal
segment on the chart**. That is the confusion this audit exists to prevent, and it is a
worse failure mode than drawing the wrong thing consistently: the customer picks the concept
because the card looked right, and the chart then contradicts it.

`preview_agrees` records it per row: 27 `differs`, 4 `agrees` (the four where the field
branch and the id set happen to coincide), 1 `not_rendered` (`smt_divergence`, which has no
emitter on the scan route and so shows its "nothing to draw" hairline).

## No parameter is honoured

16 of the 32 rows declare `swing_lookback`; **0 of the 32 honour any parameter**
(`parameters.honoured: []` everywhere). The declared default is 10
([concepts.rs:190](../../../../crates/market-data/src/concepts.rs#L190)); the engine
replays at `structure_lookback: 3`. So the panel advertises a knob the scan route has no
way to receive, and the picture on the chart is the one a lookback of 3 makes — which is why
`smc.bos` fires 18 times where a lookback of 10 would fire a handful. This is a
`rule_as_data` problem, not a drawing problem, and it is why the ledger carries the field at
all: the combined-concept spec needs a rule that can be *parameterised*, and today it cannot.

## The 32 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `lifecycle_box` (12) | fvg, ifvg, bpr, order_block, breaker_block, mitigation_block, rejection_block, premium_discount, silver_bullet, unicorn, liquidity_void_fill, mtf_zone_alignment | 12 new | a right edge from `status`, mitigation removes the zone, and a `forming` box that reaches the newest bar without repainting history |
| `level_run` (9) | bos, choch, mss, inducement, equal_levels, internal_range_liquidity, external_range_liquidity, equilibrium, consequent_encroachment | 9 new | a level with a *run* — one draw whose `to_ns` comes from a length parameter, dashed/solid by class, labelled once with the trader's abbreviation |
| `time_box` (4) | judas_swing, power_of_three, model2022, market_maker_model | 4 new | a full-height region with no price extent; the engine's `ScriptDraw.Box` plus the shell's clipping already do the work, so this is a wire gap, not an engine limit |
| `candle_mark` (3) | liquidity_grab, displacement, mtf_alignment | 3 wiring | the variant exists; the router must stop routing them to `segment`, and the label must stop being the wire name |
| `segment` (2) | liquidity_sweep, turtle_soup | 2 wiring | close today: `liquidity_sweep` already runs from the swept level's origin (median 283 bars), but it stops at the sweep bar instead of continuing, and the label is the wire name |
| `outline` (1) | trendline_sweep | 1 new | the slope through three touches is computed and then discarded — share pass 6's polyline rather than invent a trendline shape |
| `pair_link` (1) | smt_divergence | 1 new | needs a second series on the chart; `/concept-events/smt` exists and has no caller |

27 of the 32 are `new_rendering` and 5 are `wiring`, which is the audit's central
measurement: **the ceiling is the three-member vocabulary and one hard-coded id set, not the
engine** — the scripting path already draws `Line { extend }`, `Polyline`, `Polygon` and
`Band` ([script.rs:67](../../../../frontend/chart-engine/src/script.rs#L67)).

## Fixes this pass asks for

Ordered so that each one is visible on its own, and so the cheap ones come first:

1. **Labels** (`concept_shapes.js:63`): a per-concept display name — BOS, CHoCH, MSS, IDM,
   BSL/SSL, EQH/EQL, OB, BB, FVG, iFVG, BPR, CE, Premium/Discount — sourced from the
   registry rather than a second list in the shell. Nothing else in this family reads as
   familiar until this changes, and it is the only fix that is entirely a rename.
2. **One router** (`app.js:14768` vs `concept_shapes.js:27`): the card must call
   `ConceptShapes.shapes()` instead of re-deriving the branch from payload fields. This
   deletes the 10 card/chart contradictions as a class rather than one at a time.
3. **`level_run`**: a shape with an explicit right edge. `Segment` already accepts a
   `to_ns`; the missing piece is a length policy in the router, per concept, defaulting to
   something that reads as a level (the reference's own default is 10 bars).
4. **`lifecycle_box`**: make `status` mean something — `forming` reaches the newest bar,
   `expired`/`invalidated` are either removed or visibly faded (LuxAlgo's breakers keep a
   non-solid fill; a mitigated zone at full opacity is the bug), and the dead ternary at
   `concept_shapes.js:70` goes with it.
5. **A cap**: draw the most recent N per concept instead of every replayed state, so six
   enabled concepts remain readable. `concept_shapes.js:108`'s `out.slice(-200)` is a global
   cap that no concept can reach at 50 events each; it should be per concept and small.
6. **`time_box` for the four clock rows**, which is the same wire Task 9 needs anyway —
   this pass is evidence for it, and pass 4 is where it gets built.
7. **Parameters into the scan route** (`concept_routes.rs:238`), which the
   combined-concept spec owns; the ledger rows are its raw material.

None of 1-5 changes what a concept *means*: every one is a router, a label or a length.
That is deliberate — this pass is about the picture, and `fires.verdict` records where the
rule itself differs (20 `different_rule`, 8 `same_bars`, 4 `no_external_rule`) without
fixing those here.
