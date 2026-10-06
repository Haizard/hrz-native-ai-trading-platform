# 46 — Methodology Skill Library: Trader-Defined Chart Analysis Strategies

## Purpose

The skills library is where a trader's chart-analysis strategy becomes a document the
AI agent executes. A trader describes their school of analysis — Smart Money Concepts,
price action, market structure, order blocks — as knowledge + rules + drawing doctrine,
and the agent then **analyzes the chart that way**: it reads the same tools, applies the
skill's rules, and draws the skill's objects (zones as `rect`, levels as `hline`,
structure shifts as `trendline`, trades as `position_long`/`position_short`).

This document is the index of what ships and how the pieces interact.

---

## The shipped library

### Trading methodologies (`skills/trading/`, `kind: trading`)

| Skill | Category | What it encodes |
|---|---|---|
| **Smart Money Concepts** | `smart-money` | Sweep → CHoCH/BOS → return to OB/FVG → opposing pool. Premium/discount, inducement, mitigation lifecycle. |
| **Price Action Levels** | `price-action` | Reactions at levels: rejection / engulfing / failed break vs acceptance; role flips; confluence stacks. |
| **Market Structure Mapping** | `market-structure` | HH/HL vs LH/LL vs range; BOS vs CHoCH; trend phases; HTF alignment. |
| **Order Block Analysis** | `order-blocks` | OB identification from the BOS leg, freshness lifecycle (fresh/mitigated/invalidated), entry models, rectangle drawing doctrine. |
| **Trendline Method** | `trendlines` | Swing-anchored trendlines, pullback entries, break handling. |
| **Support & Resistance Levels** | `levels` | Four level sources (swings, liquidity, value edges, VWAP), confluence, role flips. |
| **Fibonacci Retracement** | `fibonacci` | Leg-anchored fibs, golden-pocket confluence, extension targets. |
| Liquidity Sweep + Absorption | `liquidity` | (pre-existing) Sweep + reclaim + absorption. |
| Footprint Absorption | `footprint` | (pre-existing) Tick-data absorption methodology. |

### Tool doctrine (`skills/tool/`, `kind: tool`)

Seven families, one per tool family; doctrine attaches to the model **through the tool
result** the first time the family is used. `chart-drawing.yaml` v2.0 now carries the
full kind-per-pattern vocabulary: `hline` levels, `rect` zones (OB/FVG/supply-demand),
`trendline`/`ray` slopes, `fib`/`fib_extension` measurements, `position_long`/
`position_short` setups, `measure`/`dateprice_range` ranges, and the parity shapes.

---

## How a chart-analysis skill drives the agent

```
Trader writes skill (UI or YAML)            Agent answers "analyze BTCUSDT with my SMC skill"
        │                                                    │
        ▼                                                    ▼
  knowledge: what SMC is                    1. Skill pinned via skill_id (or retrieved by terms)
  rules: analysis sequence                  2. Capability contract checked vs venue
  + DRAW rules (kind per pattern)           3. Ladder read (1d→4h→1h→15m) before turn 1
        │                                   4. Skill render injected into system prompt
        ▼                                   5. Agent runs the rule sequence:
  POST /skills (validated)                       detect_liquidity → detect_market_structure
  capability ids checked against                 → get_candles → create_drawing …
  the registry; tool names                       6. Drawings land on the user's chart,
  checked against the tool registry                 created_by = "ai", toggleable via AI layer
```

### The drawing doctrine, concretely

Every methodology skill states **which drawing kind expresses which pattern**:

| Pattern | `create_drawing` kind | Label convention |
|---|---|---|
| Liquidity level / range edge | `hline` | `68,500 swing low + sellside` |
| Order block / FVG / supply-demand zone | `rect` | `Bullish OB 1H 68200-68500 (fresh)` |
| BOS / CHoCH / swing map | `trendline` or `ray` | `BOS 1H @ 69,200` |
| Measured retracement / targets | `fib` / `fib_extension` | `4H fib 64200-71800` |
| The trade itself | `position_long` / `position_short` | `E 67300 / S 66850 / T 69000 (2.6R)` |

Two invariants hold everywhere:

1. **Anchors come from tool results** — swing prices from `detect_market_structure`,
   candle ranges from `get_candles`, liquidity prices from `detect_liquidity`. Never
   from the picture.
2. **Unit conversion is stated, not assumed** — `get_candles` field `t` is nanoseconds;
   drawing anchors are milliseconds; the doctrine says divide by exactly 1,000,000.

### Lifecycle rules

Methodologies with object lifecycles (OB fresh → mitigated → invalidated; trendline
intact → broken) require the agent to `update_drawing` (relabel with state) or
`delete_drawing` (with a reason) as the market evolves — a stale zone on the chart is a
false claim.

---

## Trader authoring flow

The Skills tab (`frontend/app/index.html` → `pane-skills`) offers:

1. **Template selector** — Blank / Smart Money Concepts / Price Action Levels /
   Market Structure Mapping / Order Block Analysis. A template fills category,
   knowledge, rules, timeframes and capability checkboxes as an editable draft.
2. **Capability checkboxes** — the data contract (`capability_requirements.required`):
   market structure, liquidity, absorption, zones, volume profile, VWAP, delta/CVD,
   chart drawing. The agent refuses the skill honestly on a venue that cannot supply
   a required capability instead of hallucinating the missing read.
3. **Free-form fields** — name, knowledge, rules (one per line; `DRAW …` rules use the
   `create_drawing` kinds), timeframes, markets, max risk %.

`POST /skills` validates capability ids against the capability catalog and tool names
against the tool registry; `PUT /skills/{id}` appends the next version (append-only —
a past thesis stays explainable against the version that produced it).

---

## Multi-panel and snapshot interplay

The existing multi-pane layout (`createChartPane`, `.chartRow` grid) and the engine's
scene/snapshot machinery stay untouched: drawings the agent creates are stored rows
(`db::drawings`), so they render on **every** pane showing that symbol — the trader can
watch the agent's 4H range lines and 15m OB rect materialize on their respective panels.
Per-symbol scoping means a BTCUSDT thesis never leaks onto an ETHUSDT pane.

---

## Verification

- `cargo test --package ai-agent --lib skills` — 17/17 pass, including the shipped
  library golden test (7 tool skills + 9 trading skills, all capability ids and tool
  names checked against the real registries).
- `cargo test --package capabilities` — 18/18 pass with the new `chart_drawing`
  capability row.
- `cargo check --package api-gateway` — clean.
- `node --check frontend/app/app.js` — syntax valid.

## Related docs

- `docs/09-AI-AGENT-SYSTEM.md` — the agent loop this plugs into
- `docs/40-SKILL-SCHEMA-V2.md` — the schema these skills use
- `docs/42-CHART-TOOL-SKILLS-USE-CASE.md` — earlier use-case walkthrough
- `docs/45-ADVANCED-CHART-ANALYSIS-SYSTEM.md` — multi-panel/snapshot design notes
