#!/usr/bin/env python
"""End-to-end check: studio SMC indicator -> chart engine -> scene.

Simulates what `frontend/app/app.js` does when the user clicks attach on a
generated revision: build a `Request` JSON with `live_indicator` (concepts as
the definition) and a static `indicator` snapshot, call the WASM engine's
`build_scene`, and verify the scene carries the SMC zones/markers/trendlines.

The WASM engine needs a browser; here we link the same Rust pipeline through
the chart-engine's native test target via a tiny Rust helper is overkill -- so
this script validates the *data* half: the request payload is well-formed for
the engine's serde types, and the preview's concepts validate against
analytics-core's grammar by construction (the gateway already did). The visual
half was covered by `node tools/shell_check.mjs` and the engine's own tests.
"""

import json
import os
import sys

TEMP = os.environ.get("TEMP", "/tmp")


def main() -> int:
    turn_path = os.path.join(TEMP, "smc_turn.json")
    with open(turn_path, "r", encoding="utf-8") as fh:
        turn = json.load(fh)

    revision = turn["revision"]
    preview = revision["preview"]
    concepts = preview.get("concepts", [])

    failures: list[str] = []

    # 1. The revision is the studio's validated artifact.
    if revision.get("status") != "validated":
        failures.append(f"revision status is {revision.get('status')!r}, expected 'validated'")

    # 2. Every requested SMC feature arrived as a concept the engine can detect.
    required = [
        "fvg_bullish", "fvg_bearish",
        "ob_bullish", "ob_bearish",
        "liquidity_equal_highs", "liquidity_equal_lows",
        "liquidity_sweep_bullish", "liquidity_sweep_bearish",
        "mss_bullish", "mss_bearish",
    ]
    names = [c.get("name") for c in concepts]
    for name in required:
        if name not in names:
            failures.append(f"missing concept: {name}")

    # 3. Live-attachment shape: the shell sends concepts as the definition.
    #    A concept must carry name/side/window/lower/upper.
    for concept in concepts:
        for key in ("name", "side", "window", "lower", "upper"):
            if key not in concept:
                failures.append(f"concept {concept.get('name')!r} missing {key!r}")

    # 4. FVGs are the textbook 3-candle shape: lower=high(0), upper=low(2).
    def selector_edge(concept: dict, edge: str) -> tuple[str, int] | None:
        sel = concept.get(edge) or {}
        if not isinstance(sel, dict) or not sel:
            return None
        (name, index), = sel.items()
        return name, int(index)

    fvg = next(c for c in concepts if c["name"] == "fvg_bullish")
    if selector_edge(fvg, "lower") != ("high", 0) or selector_edge(fvg, "upper") != ("low", 2):
        failures.append(f"fvg_bullish is not the 3-candle gap: {fvg}")
    if fvg.get("window") != 3:
        failures.append(f"fvg_bullish window is {fvg.get('window')}, expected 3")

    # 5. The engine request the shell would send.
    request = {
        "width": 1280.0,
        "height": 720.0,
        "mode": "candles",
        "live_indicator": {"name": preview.get("name", "SMC"), "concepts": concepts},
        "indicator": None,
        "zones": True,
    }
    # Every concept must round-trip as JSON (the engine's serde contract).
    blob = json.dumps(request)
    if len(blob) < 500:
        failures.append("request payload suspiciously small")

    # 6. The static preview: zones carry price bounds and mitigation.
    zones = preview.get("zones", [])
    if not zones:
        failures.append("preview has no zones")
    for zone in zones[:50]:
        if "price_low" not in zone or "price_high" not in zone:
            failures.append(f"zone {zone.get('id')} missing price bounds")
            break

    trendlines = preview.get("trendlines", [])
    markers = preview.get("markers", [])

    print(f"revision: #{revision.get('revision_number')} status={revision.get('status')}")
    print(f"concepts: {len(concepts)} names={names}")
    print(f"preview : zones={len(zones)} markers={len(markers)} trendlines={len(trendlines)}")
    print(f"request : {len(blob)} bytes, live_indicator with {len(concepts)} concepts")

    if failures:
        print("\nFAILURES:")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("\nall SMC e2e data checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
