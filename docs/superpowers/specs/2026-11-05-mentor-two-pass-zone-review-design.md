# Mentor two-pass zone review — design

Date: 2026-11-05
Status: awaiting review
Scope: `crates/ai-agent`, `crates/api-gateway`, `frontend/chart-engine`, `frontend/app`

## The problem

Two gaps, found by testing the feature that shipped in `bf59e9d`/`96e4c3b`.

**The agent never looks at its own markings.** A mentor run reads the timeframe
ladder once, numerically, and answers. It draws zones while answering, but it
never sees the chart it just marked. An explanation of a setup that was written
before the marks existed is a claim about a picture nobody has looked at — and
it is the difference between a tool that says "there is a demand zone here" and
a teacher who shows you the zone, then checks whether the price is actually
reacting inside it.

**The plan is painted, not drawn.** `TradeThesis` carries three single prices
([crates/ai-agent/src/thesis.rs:152-156](../../../crates/ai-agent/src/thesis.rs))
and the shell turns entry→target into a band stretched across the entire plot
width at 0.12 alpha
([frontend/app/app.js:3427-3430](../../../frontend/app/app.js)). The user's
complaint — "they cover all the chart" — is literally correct. A zone is a
bounded area at specific prices; the current overlay is a stripe over
everything, and it is the reason the agent cannot express "fill me between
4297 and 4339" even when `detect_zones` reported exactly that range.

## Decisions taken with the product owner

| Question | Decision |
| --- | --- |
| When does the two-pass loop run? | Mentor / teaching mode only. Ordinary questions stay single-pass and cheap. |
| How many frames does the marked pass re-photograph? | All six, always — the same six rungs as the plain pass. |
| Can the agent express an entry area at all? | Yes: **all three legs become zones**, each with a low and a high. |
| How is the loop driven? | Fixed phases in the orchestrator; the model does the marking. Not prompt-only, not two runs. |
| Is the engine zone treatment in scope now? | Yes, in the same change. |

## Architecture

One mentor question runs in four phases, two of which already exist.

```
1. plain pass      six frames ride the first user message        (0 turns)
2. analysis        ladder numbers + the six pictures; the model
                   marks zones and the plan as AI drawings       (normal turns)
3. marked pass     review_marked_charts -> six frames WITH marks (1 turn)
4. confirmation    trend confirmed against the marked charts,
                   submit_thesis                                 (answer turns)
```

Phase 1 is free because mentor mode already forces chart-attach, and the shell
captures the ladder into the request
([frontend/app/app.js:11911-11947](../../../frontend/app/app.js)). `MAX_SCREENSHOTS`
is 6 and `LADDER_TIMEFRAMES` is `1w 1d 4h 1h 15m 5m`, so the plain pass needs no
new machinery — only a caption that says these are the *plain* frames.

Phase 3 is the one new tool. Phase 4 is the existing answer phase with one more
duty in front of it.

## 1. The thesis carries zones

```rust
pub struct PriceZone { pub low: f64, pub high: f64 }

pub struct TradeThesis {
    // …
    pub entry:  PriceZone,   // was entry_price: f64
    pub stop:   PriceZone,   // was stop_price: f64
    pub target: PriceZone,   // was target_price: f64
    pub risk_reward: f64,    // recomputed from zone midpoints, in Rust
    // …
}
```

`low == high` is legal, so an exact stop is still expressible and nothing that
today produces one price has to invent a second.

**`finalize` rules** ([crates/ai-agent/src/thesis.rs:218-311](../../../crates/ai-agent/src/thesis.rs)):

- both edges finite and > 0, `low <= high`;
- the stop zone sits **entirely** on the wrong side of the entry zone for the
  direction (a stop that overlaps entry is not a stop);
- the existing `MIN_RISK_FRACTION` floor still applies, measured midpoint to
  midpoint, so a stop parked on the entry is still refused;
- a zone thinner than `MIN_ZONE_FRACTION` of the distance between the entry and
  stop midpoints is refused unless `low == high` exactly — a band that thin is
  a single price wearing a zone's clothes, and the honest way to say one price
  is one price;
- `risk_reward` from midpoints, never from the model;
- **every edge is grounded separately**, six checks where there was one.

**Grounding must learn what a zone edge is.** `PriceRange::check` takes a single
value, and the keys it folds from tool results (`PRICE_KEYS`,
[crates/ai-agent/src/thesis.rs:400-424](../../../crates/ai-agent/src/thesis.rs))
do **not** include `top` or `bottom` — the field names `detect_zones` actually
returns. Today a zone edge the tool reported is not a citable price, which is
why single prices were invented instead. Adding those two keys is what makes
"the edges come from a reported zone" enforceable rather than aspirational.

**Consumers changed in the same commit**, no shims: the `submit_thesis` schema
and `parse_thesis`, `fallback_narrative`, the memory fact prose
([crates/ai-agent/src/agent_memory.rs:240-256](../../../crates/ai-agent/src/agent_memory.rs)),
`synthesize_stand_aside` zeroing, the REST/socket response (serialized verbatim
from `AskResponse.thesis`, [crates/api-gateway/src/agent_routes.rs:78](../../../crates/api-gateway/src/agent_routes.rs)),
the chat's thesis card, `tools/agent-cli`, and docs 09/13/14.

**Stored history**: chat history keeps the whole answer as JSONB
(`crates/db/migrations/0014_chat_history.sql`). Old rows keep the old shape; the
card reads defensively rather than a migration rewriting history that is a
record of what was said.

## 2. The plan is drawn, not painted

The full-width `thesisOverlays` band is deleted. During a mentor run the agent
marks the chart with **AI drawings**, because that is the only representation
that satisfies all three constraints: it exists on the chart *before* phase 3
photographs it, it persists with `created_by = "ai"`, `reason` and `confidence`
under the AI layer, and the user can delete it.

Per `docs/46` doctrine: zones → `rect`, trade → `position_long`/`position_short`
(a time-bounded risk box with an entry segment, a filled profit rect, an
outlined stop rect and target/stop text).

### What an ordinary (non-mentor) answer shows

Deleting the stripe must not leave a directional answer with no plan on the
chart. Three zones replace one stripe: `thesisOverlays` emits a band per leg at
that leg's own `low`/`high`, and no band spans entry→target any more. The
complaint was about a band that covered the whole vertical range of the chart;
three thin, price-accurate bands fix that with no engine work, even though they
are still full-width rather than bounded in time. Time-bounded boxes are what
the mentor run draws as objects, because only those exist before phase 3
photographs them.

### Engine zone treatment

Today a persisted drawing is filled at `globalAlpha = 0.1` with a `strokeRect`
([frontend/app/app.js:3329-3335](../../../frontend/app/app.js)) — a smear, not a
zone. `docs/31-BOX-BORDERS.md` already states the principle: a border distinct
from the fill is "what makes a zone read as a zone and not a translucent
smear".

The reference the product owner supplied (a TradingView XAUUSD shot) shows four
things, and the codebase already knows how to do three of them:

| In the image | Where it exists |
| --- | --- |
| Bounded box over a price band and a time span | `rect`, `dateprice_range`, `position_*` |
| Solid sub-band on the untested part, faded spent part | `SceneRegion { mitigated, fresh }` + `drawRegions` ([app.js:2180-2212](../../../frontend/app/app.js)), `docs/14` |
| A name beside the box ("First Area for buy", "KEY") | `dateprice_range`, `position_*`, `price_range` text parts |
| Price tags at the right edge | `price_label` renders *inside* the plot at `plot.x + plot.w - 60` ([draw/annotation.rs:108-137](../../../frontend/chart-engine/src/draw/annotation.rs)); the axis gutter itself is shell-side `drawGrid` ([app.js:2981-2996](../../../frontend/app/app.js)) |

So: a zone drawing gains (a) a border with its own weight and colour, distinct
from the fill, (b) an optional solid sub-band for the fresh portion, (c) a text
label, and (d) edge price tags rendered just inside the right edge.

**Deliberately not in scope: tags in the axis gutter.** The shell owns the axis
and has no price scale of its own — that is exactly why thesis overlays were
moved engine-side in the first place ([app.js:3870-3876](../../../frontend/app/app.js)).
Putting agent tags in the gutter means the engine emits gutter positions and the
shell stops owning the axis alone, for a few pixels of visual gain. Just-inside
tags read the same and keep the one-owner rule. `request.zones` is a **bool**
that turns on the engine's own detector
([scene.rs:281](../../../frontend/chart-engine/src/scene.rs)), so the region
renderer cannot be fed client zones today; making it so is a separate change
with its own review, not a dependency of this one.

Rendering lives in Rust→WASM, so this needs a `chart-engine` rebuild and the
artifact committed, as in `55ee763`.

## 3. Phase 3: `review_marked_charts`

One tool call, six frames.

- Takes an optional `timeframes` array (defaults to the six ladder rungs) and an
  optional `symbol`, the same shape `look_at_chart` already accepts, so a model
  that wants to re-check only 4h and 1h can say so.
- Dispatches six `capture_chart` ui-commands (one per `LADDER_TIMEFRAMES` rung of
  the symbol under review) in a single turn.
- Waits for the uploads **concurrently**. The current rendezvous polls each
  capture id for 24 × 250 ms in series
  ([crates/ai-agent/src/agent.rs:1605-1650](../../../crates/ai-agent/src/agent.rs));
  six sequential waits is up to 36 seconds of nothing on the screen.
- Returns one tool result carrying six labelled image blocks, captioned
  `PASS 2 · <TF> · marked`, plus an explicit "no image for <TF>" text block for
  any rung that did not answer in time. A missing frame is reported, never
  silently dropped.
- Images enter as tool-result content blocks, which is already how
  `look_at_chart` works (`Message::tool_results(..).content.extend(visual_blocks)`)
  — no new conversation shape.

**Capture budget.** `MAX_CAPTURES_PER_RUN` is 3
([agent.rs:1133](../../../crates/ai-agent/src/agent.rs)), so twelve frames do not
fit today. It becomes mode-aware: a mentor run may spend 12 captures, an ordinary
run keeps 3. The budget is a cost guard, not a correctness rule, and mentor mode
is the case the user asked to pay for.

**The marked-review duty.** Alongside `drawing_owed`
([agent.rs:1149-1207](../../../crates/ai-agent/src/agent.rs)), a
`marked_review_owed` flag refuses the first `submit_thesis` of a mentor run until
`review_marked_charts` has returned at least one frame, and tells the model in
one line what it still has to do. It is refused **once**, the way the existing
corrections are: a second refusal is a run burning turns on a mistake it has
already been told about, and an answer that admits it never re-checked the
marked charts beats a run that dies at the turn limit. When the refusal is spent,
the miss is recorded in the thesis trace so the answer card can say so. It discharges — with a note, not a hang — when
there is no screen attached (`request.captures == None`) or when every frame
timed out, mirroring the existing posture when no drawing writer is attached:
say what you would have checked, rather than pretending you checked it.

**Progress.** `ReviewFrame` gains a marked/pass discriminator
([crates/ai-agent/src/progress.rs:61-70](../../../crates/ai-agent/src/progress.rs))
so the shell can caption "AI REVIEW · MARKED" and the user can tell the two
passes apart. Without it the second pass is indistinguishable from the first.

**Shell.** The marked frames must actually contain the marks. `aiLayerOn` is a
module-level global ([app.js:950](../../../frontend/app/app.js)) and the drawing
filter is applied when the request is built ([app.js:3854](../../../frontend/app/app.js)),
so an offscreen `renderOffscreen` pane inherits whatever the layer state is —
mentor mode already forces it on when the agent's drawings land
(`showAiDrawingsOnCharts`). This is a load-bearing dependency and gets a test,
not a comment: a marked pass photographed with the layer off is a plain pass
lying about being marked.

## 4. Failure handling

| Failure | Behaviour |
| --- | --- |
| No screen attached (server-side ask) | Duty discharges; the answer states the marked pass did not run. |
| A rung never uploads | That rung reported as missing in the tool result; the other five still land. |
| All rungs time out | Duty discharges with a note; the model is told it has no marked pictures. |
| Model submits a thesis with ungrounded edges | Existing `Correcting` loop, now per edge. |
| Model draws nothing | Existing `drawing_owed` correction, unchanged. |
| A symbol has no series for a rung | That rung is not captured and not claimed, as today. |

## 5. Testing

**Rust, `crates/ai-agent`**

- `finalize`: midpoint R:R; a stop zone overlapping entry is refused; a zone
  below the height floor is refused; each of the six edges grounded
  independently; `low == high` accepted; `Bias::None` zeroes all six.
- `top`/`bottom` in `PRICE_KEYS`: a `detect_zones` edge is citable, an invented
  one is not.
- `review_marked_charts`: six capture ids dispatched, concurrent wait proven by
  a source that answers out of order, a missing rung reported, no-screen
  honest error.
- Duty: `submit_thesis` refused before the marked pass, accepted after;
  discharged when no captures attached; discharged when all frames time out.
- Wire: `ReviewFrame` serializes with the marked discriminator the shell reads
  (extends `a_review_frame_is_the_shape_the_shell_reads`).

**Rust, `frontend/chart-engine`**

- A zone drawing emits the parts the reference needs: border, fill, fresh
  sub-band, label, two edge tags.
- `position_long`/`position_short` still correct with zone-shaped inputs.

**Shell**

- `thesisOverlays` no longer emits a full-width band (the regression this whole
  half exists to fix).
- The marked pass cannot be captured with the AI layer off.

**Live acceptance** on the :8080 gateway, one mentor question — "teach me this
setup" — watched end to end: six plain cards, marks landing, six marked cards,
then a plan drawn as bounded boxes. This spends a real provider call and needs
the product owner's go-ahead at the time.

## Build order

Each step is shippable and testable on its own, and the order is forced by the
dependencies:

1. **Thesis zones + grounding** (`top`/`bottom` admitted, per-edge checks) and
   the three thin overlay bands. This alone fixes the "covers all the chart"
   complaint and is visible without any of the loop.
2. **Engine zone treatment** + wasm rebuild.
3. **`review_marked_charts`**, the concurrent wait, the mode-aware capture
   budget, and the `ReviewFrame` marked discriminator.
4. **The duty and the mentor prompt**, which is where the loop becomes a
   guarantee rather than a suggestion.

Step 4 is the only one that can mislead a user if it ships alone, which is why
it is last.

## Out of scope

- Axis-gutter price tags.
- Feeding client-authored zones to the `SceneRegion` renderer.
- The two-pass loop for non-mentor questions.
- Strategy/position-engine `entry_price` fields — same name, unrelated type
  (`trades_executed`, `crates/db/src/paper.rs`).
- The pre-existing `load_flow.rs` bot-plan-limit failure.

## Known cost

A mentor question carries 12 images and 2 extra turns against a budget of
`DEFAULT_MAX_TURNS = 6` + `ANSWER_TURNS = 2`. That is the product owner's
explicit choice ("always all six twice"); the guard is that it applies to mentor
mode only.
