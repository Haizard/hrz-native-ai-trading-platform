# Type 2: Structure-Anchored Volume Profile

Type 2 segments a chart's auction at confirmed market-structure breaks. Each
completed segment keeps its own price-by-volume histogram, POC, value-area
boundaries, and BOS/CHoCH marker; a new live histogram starts at the break.
The implementation uses `analytics_core::market_structure`, so swing points
are confirmed before a break can anchor a profile and the same detector is used
by other platform features.

## Data semantics

- Binance candle volume is distributed evenly across each candle's high-low
  range. This is an OHLCV estimate, not tick-level volume-at-price. Buy/sell
  shading uses the candle's available aggressor-volume split.
- Deriv has no traded-volume or aggressor-side stream in the chart feed. When
  quote history is available, each quote contributes one activity observation;
  otherwise candles contribute equal range-activity weight. The CVD pane uses a
  price-direction activity proxy on Deriv and labels it as a proxy.
- CVD can reset on CHoCH, BOS and CHoCH, UTC day, or Monday-aligned UTC week.
- POC lines can be hidden, extended, or filtered to levels price has not
  revisited. Value-area high/low lines can be hidden independently.
- The scene caps visible structure segments at twelve to keep rendering bounded.

## Source and implementation notes

The user supplied the Pine Script *Market Structure Volume Profiles* by
KioseffTrading, under the Mozilla Public License 2.0, together with its
explanatory transcript. This implementation is an independent Rust
implementation of the described structure-anchored profile behavior and uses
the platform's existing confirmed-swing detector. It does not embed, translate,
or distribute the supplied Pine source. The source's ATR zigzag, lower-timeframe
trade direction, and TradingView drawing objects are not available through this
platform's feeds/runtime; the differences above are deliberate and surfaced in
the chart note.

The indicator describes observed auction segments; BOS and CHoCH are structural
classifications, not guarantees of continuation or reversal.
