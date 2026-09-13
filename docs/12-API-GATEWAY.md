# 12 — API Gateway

## Purpose
The single REST + WebSocket surface the frontend talks to. Owns authentication, request
routing to internal engines, and real-time fan-out of market/AI data. Built with Axum +
Tokio.

## REST endpoints (representative, extend as needed — document additions here as built)

```
POST   /auth/register
POST   /auth/login
GET    /auth/me

GET    /symbols
GET    /candles?symbol=&timeframe=&from=&to=
GET    /orderbook?symbol=

GET    /skills
POST   /skills
GET    /skills/{id}
PUT    /skills/{id}          # creates a new version, never mutates in place

POST   /strategies                       # create from raw DSL or from agent output
GET    /strategies/{id}
POST   /strategies/{id}/validate
POST   /strategies/{id}/backtest
GET    /backtests/{id}

POST   /bots                             # create paper or live bot from a strategy
GET    /bots/{id}
POST   /bots/{id}/pause
POST   /bots/{id}/resume
DELETE /bots/{id}

POST   /agent/ask                        # natural-language request -> TradeThesis
POST   /agent/generate-strategy          # natural-language -> StrategyDocument
```

## WebSocket channels
```
/ws/market/{symbol}/{timeframe}   -> live candle + MarketState updates
/ws/orderbook/{symbol}            -> live order book deltas
/ws/agent/{session_id}            -> streaming AI chat/thesis responses
/ws/bots/{bot_id}                 -> live bot status/trade events
```
Use binary framing (e.g. a compact serialization like MessagePack or protobuf) for
high-frequency market channels; JSON is fine for the lower-frequency agent/bot channels.

## Auth
- JWT-based session auth for Phase 1 simplicity; keep the auth middleware isolated so it
  can be swapped for OAuth/SSO later without touching route handlers.
- Every route handler receives an authenticated `UserContext`; there is no
  unauthenticated access to any endpoint beyond `/auth/*` and public market data reads
  (if the product decides to allow anonymous chart viewing — decide explicitly, don't
  default to open).

## Rate limiting & backpressure
- Per-user rate limits on `/agent/*` endpoints specifically (LLM calls have real cost).
- WebSocket fan-out must apply backpressure per-connection (drop or coalesce ticks for a
  slow client) rather than allowing one slow consumer to back up the whole broadcast
  pipeline.

## Error contract
- Consistent error envelope across REST and WS:
```json
{ "error": { "code": "STRATEGY_VALIDATION_FAILED", "message": "...", "details": {...} } }
```
- Validation errors from `strategy-dsl` (`docs/06-STRATEGY-DSL.md`) are surfaced with
  their specific field-level detail, not flattened to a generic message — the frontend
  strategy editor and the AI agent's retry loop both depend on this detail.

## Done criteria
- OpenAPI (or equivalent) spec generated from the route definitions and kept in sync via
  a CI check.
- Load test confirms WebSocket fan-out to N concurrent clients does not degrade the
  Market Data Engine's own ingestion latency.
