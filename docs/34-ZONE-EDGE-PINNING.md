# 34 — Zone edge pinning: drag a zone's edge, hold it there

## Purpose

Interactive zones (#3-full). A detected zone is the detector's claim; a
trader who knows the level wants to *correct the boundary*, not just read
it. Hovering a live zone's top or bottom edge now shows the resize cursor;
dragging it previews the adjusted band; releasing pins that edge; the pin
re-lands on every fresh re-detection until it is released (double-click the
accent line). This is the piece TradingView does not have: the indicator's
zone is explainable **and** adjustable.

## The honest semantics

A pinned zone is still a *detected* zone:

- The pin moves one edge of a band the detector found. It never invents a
  band — a pin whose zone no longer detects is dropped, with the reason in
  the scene note, because a pin that silently did nothing would read as the
  zone ignoring the user.
- The lifecycle state (fresh / tapped / spent) is **not** re-derived against
  the pinned band: the state describes the market's history with the
  *detected* band, and re-scoring it would be a second, weaker mitigation
  detector. What changes instead is the evidence text — the inspector and
  tooltip say "… · top edge pinned by you", so a dragged edge is never
  presented as the detector's own claim.
- The pin applies to the **live** layer only. Frozen snapshots (the stored
  preview, the revision diff) are historical documents.

## Contract

- Matching is by **birth anchor** — `(label, start_time)` — never by the
  band's edges (those are what the pin changes) and never by the `live-{i}`
  id (a merge can re-index). The anchor survives the merge-extension that
  can move a live band's span as new bars arrive.
- Request: `zone_constraints: [{ label, start_time, edge: "top"|"bottom",
  price }]`, applied in `IndicatorOutput::apply_zone_constraints` after
  `refresh_from_concepts`. Inverting pins (top dragged through the bottom)
  and non-finite prices are refused with notes.
- Wire: `SceneIndicatorZone.pinned_edge` — present only when pinned; old
  scenes deserialize to "not pinned". The zone's geometry (`y_top`, `h`,
  `price_low/high`) is the pinned band's, so everything downstream — the
  painter, the inspector — reads the band as drawn.
- The shell persists pins per symbol+indicator in `localStorage`
  (`atp.zonePins.{symbol}.{name}`), re-sent on every rebuild.

## The shell keeps its one rule

The drag preview follows the **pointer's y** — the shell never maps a price
to a pixel. The committed price is read off the pointer's y at release
through the engine's `price_at_y` export (docs/33), so both directions of
the price/pixel mapping have exactly one implementation, in the engine.

## Tests

- `indicator::tests::a_pin_moves_the_edge_and_says_so_in_the_evidence` — the
  pin lands, the other edge is untouched, the evidence says it is the
  user's, the output still validates.
- `indicator::tests::a_pin_that_matches_nothing_or_inverts_or_is_not_a_number_is_dropped`
  — all three refusal kinds report and leave the detection untouched.
- `scene::tests::a_pinned_zone_edge_survives_re_detection_and_marks_the_wire`
  — end to end through `build`: pinned geometry via the scene's own scale,
  `pinned_edge` on the wire, old wire shape parses.
- `scene::tests::an_unmatched_or_inverting_pin_is_dropped_with_a_note` — the
  notes, and the detected geometry left alone.

## What this is not

A pin constrains an *edge*, it does not re-query the detector ("find bands
whose boundary is here") — the concept vocabulary has no price-constraint
selector, and adding one is an analytics-core design of its own. Pins are
also not yet part of the workspace document on the server; localStorage is
per-browser, which suits a visual preference but not a shared workspace.
