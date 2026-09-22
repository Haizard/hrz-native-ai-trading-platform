#!/usr/bin/env node
/*
 * One-off probe for "generated indicator attaches but draws nothing".
 * Builds a scene exactly as app.js does, with an IndicatorOutput shaped like
 * the one the workspace preview stores (evidence + zones + markers in triplets),
 * and reports what the engine did with it.
 */
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const wasmBytes = await readFile(join(here, "..", "frontend", "app", "chart_engine.wasm"));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const wasm = instance.exports;

function build(request) {
  const json = new TextEncoder().encode(JSON.stringify(request));
  const pointer = wasm.alloc(json.length);
  new Uint8Array(wasm.memory.buffer, pointer, json.length).set(json);
  const status = wasm.build_scene(pointer, json.length);
  wasm.dealloc(pointer, json.length);
  if (status !== 0) {
    const start = wasm.last_error_ptr();
    const length = wasm.last_error_len();
    return { error: new TextDecoder().decode(new Uint8Array(wasm.memory.buffer, start, length)) };
  }
  const start = wasm.scene_ptr();
  const length = wasm.scene_len();
  const bytes = new Uint8Array(wasm.memory.buffer, start, length).slice();
  return JSON.parse(new TextDecoder().decode(bytes));
}

// --- a real-looking candle window: 500 x 5m candles ending now -------------
const now = Date.now(); // ms
const M5 = 5 * 60 * 1000;
const candles = [];
for (let i = 0; i < 500; i++) {
  const t = now - (500 - i) * M5;
  const base = 100 + Math.sin(i / 10) * 5;
  candles.push({
    symbol: "BTCUSDT",
    timeframe: "5m",
    open_time: t * 1_000_000, // nanos
    open: base,
    high: base + 2,
    low: base - 2,
    close: base + 1,
    volume: 10,
    buy_volume: 6,
    sell_volume: 4,
  });
}

// --- an IndicatorOutput like the workspace stores: triplets, recent window --
const evidence = [], zones = [], markers = [];
// Half inside the visible window (recent), half in the past week (culled set would
// be older, but the stored preview keeps the most recent 166) — use recent times.
for (let i = 0; i < 166; i++) {
  const tMs = now - (300 - i * 1) * M5; // indices 134..300 -> inside the window
  const tNs = tMs * 1_000_000;
  evidence.push({
    id: `concept-${i}`,
    event: "fvg",
    time: tNs,
    price: 98 + (i % 50) * 0.1,
    explanation: "detected fvg on the 5m chart",
  });
  zones.push({
    id: `concept-${i}`,
    start_time: tNs,
    end_time: tNs + 3 * M5 * 1_000_000,
    price_low: 97 + (i % 50) * 0.1,
    price_high: 99 + (i % 50) * 0.1,
    label: "fvg",
    state: "active",
  });
  markers.push({
    id: `concept-${i}-marker`,
    evidence_id: `concept-${i}`,
    time: tNs,
    price: 98 + (i % 50) * 0.1,
    label: "fvg",
    kind: "bullish",
  });
}

const indicator = { revision_id: "rev-1", evidence, zones, markers, links: [] };
const total = evidence.length + zones.length + markers.length + links0(indicator);
function links0() { return 0; }

const request = {
  candles,
  width: 1200,
  height: 600,
  mode: "candles",
  lines: [],
  zones: false,
  drawings: [],
  overlays: [],
  indicator,
};

const scene = build(request);
if (scene.error) {
  console.log("build_scene REFUSED:", scene.error);
} else {
  console.log("build_scene ok");
  console.log("scene.note:", JSON.stringify(scene.note));
  console.log("scene.indicator present:", Boolean(scene.indicator));
  if (scene.indicator) {
    const z = scene.indicator.zones;
    const m = scene.indicator.markers;
    console.log(`zones: ${z.length}, markers: ${m.length}, links: ${scene.indicator.links.length}`);
    const onscreen = z.filter((zone) => zone.w > 0 && zone.x + zone.w > 0 && zone.x < 1200);
    console.log(`zones with visible width: ${onscreen.length}`);
    console.log("first zone:", JSON.stringify(z[0]));
    const onscreenM = m.filter((mk) => mk.x >= 0 && mk.x <= 1200);
    console.log(`markers on screen: ${onscreenM.length}`);
  }
}
