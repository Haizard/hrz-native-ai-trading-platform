// Render a scene the engine produced, as a page you can just open.
//
// ## Why this exists
//
// `tools/wasm_abi_check.mjs` ends by saying it "is not a substitute for looking
// at the page". True -- and until now there was no way to look at the page
// without the gateway running, a database behind it, and a browser on top. So
// the one thing nobody had actually done was *see* whether a new overlay draws
// in the right place.
//
// This closes that gap without inventing a second renderer. It calls the real
// wasm, takes the real scene JSON, and writes it into a page that draws it with
// the same rules `app.js` uses -- dark palette, canvas 2D, no arithmetic over
// market data. The output is one self-contained file: no server, no wasm fetch,
// no CORS, so it opens from `file://` and survives being emailed.
//
// It is a preview, not the app: no websockets, no controls, one fixed request.
// Its job is to answer "does the geometry land where it should".
//
// Usage:  node tools/chart_preview.mjs [wasm] [out.html]

import { readFile, writeFile } from "node:fs/promises";

const wasmPath = process.argv[2] ?? "frontend/app/chart_engine.wasm";
const outPath = process.argv[3] ?? "reports/chart-preview.html";

const bytes = await readFile(wasmPath);
const { instance } = await WebAssembly.instantiate(bytes, {});
const wasm = instance.exports;

function build(request) {
  const json = new TextEncoder().encode(JSON.stringify(request));
  const pointer = wasm.alloc(json.length);
  new Uint8Array(wasm.memory.buffer, pointer, json.length).set(json);
  const status = wasm.build_scene(pointer, json.length);
  wasm.dealloc(pointer, json.length);
  if (status !== 0) {
    const start = wasm.last_error_ptr();
    const message = new TextDecoder().decode(
      new Uint8Array(wasm.memory.buffer, start, wasm.last_error_len())
    );
    throw new Error(message || "the engine refused the request");
  }
  const start = wasm.scene_ptr();
  const length = wasm.scene_len();
  const sceneBytes = new Uint8Array(wasm.memory.buffer, start, length).slice();
  return JSON.parse(new TextDecoder().decode(sceneBytes));
}

/// A series with a demand zone, a supply zone, and a fresh and a mitigated one
/// where the market cooperates.
///
/// Deliberately not a smooth trend: a zone only exists where structure actually
/// breaks, and a monotone series has nothing to break. This is a decline, a
/// pause, an impulse through the swing high, a pullback into the band -- then
/// the mirror image on the other side.
function series() {
  const rows = [
    [104.0, 105.0, 103.0, 103.5],
    [103.5, 104.0, 101.0, 101.5],
    [101.5, 102.5, 100.0, 102.0],
    [102.0, 103.0, 99.0, 99.5],
    [99.5, 100.5, 97.5, 98.0],
    [98.0, 99.0, 95.0, 95.5],
    [95.5, 98.5, 95.0, 98.0],
    [98.5, 99.0, 97.5, 98.0],
    [98.0, 98.4, 96.6, 96.8],
    [96.8, 97.0, 95.4, 95.6],
    [95.6, 95.9, 94.8, 95.0],
    [95.0, 97.5, 94.9, 97.0],
    [97.0, 99.5, 96.8, 99.0],
    [99.0, 102.0, 98.8, 101.5],
    [101.5, 104.0, 101.0, 103.5],
    [103.5, 106.0, 103.0, 105.5],
    [105.5, 108.0, 105.0, 107.5],
    [107.5, 109.0, 106.0, 106.5],
    [106.5, 107.0, 104.0, 104.5],
    [104.5, 105.0, 102.0, 102.5],
    [102.5, 103.0, 100.5, 101.0],
    [101.0, 101.5, 99.0, 99.5],
    [99.5, 100.5, 98.0, 98.5],
    [98.5, 102.0, 98.4, 101.5],
    [101.5, 104.5, 101.0, 104.0],
    [104.0, 107.0, 103.5, 106.5],
    [106.5, 109.5, 106.0, 109.0],
    [109.0, 110.0, 106.5, 107.0],
    [107.0, 107.5, 104.5, 105.0],
    [105.0, 105.5, 102.5, 103.0],
    [103.0, 103.5, 101.0, 101.5],
    [101.5, 102.0, 99.5, 100.0],
    [100.0, 100.5, 98.5, 99.0],
    [99.0, 100.0, 97.5, 99.5],
    [99.5, 102.5, 99.4, 102.0],
    [102.0, 105.0, 101.5, 104.5],
    [104.5, 107.5, 104.0, 107.0],
    [107.0, 108.5, 106.0, 106.5],
    [106.5, 107.0, 104.0, 104.5],
    [104.5, 105.0, 102.0, 102.5],
    [102.5, 103.0, 100.5, 101.0],
    [101.0, 101.5, 99.0, 99.5],
    [99.5, 100.0, 97.5, 98.0],
    [98.0, 98.5, 96.0, 96.5],
    [96.5, 97.0, 94.5, 95.0],
    [95.0, 97.0, 94.5, 96.5],
    [96.5, 99.5, 96.0, 99.0],
    [99.0, 102.0, 98.5, 101.5],
    [101.5, 104.5, 101.0, 104.0],
    [104.0, 107.0, 103.5, 106.5],
    [106.5, 109.5, 106.0, 109.0],
    [109.0, 111.0, 108.0, 108.5],
    [108.5, 109.0, 106.0, 106.5],
    [106.5, 107.0, 104.5, 105.0],
    [105.0, 105.5, 103.0, 103.5],
    [103.5, 104.0, 101.5, 102.0],
    [102.0, 102.5, 100.0, 100.5],
    [100.5, 101.0, 98.5, 99.0],
    [99.0, 99.5, 97.0, 97.5],
    [97.5, 98.0, 95.5, 96.0],
    [96.0, 98.0, 95.5, 97.5],
    [97.5, 100.5, 97.0, 100.0],
    // ... a second setup, this one left untouched: a swing high at 101.5 with
    // three lower highs on each side of it, a pause, then an impulse that closes
    // through it. Price then runs to 108 and never comes back, so the zone this
    // leaves behind is still fresh -- which is the whole point of showing it.
    [100.0, 101.5, 99.5, 101.0],
    [101.0, 101.2, 99.0, 99.5],
    [99.5, 100.0, 97.5, 98.0],
    [98.0, 98.5, 96.5, 97.0],
    [97.0, 97.4, 95.6, 95.8],
    [95.8, 96.1, 94.9, 95.0],
    [95.0, 98.5, 94.9, 98.0],
    [98.0, 101.0, 97.5, 100.5],
    [100.5, 104.0, 100.0, 103.5],
    [103.5, 106.0, 103.0, 105.5],
    [105.5, 108.0, 105.0, 107.5],
  ];
  return rows.map(([open, high, low, close], i) => ({
    symbol: "BTCUSDT",
    timeframe: "5m",
    open_time: 1_788_998_400_000_000_000 + i * 300_000_000_000,
    open,
    high,
    low,
    close,
    volume: 10 + (i % 7),
    buy_volume: 6 + (i % 5),
    sell_volume: 4 + (i % 3),
  }));
}

const scene = build({
  candles: series(),
  width: 1180,
  height: 520,
  mode: "candles",
  zones: true,
});

const zones = scene.regions ?? [];
console.log(
  `${scene.candles.length} candles, ${zones.length} zone(s): ` +
    zones.map((z) => `${z.label} ${z.price_low}..${z.price_high}`).join(", ")
);

const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>chart-engine preview — supply/demand zones</title>
<style>
  :root {
    --bg: #0d1117; --panel: #161b22; --line: #2a313c;
    --text: #e6edf3; --muted: #8b949e; --accent: #58a6ff;
    --mono: ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace;
  }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: var(--text);
         font: 13px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; padding: 20px; }
  h1 { font-size: 14px; margin: 0 0 4px; font-weight: 600; }
  p.lede { color: var(--muted); margin: 0 0 16px; max-width: 76ch; }
  .wrap { position: relative; border: 1px solid var(--line); border-radius: 8px;
          background: var(--panel); overflow: hidden; }
  canvas { display: block; width: 100%; }
  ul { list-style: none; padding: 0; margin: 14px 0 0; display: flex; flex-wrap: wrap; gap: 8px; }
  li { font-family: var(--mono); font-size: 11px; border: 1px solid var(--line);
       border-radius: 999px; padding: 3px 10px; color: var(--muted); }
  code { font-family: var(--mono); color: var(--accent); }
  .note { color: var(--muted); font-size: 11px; margin-top: 14px; max-width: 90ch; }
</style>
</head>
<body>
<h1>The engine's own output, drawn</h1>
<p class="lede">
  The scene below is not hand-drawn and not simulated: it is the JSON
  <code>chart_engine.wasm</code> returned for a request with <code>zones: true</code>,
  rendered with the same rules the shell uses. Zones sit behind the candles because a
  supply/demand band is a backdrop the price is read against, not a mark on top of it.
</p>
<div class="wrap"><canvas id="c"></canvas></div>
<ul id="legend"></ul>
<p class="note">
  A fresh zone is drawn solid; a mitigated one is faded, because a zone price has already
  traded back through is not a level any more. The band arrives with its height as
  <code>h</code> rather than as two edges to subtract — the same shape
  <code>ProfileBar</code> already had, so the shell's fill is
  <code>fillRect(x, y_top, w, h)</code>.
</p>
<script>
const scene = ${JSON.stringify(scene)};

const COLORS = {
  up: "#26a69a", down: "#ef5350", wick: "#8b949e", grid: "#21262d",
  text: "#8b949e", vwap: "#d29922", poc: "#e6edf3", vah: "#8b949e", val: "#8b949e",
  demand: "#26a69a", supply: "#ef5350",
};

const canvas = document.getElementById("c");
const ratio = window.devicePixelRatio || 1;
canvas.width = scene.width * ratio;
canvas.height = scene.height * ratio;
canvas.style.height = scene.height + "px";
const ctx = canvas.getContext("2d");
ctx.setTransform(ratio, 0, 0, ratio, 0, 0);

// --- the same order draw() uses ---------------------------------------------

for (const tick of scene.ticks) {
  ctx.strokeStyle = COLORS.grid;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(scene.plot.x, tick.y + 0.5);
  ctx.lineTo(scene.plot.x + scene.plot.w, tick.y + 0.5);
  ctx.stroke();
}

// Zones first: behind everything.
for (const zone of scene.regions) {
  const colour = COLORS[zone.kind] || COLORS.text;
  ctx.globalAlpha = zone.fresh ? 0.16 : 0.07;
  ctx.fillStyle = colour;
  ctx.fillRect(zone.x, zone.y_top, zone.w, zone.h);
  ctx.globalAlpha = 1;

  ctx.strokeStyle = colour;
  ctx.globalAlpha = zone.fresh ? 0.7 : 0.35;
  ctx.setLineDash([3, 3]);
  ctx.strokeRect(zone.x + 0.5, zone.y_top + 0.5, zone.w - 1, zone.h - 1);
  ctx.setLineDash([]);
  ctx.globalAlpha = 1;

  ctx.fillStyle = colour;
  ctx.font = "10px " + getComputedStyle(document.documentElement).getPropertyValue("--mono");
  ctx.fillText(zone.label, zone.x + 4, zone.y_top + 11);
}

for (const bar of scene.candles) {
  const colour = bar.up ? COLORS.up : COLORS.down;
  ctx.strokeStyle = colour;
  ctx.fillStyle = colour;
  ctx.beginPath();
  ctx.moveTo(bar.x + bar.w / 2, bar.wick_top);
  ctx.lineTo(bar.x + bar.w / 2, bar.wick_bottom);
  ctx.stroke();
  const height = Math.max(1, bar.body_bottom - bar.body_top);
  ctx.fillRect(bar.x, bar.body_top, bar.w, height);
}

for (const level of scene.levels) {
  ctx.strokeStyle = COLORS[level.kind] || COLORS.text;
  ctx.setLineDash([4, 4]);
  ctx.beginPath();
  ctx.moveTo(scene.plot.x, level.y);
  ctx.lineTo(scene.plot.x + scene.plot.w, level.y);
  ctx.stroke();
  ctx.setLineDash([]);
  ctx.fillStyle = COLORS[level.kind] || COLORS.text;
  ctx.font = "10px ui-monospace, monospace";
  ctx.fillText(level.kind.toUpperCase(), scene.plot.x + 4, level.y - 3);
}

ctx.fillStyle = COLORS.text;
ctx.font = "10px ui-monospace, monospace";
for (const tick of scene.ticks) {
  ctx.fillText(tick.price.toFixed(2), scene.plot.x + scene.plot.w + 6, tick.y + 3);
}

// --- the legend, straight from the scene ------------------------------------

const legend = document.getElementById("legend");
for (const zone of scene.regions) {
  const li = document.createElement("li");
  li.textContent =
    zone.kind + " · " + zone.price_low.toFixed(1) + "–" + zone.price_high.toFixed(1) +
    " · " + (zone.fresh ? "fresh" : Math.round(zone.mitigated * 100) + "% mitigated") +
    " · " + zone.break_kind + " at " + zone.broken_level.toFixed(1);
  legend.appendChild(li);
}
</script>
</body>
</html>
`;

await writeFile(outPath, html);
console.log(`wrote ${outPath}`);
