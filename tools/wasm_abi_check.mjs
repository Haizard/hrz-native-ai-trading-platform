// Exercise the chart engine's ABI the way the browser does.
//
// ## Why this exists
//
// The scene builder is unit-tested on the host, and the shell is JavaScript
// that only a browser runs. Between them is the thing neither covers: the ABI.
// Four functions, two buffers, one JSON contract -- and if any of it is wrong,
// the failure is a blank canvas in a browser nobody is watching.
//
// Node instantiates the same wasm binary the browser fetches and calls the same
// exported functions, so this checks the contract for real rather than
// describing it. It is not a substitute for looking at the page; it is what
// catches "the exports were renamed" before anyone opens the page.
//
// Usage:  node tools/wasm_abi_check.mjs [path-to-wasm]

import { readFile } from "node:fs/promises";

const path = process.argv[2] ?? "frontend/app/chart_engine.wasm";

const bytes = await readFile(path);
const { instance } = await WebAssembly.instantiate(bytes, {});
const wasm = instance.exports;

let failures = 0;
const check = (name, ok, detail = "") => {
  console.log(`${ok ? "  ok  " : "  FAIL"}  ${name}${detail ? ` -- ${detail}` : ""}`);
  if (!ok) failures += 1;
};

// --- the exports the shell calls -------------------------------------------

for (const name of [
  "alloc",
  "dealloc",
  "build_scene",
  "scene_ptr",
  "scene_len",
  "last_error_ptr",
  "last_error_len",
  "memory",
]) {
  check(`exports ${name}`, typeof wasm[name] !== "undefined");
}

/// Call the engine exactly as `app.js` does.
function build(request) {
  const json = new TextEncoder().encode(JSON.stringify(request));
  const pointer = wasm.alloc(json.length);
  new Uint8Array(wasm.memory.buffer, pointer, json.length).set(json);

  const status = wasm.build_scene(pointer, json.length);
  wasm.dealloc(pointer, json.length);

  if (status !== 0) {
    const start = wasm.last_error_ptr();
    const length = wasm.last_error_len();
    const message = new TextDecoder().decode(
      new Uint8Array(wasm.memory.buffer, start, length)
    );
    throw new Error(message || "refused");
  }

  const start = wasm.scene_ptr();
  const length = wasm.scene_len();
  const sceneBytes = new Uint8Array(wasm.memory.buffer, start, length).slice();
  return JSON.parse(new TextDecoder().decode(sceneBytes));
}

/// A rising series with a real range, so the profile has something to bucket.
function candles(count) {
  return Array.from({ length: count }, (_, i) => {
    const base = 100 + i * 0.5;
    const open = base;
    const close = base + (i % 2 === 0 ? 0.4 : -0.2);
    return {
      symbol: "BTCUSDT",
      timeframe: "5m",
      open_time: i * 300_000_000_000,
      open,
      high: Math.max(open, close) + 1,
      low: Math.min(open, close) - 1,
      close,
      volume: 10 + i,
      buy_volume: 6 + i * 0.5,
      sell_volume: 4 + i * 0.5,
    };
  });
}

// --- a real request ---------------------------------------------------------

const scene = build({ candles: candles(120), width: 900, height: 420, mode: "candles" });
check("a scene comes back", typeof scene === "object" && scene !== null);
check("every candle has a bar", scene.candles.length === 120, `got ${scene.candles.length}`);
check("the profile is populated", scene.profile.length > 0, `got ${scene.profile.length}`);
check("levels are drawn", scene.levels.length >= 1, `got ${scene.levels.length}`);
check(
  "geometry is finite",
  scene.candles.every((b) =>
    [b.x, b.w, b.body_top, b.body_bottom, b.wick_top, b.wick_bottom].every(Number.isFinite)
  ),
  "a NaN would make the canvas silently blank"
);
check(
  "bars stay inside the plot",
  scene.candles.every((b) => b.x >= scene.plot.x - 1 && b.x + b.w <= scene.plot.x + scene.plot.w + 1)
);

const footprint = build({ candles: candles(120), width: 900, height: 420, mode: "footprint" });
check("footprint mode produces cells", footprint.cells.length > 0, `got ${footprint.cells.length}`);
check(
  "and says which footprint it is",
  typeof footprint.note === "string" && footprint.note.includes("no tick data"),
  footprint.note
);

// --- the failure path -------------------------------------------------------
//
// A trap would take the whole page down with no explanation; a non-zero return
// with a message is something the shell can show.

const badJson = new TextEncoder().encode("{not json");
const pointer = wasm.alloc(badJson.length);
new Uint8Array(wasm.memory.buffer, pointer, badJson.length).set(badJson);
const status = wasm.build_scene(pointer, badJson.length);
wasm.dealloc(pointer, badJson.length);
check("malformed JSON returns non-zero rather than trapping", status !== 0);

const errorLength = wasm.last_error_len();
const errorMessage = new TextDecoder().decode(
  new Uint8Array(wasm.memory.buffer, wasm.last_error_ptr(), errorLength)
);
check("and explains itself", errorMessage.length > 0, errorMessage);

// --- an empty request is a scene, not an error ------------------------------
//
// The shell renders whatever comes back; "nothing to draw" must be data.

const empty = build({ candles: [], width: 900, height: 420 });
check("an empty window is a scene with a note", empty.candles.length === 0 && !!empty.note, empty.note);

console.log(
  failures === 0
    ? `\nall checks passed (${path})`
    : `\n${failures} check(s) FAILED (${path})`
);
process.exit(failures === 0 ? 0 : 1);
