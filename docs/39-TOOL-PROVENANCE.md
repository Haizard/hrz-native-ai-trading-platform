# 39 — Tool provenance: every answer carries where it rests

## Purpose

#17-full. docs/38 built the registry that *can* say what a provider honestly
supplies; this phase makes the agent's tool results actually say it, on every
call, in the payload the model reads. Two findings close here:

- **P3** (no capability model consumed at the tool boundary): availability
  was re-derived ad hoc per tool — `trades.is_empty()` here, a footnote there.
- **P4** (`MarketState` carried no provenance): absorption rendered as a
  silent `[]` without trades, the profile's candle fallback was unlabeled
  inside the aggregate, and the only honesty mechanism was a `data_note`
  string patched on at render time.

The rule, restated: **a number without its provenance is an invitation to
overclaim**, and the model overclaims in exactly the direction the user
cannot check.

## The two halves, and why they are separate

A provenance label has two independent inputs, and Phase 1 keeps them in the
crates that can see them:

- **Observed** — what the window held: bar count, trade count, what the
  profile was built from. Computed in `analytics-core` at build time, where
  the input slices are still in hand: `MarketState.provenance`
  (`StateProvenance`). This is the only analytics-core change in the whole
  roadmap, and it is pure: same slices in, same block out.
- **Declared** — what the provider can supply: split fidelity per channel,
  which kinds exist at all. That is a venue fact (attribution happens at the
  venue boundary; a `Candle` carries plain numbers), so it lives in the
  `capabilities` registry and is resolved per request scope.

They meet in `ai-agent`'s `capability_view.rs` — `CapabilityView`, an
`Arc<Registry>` plus the request's (provider, symbol class) — which is also
why the dependency edge runs ai-agent → capabilities and never
analytics-core → capabilities: the leaf that computes cannot cite the
registry that declares, or the two would drift into one tangled claim.

## What changed, concretely

- `MarketState` gains `provenance` (`#[serde(default)]`: rows stored before
  the block existed still load, reading as *provenance unknown*).
- Every `analyze_timeframe` / `analyze_multi_timeframe` render gains a
  `provenance` block instead of the `data_note` patch: observed facts always
  (`window_bars`, `trades`, `profile: trades|candles`), footprint-level
  sections labelled `available`/`unavailable` with the why, and — when a
  view is attached — the split readers (`delta`, `cvd`, `volume_score`)
  labelled `true`/`derived`/`unavailable`.
- Every capability-backed tool result gains a `provenance` field at dispatch
  — one attach point in `ToolRegistry::execute`, not twenty-four edits. The
  block *is* the registry's `Resolution`: the same availability, basis,
  caveats and explanation `/capabilities` serves, so the model and the UI
  never read two different stories. Plumbing tools (memory, drawings,
  backtests) have no capability row and get no block; a failed tool call gets
  none either — the error is already the honest statement.
- The render rules for the split readers: trades present ⇒ `true` (the tape
  is live-built on both current venues); no trades ⇒ the window is history ⇒
  the label is the provider's **REST candle** fidelity — real ⇒ `true`,
  attributed ⇒ `derived` with the venue's own caveat in `why`, absent ⇒
  `unavailable`. That is the Bybit leak closed: a Bybit history window can no
  longer present a direction-attributed delta as a measurement.

## Wiring

The gateway attaches the view on both agent entry points (`POST /agent/ask`,
the agent socket — one is never the quiet path). The scope comes from
`venue_routes::market_scope()`: binance/spot today, hardcoded next to
`KNOWN_VENUES` with the note that says where per-symbol scope begins the day
the Bybit codecs get a deployment switch. A client never names its own venue;
fidelity labels are not user input.

## Tests

- `analytics-core`: provenance records bars/trades/profile basis; a state
  stored before the block existed still deserializes.
- `ai-agent::capability_view`: tool blocks carry the registry's answer;
  plumbing tools get none; no-trades marks footprint sections unavailable
  with the why; a Bybit-shaped registry labels a history window's delta
  `derived` (never `true`) and a live window's `true`; the block stays under
  900 bytes of prompt budget.
- `ai-agent::tools`: `analyze_timeframe` shows the structured block (the
  `data_note` assertion is gone); with a view attached, `get_delta`'s result
  carries the `delta` resolution including the live-window caveat.
- `capabilities`: `Registry::for_tool` joins on `exposed_tool`, with the
  catalog test from docs/38 pinning that every data-backed tool has a row.

## What this is not

Not yet: **Phase 3**'s capability summary table in the system prompt and
per-request tool exposure (the model still sees every tool; it now sees what
each answer rests on); per-channel window-source tracking into `MarketState`
(the split rule infers REST from "no trades", which is exact for both current
venues but is still an inference); `Degraded` adjustments from live freshness
(the registry stays static; freshness is `/capabilities`' data half, and the
join belongs at the host's call site when a consumer needs it); skill-side
capability requirements (Phase 2).
