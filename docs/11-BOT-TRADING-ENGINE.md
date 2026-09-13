# 11 — Bot Trading Engine (Paper & Live Execution)

## Purpose
Take an approved `StrategyDocument` and run it continuously against live market data —
first in simulation (paper), later with real order placement (live), always through the
same `strategy-runtime`/sandbox path used by the backtester (principle #5).

## Paper trading (Phase 6)
- Subscribe to live `MarketState` updates for the strategy's declared symbol/timeframes
  (via the Market Data Engine's pub/sub).
- Feed each closed candle into the sandboxed strategy exactly as the backtester does.
- On a `Signal`, simulate order placement: track a simulated position, apply a
  configurable slippage/fee model, update simulated PnL.
- Persist every simulated trade and every `on_candle` decision (including "no signal")
  for later audit and for the AI agent's "why did it lose money" investigations.

## Live trading (Phase 8, gated)
- Same pipeline as paper trading, except `Signal`s are routed to a real
  `ExchangeAdapter` implementing order placement, cancellation, and fill reconciliation.
- Live trading for a given venue is only enabled after: (a) the strategy has a paper
  track record over a minimum configured window, (b) the user explicitly opts in per
  venue and per strategy, (c) the Risk Engine's limits are configured and active.

```rust
#[async_trait]
pub trait ExchangeAdapter {
    async fn place_order(&self, order: OrderRequest) -> Result<OrderAck, ExecutionError>;
    async fn cancel_order(&self, order_id: &str) -> Result<(), ExecutionError>;
    async fn reconcile(&self) -> Result<Vec<OrderStatus>, ExecutionError>;
}
```

## Risk engine (shared by paper and live, always active)
- Per-trade risk cap (from the strategy's own `risk.max_risk_pct`, clamped to a platform
  hard ceiling — see `docs/06-STRATEGY-DSL.md` validator).
- Per-account daily/weekly loss limits.
- Max concurrent open positions.
- A manual and automatic **kill-switch**: on breach of any limit, halt new signal
  execution for the affected strategy/account and notify the user; existing positions
  are handled per a documented (not silent) policy — e.g. close, or hold with alert,
  configurable per user preference.

## Idempotency & reconciliation (live only)
- Every order carries a client-generated idempotent order ID so retried requests after a
  network blip never double-place an order.
- A reconciliation job periodically compares the platform's view of open
  orders/positions against the exchange's and raises an alert on any mismatch — never
  silently "trusts" its own in-memory state as authoritative for real money.

## Notifications & audit log
- Every simulated or real trade, and every risk-limit breach, is logged to an
  append-only audit trail (`docs/13-DATABASE-SCHEMA.md`) and optionally pushed to the
  user (in-app notification at minimum; email/webhook are later additions).

## Done criteria (Phase 6 — paper trading)
- The Phase 3 sample strategy runs as a paper bot continuously for ≥48h against live
  data without crashing, produces simulated trades consistent with what a manual replay
  of the same period through the backtester would produce, and respects configured risk
  limits (verified by deliberately configuring a tight limit and confirming the
  kill-switch fires).

## Done criteria (Phase 8 — live trading, additional)
- A funded test account places and reconciles at least one real order end-to-end, and an
  injected network failure during order placement does not result in a duplicate order.
