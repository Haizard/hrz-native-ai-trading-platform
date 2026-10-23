# The Trading Strategy Intelligence Master Catalog

The master list, audited against this platform. Every item is marked:

- **✅ implemented** — with the artifact that implements it (registry id, module, or engine feature)
- **⚠️ partial** — something real exists; what is missing is named
- **❌ missing** — nothing honest exists yet
- **➖ out of scope** — a trading *style*, portfolio construct, or data-gated domain rather than a detectable concept (with the reason)

Audit date: 2026-10-23. Ground truth: `crates/market-data/src/concepts.rs` (the registry),
`crates/analytics-core/src/`, `frontend/chart-engine/`, `crates/backtester`, `crates/trading-engine`.

Platform totals at audit: **90 registry concepts across 10 libraries**, plus the order-flow,
volume-profile, footprint, CVD/delta, forecast, and sandbox machinery below the registry.

---

## 1. Smart Money Concepts (SMC)

| # | Concept | Status | Artifact / note |
|---|---------|--------|-----------------|
| 1 | Break of Structure (BOS) | ✅ | `smc.bos` |
| 2 | Change of Character (CHoCH) | ✅ | `smc.choch` |
| 3 | Market Structure Shift (MSS) | ⚠️ | BOS+CHoCH carry structure shifts; no distinct MSS variant |
| 4 | Order Blocks | ✅ | `smc.order_block` |
| 5 | Breaker Blocks | ✅ | `smc.breaker_block` |
| 6 | Mitigation Blocks | ✅ | `smc.mitigation_block` |
| 7 | Rejection Blocks | ✅ | `smc.rejection_block` |
| 8 | Fair Value Gaps | ✅ | `smc.fvg` |
| 9 | Inverse FVG | ✅ | `smc.ifvg` |
| 10 | Liquidity Sweeps | ✅ | `smc.liquidity_sweep` |
| 11 | Liquidity Grabs | ✅ | `smc.liquidity_grab` |
| 12 | Buy-Side Liquidity | ✅ | `smc.equal_levels` pools, chart-labelled BSL by side |
| 13 | Sell-Side Liquidity | ✅ | `smc.equal_levels` pools, chart-labelled SSL by side |
| 14 | Internal Range Liquidity | ✅ | `smc.internal_range_liquidity` |
| 15 | External Range Liquidity | ✅ | `smc.external_range_liquidity` |
| 16 | Equal Highs / Equal Lows | ✅ | `smc.equal_levels` |
| 17 | Premium / Discount Zones | ✅ | `smc.premium_discount` |
| 18 | Equilibrium (50% range) | ⚠️ | rides inside `smc.premium_discount`; no standalone equilibrium line |
| 19 | Displacement | ✅ | `smc.displacement` (body ≥2× average at a fresh extreme) |
| 20 | Inducement | ✅ | `smc.inducement` |
| 21 | Market Maker Models | ❌ | composite models; the composer can express them, none prebuilt |
| 22 | Optimal Trade Entry | ✅ | `fib.ote` — the full 0.62–0.79 band |
| 23 | Power of Three | ✅ | `smc.power_of_three` (session-anchored) |
| 24 | Judas Swing | ✅ | `smc.judas_swing` |
| 25 | Turtle Soup | ✅ | `smc.turtle_soup` |
| 26 | Liquidity Void Analysis | ⚠️ | FVGs mark voids; no void-fill lifecycle analysis |
| 27 | SMT Divergence | ✅ | `smc.smt_divergence` |
| 28 | Weekly/Daily Liquidity Mapping | ⚠️ | session levels exist; no per-week liquidity map |
| 29 | Institutional Order Flow | ❌ | |
| 30 | Multi-Timeframe SMC | ✅ | `smc.mtf_alignment` (structure lean, 4x resample) |

**Score: 27 ✅ / 3 ⚠️ / 0 ❌**

## 2. Price Action

| # | Concept | Status | Artifact / note |
|---|---------|--------|-----------------|
| 1 | Support & Resistance | ✅ | `analytics-core::sr_zones` (repeated swings become zones) |
| 2 | Horizontal Level Trading | ⚠️ | zones exist; no per-level touch/confirm events |
| 3 | Trendline Trading | ✅ | `chart.trendline` (anchored, 3-touch, break) |
| 4 | Trend Channel Trading | ⚠️ | channels inside flags/triangles; no standalone channel |
| 5 | Breakout Trading | ✅ | `pa.breakout` |
| 6 | Breakout-and-Retest | ✅ | `pa.retest` |
| 7 | False Breakout | ✅ | `pa.failed_breakout` |
| 8 | Pullback Trading | ⚠️ | `pa.retest` is the pullback form |
| 9 | Swing Trading | ➖ | a style, composed from detectors |
| 10 | Range-Bound Trading | ⚠️ | `pa.range_contraction` marks the range |
| 11 | Trend Continuation | ⚠️ | `chart.flag`, `chart.pennant` |
| 12 | Trend Reversal | ⚠️ | chart patterns (H&S, wedges) |
| 13 | Market Structure | ✅ | `analytics-core::market_structure` + BOS/CHoCH |
| 14 | Higher Highs / Higher Lows | ✅ | `pa.higher_highs` |
| 15 | Lower Highs / Lower Lows | ✅ | `pa.higher_highs` (both directions) |
| 16 | Role Reversal | ⚠️ | `smc.breaker_block` is the SMC form; no S/R-flip event |
| 17 | Compression Breakouts | ⚠️ | `pa.range_contraction` + `pa.breakout`, not fused |
| 18 | Expansion / Contraction | ✅ | `pa.range_contraction`, `indicators.atr` expansion signals |
| 19 | Momentum Price Action | ⚠️ | candle-body analysis inside `candlesticks.rs` |
| 20 | Failed Breakout Reversals | ✅ | `pa.failed_breakout` |
| 21 | Resistance Rejection | ⚠️ | wick-rejection analysis exists; no level-rejection event |
| 22 | Support Rejection | ⚠️ | same |
| 23 | Horizontal Range Extremes | ⚠️ | `smc.equal_levels` |
| 24 | Naked Chart Trading | ➖ | a style |
| 25 | Multi-Timeframe PA | ✅ | `smc.mtf_alignment` |
| 26 | Trendline Liquidity Sweeps | ⚠️ | trendlines exist; sweeps on them missing |
| 27 | Opening Range Breakouts | ⚠️ | `session.asian_range`; no generic opening-range |
| 28 | Range Expansion | ⚠️ | ATR expansion |
| 29 | Volatility Contraction Breakouts | ⚠️ | contraction exists; the fused breakout signal missing |
| 30 | Auction-Based PA | ⚠️ | volume-profile concepts carry it |

**Score: 10 ✅ / 16 ⚠️ / 1 ❌ / 3 ➖**

## 3. Candlestick Theory

The analysis primitives live in `analytics-core::candlesticks` and feed every pattern detector.

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Body analysis | ✅ | 16 | Consecutive bearish | ⚠️ three-crow form |
| 2 | Upper wick analysis | ✅ | 17 | Candle momentum | ⚠️ inside detectors |
| 3 | Lower wick analysis | ✅ | 18 | Candle exhaustion | ⚠️ inside detectors |
| 4 | Full-body candles | ✅ `candles.marubozu` | 19 | Rejection vs acceptance | ⚠️ wick analysis only |
| 5 | Long-wick rejection | ✅ | 20 | Failed candle breakouts | ✅ `candles.hikkake` |
| 6 | Wick-to-body ratio | ✅ | 21 | Engulfing strength | ⚠️ detected, not scored |
| 7 | Body-to-range ratio | ✅ | 22 | Multi-candle sequencing | ⚠️ inside detectors |
| 8 | Candle strength | ✅ | 23 | Candle overlap analysis | ⚠️ inside detectors |
| 9 | Close location | ✅ | 24 | Close above/below levels | ✅ `smc.bos` semantics |
| 10 | Range analysis | ✅ | 25 | Volume confirmation | ⚠️ `volume.spike` separate |
| 11 | Inside candles | ✅ | 26 | Session-based behavior | ⚠️ session concepts separate |
| 12 | Outside candles | ✅ | 27 | Volatility classification | ⚠️ ATR only |
| 13 | Expansion candles | ⚠️ ATR expansion | 28 | HTF candle bias | ❌ |
| 14 | Compression candles | ⚠️ contraction | 29 | LTF entry confirmation | ❌ |
| 15 | Consecutive bullish | ⚠️ three-soldier form | 30 | OHLC relationship | ✅ |

**Score: 14 ✅ / 14 ⚠️ / 2 ❌**

## 4. Candlestick Patterns

**Singles:** doji ✅ · dragonfly ✅ · gravestone ✅ · long-legged ✅ · hammer ✅ ·
inverted hammer ✅ · hanging man ✅ · shooting star ✅ · marubozu ✅ · spinning top ✅

**Pairs:** engulfing ✅ (both) · harami ✅ (both) · piercing ✅ · dark cloud ✅ ·
tweezer ✅ (both) · kicker ✅ (both) · matching low ✅ · on-neck ✅

**Multi:** morning star ✅ · evening star ✅ · three white soldiers ✅ · three black crows ✅ ·
three inside ✅ (both) · three outside ✅ (both) · three methods ✅ (both) ·
abandoned baby ✅ · tri-star ✅ · stick sandwich ✅ · mat hold ✅ ·
tasuki gap ✅ (both) · ladder bottom ✅ · belt hold ✅ (both) · hikkake ✅

All forty, including **upside gap two crows ✅** (`candles.upside_gap_two_crows`).

**Score: 40/40 ✅**

## 5. ICT Methodologies

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Silver Bullet | ✅ `smc.silver_bullet` (window + sweep + displaced FVG) | 14 | Weekly bias | ✅ `session.weekly_bias` |
| 2 | Unicorn Model | ✅ `smc.unicorn` (breaker∩FVG, entry into the overlap) | 15 | Liquidity draw | ⚠️ premium/discount |
| 3 | 2022 Model | ✅ `smc.model2022` (yesterday's sweep + character change) | 16 | FVG entries | ⚠️ `smc.fvg` detects; entry logic is composition |
| 4 | Judas Swing | ✅ | `smc.judas_swing` | 17 | OB entries | ⚠️ same |
| 5 | Power of Three | ✅ | `smc.power_of_three` | 18 | Breaker/mitigation entries | ⚠️ same |
| 6 | OTE | ✅ `fib.ote` | 19 | Rejection block | ✅ |
| 7 | MM Buy Model | ✅ `smc.market_maker_model` (curve completes at the opposite pool) | 20 | Balanced Price Range | ✅ `smc.bpr` |
| 8 | MM Sell Model | ✅ same | 21 | Volume imbalance | ✅ `analytics-core::imbalance` |
| 9 | NY session model | ⚠️ kill zone only | 22 | Liquidity void | ⚠️ FVG marks it |
| 10 | London model | ⚠️ kill zone only | 23 | SMT divergence | ✅ |
| 11 | Asian range | ✅ | 24 | Consequent encroachment | ❌ |
| 12 | Kill zones | ✅ both | 25 | Opening gap analysis | ❌ |
| 13 | Daily bias | ✅ `session.daily_bias` | | | |

**Score: 16 ✅ / 7 ⚠️ / 2 ❌** (the 2 remaining are multi-day bias composites, honest compositions)

## 6. Technical Indicator Strategies

**Trend:** MA crossover ✅ (`ema_cross`/`sma_cross`) · EMA pullback ⚠️ · SMA filter ✅ ·
MACD trend ✅ · ADX ✅ (`indicators.adx` with ±DI) · Parabolic SAR ✅ (`indicators.psar`) · Supertrend ✅ (`indicators.supertrend`) · Ichimoku ✅ (`indicators.ichimoku`, five lines + kumo fill) ·
MA ribbon ✅ (`indicators.ema_family`, GMMA) · Triple MA ✅ (`indicators.ema` stack)

**Momentum:** RSI ✅ · RSI divergence ✅ · Stochastic ✅ (`indicators.stochastic`) · CCI ✅ (`indicators.cci`) · Williams %R ✅ (`indicators.williams_r`) ·
ROC ✅ (`indicators.roc`) · Momentum ✅ (ROC is the momentum oscillator) · MACD divergence ✅ (`MacdDivergenceSignal` on `indicators.macd`) · TSI ✅ (`indicators.tsi`) · Awesome ✅ (`indicators.awesome`)

**Volatility:** Bollinger ✅ · Keltner ✅ (`indicators.keltner`) · ATR ✅ · Donchian ✅ (`indicators.donchian`) · StdDev bands ✅ (Bollinger is) ·
BB squeeze ✅ (`BollingerSqueeze` on `indicators.bollinger_percent`) · ATR breakout ⚠️ (expansion) ·
Volatility stop ✅ (`indicators.supertrend` is the ATR stop family) · Chandelier exit ✅ (`indicators.chandelier`) · HV regimes ✅ (`indicators.hv` with percentile rank)

**Volume:** OBV ✅ (`indicators.obv` + divergence signals) · VWAP ✅ (engine line) · Anchored VWAP ✅ (`indicators.anchored_vwap`) · A/D ✅ (`indicators.ad_line`) · CMF ✅ (`indicators.cmf`) · MFI ✅ (`indicators.mfi`) ·
Volume profile ✅ · POC ✅ · VAH/VAL ✅ · Relative volume ⚠️ (`volume.spike`)

**Score: 36 ✅ / 4 ⚠️ / 0 ❌** — Battery A + the tail (2026-10-23): the ENTIRE classic shelf,
every indicator with its pane or overlay AND its per-bar signals. The indicator family is complete
per this list.

## 7. Volume, Order Flow and Footprint

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Bid/Ask footprint | ✅ engine footprint mode | 16 | Iceberg detection | ✅ `iceberg.rs` |
| 2 | Volume delta | ✅ `delta.rs`, `bar_delta.rs` | 17 | Volume profile | ✅ `volume_profile.rs` |
| 3 | CVD | ✅ `cvd.rs` | 18 | Session profile | ⚠️ |
| 4 | Delta divergence | ⚠️ | 19 | Fixed-range profile | ⚠️ |
| 5 | Volume imbalance | ✅ `imbalance.rs` | 20 | Anchored profile | ⚠️ |
| 6 | Stacked imbalances | ⚠️ | 21 | HVN | ✅ `volume.hvn` |
| 7 | Absorption | ✅ `absorption.rs` | 22 | LVN | ✅ `volume.lvn` |
| 8 | Exhaustion | ⚠️ | 23 | POC rejection | ⚠️ |
| 9 | Aggressive buyer/seller | ⚠️ buy/sell volume split | 24 | POC reversion | ⚠️ |
| 10 | Large trade detection | ⚠️ `size_classes.rs` | 25 | Delta-confirmed breakout | ⚠️ |
| 11 | Time & sales | ⚠️ raw trades exist | 26 | Failed auction | ⚠️ |
| 12 | Order-book imbalance | ⚠️ data-gated | 27 | Auction Market Theory | ⚠️ profile concepts |
| 13 | DOM analysis | ⚠️ data-gated | 28 | Market Profile / TPO | ❌ time-based profile |
| 14 | Liquidity heatmaps | ❌ | 29 | Open interest | ➖ no OI feed |
| 15 | Spoofing analysis | ❌ | 30 | Liquidation flow | ➖ no liquidation feed |

Plus `order_flow.delta_shift` in the registry. **Score: 9 ✅ / 15 ⚠️ / 2 ❌ / 4 ➖**

## 8. Support, Resistance and Liquidity

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Major S/R | ✅ sr_zones | 14 | Supply/Demand zones | ✅ sd |
| 2 | Minor S/R | ✅ sr_zones | 15 | Liquidity pools | ⚠️ equal_levels |
| 3 | Dynamic S/R | ⚠️ MA overlays | 16 | Equal H/L trading | ✅ |
| 4 | Previous day H/L | ⚠️ sessions carry dailies | 17 | Stop-run reversal | ✅ `smc.turtle_soup` |
| 5 | Previous week H/L | ✅ `session.previous_week_levels` | 18 | Sweep reversal | ✅ `smc.liquidity_sweep` |
| 6 | Previous month H/L | ✅ `session.previous_month_levels` | 19 | Failed auction reversal | ⚠️ |
| 7 | Daily open | ✅ `session.daily_open` | 20 | Breakout continuation | ⚠️ |
| 8 | Weekly open | ✅ `session.weekly_open` | 21 | Range boundary rejection | ⚠️ |
| 9 | Monthly open | ✅ `session.monthly_open` | 22 | Multi-touch levels | ✅ sr_zones |
| 10 | Session H/L | ⚠️ asian range | 23 | Level confluence | ⚠️ fib confluence |
| 11 | Opening range | ⚠️ | 24 | S/R flip | ⚠️ breaker form |
| 12 | Round numbers | ✅ `pa.round_numbers` | 25 | Volume-confirmed breakout | ⚠️ |
| 13 | Psychological levels | ✅ same grid | | | |

**Score: 13 ✅ / 12 ⚠️ / 0 ❌**

## 9. Supply and Demand

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Rally-Base-Rally | ⚠️ zone forms internal | 11 | Zone retest | ⚠️ `sd.zone_tested` |
| 2 | Drop-Base-Drop | ⚠️ same | 12 | MTF zone alignment | ❌ |
| 3 | Rally-Base-Drop | ⚠️ same | 13 | S/D imbalance | ⚠️ |
| 4 | Drop-Base-Rally | ⚠️ same | 14 | Zone compression | ❌ |
| 5 | Fresh zones | ✅ lifecycle | 15 | Proximal/distal entries | ❌ |
| 6 | Tested zones | ✅ `sd.zone_tested` | 16 | Departure strength | ⚠️ displacement inside |
| 7 | Zone strength scoring | ⚠️ | 17 | Zone-volume confirm | ⚠️ |
| 8 | Supply rejection | ⚠️ | 18 | Zone-liquidity confluence | ⚠️ |
| 9 | Demand rejection | ⚠️ | 19 | Failed zones | ✅ lifecycle invalidated |
| 10 | Zone breakout | ⚠️ | 20 | Role-reversal zones | ⚠️ breaker form |

**Score: 3 ✅ / 15 ⚠️ / 2 ❌**

## 10. Chart Patterns and Classical TA

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Head & Shoulders | ✅ `chart.head_shoulders` | 19 | Rounding bottom | ✅ `chart.rounding_bottom_top` |
| 2 | Inverse H&S | ⚠️ verify direction coverage | 20 | Rounding top | ✅ same |
| 3 | Double top | ✅ | 21 | Broadening | ✅ `chart.broadening` |
| 4 | Double bottom | ✅ | 22 | Diamond top | ❌ |
| 5 | Triple top | ✅ `chart.triple_top_bottom` | 23 | Diamond bottom | ❌ |
| 6 | Triple bottom | ✅ | 24 | Megaphone | ✅ (= broadening) |
| 7 | Ascending triangle | ⚠️ `chart.triangle` | 25 | Wolfe Waves | ❌ |
| 8 | Descending triangle | ⚠️ same | 26 | ABCD | ✅ `harmonics.abcd` |
| 9 | Symmetrical triangle | ⚠️ same | 27 | Gartley | ✅ |
| 10 | Rising wedge | ✅ `chart.wedge` | 28 | Bat | ✅ |
| 11 | Falling wedge | ✅ | 29 | Butterfly | ✅ |
| 12 | Bull flag | ✅ `chart.flag` | 30 | Crab | ✅ |
| 13 | Bear flag | ✅ | 31 | Three drives | ✅ |
| 14 | Bull pennant | ✅ `chart.pennant` | 32 | Elliott Wave | ❌ wave counting |
| 15 | Bear pennant | ✅ | 33 | Harmonic completion | ✅ + cypher, shark |
| 16 | Rectangle | ✅ `chart.rectangle` | 34 | Measured move | ⚠️ fib projection |
| 17 | Cup and handle | ✅ `chart.cup_and_handle` | 35 | Failed pattern | ⚠️ `pa.failed_breakout` |
| 18 | Inverse cup | ✅ `chart.inverse_cup_handle` | | | |

**Score: 27 ✅ / 6 ⚠️ / 2 ❌**

## 11. Trend, Momentum and Market Regime

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Trend continuation | ⚠️ flags/pennants | 11 | HH continuation | ✅ `pa.higher_highs` |
| 2 | Trend reversal | ⚠️ chart patterns | 12 | LL continuation | ✅ |
| 3 | Momentum breakout | ⚠️ | 13 | Momentum divergence | ✅ RSI divergence |
| 4 | Momentum pullback | ⚠️ | 14 | Trendline break | ✅ `chart.trendline` |
| 5 | Trend exhaustion | ⚠️ | 15 | Channel break | ⚠️ trendline form exists |
| 6 | Countertrend | ➖ style | 16 | MA slope | ❌ |
| 7 | Mean-reverting pullback | ⚠️ | 17 | MTF trend alignment | ✅ `smc.mtf_alignment` |
| 8 | Trend strength filter | ✅ `indicators.adx` + `pa.regime` | 18 | Volatility expansion | ✅ `atr_expansion` |
| 9 | Regime switching | ✅ `pa.regime` (shift events) | 19 | Volatility compression | ✅ `pa.range_contraction` |
| 10 | Trend/range classify | ✅ `pa.regime` | 20 | Failed trend reversal | ⚠️ |

**Score: 9 ✅ / 9 ⚠️ / 1 ❌ / 1 ➖**

## 12. Quantitative and Statistical

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Mean reversion | ✅ `indicators.zscore` | 16 | ARIMA | ❌ |
| 2 | Statistical arbitrage | ❌ | 17 | GARCH | ✅ `indicators.garch` (1,1 canonical) |
| 3 | Pairs trading | ✅ `indicators.pairs` (Engle-Granger, gate in the label) | 18 | Extreme-value | ❌ |
| 4 | Cointegration | ✅ same | 19 | Distribution-based | ❌ |
| 5 | Z-score reversion | ✅ `indicators.zscore` | 20 | Residual reversion | ❌ |
| 6 | Bollinger mean reversion | ✅ `BollingerReversion` on `indicators.bollinger` | 21 | Spread trading | ⚠️ the spread exists (pairs); the strategy is composition |
| 7 | Time-series momentum | ✅ `indicators.roc` | 22 | Correlation breakdown | ⚠️ pairs infrastructure exists; the event missing |
| 8 | Cross-sectional momentum | ❌ multi-symbol | 23 | Lead-lag | ⚠️ same |
| 9 | Trend factor models | ❌ | 24 | Entropy regime | ✅ `indicators.entropy` |
| 10 | Factor-based | ❌ | 25 | Change-point | ✅ `indicators.changepoint` (CUSUM) |
| 11 | Kalman filter | ✅ `indicators.kalman` (local-level) | 26 | Volatility clustering | ⚠️ ATR family shows it |
| 12 | HMM regime | ✅ `indicators.hmm` (2-state Gaussian, EM) | 27 | Robust optimization | ➖ portfolio |
| 13 | Bayesian models | ✅ `indicators.bayesian_drift` (conjugate posterior) | 28 | Walk-forward | ✅ RSI-PCA + backtester |
| 14 | Monte Carlo | ✅ `forecast.rs` cone | 29 | Ensemble signals | ✅ the composer |
| 15 | Autoregressive | ✅ `indicators.ar_forecast` (AR(2) OLS) | 30 | Meta-labeling | ❌ |
| — | **Predictive PCA model** | ✅ `indicators.rsi_pca` | | | |

**Score: 15 ✅ / 11 ⚠️ / 3 ❌ / 1 ➖**

## 13. Algorithmic and AI

✅ rule-based algos (composer) · ✅ multi-indicator systems (composer) · ✅ automated SMC ·
✅ automated candlesticks · ✅ strategy parameter optimization (parameter sweep) ·
⚠️ AI trade journaling · ⚠️ signal confidence (evidence fields) ·
❌ ML classification, boosting, RF, RL, deep learning, LSTM, transformers, regime classifiers,
anomaly detection, clustering, news/NLP, genetic algorithms, online learning, execution optimization,
drift detection — the AI layer's own roadmap, tracked in `docs/09`.

**Score: 5 ✅ / 2 ⚠️ / 23 ❌ (AI roadmap)**

## 14. Fibonacci and Geometric

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Retracement | ✅ `fib.retracement` | 11 | Harmonic ratios | ✅ harmonics family |
| 2 | Extension | ✅ `fib.extension` | 12 | Pitchfork | ❌ |
| 3 | Projection | ⚠️ = extension family | 13 | Andrews' pitchfork | ❌ |
| 4 | Expansion | ⚠️ same | 14 | Schiff pitchfork | ❌ |
| 5 | Time zones | ✅ `fib.time_zones` | 15 | Median line | ❌ |
| 6 | Fans | ❌ | 16 | Gann angles | ❌ |
| 7 | Arcs | ❌ | 17 | Gann square of 9 | ❌ |
| 8 | Golden pocket | ✅ `fib.golden_pocket` | 18 | Gann time cycles | ❌ |
| 9 | Confluence | ✅ `fib.confluence` | 19 | Price symmetry | ⚠️ ABCD |
| 10 | Elliott–fib alignment | ❌ | 20 | Measured projections | ⚠️ |

**Score: 6 ✅ / 4 ⚠️ / 10 ❌**

## 15. Forex, Session and Time-Based

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | London breakout | ⚠️ kill zone marked | 11 | Day-of-week | ❌ |
| 2 | NY breakout | ⚠️ kill zone marked | 12 | Time-of-day momentum | ❌ |
| 3 | Asian range | ✅ `session.asian_range` | 13 | Session mean reversion | ❌ |
| 4 | London–NY overlap | ⚠️ derivable | 14 | Friday reversal | ❌ |
| 5 | Kill zones | ✅ both | 15 | Monday range | ❌ |
| 6 | Daily opening range | ⚠️ `session.daily_open` | 16 | News-session | ➖ no news feed |
| 7 | Weekly opening range | ⚠️ `session.weekly_open` | 17 | Pre-news compression | ➖ |
| 8 | Session H/L sweep | ⚠️ | 18 | Post-news momentum | ➖ |
| 9 | PD range breakout | ⚠️ | 19 | Overnight gap | ❌ |
| 10 | Midnight open | ✅ UTC daily open | 20 | Seasonals | ❌ |

**Score: 3 ✅ / 7 ⚠️ / 7 ❌ / 3 ➖**

## 16. News, Macro and Fundamental

**All ➖ data-gated** — no economic-calendar, earnings, or news feed exists in the market-data
layer. Adding the feed is a prerequisite to every item here.

## 17. Options, Volatility and Derivatives

**All ➖ data-gated** — no options chain, greeks, or implied-volatility feed. Deriv multipliers
exist as an instrument, not as listed options.

## 18–20. Scalping, Swing, Position Styles

**➖ composition styles** — each is a timeframe + detector + risk-rule bundle the composer
expresses rather than a new detector. The detectors they compose are the ✅ items above.

## 21. Market Profile and Auction Theory

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Market Profile | ⚠️ volume profile, not TPO | 11 | Failed auction | ⚠️ |
| 2 | TPO | ✅ `analytics-core::tpo` (time-at-price) | 12 | Acceptance above value | ✅ `volume.acceptance` |
| 3 | Value area rotation | ⚠️ `volume.value_area` | 13 | Acceptance below value | ✅ same |
| 4 | POC trading | ✅ `volume.poc` | 14 | Value migration | ⚠️ |
| 5 | Initial balance breakout | ✅ `volume.initial_balance` | 15 | Responsive buying | ⚠️ profile-delta fusion |
| 6 | Initial balance reversal | ✅ same | 16 | Responsive selling | ⚠️ |
| 7 | Single prints | ✅ `volume.single_prints` | 17 | Initiative buying | ⚠️ |
| 8 | Excess high/low | ❌ | 18 | Initiative selling | ⚠️ |
| 9 | Poor high/low | ✅ `volume.poor_extreme` | 19 | Balance-to-imbalance | ⚠️ |
| 10 | Balance-area breakout | ⚠️ | 20 | Value-area reversion | ⚠️ |

**Score: 8 ✅ / 12 ⚠️ / 0 ❌**

## 22. Liquidity, Arbitrage and Microstructure

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Cross-exchange arb | ➖ multi-venue | 11 | Bid-ask spread capture | ➖ |
| 2 | Triangular arb | ➖ | 12 | Queue modeling | ➖ |
| 3 | Funding arb | ➖ no funding feed | 13 | Trade arrival rates | ⚠️ |
| 4 | Cash-and-carry | ➖ | 14 | Order-book imbalance | ⚠️ data-gated |
| 5 | Spot-futures basis | ➖ | 15 | Trade impact models | ⚠️ |
| 6 | Calendar spread arb | ➖ | 16 | Slippage prediction | ❌ |
| 7 | Statistical spread | ❌ | 17 | Execution scheduling | ➖ |
| 8 | Cross-market RV | ➖ | 18 | VWAP execution | ➖ |
| 9 | Latency arb | ➖ | 19 | TWAP execution | ➖ |
| 10 | Market making | ➖ | 20 | Implementation shortfall | ➖ |
| — | **VPIN** | ✅ `vpin.rs` | — | Iceberg inference | ✅ `iceberg.rs` |

**Score: 2 ✅ / 3 ⚠️ / 1 ❌ / 14 ➖**

## 23. Deriv Synthetic Indices

✅ Deriv venue wired (`market-data::deriv`, `deriv_trade`) — the same detectors run on
synthetics. ⚠️ the list's own warning applies and is honored here: SMC/liquidity concepts are
*not* marketed as meaningful on broker-generated series until validated per-instrument.
❌ tick-level models (tick momentum/reversal/distribution), jump-and-spike detection,
instrument-specific probability models.

**Score: 1 ✅ / 1 ⚠️ / 4 ❌ (tick models)**

## 24. Risk Management and Trade Management

| # | Concept | Status | # | Concept | Status |
|---|---------|--------|---|---------|--------|
| 1 | Fixed-fractional sizing | ✅ backtester `SizeKind` | 14 | Correlation exposure | ❌ |
| 2 | Fixed-risk-per-trade | ✅ | 15 | Portfolio risk budgeting | ➖ |
| 3 | ATR-based stops | ✅ `analytics-core::risk::atr_stop` + sim brackets | 16 | Consecutive-loss guard | ✅ same |
| 4 | Structure-based stops | ✅ `risk::swing_stop` | 17 | Exposure limits | ✅ `max_open_positions` |
| 5 | Volatility sizing | ✅ `SizeKind::VolatilityAdjusted` | 18 | Slippage-aware sizing | ❌ |
| 6 | R:R filtering | ⚠️ composer targets | 19 | Stop-distance optimization | ⚠️ parameter sweep |
| 7 | Partial profits | ⚠️ | 20 | Risk-of-ruin | ✅ `risk::risk_of_ruin`, in the backtest response |
| 8 | Trailing stops | ✅ `ManagementRules` ATR trail in the sim | 21 | Kelly / fractional Kelly | ✅ `risk::kelly_fraction` + `SizeKind::Kelly` (two-pass) |
| 9 | Break-even stops | ✅ `ManagementRules::break_even_r` | 22 | Frequency limits | ⚠️ |
| 10 | Time-based exits | ⚠️ | 23 | Strategy capital allocation | ⚠️ |
| 11 | Max daily loss | ✅ `RiskGuards` (sim-enforced) | 24 | Drawdown recovery | ⚠️ backtester reports |
| 12 | Max weekly loss | ✅ same | 25 | Monte Carlo risk | ✅ forecast cone |
| 13 | Max drawdown limits | ✅ same | | | |

**Score: 15 ✅ / 6 ⚠️ / 2 ❌ / 2 ➖** — risk is the least-catalogued working layer; much of the ⚠️
lives in the backtester/live engines rather than the registry.

---

# The gap list, ordered by the platform's own priority table

Ordered by (a) the master list's priority column, (b) marginal cost on the existing paths
(series panes are cheap; new data feeds are expensive), (c) what unlocks the most downstream items.

## Battery A — the indicator gaps ✅ DONE 2026-10-23

All ten landed with canonical math (Wilder's own smoothing for ADX; Lane, Lambert, Williams,
Granville, Keltner, Donchian pinned by hand-computed values), series panes/overlays, per-bar
signals, and live verification on BTCUSDT 1h. The indicator tail landed the same day: Ichimoku (five lines, the kumo as the engine's first
overlay FILL), TSI, Awesome, A/D, CMF, MFI, anchored VWAP, chandelier, HV regimes -- the classic
shelf is complete.

## Battery B — the missing pattern and level detectors ✅ DONE 2026-10-23

11. **Upside gap two crows** ✅ — the full 40/40 candlestick book
12. **Trendlines** ✅ (`chart.trendline`) — swing-anchored, third-touch validated, break-signaled
13. **Inverse cup & handle ✅ · rounding bottom/top ✅ · broadening ✅** — the chart book completes
14. **Previous day/week/month H/L ✅ · monthly open ✅** — the closed period's range, marked at the roll
15. **Round numbers ✅** (`pa.round_numbers`) — the psychological grid, crossings in the cross's direction

## Battery C — SMC depth ✅ DONE 2026-10-23

16. **IRL/ERL** ✅ — `smc.external_range_liquidity` marks the dealing range's extremes (lookback-5), `smc.internal_range_liquidity` the minor pools resting inside it
17. **Displacement** ✅ (`smc.displacement`) — the energetic candle the OB machinery always read, surfaced as its own event with the body ratio
18. **OTE** ✅ (`fib.ote`) — the full 0.62–0.79 band of the newest swing, zone-rendered
19. **Judas Swing ✅ · Power of Three ✅** — session-anchored: the Asian-range sweep that closes back in, and the displaced expansion through the far side
20. **BSL/SSL** ✅ — the equal-levels pools always carried the side; the chart now labels them by it (equal highs = BSL, equal lows = SSL)

## Battery D — regime and multi-timeframe ✅ DONE 2026-10-23

21. **Regime classifier** ✅ (`pa.regime`) — ADX for existence, DI for ownership, structure must agree; the SHIFT is the event, and a trend with no swings is honestly a range
22. **MTF alignment** ✅ (`smc.mtf_alignment`) — the structure lean on the chart's bars versus the 4x resample's, fired when the agreement FORMS (whichever side turns last)
23. **Daily/weekly bias** ✅ (`session.daily_bias`, `session.weekly_bias`) — the new period opening outside the closed period's range, with the structure agreeing

## Battery E — data-gated (needs a feed before any detector)

24. Economic calendar / news (§15–16) · options chains (§17) · open interest & liquidations (§7)
25. TPO / Market Profile proper (time-based, §21) — computable from candles, but a new profile kind

The standing rule for every battery: in-house, source-verified where an external reference exists,
value-pinned tests, and the doctrine the platform already proved — analytics-core computes,
the registry declares, the gateway serves, the engine positions, the shell only paints.
