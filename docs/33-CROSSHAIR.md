# 33 — The crosshair

## Purpose

TradingView's default cursor is a crosshair: dashed guides that snap to the
hovered bar, a price tag on the right axis, the bar's time on the bottom
axis, and the bar's OHLC at the top-left. Our chart showed nothing until
this doc — the cursor had a `crosshair` CSS shape and no readout. This adds
the readout without breaking the platform's one rule.

## The rule it keeps

The shell does no arithmetic over market data — and a crosshair is nothing
*but* "what price is this pixel". So the inverse mapping lives in the
engine, where `price_to_y` already is:

- `scene::price_from_y` mirrors `Frame::price_at` and is host-tested as the
  round-trip inverse of `price_to_y` (flat spans and zero-height plots
  collapse to the one honest value instead of dividing by zero).
- A new wasm export, `price_at_y(plot_y, plot_h, price_min, price_max, y)`,
  is a pure five-f64s-in/one-out shim over it — a mousemove cannot afford a
  scene rebuild (that would re-run every attached script per cursor tick),
  and a scalar export costs microseconds.
- `Bar` gains `open_time`, `open`, `high`, `low`, `close` — the candle's own
  values in value form, so the OHLC readout and time tag are formatting, not
  recovery of a price from a y. All five are `#[serde(default)]`: a scene
  built before the crosshair existed still deserializes (pinned by test).

## Behavior

- Hover tracking is repaint-only: the pane records the pointer while it is
  over the price plot and repaints from the **existing** scene on its own
  rAF throttle (`scheduleHoverPaint`), never through `scheduleRender`'s
  rebuild path. `pointerleave` clears it — a chart showing where the cursor
  *was* reads as stale data.
- The vertical guide snaps to the hovered bar's center (the bar whose
  body-plus-half-gap span holds the pointer; a lookup over engine
  positions). In line/area/footprint modes (`scene.candles` empty) the
  guides and price tag still draw; the bar readout stays off.
- Bar features key on `open_time != 0`, so a scene from a pre-crosshair
  engine shows the guides and the price tag but no zero-filled OHLC row.
- The OHLC readout takes the bar's direction color, top-left of the price
  pane, the same slot sub-pane labels use in their own panes.

## Tests

- `scene::tests::price_from_y_inverts_price_to_y` — round-trip at the
  range's ends and middle; degenerate inputs.
- `scene::tests::a_bar_carries_its_candles_values_for_the_crosshair` — the
  wire values match the source candle verbatim; the pre-crosshair wire
  shape still parses.

## What this is not

Cross-pane tracking (the vertical guide continuing through script sub-panes,
with each pane's own value tag) is the natural follow-up; it needs the pane
stack's geometry shared with the pointer handler, which the current
per-painter layout does not expose.
