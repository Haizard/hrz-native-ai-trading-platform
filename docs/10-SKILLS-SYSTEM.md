# 10 — Skills System (Persistent Trader Methodology)

## Purpose
Store the user's trading methodology as versioned, structured, retrievable documents —
never as an ever-growing system prompt. This is principle #7.

## Storage format
```
skills/
├── liquidity/
│   ├── sweep.yaml
│   └── equal-high.yaml
├── footprint/
│   ├── absorption.yaml
│   └── imbalance.yaml
├── market-structure/
│   ├── bos.yaml
│   └── choch.yaml
└── risk/
    ├── fixed-risk.yaml
    └── rr.yaml
```

## Skill document shape
```yaml
name: "Liquidity Sweep + Absorption"
version: "2.1"
category: "liquidity"
knowledge: >
  A liquidity sweep occurs when price briefly breaks a prior swing high/low to trigger
  resting stop orders, then reverses. Combined with absorption (heavy opposing volume
  failing to continue price), this often marks a high-probability reversal point.
rules:
  - "Higher timeframe (4H) structure must be bullish for long setups."
  - "Sell-side liquidity must be swept before entry consideration."
  - "Positive delta must appear on the entry timeframe after the sweep."
  - "Absorption must be detected at the sweep level."
conditions:
  timeframes: ["4h", "1h", "5m"]
  risk:
    max_risk_pct: 1.0
examples:
  - description: "BTCUSDT 2024-03-12: swept 71,200 low, absorption, reclaimed, +2.8R"
invalidation:
  - "5m close below the swept low with no reclaim within N candles."
preferred_markets: ["BTCUSDT", "ETHUSDT"]
preferred_timeframes: ["4h", "1h", "5m"]
```

## Versioning
- Skills are versioned (`2.1` above) so the platform can compare whether a revised
  version of a skill actually improves backtested performance (`backtest_similar_setups`
  in `docs/09-AI-AGENT-SYSTEM.md` can be run per-version).
- Never mutate a skill in place when the user "improves" it — write a new version and
  keep history, so past theses remain explainable against the skill version that
  produced them.

## Retrieval
- Skills are retrieved **contextually** per request — by category match, explicit
  reference (`skill_ref` in a `StrategyDocument`), or the market/timeframe the user is
  currently discussing — not injected wholesale into every agent call.
- Implement retrieval as a straightforward metadata/category filter first (category,
  preferred_markets, preferred_timeframes); only reach for embeddings/semantic search if
  the skill library grows large enough that keyword/category filtering stops being
  precise enough.

## Guardrail: the agent adapts to the user's methodology, it doesn't invent its own
If the user has no skill covering, say, RSI-based entries, the agent must not introduce
RSI reasoning on its own initiative — it should reason strictly within the retrieved
skills' vocabulary and rules, or explicitly tell the user no matching skill exists for
what they're describing and ask whether to define one.

## Persistence
Skills live in the database (see `docs/13-DATABASE-SCHEMA.md`, `skills` table) once
past the local-file prototyping stage, versioned per row, scoped per user.

## Done criteria
- Skill CRUD + versioning implemented and unit-tested.
- Given a natural-language request, the agent retrieves the correct skill(s) for the
  mentioned market/category and cites the skill name+version in its resulting
  `TradeThesis`.
