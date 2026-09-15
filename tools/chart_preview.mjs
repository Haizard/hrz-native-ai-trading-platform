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

/// A fair value gap, written as a document and nothing else.
///
/// Nothing in this workspace knows what a fair value gap is: there is no
/// detector for it, no enum variant, no field, no entry in any list. Three
/// numbers -- a window, a band and one requirement -- are the entire definition.
/// This is the shape a client is meant to arrive with.
///
/// `min_band_ratio` is the one size knob, and it is set here because the rule
/// itself is *any* three-candle separation: a five-candle impulse separates on
/// every window it spans, so without it this series answers with a stack of
/// overlapping slivers instead of the gaps a trader would mark.
function conceptDocument() {
  return {
    name: "bullish_gap",
    label: "bullish gap",
    side: "Buy",
    window: 3,
    lower: { high: 0 },
    upper: { low: 2 },
    require: [{ left: { high: 0 }, op: "below", right: { low: 2 } }],
    min_band_ratio: 0.2,
  };
}

const scene = build({
  candles: series(),
  width: 1180,
  height: 520,
  mode: "candles",
  // Both producers in one request: the bands the engine ships with, and a band
  // nobody taught it. They come back through the same `regions` array, because
  // the built-in detector is not privileged.
  zones: true,
  concepts: [conceptDocument()],
});

const regions = scene.regions ?? [];
console.log(
  `${scene.candles.length} candles, ${regions.length} region(s):\n` +
    regions
      .map((r) => `  ${r.name} (${r.side}) ${r.price_low}..${r.price_high} — ${r.label}`)
      .join("\n")
);

const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>chart-engine preview — regions, built-in and client-defined</title>
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
  <code>chart_engine.wasm</code> returned for a request with <code>zones: true</code>
  and one <code>concepts</code> document, rendered with the same rules the shell uses.
  Regions sit behind the candles because a band is a backdrop the price is read
  against, not a mark on top of it.
</p>
<p class="lede">
  Both kinds of band are here, and they are the same kind of thing. The
  <code>demand</code> bands come from the detector the engine ships with. The
  <code>bullish gap</code> bands come from three numbers in the request — a window, a
  band and one requirement — and <strong>nothing in this workspace knows what a fair
  value gap is</strong>. No detector, no enum variant, no field, no entry in a list. The
  request carries the definition as data and the engine measures it, which is the
  whole point: a client adds a word to the vocabulary without adding code.
</p>
<div class="wrap"><canvas id="c"></canvas></div>
<ul id="legend"></ul>
<p class="note">
  A fresh region is drawn solid; a mitigated one is faded, because a band price has
  already traded back through is not a level any more. The band arrives with its height
  as <code>h</code> rather than as two edges to subtract — the same shape
  <code>ProfileBar</code> already had, so the shell's fill is
  <code>fillRect(x, y_top, w, h)</code>.
  <br /><br />
  Colour comes from the region's <code>name</code>, so a concept is coloured by its own
  name the moment there is an entry for it — and before then it falls back to
  <code>side</code>, which every region carries. That fallback is why a band the shell
  has never heard of still reads as a direction rather than as a grey rectangle.
  <br /><br />
  There are six gap bands and only two displacements, and that is the rule being honest
  rather than a bug: <em>any</em> three candles that separate is a match, so one
  five-candle impulse matches on every window it spans and the bands stack. A concept is
  a pattern, not a detector with a notion of "the" gap — which is what the one size knob,
  <code>min_band_ratio</code>, is for. Raise it and the overlapping slivers go; raise it
  far enough and the honest answer is nothing.
</p>
<script>
const scene = ${JSON.stringify(scene)};

const COLORS = {
  up: "#26a69a", down: "#ef5350", wick: "#8b949e", grid: "#21262d",
  text: "#8b949e", vwap: "#d29922", poc: "#e6edf3", vah: "#8b949e", val: "#8b949e",
  // The bands the engine ships with, keyed by the region's name...
  demand: "#26a69a", supply: "#ef5350",
  // ...and the fallback, keyed by its side. Every region carries one, so a
  // concept with no entry here still reads as a direction instead of grey.
  buy: "#26a69a", sell: "#ef5350",
  "bullish gap": "#58a6ff",
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

// Regions first: behind everything.
for (const region of scene.regions) {
  const colour = COLORS[region.name] || COLORS[region.side] || COLORS.text;
  ctx.globalAlpha = region.fresh ? 0.16 : 0.07;
  ctx.fillStyle = colour;
  ctx.fillRect(region.x, region.y_top, region.w, region.h);
  ctx.globalAlpha = 1;

  ctx.strokeStyle = colour;
  ctx.globalAlpha = region.fresh ? 0.7 : 0.35;
  ctx.setLineDash([3, 3]);
  ctx.strokeRect(region.x + 0.5, region.y_top + 0.5, region.w - 1, region.h - 1);
  ctx.setLineDash([]);
  ctx.globalAlpha = 1;

  ctx.fillStyle = colour;
  ctx.font = "10px " + getComputedStyle(document.documentElement).getPropertyValue("--mono");
  ctx.fillText(region.label, region.x + 4, region.y_top + 11);
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
for (const region of scene.regions) {
  const li = document.createElement("li");
  const origin =
    region.origin.source === "structure_break"
      ? region.origin.kind + " at " + region.origin.level.toFixed(1)
      : "pattern";
  li.textContent =
    region.name + " · " + region.price_low.toFixed(1) + "–" + region.price_high.toFixed(1) +
    " · " + (region.fresh ? "fresh" : Math.round(region.mitigated * 100) + "% mitigated") +
    " · " + origin;
  legend.appendChild(li);
}
</script>
</body>
</html>
`;

await writeFile(outPath, html);
console.log(`wrote ${outPath}`);
