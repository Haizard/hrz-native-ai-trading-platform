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
    fn venue(&self) -> &str;
    async fn place_order(&self, order: OrderRequest) -> Result<OrderAck, ExecutionError>;
    async fn cancel_order(&self, order_id: &str) -> Result<(), ExecutionError>;
    async fn reconcile(&self) -> Result<Vec<OrderStatusReport>, ExecutionError>;
}
```

`venue()` is required rather than defaulted: it is written into every `live_orders` row
and compared against the opt-in list, and a default of `"unknown"` would let an adapter
record orders against a venue nobody opted in to.

**The pipeline, one hop different.** `LiveBot` (`crates/trading-engine/src/live.rs`)
asks the strategy exactly when `PaperBot` does — the same ladder, the same engine, the
same risk checks, and the same `PositionView`, because a live bot and a paper bot must
not differ in what the strategy can *see*, only in what a signal *becomes*. A live
decision blocks the bot's candle loop while it places, deliberately: spawning it and
carrying on would let the bot take a second entry while the first is still in flight,
which is the concurrency limit defeated by scheduling.

**Protective orders are not bookkeeping.** An entry is placed with its stop and, when
the document declares one, its take-profit. If reconciliation finds a position with no
protective order on the exchange, the bot **closes at market** and says so. A
reconciliation mismatch is a report; an unprotected position is a loss waiting to
happen, and the two are not the same severity.

**Order placement never touches the sandbox.** `docs/15` forbids the sandbox from seeing
a credential, and nothing here changes that: the sandbox produces a `Signal`, and
everything in `execution.rs` happens afterwards, on the outside, driven only by it.

## Risk engine (shared by paper and live, always active)
- Per-trade risk cap (from the strategy's own `risk.max_risk_pct`, clamped to a platform
  hard ceiling — see `docs/06-STRATEGY-DSL.md` validator).
- Per-account daily/weekly loss limits.
- Max concurrent open positions.
- A manual and automatic **kill-switch**: on breach of any limit, halt new signal
  execution for the affected strategy/account and notify the user; existing positions
  are handled per a documented (not silent) policy — e.g. close, or hold with alert,
  configurable per user preference.

The manual half is `POST /bots/{id}/kill`. It trips the switch and **liquidates now**
rather than on the next decision bar: the switch is normally read inside the decision
loop, which for a five-minute strategy is five minutes away, and a button labelled
"stop" that takes five minutes is not a stop. `docs/15` carries the full behaviour.

## Idempotency & reconciliation (live only)
- Every order carries a client-generated idempotent order ID so retried requests after a
  network blip never double-place an order.
- A reconciliation job periodically compares the platform's view of open
  orders/positions against the exchange's and raises an alert on any mismatch — never
  silently "trusts" its own in-memory state as authoritative for real money.

### The rule, stated as three branches rather than a retry policy

A retry policy is what *creates* duplicate orders. The hard case is a request that
reached the exchange whose response did not reach us, because at that moment the platform
does not know whether it has a position. `OrderGateway` therefore never guesses:

1. **The exchange has it** — adopt their acknowledgement. Do not place anything.
2. **The exchange does not have it** — the request genuinely did not arrive. Retry
   **once**, with the *same* id, which is what makes the retry safe.
3. **The exchange cannot be asked** — **stop** and return the error. Not knowing is not
   permission to place again.

The id is `{bot}_{symbol}_{base36(nanos)}_{intent}`, derived from the decision, so a
retry regenerates it. Worst case 35 characters, against Binance's 36-character limit —
enforced by construction and pinned by a test using the worst case for every component,
because the first version used decimal nanoseconds and produced 39.

`live_orders` is the second line of defence: `client_order_id` is its primary key and
the insert is `ON CONFLICT DO NOTHING`, so even if two tasks raced on one decision only
one placement is recorded. `crates/trading-engine/tests/live_flow.rs` proves the whole
thing end to end against a venue double that dedups by client id exactly as Binance
does: an injected failure that the exchange never saw places exactly one order, and one
it *did* see places none.

### Reconciliation reads our book, not theirs

`live_orders` records what the platform believes it sent. Filling it from the exchange's
answer would make every comparison trivially clean, which is the opposite of the point.
The venue's report *updates* a row; it does not create one. Terminal orders are excluded
from the mismatch count, because open-order lists do not contain history and a filled
order's absence is normal — getting that wrong makes reconciliation cry wolf on every
completed trade.

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
