# 40 — Skill schema v2: data contracts, tool doctrine, capability-aware selection

## Purpose

#17-full. Phase 0 gave the platform a registry that *can* say what data a
venue honestly supplies; Phase 1 made tool results carry that answer. Phase 2
extends it to **methodology**: a skill now declares the data it leans on, and
selection refuses a skill — by name, with the gap stated — when the venue
cannot support it. The audit's P2 is closed: asking about a symbol on a
class with no tape refuses footprint skills instead of letting the model
quote a silent empty footprint as evidence.

## Schema v2

`Skill` gains four fields, all `#[serde(default)]` — every v1 document parses
unchanged and means what it always meant (candle-level methodology, no
declared data requirements), and v2 fields are ignored by old readers since
the loader never denied unknown fields:

- `kind: trading | tool` — methodology vs doctrine. Deliberately **not**
  named `category`: the existing `category` (`liquidity`, `footprint`) is the
  topical grouping retrieval scores on, and repurposing it would silently
  de-scope every existing document.
- `artifact_kind: thesis | strategy-dsl | pine-script | monitor` — what
  following the skill produces. The caller uses it, not the scorer: relevance
  and product are different questions.
- `capability_requirements: {required, preferred, fallback}` — the
  data-awareness contract. `required` ids must resolve `available` (or a
  `fallback` rule must accept the `derived` answer); `preferred` misses
  surface as gaps, never refusals; `fallback: [{needs, accept: derived}]` is
  the document's own opt-in to estimates, never the resolver's assumption.
- `applies_to.tools: [...]` — the family a tool skill covers.

A skill's render carries a one-line `DATA CONTRACT:` so a model following it
knows what it needs before it starts reasoning.

## Selection = capability filter after scoring

`select_skill` ranks as before, then takes the first candidate that is a
`trading` skill *and* passes `CapabilityView::check_skill`. Three honesty
rules, all pinned by golden tests in `agent.rs`:

1. A **pinned** skill whose contract refuses is an error naming the gap —
   pinning chooses methodology, it does not license claims the venue cannot
   support.
2. A retrieval where **every** candidate refused is an error listing each
   refusal — "no skill matched" would misreport a data refusal as an absence
   of methodology.
3. **No view attached** (tools-only builds) means no checks — the v1
   behaviour exactly, the same absent-means-absent posture as the tool layer.

Eligible-with-gaps skills run; their gaps are listed in the prompt next to
the skill they qualify, before the model reasons. Caveats on an *available*
resolution (live-window only, attributed history) are **not** copied into the
gaps — they already ride every tool result's provenance block (docs/39), and
repeating them would show the model the same sentence twice per turn. The
static/dynamic boundary from docs/38 holds: a venue *with* a live tape
resolves footprint `available` even when its history is thin; the refusal
case is a class with no data at all.

## Tool doctrine attaches through the tools

Seven tool skills ship under `skills/tool/` (footprint, delta, volume
profile, structure/liquidity, chart drawing, research, memory — §18.2's
coherent families). They are never selected as thesis methodology; they
reach the prompt **through the tools**: the first time a family is exercised
in a turn, `run_tool` injects the skill's rules into the tool result as
`doctrine`. Doctrine arrives exactly when it applies, a question that never
touches the family never pays for it, and a shown-set keeps a busy family
from repeating itself — prompt-stuffing by another door.

## Write-time validation

`/skills` writes now validate the v2 references against their vocabularies
(`skills_routes::validate_skill`, following the file's existing 400+coded
error convention): unknown capability id ⇒ `SKILL_CAPABILITY_UNKNOWN` with
the catalog listed; unknown tool name ⇒ `SKILL_TOOL_UNKNOWN`. A typo'd
requirement would otherwise refuse at runtime indistinguishably from a real
data gap; a typo'd tool name is doctrine that never fires.

The two shipped trading skills moved to `skills/trading/` (loading is
recursive) and carry their contracts: absorption *requires* `absorption`
(its central test reads the tape), the sweep *requires* `liquidity` and
*prefers* `absorption` — its pre-existing "mark UNKNOWN when tick data is
unavailable" rule, now machine-readable. A golden test in `skills.rs` loads
the real `skills/` directory and pins: 9 documents parse, 7 tool families
cover each tool exactly once, both trading skills declare contracts, and
every referenced id is registered.

## Tests

- `skills.rs`: v1 defaults; v2 tool-skill parsing with contract render;
  `tool_doctrine` lookup; the shipped-library golden test.
- `capability_view.rs`: contract verdicts — no-contract runs anywhere,
  missing requirement refuses with the gap named, live-tape satisfies
  statically, derived refuses unless the document accepts it, preferred
  misses are gaps.
- `agent.rs`: pinned refusal names the gap; retrieval skips refused for
  eligible; all-refused errors honestly; no view = v1 behaviour; a tool
  skill is never thesis methodology; doctrine rides the first result only.
- `skills_routes.rs`: 400-coded refusals for unknown capability ids
  (including in `fallback`) and unknown tools; a valid v2 document passes.

## What this is not

Not yet: **Phase 3**'s capability summary in the system prompt and
per-request tool *exposure* (the model still sees every tool; selection and
doctrine are honest, exposure is still flat). The gated AI-proposed skill
flow (§24.4) is unchanged. Degraded-by-freshness adjustments remain the
host's call-site concern.
