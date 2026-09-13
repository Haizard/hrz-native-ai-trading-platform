# 14 — Frontend & Chart Engine

## Purpose
The trading workstation UI: candlestick/footprint/volume-profile chart, order book/DOM,
AI chat panel with chart-region highlighting, strategy editor (three modes), and
account/settings screens.

## Decision gate: Rust/WASM-first vs. React+TypeScript

The source research strongly favors a Rust-first frontend (Leptos/Dioxus + WASM +
WebGPU/Canvas) so the chart engine shares `analytics-core` directly with the backend,
and so footprint/volume-profile rendering with potentially hundreds of thousands of
visual primitives doesn't bottleneck on per-cell DOM elements. That is the target
architecture for this roadmap.

**Pragmatic exception:** if agent/team velocity in Rust web frameworks proves
significantly slower than in React+TypeScript during early prototyping, it is
acceptable to build the **application shell** (panels, forms, settings, strategy
editor, AI chat) in React+TypeScript while keeping the **chart engine itself** as a
Rust/WASM module compiled from `analytics-core` and mounted into the React app via a
canvas/WebGPU element. What must never happen is a *second* implementation of the
trading math in TypeScript — the chart engine, whichever shell hosts it, calls into the
same Rust analytics code, compiled to WASM. Make this decision explicitly and document
it in this file once made; don't let it drift undecided.

## Chart engine architecture (either hosting option)

```
Market data (via WebSocket)
        ↓
Rust/WASM chart engine
        ↓
   ┌────┴─────┐
   │          │
CPU calc   GPU-friendly buffers
   │          │
   └────┬─────┘
        ▼
   Canvas / WebGL / WebGPU
```

- Do not render each footprint cell or volume-profile bar as an individual DOM/UI
  component. Build a compact in-memory scene representation (arrays of positioned
  rectangles/bars with color) and draw it directly via Canvas 2D (simplest, ship first)
  or WebGL/WebGPU (if profiling shows Canvas 2D is insufficient at target data density).
- Reuse `analytics-core` compiled to `wasm32-unknown-unknown` for any client-side
  recomputation (e.g. adjusting a volume-profile bucket size interactively without a
  round-trip) — never reimplement the math in JS/TS.

## Required views
- **Chart**: candlesticks, footprint mode (bid/ask ladder per price level), volume
  profile overlay, VWAP/POC/VAH/VAL lines, delta/CVD sub-panel, drawing tools
  (trendlines, Fibonacci, rectangles — start minimal, expand later).
- **DOM/order book panel**: live bid/ask ladder from `/ws/orderbook/{symbol}`.
- **AI chat panel**: natural-language input, streaming responses via
  `/ws/agent/{session_id}`, action buttons (Analyze / Create Strategy / Backtest /
  Create Bot) matching the source research's UI sketch, and the ability to highlight the
  exact chart region(s) referenced in the AI's explanation (map thesis fields like
  `entry_price`/timestamps back to chart coordinates).
- **Strategy editor**: three modes sharing one underlying `StrategyDocument` — natural
  language (delegates to `/agent/generate-strategy`), visual builder (condition blocks
  composed via UI, serialized to the same schema), and raw DSL (YAML/JSON text editor
  with the validator's errors shown inline).
- **Backtest/bot dashboards**: performance report visualization, trade list, bot
  status/controls (pause/resume/kill).

## Real-time data handling
- Subscribe to `/ws/market/{symbol}/{timeframe}` for the active chart; resubscribe on
  symbol/timeframe change; unsubscribe on unmount to avoid leaking server-side fan-out
  resources.
- Apply client-side interpolation/coalescing for very high-frequency updates if the
  render loop can't keep up — never let the UI thread block waiting on network data.

## Done criteria
- A user can load a symbol, switch to footprint mode, see live-updating footprint cells
  driven by real trade data, ask the AI a question, and see the response's referenced
  price levels highlighted on the chart — all without a full page reload.
- The chosen hosting architecture (pure Rust/WASM vs. React shell + WASM chart module)
  is documented here as an explicit decision with the date and rationale it was made.
