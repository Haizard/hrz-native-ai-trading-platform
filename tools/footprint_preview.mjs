// Render a footprint scene the engine produced, as a page you can just open.
//
// ## Why this exists
//
// `tools/chart_preview.mjs` covers the candle side of the engine -- regions,
// zones, levels. It hardcodes `mode: "candles"`, and the footprint is the one
// scene whose whole value is *presentation*: a ladder of `bid x ask` pairs whose
// cells are tinted by which side won. That is not something a jsdom check can
// judge -- jsdom lays nothing out -- and the harness cannot see a colour. So the
// one thing nobody had actually done was look at a footprint.
//
// This closes that gap the same way `chart_preview.mjs` does: it calls the real
// wasm with a real `Mode::Footprint` request and writes one self-contained HTML
// file. No server, no wasm fetch, no CORS, so it opens from `file://`.
//
// The drawing code below is copied from `drawFootprintGrid` in
// `frontend/app/app.js`, line for line. It is deliberately a copy rather than an
// import: the point is to look at *what the shell draws*, and a shared module
// would be a third place for the two to disagree.
//
// Usage:  node tools/footprint_preview.mjs [wasm] [out.html]

import { readFile, writeFile } from "node:fs/promises";

const wasmPath = process.argv[2] ?? "frontend/app/chart_engine.wasm";
const outPath = process.argv[3] ?? "reports/footprint-preview.html";

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

// A deterministic generator, so two runs of this tool produce the same page and
// a difference in the output is a difference in the drawing rather than in the
// data. `Math.random()` would make every look a different market.
//
// `Math.imul` rather than `seed * 1103515245`: the product overflows 2^53, the
// low bits are lost to float precision, and the sequence degenerates -- which it
// did, quietly, producing ladders with four levels in them.
let seed = 0x2f6e2b1;
const rnd = () => {
  seed = (Math.imul(seed, 1103515245) + 12345) & 0x7fffffff;
  return seed / 0x7fffffff;
};

// $40, which is the size `footprint_routes` would pick for a window like this:
// it sizes the bucket from the price span so a ladder lands at roughly 45 rows.
// The bucket matters here -- a fixed $10 over a $1,500 window is 150 levels, and
// the engine would truncate to what fits, so the preview would show the
// truncation path rather than the ladder.
const BUCKET = 40;
const BASE = 76_500;
// Overridable, because the two questions this page answers need different
// windows: "does a real one look right" wants the 18 columns a real chart has,
// and "what is a single cell doing" wants six, so the cells are big enough to
// read the numbers in a screenshot.
const COLUMNS = Number(process.env.FP_COLUMNS) || 18;
const OPEN_MS = 1_788_998_400_000;

// The price wanders about ten buckets either side of the base, so the union of
// every ladder is roughly twenty price levels -- which is what a real window
// looks like. It matters that it is realistic: `footprint_routes` sizes its
// bucket for 45 rows, and a preview built on a wider range would show the
// truncation path rather than the ladder.

/// One candle's ladder, with the shape `/footprint` returns.
///
/// Built here rather than fetched because the route needs the gateway, a
/// database and two days of trades behind it, and the question this tool exists
/// to answer -- "does the ladder read as a ladder" -- does not depend on whose
/// trades they are. The engine does the layout either way, which is the part
/// being looked at.
function column(index) {
  // `Math.round(...) * BUCKET`, not `Math.round(... * BUCKET)`: the bucket count
  // is what is rounded, so every price lands on the grid. Rounding the product
  // instead scatters the levels one point apart, and the engine then reports 203
  // price levels in a $1,000 window.
  const open = BASE + Math.round((rnd() - 0.5) * 8) * BUCKET;
  const close = open + Math.round((rnd() - 0.5) * 30) * BUCKET;
  const low = Math.min(open, close) - Math.round(rnd() * 5) * BUCKET;
  const high = Math.max(open, close) + Math.round(rnd() * 5) * BUCKET;

  const cells = [];
  for (let price = low; price <= high; price += BUCKET) {
    // Most of the volume sits near the middle of the candle, which is what makes
    // a footprint worth looking at: the shape of the ladder is the information.
    const edge = 1 - Math.abs(price - (low + high) / 2) / ((high - low) / 2 + 1);
    const scale = 0.35 + rnd() * 1.6;
    let bid = Math.round(edge * scale * 400) / 100;
    let ask = Math.round(edge * (0.4 + rnd() * 1.7) * 400) / 100;
    if (rnd() < 0.12) bid = 0;
    if (rnd() < 0.12) ask = 0;
    if (bid + ask <= 0) continue;

    // A diagonal imbalance, the way the detector reports one: one side far
    // larger than the other, with a run length. Present on about a sixth of the
    // levels, which is roughly what a real window looks like.
    let imbalance = null;
    if (rnd() < 0.17) {
      const buy = ask > bid;
      const ratio = 1.4 + rnd() * 3.2;
      imbalance = { side: buy ? "buy" : "sell", ratio, stacked: 1 + Math.floor(rnd() * 4) };
    }
    cells.push({ price, bid, ask, delta: ask - bid, imbalance });
  }

  const bid_volume = cells.reduce((sum, c) => sum + c.bid, 0);
  const ask_volume = cells.reduce((sum, c) => sum + c.ask, 0);
  const poc = cells.reduce((best, c) => (c.bid + c.ask > best.bid + best.ask ? c : best), cells[0]);

  return {
    open_time: (OPEN_MS + index * 300_000) * 1_000_000,
    open,
    high,
    low,
    close,
    volume: bid_volume + ask_volume,
    bid_volume,
    ask_volume,
    delta: ask_volume - bid_volume,
    poc: poc.price,
    cells,
  };
}

const columns = Array.from({ length: COLUMNS }, (_, index) => column(index));

// The candles are not drawn in this mode; the engine uses them for the price
// axis, which is the same axis every ladder is placed against.
const candles = columns.map((c) => ({
  symbol: "BTCUSDT",
  timeframe: "5m",
  open_time: c.open_time,
  open: c.open,
  high: c.high,
  low: c.low,
  close: c.close,
  volume: c.volume,
  buy_volume: c.ask_volume,
  sell_volume: c.bid_volume,
}));

const scene = build({
  candles,
  width: 1180,
  height: 560,
  mode: "footprint",
  footprint: columns,
  footprint_trades: 143_960,
  lines: ["poc", "vah", "val"],
});

const grid = scene.footprint;
console.log(
  `${scene.candles.length} candles -> ${grid ? grid.columns.length : 0} columns, ` +
    `${grid ? grid.rows.length : 0} rows, font ${grid ? grid.font_px.toFixed(2) : "-"}px, ` +
    `${grid ? grid.stats.trades : 0} trades`
);
if (!grid) throw new Error("the engine returned no grid: the request had no footprint columns");

const imbalanced = grid.columns.reduce(
  (sum, column) => sum + column.cells.filter((cell) => cell.side).length,
  0
);
const drawn = grid.columns.reduce(
  (sum, column) =>
    sum + column.cells.filter((cell) => cell.bid + cell.ask > 0).length,
  0
);
console.log(`  ${drawn} cells with volume, ${imbalanced} of them imbalanced`);
console.log(`  note: ${scene.note || "(none)"}`);

const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>chart-engine preview — the footprint ladder</title>
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
  p.lede { color: var(--muted); margin: 0 0 16px; max-width: 88ch; }
  .wrap { position: relative; border: 1px solid var(--line); border-radius: 8px;
          background: var(--panel); overflow: hidden; }
  canvas { display: block; width: 100%; }
  .stats { display: flex; flex-wrap: wrap; gap: 14px; margin-top: 10px;
           font-family: var(--mono); font-size: 11px; color: var(--muted); }
  .stats b { color: var(--text); font-weight: 500; }
  ul { list-style: none; padding: 0; margin: 12px 0 0; display: flex; flex-wrap: wrap; gap: 8px; }
  li { font-family: var(--mono); font-size: 11px; border: 1px solid var(--line);
       border-radius: 999px; padding: 3px 10px; color: var(--muted); }
  .swatch { display: inline-block; width: 9px; height: 9px; border-radius: 2px;
            margin-right: 5px; vertical-align: -1px; }
  code { font-family: var(--mono); color: var(--accent); }
  .note { color: var(--muted); font-size: 11px; margin-top: 14px; max-width: 96ch; }
</style>
</head>
<body>
<h1>The footprint ladder, drawn the way the shell draws it</h1>
<p class="lede">
  A real <code>Mode::Footprint</code> scene from <code>chart_engine.wasm</code>, rendered with
  the same rules as <code>drawFootprintGrid</code> in <code>app.js</code>. Every coordinate and
  every string below comes from the engine; the drawing only picks colours and calls
  <code>fillText</code>.
</p>
<p class="lede">
  The three things to look at: <strong>every cell with volume is filled</strong>, tinted by
  which side won the level; <strong>the value area is banded per column</strong>, so it is a
  band inside each ladder rather than a stripe across all of them; and <strong>the pair reads
  as <code>bid x ask</code></strong>, centred, rather than as two numbers pushed into the two
  halves of the cell.
</p>
<div class="wrap"><canvas id="c"></canvas></div>
<div class="stats" id="stats"></div>
<ul id="legend"></ul>
<p class="note">
  <strong>An imbalance is outlined, and its outline is the data.</strong> A diagonal imbalance
  is this level measured against the level below it — not simply which of this cell's two
  numbers is larger — which is why a bid-heavy cell can still be flagged as a buy imbalance.
  The <code>ratio</code> sets how saturated the outline is and <code>stacked</code> sets how
  thick, so both numbers the detector produces are load-bearing rather than carried and unused.
  <br /><br />
  A pair that does not fit in its cell is <strong>not drawn</strong>. A truncated number is a
  wrong number, and a ladder of wrong numbers is worse than a ladder of colours — so below
  roughly 54px a column is a heat map, which is the honest thing for it to be. Narrow the
  window and that is what you should see.
</p>
<script>
const scene = ${JSON.stringify(scene)};

const COLORS = {
  up: "#26a69a", down: "#ef5350", wick: "#8b949e", grid: "#21262d",
  text: "#8b949e", vwap: "#d29922", poc: "#e6edf3", vah: "#8b949e", val: "#8b949e",
  footBuy: "#a371f7", footSell: "#4aa3ff", footValue: "#58a6ff",
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

// --- drawFootprintGrid, copied from frontend/app/app.js ----------------------

function drawFootprintGrid(ctx, scene) {
  const grid = scene.footprint;
  const font = Math.max(6, Math.min(11, grid.font_px));
  // The engine decides, not the shell -- see drawFootprintGrid in app.js.
  const showText = grid.show_text;
  // Looser than the engine's own 3px-a-side assumption, deliberately -- see
  // drawFootprintGrid in app.js.
  const margin = Math.max(2, font * 0.3);

  ctx.font = font + "px ui-monospace, monospace";
  ctx.textBaseline = "middle";
  ctx.textAlign = "center";

  for (const column of grid.columns) {
    for (const cell of column.cells) {
      if (!cell.in_value_area) continue;
      ctx.globalAlpha = 0.14;
      ctx.fillStyle = COLORS.footValue;
      ctx.fillRect(cell.x, cell.y, cell.w, cell.h);
      ctx.globalAlpha = 1;
    }
  }

  for (const column of grid.columns) {
    // A faint column background, then the frame -- see drawFootprintGrid in app.js.
    ctx.fillStyle = "#12171f";
    ctx.fillRect(column.x, scene.plot.y, column.w, scene.plot.h);
    ctx.strokeStyle = COLORS.grid;
    ctx.lineWidth = 1;
    ctx.strokeRect(column.x + 0.5, scene.plot.y + 0.5, column.w - 1, scene.plot.h - 1);

    for (const cell of column.cells) {
      const total = cell.bid + cell.ask;
      if (total <= 0) continue;

      const buyShare = cell.ask / total;
      const leansBuy = buyShare >= 0.5;
      const oneSided = Math.abs(buyShare - 0.5) * 2;
      const colour = leansBuy ? COLORS.footBuy : COLORS.footSell;

      ctx.globalAlpha = 0.3 + oneSided * 0.42;
      ctx.fillStyle = colour;
      ctx.fillRect(cell.x + 1, cell.y, cell.w - 2, cell.h - 1);
      ctx.globalAlpha = 1;

      if (cell.is_poc) {
        ctx.strokeStyle = COLORS.poc;
        ctx.lineWidth = 1;
        ctx.strokeRect(cell.x + 1.5, cell.y + 0.5, cell.w - 3, Math.max(1, cell.h - 1));
      }

      if (cell.side) {
        const strength = Math.max(0, Math.min(1, ((cell.ratio || 1) - 1) / 3));
        ctx.strokeStyle = colour;
        ctx.globalAlpha = 0.7 + strength * 0.3;
        ctx.lineWidth = 1 + Math.min(2, (cell.stacked || 1) - 1);
        ctx.strokeRect(cell.x + 1.5, cell.y + 0.5, cell.w - 3, Math.max(1, cell.h - 1));
        ctx.globalAlpha = 1;
      }

      if (!showText) continue;
      const pair = cell.bid_text + " x " + cell.ask_text;
      if (ctx.measureText(pair).width > cell.w - margin * 2) continue;
      ctx.fillStyle = "#e6edf3";
      ctx.fillText(pair, cell.x + cell.w / 2, cell.y + cell.h / 2);
    }

    const summary = column.summary;
    ctx.fillStyle = "#161b22";
    ctx.fillRect(summary.x + 1, summary.y, summary.w - 2, summary.h);
    ctx.strokeStyle = COLORS.grid;
    ctx.lineWidth = 1;
    ctx.strokeRect(summary.x + 0.5, summary.y + 0.5, summary.w - 1, summary.h - 1);
    if (showText) {
      ctx.textAlign = "center";
      ctx.fillStyle = "#e6edf3";
      ctx.fillText(summary.volume_text, summary.x + summary.w / 2, summary.y + font * 0.95);
      ctx.fillStyle = summary.delta_positive ? COLORS.up : COLORS.down;
      ctx.fillText(summary.delta_text, summary.x + summary.w / 2, summary.y + summary.h - font * 0.75);
    }
  }
  ctx.textAlign = "left";
}

drawFootprintGrid(ctx, scene);

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
ctx.textAlign = "left";
for (const tick of scene.ticks) {
  ctx.fillText(tick.price.toFixed(2), scene.plot.x + scene.plot.w + 6, tick.y + 3);
}

// --- the footer strip, as the shell renders it -------------------------------

const s = scene.footprint.stats;
const field = (label, value, cls) =>
  '<span><b>' + label + '</b><span class="' + (cls || "") + '">' + value + "</span></span>";
document.getElementById("stats").innerHTML = [
  field("trades", s.trades.toLocaleString()),
  field("columns", s.columns),
  field("rows", s.rows),
  field("bid", s.bid_text),
  field("ask", s.ask_text),
  field("total", s.total_text),
  field("delta", s.delta_text),
  field("max delta", s.max_delta_text),
  field("min delta", s.min_delta_text),
  field("font", scene.footprint.font_px.toFixed(1) + "px"),
  field("text drawn", scene.footprint.show_text ? "yes" : "no — cells are a heat map"),
  field("engine note", scene.note || "(none)"),
].join("");

const legend = document.getElementById("legend");
for (const [label, colour] of [
  ["ask side won the level", COLORS.footBuy],
  ["bid side won the level", COLORS.footSell],
  ["value area", COLORS.footValue],
  ["point of control (per column)", COLORS.poc],
]) {
  const li = document.createElement("li");
  li.innerHTML =
    '<span class="swatch" style="background:' + colour + '"></span>' + label;
  legend.appendChild(li);
}
</script>
</body>
</html>
`;

await writeFile(outPath, html);
console.log(`wrote ${outPath}`);
