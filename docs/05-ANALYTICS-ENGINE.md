# 05 — Analytics Core (Order-Flow & Indicator Engine)

## Purpose
One pure-Rust, I/O-free library implementing every trading calculation the platform
needs. This crate is the single source of truth referenced by principle #1 in
`00-VISION-AND-PRINCIPLES.md` — it must compile to native and to `wasm32-unknown-unknown`
unchanged.

## Hard constraints
- No `async`, no network, no filesystem, no database access in this crate.
- All functions are pure: given the same input data, always return the same output.
- Every function has unit tests with hand-verifiable or reference-checked expected
  values (golden files), not just "does it run."

## Required calculations

### Delta & CVD
- **Delta** per candle: `buy_volume - sell_volume` (classified via trade aggressor side,
  i.e. `is_buyer_maker`).
- **CVD** (Cumulative Volume Delta): running sum of delta across candles in a session or
  window; must support session reset boundaries.

### VWAP
- Standard volume-weighted average price, with support for anchored VWAP (from a
  specific timestamp, e.g. session open).

### Volume Profile
- **POC** (Point of Control): price level with highest traded volume in the window.
- **VAH/VAL** (Value Area High/Low): boundaries of the value area (typically 70% of
  volume) around the POC.
- **HVN/LVN** (High/Low Volume Nodes): local maxima/minima in the volume-by-price
  histogram.
- Must support both fixed-range and rolling/session-based profiles.

### Footprint
- Per-candle, per-price-level breakdown of bid vs. ask executed volume (from trades
  bucketed by price level within the candle's range).
- Output structure:
```rust
pub struct FootprintCell {
    pub price_level: f64,
    pub bid_volume: f64,
    pub ask_volume: f64,
    pub delta: f64,
}
pub struct FootprintCandle {
    pub candle: Candle,
    pub cells: Vec<FootprintCell>,
    pub imbalances: Vec<ImbalanceEvent>,
}
```

### Imbalance detection
- Diagonal/stacked bid-ask imbalance detection at configurable ratio threshold
  (e.g. 300%), per the footprint cell data.

### Absorption detection
- Identify price levels where large opposing volume was absorbed without the expected
  price move (e.g. heavy sell volume at a low that fails to make a new low, accompanied
  by rising delta) — this is a named, testable function, not a vague heuristic:
  `detect_absorption(candles: &[FootprintCandle], config: AbsorptionConfig) -> Vec<AbsorptionEvent>`.

### Liquidity detection
- Equal highs/lows, swept liquidity, resting liquidity pools above/below recent
  swing points.

### Market structure
- Swing high/low detection (configurable lookback).
- Break of Structure (BOS) / Change of Character (CHoCH) classification.

### Classic indicators
- EMA, SMA, RSI, ATR — included mainly for completeness/parity and because they're
  useful building blocks inside user-authored Strategy DSL conditions.

## Public API shape

Every calculation should be exposed as a stateless function or a small stateful
"engine" struct with a clear `update`/`result` pair, e.g.:

```rust
pub fn calculate_delta(candle: &Candle) -> f64;
pub fn calculate_cvd(candles: &[Candle], reset_at_session: bool) -> Vec<f64>;
pub fn calculate_volume_profile(trades: &[Trade], bucket_size: f64) -> VolumeProfile;
pub fn build_footprint(candle_trades: &[Trade], bucket_size: f64) -> FootprintCandle;
pub fn detect_imbalances(footprint: &FootprintCandle, ratio_threshold: f64) -> Vec<ImbalanceEvent>;
pub fn detect_absorption(candles: &[FootprintCandle], config: AbsorptionConfig) -> Vec<AbsorptionEvent>;
pub fn detect_liquidity_levels(candles: &[Candle], lookback: usize) -> Vec<LiquidityLevel>;
pub fn detect_market_structure(candles: &[Candle], config: StructureConfig) -> MarketStructure;
```

## The `MarketState` aggregate object

This is what the AI Agent Engine's tools actually return — a single structured snapshot
combining the above, matching the shape sketched in the source research document:

```rust
pub struct MarketState {
    pub symbol: String,
    pub timeframe: String,
    pub price: f64,
    pub delta: f64,
    pub cvd: f64,
    pub poc: f64,
    pub vah: f64,
    pub val: f64,
    pub volume: f64,
    pub buy_volume: f64,
    pub sell_volume: f64,
    pub imbalances: Vec<ImbalanceEvent>,
    pub absorption: Vec<AbsorptionEvent>,
    pub liquidity: Vec<LiquidityLevel>,
    pub swing_highs: Vec<f64>,
    pub swing_lows: Vec<f64>,
}
```

## Testing strategy
- Golden-file tests: check in small, fixed OHLCV/trade datasets with pre-computed
  expected outputs (compute the reference values independently, e.g. by hand or with a
  well-known reference implementation, before writing the assertion).
- Property tests (e.g. via `proptest`): CVD of a full session should equal the sum of
  per-candle deltas; VAH ≥ POC ≥ VAL always holds; footprint cell volumes sum to the
  candle's total volume.
- Cross-target test: run the full suite compiled to `wasm32-unknown-unknown` via
  `wasm-bindgen-test` in addition to native, to catch any accidental platform-specific
  behavior (e.g. floating-point differences) early.

## Done criteria
- Every function above implemented, documented with a doc-comment example, and covered
  by golden-file + property tests.
- Crate builds for both `native` and `wasm32-unknown-unknown` with zero warnings.
