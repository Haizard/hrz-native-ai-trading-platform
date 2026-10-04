# 27 — Viewport culling: off-window geometry never crosses the ABI

## Purpose

The scene is serialized to the shell on every frame. Before this document, a
generated indicator's zones and markers crossed that boundary whether or not
the current window could ever show them — the canvas clipped them to nothing
after paying the serialization, the JSON parse, and the paint-loop iteration.
The scene now culls them at build time, where the frame is known.

## The rule

Detection still runs on the **whole** request series — a zone's birth context
is the whole series, and detecting on a slice would silently change what
exists. Culling applies to the **mapping**, where correctness is already
settled:

- A zone is kept when its life intersects the frame
  (`end_time >= frame.from && start_time <= frame.to`); otherwise it never
  enters the scene.
- A kept zone is kept **whole** — never clamped to the window — because the
  inspector (`docs/26`) shows its true birth time and price band, and a
  clamped zone would lie about both. The canvas clips the paint, as it always
  has; what changes is that the invisible never ships.
- A marker is kept when its time is inside the frame. A kept zone may lose
  its off-window birth marker: the band the trader scrolled back for stays,
  its invisible glyph does not.

This is the discipline `region_rects` has always had for the built-in
detectors, extended to generated indicator zones and markers, with clamping
deliberately not carried over (regions clamp for historical reasons; zones
answer an inspector now).

## What this is not

- **Not detection culling.** `refresh_from_concepts` still sees every candle;
  a zone born outside the window and extending into it is detected and kept.
- **Not script-object culling.** Scripts already run on the visible slice
  (`docs/23`), and their objects are bounded by the 256-object cap; there is
  nothing off-window to cull.
- **Not a render scheduler.** The CriticalImmediate/Deferred/Background
  priority tiers from the original proposal remain future work; at current
  budgets (500 primitives, culled to the window) the paint loop is not the
  bottleneck — the per-frame ABI payload was, and this document is the fix.
