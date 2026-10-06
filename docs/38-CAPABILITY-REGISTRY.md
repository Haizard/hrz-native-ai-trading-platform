# 38 — The capability registry, and the agent's research tools

## Purpose

#16-full (Phase 0) and Phase A. Two gaps in the same seam, closed together.

**Phase 0 — the platform could not say what it can do.** A footprint analysis
on a venue without trades, a delta read from a candle whose buy/sell split
was invented by the venue layer, a gamma question on a platform with no
options data: each was answerable only by trying and failing, or worse, by
succeeding with numbers that mean less than they appear to. The fix is a
registry, not a wrapper per tool: **capabilities declare what they need,
providers declare what they can honestly supply, and a resolver joins the
two into answers with a basis and an explanation.**

**Phase A — the agent's backtest tools were registered but dead.**
`backtest_strategy` and `backtest_similar_setups` had a trait, two tool
implementations, and zero call sites that ever attached a runner; every
invocation answered "no backtest runner is attached". And the agent reasoned
over the *shipped* skill library only: a skill the user published through
`PUT /skills/{id}` was visible to `GET /skills` and invisible to the model.
Both are wiring, and both are now wired.

## The vocabulary (crates/capabilities)

A leaf crate — `analytics-core` is its only internal edge, it builds for
wasm32, and it computes nothing about markets. Four types carry the model:

- `DataKind` — the closed set of data kinds (candles, trades, ticks, book
  snapshots, open interest, funding, liquidations, options chains).
- `SplitQuality` — how much of a candle's buy/sell split is real:
  `Absent < Attributed < Real`. *Attributed* is the name for the
  direction-attributed fallback (`close >= open`) that agrees with the
  candle by construction and is not a measurement.
- `CapabilityDescriptor` — what an analysis needs, as an ordered list of
  `SourceRule`s, best first. Footprint has one rule (trades) and no derived
  rule **on purpose**: a candle-spread footprint is a rendering, not an
  analysis, and refusing beats inventing. Volume profile has two (trades
  true, candles derived-with-caveat). The catalog is `descriptor::STANDARD`.
- `Resolution` — the answer: an `Availability`
  (`available | derived | partial | degraded | unavailable`), a `Basis`
  (`true | derived`) when one exists, the missing kinds, caveats, and a
  one-sentence explanation written for a user or an agent prompt.

Two rows in the catalog are `implemented_by: None` — gamma exposure,
funding/open interest, liquidations. They exist so the answer is
*"not implemented on this platform"*, in words, instead of the capability
being invisible.

## The providers (market-data)

`market_data::exchanges::profile` declares `binance()` and `bybit()` as
**values**, next to the codecs and venue column maps that make them true —
the file says which source line each fact rests on, because a profile kept
anywhere else would drift from the parser it describes. The load-bearing
declarations:

- Binance klines carry `takerBuyBaseAssetVolume` (column 9), so candles have
  a **real** split on both channels; trade history is pageable aggTrades.
- Bybit v5 klines have no taker column (the seventh is turnover), so REST
  candles are **attributed** — declared with the caveat *"not a
  measurement"* — while its live candles are trade-built and real. Trades
  and the book are **live-only**: no history can be fetched.

Fidelity hangs on the *channel* (`KindSupport { live, rest }`), not the
kind, because one venue serves the same kind at two fidelities and a single
field would force one of them to lie.

## The resolver

`Registry::new(descriptor::STANDARD, profile::standard())` — assembled once
in `main.rs` — and `resolve(id, scope)`: first satisfiable rule wins; a rule
is satisfied when every kind it needs is supplied at the split fidelity it
asks for. The resolver is a **pure function over declarations**: whether the
live tape currently holds trades for BTCUSDT is a window fact the resolver
does not have. Phase 1 layers that on at the tool call site as `Degraded`
adjustments; what the registry guarantees is the static truth — a venue that
can never supply trades resolves footprint `Unavailable` no matter what is
in RAM, and one whose trade kind is live-only resolves `Available` *with*
the caveat that says the depth depends on how long the symbol has been
watched.

Concrete `Registry`, not a `CapabilityResolver` trait: one algorithm, one
catalog, and a mocked resolver is how a tool test ends up asserting
availability answers the real registry would never give.

## The report

`GET /capabilities` gains an `analysis` array next to the deployment and
freshness halves: every catalog capability resolved against every declared
(provider, symbol class). "Is the database configured" and "is footprint
available on binance spot" are the two halves of *can I trust what I am
about to look at*, and making a client join two endpoints is how one goes
unread. Additive only: existing fields untouched.

## The agent's backtests (research.rs)

`WindowBacktestRunner` implements `ai_agent::tools::BacktestRunner` in the
gateway — the one crate that already holds both edges (`backtester` may not
depend on `market-data`; `ai-agent` may not depend on `backtester`). Its
rules:

- Candles come from the **window service**, not the database: the agent's
  base rate and the user's chart read the same tape, RAM first, venue
  backfill for the shortfall.
- The venue cap is honest: a window that hit `MAX_VENUE_BARS` says *"the
  start of the window was clamped"* in the summary's note.
- An all-empty input is the backtester's own `MissingData` refusal, enriched
  with which timeframes were empty — a data gap, named as one, never a
  zero-trade verdict.
- The report's `net_return_pct` holds **R multiples** despite its name
  (backtester::report's header owns that history); the summary the agent
  quotes is `net_return_r`, so the number is never read as a compounding
  percent in prose.
- `similar_setups(skill_ref)` scans `strategies/` for the document whose
  `metadata.skill_ref` names the skill — the mapping lives in the documents,
  not in a table that can drift. A skill with no reference document is an
  error that says so and names the skills that have one.

Attached at both agent entry points (`POST /agent/ask`, the agent socket)
from server state, never from the request body — the runner is a deployment
capability, like the drawing writer before it.

## The agent's skills

`provider_routes::merged_library`: the shipped files merged with the asking
user's stored skills, on `GET /skills`' rule — **the user's copy wins by
name**, every owned version kept so `skill_id` pinning still works, an
unreadable row skipped with a warning, a failed query falling back to the
shipped library with a warning (never to an empty library: "we could not
load your skills" must not read as "no skill matched"). Both branches of
`resolve_agent` use it; the boot-time primary agent gets the merged view via
`Agent::with_skills`, which swaps the library without rebuilding the client.

## Tests

- `capabilities`: split-fidelity ordering pinned; wire names pinned; the
  Deriv story end-to-end (footprint unavailable naming `trades`, volume
  profile derived with the uniform-spread caveat, structure analytics
  available); attributed-split delta resolves `Derived`, never `True`;
  unimplemented capabilities explain themselves; unknown ids refuse rather
  than error; catalog self-consistency and unique ids.
- `market-data`: the real profiles resolve the way the audit found — binance
  footprint true, bybit footprint available-with-live-window-caveat, greeks
  unavailable, a spot-only venue refusing perpetuals by name.
- `api-gateway`: the analysis view covers every (provider, class, capability)
  and serializes; the research runner resolves skills to documents, names
  what exists when none matches, refuses a missing directory, ignores
  non-strategy files, and turns an all-empty window into an error that names
  the gap; the skill merge keeps every owned version, drops superseded
  shipped copies, and leaves the shipped library untouched when the user
  owns nothing.

## What this is not

Not yet: **Phase 1** (provenance attached to tool results at the call site,
live-window `Degraded` adjustments, the agent prompt's capability summary);
**Phase 2** (skill frontmatter `requires:` checked against the registry);
per-symbol overrides and per-timeframe answers (the scope carries symbol and
timeframes, unused by static resolution); ingest for funding, open interest,
liquidations (the catalog rows wait, honestly `implemented_by: None`); MCP
exposure of the registry (Phase 6 in the design document,
`reports/architecture-capability-design-2026-10-04.md`).
