# 26 — The zone inspector: every zone answers why it exists

## Purpose

Extend the evidence tooltip from markers to **zones**. Hovering a generated
zone shows its label, its lifecycle state in words, its price band, and the
explanation of the evidence that birthed it. This is the chart's answer to the
question a static indicator can never answer: *why is this rectangle here?*

## The data: zones already knew, the scene just dropped it

The indicator output model carries everything the inspector shows —
`price_low`, `price_high`, `start_time`, `end_time` on every zone, and an
`explanation` on every evidence node. The scene mapping used to keep only the
pixels, because pixels were all the paint pass needed. The inspector is fed by
passing the rest through:

- `SceneIndicatorZone` gains `price_low`, `price_high`, `start_time`,
  `end_time` — copied, not derived. The shell displays the numbers; it never
  un-maps pixels back into prices, which is the `docs/14` line.
- `explanation` is joined from the evidence node **with the same id as the
  zone**. That join is sound because both producers build the pair from one
  index: the generator's previews and `refresh_from_concepts` both mint
  `zone-{i}`/`live-{i}` for the zone and its birth evidence together. A zone
  no evidence names — a pasted output with no evidence graph — serializes
  without the key, so "no reason recorded" is distinguishable from a blank
  reason.

Both directions are pinned by scene tests, and the wire keys the shell reads
are pinned alongside: a rename compiles everywhere and the tooltip silently
goes empty, which is exactly the failure mode the pins exist to catch.

## The interaction

`updateEvidenceTip` resolves in priority order:

1. **Marker** within hit radius — the precise event wins.
2. **Topmost zone containing the pointer** — zones paint in array order, so
   the hit-test walks the array backwards; only the plot area counts, so a
   zone clipped at the axis cannot be hovered from the price axis.
3. Nothing — the tip hides.

The tooltip's lifecycle wording is the same tier the paint uses (fresh /
tapped / spent, `docs/25`), so the text never describes a zone differently
from how it is drawn.

## Non-goals

- No click-to-pin, no per-zone Q&A chat entry point yet — the inspector is
  read-only. A "ask about this zone" gesture is a chat-routing feature, not a
  rendering one.
- No built-in region (`scene.regions`) inspection: those zones are the
  platform's own detectors, not a generated indicator's claims, and their
  provenance is the detector name, which the chart already labels.
- No JavaScript arithmetic: the tip formats prices with the existing
  `fmtPrice` and shows times the engine positioned; every number it displays
  arrived as a number.
