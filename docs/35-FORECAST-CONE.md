# 35 — The forecast cone

## Purpose

Predictive visualization (#4, first slice). TradingView has no native Monte
Carlo; ours is a **stationary bootstrap**: each simulated path walks forward
from the last close, multiplying by a randomly redrawn historical log return
per step. No drift is fitted, no distribution assumed — the cone is the
window's own recent behavior replayed in shuffled order, and the label on
the chart says so: `forecast · 500 paths · 30 bars`.

## The honest properties

- **Determinism.** The sampler is a seeded xorshift (analytics-core's
  `forecast` module), and the engine derives the seed from the window's
  identity (slice offset + first/last open times). The same view draws the
  same cone on every frame — a forecast that reshuffled on every redraw
  would read as a malfunction, not a model.
- **Refusal over costume.** Fewer than 8 usable returns in the window and
  the forecast is `None`, with the reason in the scene note. A cone fitted
  to three moves would be a costume.
- **Real prices.** The cone simulates from the window's **real** closes, not
  the drawn ones — in Heikin-Ashi mode the plotted closes are averages
  nobody traded at, the same promise the volume profile keeps.
- **The cone joins the axis.** The price fit extends to the cone's 5–95%
  envelope, because a cone clipped at the plot edge would read as "the
  future is flat" — the one claim the simulation did not make. A user's own
  price range still wins exactly, as it always has.

## The time-axis extension (the invariant that makes it safe)

With a forecast, the scene reserves `steps` future slots past the last bar:
`scene.to` grows by `steps × bar_nanos` **and** the slot count grows by the
same number of bars, so the load-bearing invariant — a candle's open time
maps through `x_at_nanos` to its slot's left edge — holds across the
extension. The candles keep their width in bars; the cone gets the space to
the right; everything time-anchored (zones, drawings, script objects) maps
through the extended frame without a special case.

The reported `scene.viewport` is the **pre-extension** window: an echo that
included the future would feed back into the next request and walk the
window a horizon to the right every frame.

## Wire

- Request: `forecast: { steps?: number, paths?: number }` (defaults 30/500,
  capped at 120/2000 in analytics-core).
- Scene: `forecast: { bands: [{ quantile, points }] , paths, steps }` — five
  polylines (5/25/50/75/95%), each anchored at the last bar's close so the
  cone grows out of the price line. Omitted when off or refused; old scenes
  parse unchanged.

## Tests

- analytics-core `forecast` module: determinism, seed separation, quantile
  ordering, flat-series flat cone, thin-history refusal, cap clamping.
- `scene::tests::a_forecast_extends_the_time_axis_and_positions_its_cone` —
  the extension arithmetic, the anchor's position, band ordering in canvas
  space, cross-build determinism, the un-extended viewport echo, old-scene
  deserialization.
- `scene::tests::a_forecast_on_a_thin_window_is_refused_with_a_note`.

## What this is not

Not a parametric model (no GBM drift/vol estimate — the bootstrap is the
honest choice when the alternative is fitting two numbers and implying
authority), and not the what-if slice of #4 ("drag the cone's assumptions"
needs an interaction design of its own).
