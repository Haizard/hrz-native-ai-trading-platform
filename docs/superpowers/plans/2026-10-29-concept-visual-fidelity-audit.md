# Concept Visual Fidelity Audit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development
> or superpowers:executing-plans to work this list task-by-task. Steps use checkbox
> syntax (`- [ ]`) for tracking.

**Goal:** Produce the drawing contract for all 182 registered concepts — a cited
reference verdict per concept, a photograph of what our engine actually draws today,
and a machine-checked ledger — without fixing a single detector along the way.

**Architecture:** Two small frontend modules give the audit one opinion to share with
the app instead of a second copy of it: the family taxonomy and the shape router plus
its painter. One Node tool then does four things with them — seed the ledger from
`GET /concepts`, render a family's real output through the real wasm, fill nothing by
hand, and check that nothing is empty. The twelve family passes are research: open the
reference, quote it, judge our render, write the row.

**Tech Stack:** Vanilla classic-script JS with a UMD shim so Node can `require()` it
(the pattern `frontend/app/builder.js` already uses), the `chart_engine.wasm` scene
builder, Node 24 `fetch` against the gateway on `:8080`, Markdown for the audit.

**Spec:** [docs/superpowers/specs/2026-10-29-concept-visual-fidelity-audit-design.md](../specs/2026-10-29-concept-visual-fidelity-audit-design.md)

## Global Constraints

- Work on `main`; never push unless the user asks.
- **No new renderer during the audit.** The scene's concept field is already
  `Vec<ScriptDraw>` (`frontend/chart-engine/src/scene.rs:1061`); anything expressible
  in it is *wiring*, and the ledger's `target_shape.status` says which.
- **No arithmetic over market data in the shell** (`docs/14:53`). A figure arrives from
  Rust already formatted; the audit changes no code, so this binds the tool only.
- **Photograph the defect, do not fix it.** `app.js:6066`'s
  `? toNs : toNs` stays exactly as it is until pass 1's rows and render exist.
- Detection semantics are not changed. `fires` records disagreement; acting on it is a
  later per-family decision.
- Every claim about how a pattern should look carries `{ source, url, quote, checked }`,
  or `house_style` with a reason. "Commonly drawn as" is not a citation.
- One concept, one id: never split a concept id per venue.
- Do **not** run `cargo fmt --all` (the repo is not rustfmt-clean) and do **not** run
  `tools/guard_check.py` while the user is testing on `:8080`.
- Verification commands, run at the end of every task:

```bash
NODE_PATH="/Users/macbook/.workbuddy-node/node_modules" node tools/shell_check.mjs
NODE_PATH="/Users/macbook/.workbuddy-node/node_modules" node tools/ui_check.mjs
node tools/wasm_abi_check.mjs
node tools/component_check.mjs
node tools/concept_visual_audit.mjs check
```

## File Structure

| File | Responsibility |
| --- | --- |
| `frontend/app/concept_taxonomy.js` *(new)* | The family table and `familyOf(id)`. One opinion about which family a concept is in, shared by the panel's rail and the ledger's shards. |
| `frontend/app/concept_shapes.js` *(new)* | The shape router (`from`), the concept painter (`paint`) and the packed-colour helper (`rgba`). One opinion about what a detection looks like, shared by the chart and the audit's photographs. |
| `frontend/app/app.js` *(modified)* | Calls the two modules; keeps its own colour constants and every other painter. |
| `frontend/app/index.html` *(modified)* | Two new script tags, both before `builder.js`. |
| `tools/concept_visual_audit.mjs` *(new)* | `seed`, `render <family>`, `check`. Holds no rendering rules of its own. |
| `reports/concept-visual-audit/ledger/<family>.json` *(new)* | The rows, sharded by family so two passes never write one file. |
| `reports/concept-visual-audit/<family>/<concept_id>.html` *(new)* | What our engine draws today for that one concept, from real events through real wasm. |
| `docs/superpowers/specs/concept-visual-audit/00-vocabulary.md` *(new)* | The four kinds, the shape list with `wiring`/`new_rendering`, the tier rule, the warm-up contract. |
| `docs/superpowers/specs/concept-visual-audit/NN-<family>.md` *(new)* | One file per pass: reference quoted, render attached, verdicts. |

Sharding the ledger and the pass sections is a deliberate deviation from the spec's
single `ledger.json` and single audit document: twelve passes writing one JSON file and
one Markdown file cannot run in parallel and lose rows on the merge. The spec's
completeness promise is unaffected — `check` reads the union and fails on a gap.

## The row

`check` is the only judge of what a complete row is. It is written before the first
pass so no pass can quietly define its own schema.

```jsonc
{
  "concept_id": "smc.bos",
  "family": "smc",                       // seeded: familyOf(id)
  "version": "1.0.0",                    // seeded
  "registry": {                          // seeded, verbatim from GET /concepts
    "pane": false, "overlay": false,
    "channels": ["candles"],
    "parameters": ["swing_lookback"]
  },
  "kind": "event",                       // event | series | interval | readout
  "reference": { "source": "", "url": "", "quote": "", "checked": "2026-10-29" },
  "draw":    { "today": "", "canonical": "", "verdict": "" },   // matches | differs_minor | differs_major | impossible_in_vocabulary
  "fires":   { "our_rule": "", "their_rule": "", "verdict": "" }, // same_bars | different_rule | no_external_rule | continuous
  "rule_as_data": { "form": "predicate", "rule": "", "inputs": [{ "name": "candles", "channel": "CANDLES" }] },
  "warmup":  { "bars": 0, "before": "" },
  "target_shape": { "name": "", "status": "wiring", "scale": "price" }, // status: wiring | new_rendering · scale: price | own_pane | time_only
  "detector": "analytics_core::events::detect_structure",
  "parameters": { "declared": ["swing_lookback"], "honoured": [] },  // honoured: [] is an answer; null is not
  "render": "reports/concept-visual-audit/smc/smc.bos.html",
  "preview_agrees": "agrees"   // agrees | differs | not_rendered
}
```

The gate's rules, and the reason each exists:

1. Every registry concept has exactly one row, in its `familyOf(id)` shard — and the row's
   `version` is the registry's current one, because a row judged against a definition that
   has since moved is a finding about a concept that no longer exists.
2. No empty string, no `null`, no row left at its seeded value — and no value outside the
   ledger's vocabulary: `kind`, the two verdicts, `form`, `status`, `scale` are each one of
   the strings above. A pass may be wrong about a concept; it may not invent a category.
3. `reference` is a citation **or** `{ "no_convention": true, "reason": "" }` — the shape
   the spec's own row table names, and `checked` has to be an actual date because the row
   is expected to age. *(The first draft of this rule said `house_style`; the spec is the
   contract, so the gate reads `no_convention`.)*
4. `fires.verdict == "continuous"` requires `kind != "event"` — a series may not
   pretend it fires on a set of bars.
5. `kind == "series"` requires `registry.pane || registry.overlay` — the registry
   already knows a series has a scale, so a row that disagrees is reporting a defect,
   and `scale` must be `own_pane` when `pane` is true.
6. `rule_as_data.inputs` is non-empty and every channel name is one `DataChannels`
   serves — the list parsed out of `crates/market-data/src/broker.rs` at gate time rather
   than copied here, so a new channel cannot make this rule a lie. Both spellings the spec
   uses are accepted: the registry's `names()` output (`trades_aggressor`) and the bit it
   cites in prose (`TRADES_AGGRESSOR`).
7. `render` points at a file that exists.
8. `preview_agrees` is filled. The Indicators face's card is a **second renderer for the
   same shapes** — `previewCandles`, `overlayPrices` and `eventPrices` in `app.js` — so a
   pass that only photographs the chart can green-light a drawing the panel contradicts.
   `differs` is not a failure; it is the finding that the spec's coupling section asked
   for, and it names which of the two the trader should believe.
9. `parameters.declared` is the registry's own list, and `parameters.honoured` is an array
   drawn from it — so the parameter gap is counted per concept instead of asserted in
   prose. `[]` is an honest answer ("the detector reads nothing"); `null` is an unjudged row.

The gate also reads `docs/superpowers/specs/concept-visual-audit/00-vocabulary.md` and
fails if that chapter stops naming a kind the gate accepts: the kinds are declared once,
in the open, and a fifth one is a decision a pass has to make in the chapter before a row
can use it.

## The pass procedure

Every pass runs these steps. The task list below states only what differs.

- [ ] **1. Render first.** `node tools/concept_visual_audit.mjs render <family>` — this
  is the photograph, and it is taken before any judgement is written, so a verdict
  describes a picture rather than an expectation.
- [ ] **2. Establish the reference.** Open the sources named in the task. For each, copy
  the URL, the date, and **one sentence** that says how the thing is drawn. Quote, do
  not paraphrase: a paraphrase is this audit's opinion wearing a citation.
- [ ] **3. Fill the rows** in `reports/concept-visual-audit/ledger/<family>.json`, one
  concept at a time, in registry order. Both verdicts, always: a concept can look right
  and mark the wrong bars, or mark the right bars in an unrecognisable shape.
- [ ] **4. Write the section** at
  `docs/superpowers/specs/concept-visual-audit/NN-<family>.md` — the family's canonical
  rendering first, then the table of concepts, then the shape vocabulary it needed and
  whether each is `wiring` or `new_rendering`.
- [ ] **5. Look at a real chart.** The gateway serves `:8080`; open `#/charts`, enable
  the family from the Indicators face, and read the page against the render. Then
  compare each concept's **card preview** in that same face against the chart it is
  describing and set `preview_agrees` — the card is a second renderer, and the spec is
  explicit that a change to the chart must land there too. A pass is not signed off from
  a JSON payload: the layout bugs earlier in this program were invisible to every
  harness.
- [ ] **6. Gate.** `node tools/concept_visual_audit.mjs check --family <family>` green,
  then the five verification commands.
- [ ] **7. Commit** the section, the shard, and the tool change if any. The renders
  themselves are not committed: a page embeds a real 500-bar scene and weighs ~250 KB,
  so the full sweep would put ~40 MB of regenerable HTML in the repo. `.gitignore` keeps
  `reports/concept-visual-audit/*/` out and the `ledger/` shard in, the same
  evidence-not-source rule `/test-shots/` already follows. The committed record of what
  a picture showed is the row's `ours.today` prose plus the section's measurements;
  `node tools/concept_visual_audit.mjs render <family>` rebuilds the page over a
  comparable window — the newest 500 bars, not the identical ones, since the gateway
  serves a moving window and each page prints the `at` it was built. That is exactly why
  the pass writes down spans and counts instead of saying "see the picture".

---

## Phase A — the tooling

### Task 1: One family table, shared with Node

**Files:** Create `frontend/app/concept_taxonomy.js`; Modify `frontend/app/app.js:14030-14048`
(`CONCEPT_GROUPS`), `frontend/app/index.html:1903` (script tag).

- [x] Create the module with the predicates moved verbatim out of `app.js`, in the
  `builder.js` UMD shape so the same file works in a `<script>` and in `require()`:

```js
(function (global, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else global.ConceptTaxonomy = api;
})(typeof globalThis === "object" ? globalThis : this, function () {
  "use strict";
  const GROUPS = [/* the seventeen rows, moved from app.js:14030-14048 unchanged */];
  return {
    GROUPS,
    familyOf(id) { return (GROUPS.find((g) => g.match(id)) || GROUPS[GROUPS.length - 1]).id; },
  };
});
```

- [x] Create `tools/concept_visual_audit.mjs` with one subcommand, `taxonomy`, whose job
  is to fail: it `require()`s the new module and asserts the counts the spec's table
  claims, so an extraction that dropped a row or reordered the catch-all is caught here
  rather than in pass 7. Task 3's `seed` consumes the same table.

```js
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const T = require("../frontend/app/concept_taxonomy.js");

// The spec's twelve-pass table, copied verbatim -- a family count that moves without
// the spec moving with it is the audit disagreeing with itself.
const EXPECTED = {
  smc: 32, sd: 4, pa: 11, session: 14, fib: 6, chart: 15, harmonics: 9,
  candles: 34, "volume-profile": 9, "order-flow": 1, trend: 12, momentum: 10,
  volatility: 4, volume: 5, statistical: 10, econ: 6, more: 0,
};
```

- [x] In `app.js`, replace the local `CONCEPT_GROUPS` with
  `const CONCEPT_GROUPS = ConceptTaxonomy.GROUPS;` — the rail, the search and the
  coverage line keep reading the same rows, and the catch-all `more` group must stay
  last because `familyOf` takes the first match.
- [x] Add `<script src="/assets/concept_taxonomy.js"></script>` immediately before
  `builder.js` in `index.html`, and confirm `/assets/{*path}` serves it (extension
  allowlist already covers `.js`; static files read from disk, so no gateway restart).
- [x] Run the harnesses. `shell_check.mjs` reads the script tags out of `index.html` and
  injects them in order, so the new file is under test the moment the tag exists.
- [x] In the browser: `#/charts` → Indicators face → the rail shows sixteen family rows
  with the same counts as before, and the search box still filters.
  *(Done with instruments, not eyes: `shell_check` builds the page with the new script in
  order and reports no uncaught error across every gesture, and `taxonomy` fails unless
  the group table answers the spec's sixteen counts. The eye-level check on a signed-in
  chart is folded into pass 1, which has to look at that chart anyway.)*
- [x] Commit (`ce8915d`).

### Task 2: One shape router and one painter, shared with Node

**Files:** Create `frontend/app/concept_shapes.js`; Modify `frontend/app/app.js`
(`BULL/BEAR/NEUT` at 6038-6040, `conceptColor` 6042-6046, `CONCEPT_ZONE_IDS` 6037,
`conceptShapes()` 6050-6104, `rgbaFromPacked` 1502, `drawConceptShapes` 1758-1795,
call sites 974 and 1033), `frontend/app/index.html`.

**Interfaces:** Produces `ConceptShapes.from(events, enabled)` → the array the chart
request sends, and `ConceptShapes.paint(ctx, scene, phase)` → the two-pass paint. Task 4
consumes both, which is the whole reason the extraction happens first: a tool that
re-implemented the routing would photograph the tool and not the chart.

- [x] Move the constants, `conceptColor`, `CONCEPT_ZONE_IDS`, `conceptShapes` and
  `drawConceptShapes` into the module **unchanged**, with two differences: `from` takes
  `(events, enabled)` instead of closing over `conceptEvents`/`enabledConcepts`, and
  `paint` reads `scene.concept_shapes` and `scene.plot` exactly as before.
- [x] Move `rgbaFromPacked` in and export it as `rgba`; at `app.js:1502` leave
  `const rgbaFromPacked = ConceptShapes.rgba;` so its other 28 uses are untouched.
- [x] At `app.js:3418` the request field becomes
  `concept_shapes: ConceptShapes.from(conceptEvents, enabledConcepts),`.
- [x] At 974 and 1033 the two calls become `ConceptShapes.paint(ctx, scene, "under")`
  and `"over"`.
- [x] **Preserve the defect.** `const end = e.status === "forming" || … ? toNs : toNs`
  is copied as it stands. Fixing it here would let pass 1 photograph its own correction.
- [x] Harnesses green, then the browser: enable `smc.fvg`, `candles.` any, `smc.bos` on
  a live chart and compare against a screenshot of the same window taken before the
  move. Identical, or the extraction changed what the trader sees.
  *(Harnesses green, and Task 4's pages prove the same fact from the other side: the
  report page inlines the browser's own `concept_shapes.js` and paints the wasm scene
  with it, so what the photograph shows is what the app draws. The signed-in live-chart
  comparison is still owed and pass 1 does it as its step 5.)*
- [x] Commit (`39535f0`).

### Task 3: The ledger, seeded from the registry rather than typed

**Files:** Create `tools/concept_visual_audit.mjs` (`seed`),
`reports/concept-visual-audit/ledger/*.json`.

- [x] `seed` fetches `GET /concepts` from `http://127.0.0.1:8080`, requires exactly 182
  entries, and writes one shard per `ConceptTaxonomy.familyOf(id)` with every row at its
  seeded value. A concept registered tomorrow appears as a row the gate demands, so
  completeness cannot rot.
- [x] Assert the counts match the spec's table; print any family that differs and fail.
  `Everything Else` must be empty — a concept landing there means the taxonomy is
  missing a family, which is a finding for the pass that notices.
- [x] Run it, commit the shards: the ledger's existence is the first artifact a reviewer
  can check without opening a browser.

### Task 4: A photograph of what our engine draws today

**Files:** Modify `tools/concept_visual_audit.mjs` (`render`),
`reports/concept-visual-audit/**`.

**Interfaces:** Consumes `ConceptShapes.from`/`paint`/`rgba` (Task 2), `familyOf`
(Task 1), `GET /concept-events/scan?symbol&timeframe`, `GET /candles`, and
`frontend/app/chart_engine.wasm`'s `build_scene`.

- [x] For each concept in the family: fetch the scan window once, route
  `from(events, new Set([id]))`, send the result as `concept_shapes` through the real
  wasm, and inline the scene JSON into a self-contained page — the same shape
  `tools/chart_preview.mjs` writes, so nobody has to trust a new renderer.
- [x] The page's script loads `concept_shapes.js` by reading the file and inlining its
  source, then calls `ConceptShapes.paint` twice, `"under"` before the candles and
  `"over"` after, matching `app.js`'s order. **This is the honesty guarantee: the tool
  cannot draw a concept better than the app can.**
- [x] Two kinds of empty are different findings and must be labelled in the page: no
  events in the window, and events that routed to zero shapes. The second is a
  vocabulary gap on camera.
- [x] `--symbol` / `--timeframe` / `--limit` flags, defaulting to the chart's own
  `BTCUSDT` `15m` `500`, because a synthetic series cannot show whether a
  session box lands on the right hours. *(Corrected while building: this said
  `BINANCE:BTCUSDT`, and the gateway answers that symbol with zero candles and zero
  events — the venue prefix is a display convention, not a key into the window store.
  Bare `BTCUSDT` is what a pane defaults to and what carries data: 501 bars, 4,278
  events.)*
- [x] Render `smc`, open two or three of the pages, and confirm what the spec predicted
  is actually there: the BOS segment stops at its break bar, and an open FVG is a
  one-bar sliver. If it is not there, the spec's finding needs correcting before any
  pass trusts it. **Both were checked, and the spec is now corrected** (see its
  "Known defects carried deliberately"): an open FVG *is* a one-bar sliver, but 43 of
  the 50 are filled zones stretched over a median of 177 bars, and a BOS segment has no
  start at all — `bars` carries the break bar twice, so the photograph is the word "bos"
  floating above a 1.3px stub. The missing swing is a detector gap, not a rendering one.
- [x] A third kind of empty, which the plan did not name and the first attempt got
  wrong: shapes that routed but lowered to fewer draws than the engine's own rule
  requires — a zone is Box + Label, a segment Line + Label, a candle mark Label, so
  `expected = sum(type === "candle_mark" ? 1 : 2)`. Testing `shapes !== scene.concept_shapes.length`
  instead flagged every healthy page, because a zone legitimately lowers to two draws.
  A page that shows this third label is an engine bug, not a vocabulary gap, and the two
  findings must not be allowed to look alike.
- [x] The pages stay out of the repo. Each embeds a real 500-bar scene and weighs
  ~250 KB, so the full sweep is ~40 MB of regenerable HTML; `.gitignore` now excludes
  `reports/concept-visual-audit/*/` and keeps the `ledger/` shards, following the
  evidence-not-source rule `/test-shots/` already sets. The durable record of a picture
  is the row's `ours.today` and the section's numbers.
- [x] Commit (`90e9f94`).

### Task 5: The completeness gate

**Files:** Modify `tools/concept_visual_audit.mjs` (`check`),
`tools/concept_visual_audit_check.mjs` (fixtures), `docs/superpowers/specs/concept-visual-audit/00-vocabulary.md` (stub).

- [x] Write the failing fixture first: five rows that must all be rejected — a missing
  `reference`, `fires.verdict: "continuous"` on `kind: "event"`, a `series` row whose
  registry says neither `pane` nor `overlay`, an unknown channel name in
  `rule_as_data.inputs`, and a `render` path with no file.
```js
// tools/concept_visual_audit_check.mjs
// `complete` is one hand-written row that satisfies all eight rules, built by
// spreading the seeded skeleton of `smc.bos` over the fields Task 3 leaves empty --
// if it is not a real row, the gate can pass a fixture and still fail the library.
const bad = [
  { ...complete, reference: undefined },
  { ...complete, fires: { ...complete.fires, verdict: "continuous" }, kind: "event" },
  { ...complete, kind: "series", registry: { ...complete.registry, pane: false, overlay: false } },
  { ...complete, rule_as_data: { form: "predicate", rule: "x", inputs: [{ name: "book", channel: "order_book" }] } },
  { ...complete, render: "reports/concept-visual-audit/nope/nope.html" },
];
```
- [x] Implement `check` against the rules in *The row*, `--family` to scope one
  pass and no flag to sweep the union. Message per failure names the concept, the field
  and the rule, because a gate nobody can satisfy from its own output is a gate that
  gets edited rather than fixed.
  *(Built, and the rule list moved from seven to nine while it was being written: the
  spec's row table asks for `parameters: { declared, honoured }` and the plan's rules
  never counted it, so rule 9 exists and `seedRow` seeds it (`honoured: null`, which the
  emptiness scan refuses until a pass chooses `[]` or a real list). `channelNames()`
  parses the constants out of `crates/market-data/src/broker.rs` instead of trusting this
  file's prose, which is how `macro` — declared, but absent from `DataChannels::names()` —
  is still a legal channel for the econ pass, and how a typo like `order_book` is not.
  Rule 3 reads `no_convention`, the shape the spec names, not the `house_style` drafted
  here. A field owned by a specific rule is skipped by the general emptiness scan, so a
  row reports one problem per field; output is grouped `rule → fields` per concept, with
  the first reason printed under it.)*
- [x] Fixture green; the full sweep fails 182 times with the *right* reasons; the
  vocabulary stub is created so later tasks have a file to grow.
  *(18 of 18 fixtures behave — the five rejections above, plus rule 5's second half, rule
  9, an unknown verdict, an unknown kind, and a `no_convention` reference with and without
  its reason. The sweep reports `FAIL 182 problem row(s) of 182 checked`, problems per rule
  `2:2002 3:728 6:182 7:134 8:182 9:182`; rule 7's 134 is 182 minus the 48 pages already
  rendered for smc, statistical and econ, which is the arithmetic it should be. Rule 1
  fires zero times: the shards and the registry agree. The stub at
  `docs/superpowers/specs/concept-visual-audit/00-vocabulary.md` declares the four kinds
  and eight candidate shapes, each tied to the measurement that asked for it, and `check`
  fails if the chapter stops naming a kind the gate accepts.)*
- [ ] Commit.

---

## Phase B — the twelve passes

The tasks below are Phase A's tool applied to Phase B's research. Each names its
reference set and what the spec expects to find; the verdicts are the pass's to make.

### Task 6 — Pass 1: Smart Money Concepts (32)
`render smc`. Reference set: LuxAlgo's *Smart Money Concepts* indicator page on
TradingView (the de-facto rendering for structure, liquidity and imbalance) and its
documented options for BOS/CHoCH line extension; TradingView's own swing-structure
documentation; the ICT corpus for what BSL/SSL, order blocks and mitigation *mean*.
Expect: the run-until-mitigated level and the living box, both
`impossible_in_vocabulary` today; `smc.equal_levels`' BSL/SSL naming already correct.
This pass also fixes the vocabulary chapter's shape list for levels and boxes, because
nine later passes reuse it.

### Task 7 — Pass 2: Supply & Demand (4)
`render sd`. Same vocabulary as pass 1 and deliberately its test: if pass 1
generalised, these four need no new shape. Reference: the zone as drawn in the ICT and
LuxAlgo treatments — origin at the base, right edge live until mitigated, faded after.

### Task 8 — Pass 3: Price Action (11)
`render pa`. Breaks and retests outside the SMC family. Reference: TradingView's
`Structural Labels`/swing documentation plus what a retest is drawn as in the
platforms' structure indicators. Expect a `fires` disagreement here even where the
shape matches, because a retest's bars are defined by the swing rule, not the picture.

### Task 9 — Pass 4: Sessions (14)
`render session`. The full-height vertical time box, which has no `ConceptShape`
variant at all — the pass that proves the ceiling is a wire, not an engine limit, since
`ScriptDraw.Box` and the shell's clipping already do the work. Reference: TradingView's
regular session and `session` box rendering, LuxAlgo's sessions indicator, and the
killzone convention for the naming.

### Task 10 — Pass 5: Fibonacci (6)
`render fib`. The named ladder across an impulse, extended, with the ratio on each
level; against our six anonymous segments. Reference: TradingView's Fibonacci
retracement tool defaults (which ratios are drawn and labelled by default), and the
OTE/golden-pocket convention as documented by whoever named it.

### Task 11 — Pass 6: Chart Patterns (15)
`render chart`. The hardest geometry: an outline traced through the pivots, a dashed
neckline, a projected target. Reference: Edwards & Magee and Bulkowski for the shape
itself, TradingView's auto-pattern indicators for how it is drawn on a live chart.
Expect `Polyline` plus a dashed `Line` plus a projection, all already in `ScriptDraw`.

### Task 12 — Pass 7: Harmonics (9)
`render harmonics`. XABCD polyline, legs named, often filled at a wash. Reference:
Scott Carney's definitions for the geometry and the naming of points, plus the
platform rendering. Reuses pass 6's machinery: if it does not, record why.

### Task 13 — Pass 8: Candlestick Patterns (34)
`render candles`. The largest count and the smallest per-concept work: the convention
is a marker at the bar with the name optional, and ours is always a word. Reference:
the pattern dictionaries (Bulkowski's candlestick pages, Steve Nison for the Japanese
canon) and the marker rendering the platforms' candlestick-pattern indicators use.
Watch for the `kind` split this family forces: a three-bar pattern is an `event`, and
`warmup.before` must say what draws on bars 0–2.

### Task 14 — Pass 9: Volume Profile (9)
`render volume-profile`. POC, VAH/VAL, HVN/LVN — the engine already has `Level` and
`ProfileBar`, so most rows here are expected to be `wiring`. Reference: Steidlmayer and
the CBT convention, plus the platform profiles' default value-area percentage (which is
what makes VAH/VAL move, and is a parameter this catalog does not honour).

### Task 15 — Pass 10: Order Flow, Trend, Momentum, Volatility, Volume (32)
`render order-flow`, `render trend`, `render momentum`, `render volatility`,
`render volume`. The series kind: the scale question the registry already answers with
`pane`/`overlay`, and the rule that a z-score of 2.7 must never touch the price axis.
Reference: TradingView's built-ins for each — their default plots, panes, colours and
whether a signal is marked at all. This is the pass that settles the vocabulary for
Task 17, so it runs before it.

### Task 16 — Pass 11: Statistical (10)
`render statistical`. `indicators.{hv,zscore,kalman,changepoint,entropy,bayesian_drift,ar_forecast,garch,hmm,pairs}`.
A deviation and its percentile have conventions; the hidden-state estimators do not, and
those rows are `house_style` with the reasoning, under the stated bar: legible, smooth,
low visual noise, quiet by default, and explainable in one line on the card. A regime is
an `interval` — pass 4's shape, reused. Rows complete on the day written and gated on
the parameter path, which this audit records and does not fix.

### Task 17 — Pass 12: Economic Releases (6)
`render econ`. The vertical event marker with the figure on it, against the
earnings-marker convention; `interval`-adjacent but time-only, so `scale: time_only`.
Reference: the platform earnings/revenue markers, and our own release countdown's
honesty rule — the clock arms you, the API delivers the number.

---

## Phase C — the contract

### Task 18: The vocabulary chapter, finalised
Grow `00-vocabulary.md` from the twelve passes' shape lists into the contract: the four
kinds and the gate rules that enforce them; every shape with `wiring` versus
`new_rendering` marked and the existing `ScriptDraw` arm named for each wiring row; the
rule that a shape never learns which data tier fed it; the warm-up contract; and the
scale read off `pane`/`overlay` instead of invented. DoD item 4 of the spec is this
task's acceptance, and its test is that each `target_shape` is implementable by someone
who has not read a pass.

### Task 19: Close-out
Run `check` over the union and require 182 rows and zero failures. Confirm the defects
list in the spec — `app.js:6066`, `pane`/`overlay` ignored by the concept path, the
catalog-versus-engine parameter defaults, 156 concepts with no parameters — each has a
row or a section that photographs it, with no code changed. Re-read the spec's Definition
of done against the artifacts one line at a time, and write the outcome at the end of
`00-vocabulary.md` including anything that could not be met and why.
