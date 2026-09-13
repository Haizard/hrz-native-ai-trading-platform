# 09 — AI Agent System

## Purpose
Orchestrate LLM reasoning over structured market data (never raw calculation), retrieve
relevant Skills, walk multiple timeframes, and produce explainable trade theses and
validated Strategy DSL documents.

## Non-negotiable boundary
The agent **calls tools that return numbers; it never computes the numbers itself.**
If a question requires a calculation not yet exposed as a tool, add the calculation to
`analytics-core` and expose a new tool — never let the LLM approximate it in text.

## Tool registry
Expose `analytics-core` and `strategy-runtime`/`backtester` functionality as
LLM-callable tools with strict JSON schemas:

```
get_candles(symbol, timeframe, limit)
get_footprint(symbol, timeframe, count)
get_volume_profile(symbol, timeframe, range)
get_orderbook(symbol)
get_delta(symbol, timeframe)
get_cvd(symbol, timeframe)
get_vwap(symbol, timeframe)
detect_liquidity(symbol, timeframe)
detect_absorption(symbol, timeframe)
detect_imbalance(symbol, timeframe)
detect_market_structure(symbol, timeframe)
analyze_timeframe(symbol, timeframe)          -> MarketState
analyze_multi_timeframe(symbol, timeframes[]) -> per-timeframe MarketState + synthesis
backtest_strategy(strategy_doc, symbol, range) -> BacktestReport
backtest_similar_setups(skill_ref, conditions) -> historical base-rate stats
create_strategy_document(spec)                 -> validated StrategyDocument (or errors)
```
Each tool's implementation is a thin wrapper calling the Rust functions from
`docs/05-ANALYTICS-ENGINE.md` / `docs/07-BACKTESTING-ENGINE.md` — no logic duplicated in
the agent layer.

## LLM client abstraction
```rust
#[async_trait]
pub trait LlmClient {
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, AgentError>;
}
```
Support tool-calling in the request/response shape; implement at least one provider
first, keep the trait provider-agnostic so others can be added without touching the
orchestration logic.

## Multi-timeframe reasoning pipeline
1. Determine the timeframe ladder relevant to the user's request (default or from the
   active Skill).
2. Call `analyze_timeframe` for each, top-down (e.g. 1D → 4H → 1H → 5M).
3. Synthesize: macro trend/context at the top, narrowing to entry trigger at the bottom
   — mirroring the worked example in `01-ARCHITECTURE-OVERVIEW.md`.
4. If a Skill's rule set is fully satisfied, optionally call
   `backtest_similar_setups()` for a historical base rate before finalizing the thesis.

## The explainable-thesis object
```rust
pub struct TradeThesis {
    pub confidence_pct: f64,
    pub higher_timeframe_checks: Vec<ConditionCheck>,   // ✓/✗ with description
    pub order_flow_checks: Vec<ConditionCheck>,
    pub entry_price: f64,
    pub stop_price: f64,
    pub target_price: f64,
    pub risk_reward: f64,
    pub invalidation: String,
    pub skill_used: Option<String>,
    pub historical_similar_setups: Option<u32>,
    pub historical_win_rate: Option<f64>,
    pub narrative: String,   // natural-language explanation, generated last from the above
}
```
The narrative field is generated **from** the structured fields, never the other way
around — the numbers are ground truth; the prose explains them.

## Strategy generation from natural language
User describes a setup in plain language → agent drafts a `StrategyDocument` → runs it
through `strategy-dsl`'s validator (`docs/06-STRATEGY-DSL.md`) → on validation failure,
agent is given the specific error and retries → on success, offers backtest/create-bot
next steps. The agent must never hand a document straight to the sandbox without this
validation round-trip.

## Multi-agent decomposition (Phase 5 stretch goal, design for it, build incrementally)
Rather than one monolithic agent, structure responsibilities so they can be split into
specialized agents later without a redesign:
- **Market Analyst** — "what's happening" (calls `analyze_*` tools).
- **Strategy Agent** — "does this satisfy the user's stated methodology" (consults
  Skills).
- **Backtesting Agent** — "did this work historically" (calls backtest tools).
- **Risk Agent** — "is this trade acceptable" (consults `docs/15-RISK-COMPLIANCE.md`
  limits).
- **Master/Orchestrator** — combines the above into the final `TradeThesis`.
Phase 5 may implement this as one agent with clearly separated internal steps; splitting
into literal separate agent processes is an optimization for later, not a Phase 5
requirement.

## Done criteria
- A natural-language request produces a `TradeThesis` whose numeric fields are all
  traceable to specific tool calls (log every tool call + its raw result alongside the
  final thesis for auditability).
- A natural-language strategy request produces a validated `StrategyDocument` or a
  clear, specific validation error — never a silently-invalid document.
