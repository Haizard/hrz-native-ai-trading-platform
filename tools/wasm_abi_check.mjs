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
check(
  "without trades the profile is drawn instead",
  footprint.profile.length > 0 && footprint.footprint === null,
  `${footprint.profile.length} profile bars, footprint=${footprint.footprint}`
);
check(
  "and says which footprint it is",
  typeof footprint.note === "string" && footprint.note.includes("no trades are stored"),
  footprint.note
);

// --- the trade-level footprint ----------------------------------------------
//
// The ladder is the one mode whose data does not come from `/candles`, so it is
// the one most likely to break at the ABI without anything else noticing.

const ladders = [
  {
    open_time: 1_788_998_400_000_000_000,
    open: 100, high: 102, low: 99, close: 101,
    volume: 12, bid_volume: 5, ask_volume: 7, delta: 2, poc: 100,
    cells: [
      { price: 100, bid: 0.4, ask: 2.4, delta: 2.0, imbalance: { side: "buy", ratio: 6.0, stacked: 2 } },
      { price: 101, bid: 1.5, ask: 1.6, delta: 0.1, imbalance: null },
    ],
  },
  {
    open_time: 1_788_998_700_000_000_000,
    open: 101, high: 103, low: 100, close: 102,
    volume: 9, bid_volume: 6, ask_volume: 3, delta: -3, poc: 101,
    cells: [
      { price: 101, bid: 2.9, ask: 0.4, delta: -2.5, imbalance: { side: "sell", ratio: 7.2, stacked: 1 } },
    ],
  },
];

const footprintScene = build({
  candles: candles(4),
  width: 900,
  height: 420,
  mode: "footprint",
  footprint: ladders,
  footprint_trades: 1_234,
});

const grid = footprintScene.footprint;
check("a trade-level grid comes back", Boolean(grid), "footprint: null");
if (grid) {
  check("the axis is the union of both ladders", grid.rows.length === 2, `got ${grid.rows.length}`);
  check("every column is drawn", grid.columns.length === 2, `got ${grid.columns.length}`);
  // Price 101 is in both ladders; price 100 is only in the first. Comparing
  // cells[0] of each would compare two different levels and prove nothing.
  const at = (column, price) => column.cells.find((c) => c.price === price);
  const sharedLeft = at(grid.columns[0], 101);
  const sharedRight = at(grid.columns[1], 101);
  check(
    "a shared level sits at one height in every column",
    Boolean(sharedLeft && sharedRight) && sharedLeft.y === sharedRight.y,
    `left=${sharedLeft && sharedLeft.y} right=${sharedRight && sharedRight.y}`
  );
  check(
    "a level only one column has still lands on the shared axis",
    Boolean(at(grid.columns[0], 100)) && !at(grid.columns[1], 100),
    "a sparse ladder must not invent rows"
  );
  check(
    "the imbalance is carried for colouring",
    grid.columns[0].cells[0].side === "buy" && grid.columns[1].cells[0].side === "sell"
  );
  check(
    "the text is formatted by the engine",
    grid.columns[0].cells[0].bid_text === "0.40" && grid.columns[0].cells[0].ask_text === "2.40",
    `${grid.columns[0].cells[0].bid_text} x ${grid.columns[0].cells[0].ask_text}`
  );
  check("the stats carry the trade count", grid.stats.trades === 1_234);
  check("the summary row sits below the plot", grid.columns[0].summary.y > footprintScene.plot.y + footprintScene.plot.h);
  check(
    "the fallback profile is not also drawn",
    footprintScene.profile.length === 0,
    "two charts at once would overlap"
  );
}

// Without ladders the same mode must fall back, and say so.
const fallback = build({ candles: candles(60), width: 900, height: 420, mode: "footprint" });
check(
  "without trades it falls back and explains",
  fallback.footprint === null && typeof fallback.note === "string" && fallback.note.includes("no trades"),
  fallback.note
);

// --- regions: supply/demand zones -------------------------------------------
//
// The first *area* the engine has ever drawn. Everything else in the scene is a
// point or a rectangle standing for one price at one time, so this is the shape
// most likely to arrive at the shell half-formed.

/// A series with a demand zone in it.
///
/// The same shape the Rust tests use: a decline that leaves a confirmed swing
/// high, a three-candle pause, and an impulse that closes through it.
function zonedCandles() {
  const rows = [
    [100.0, 101.0, 99.0, 99.5],
    [99.5, 100.0, 96.0, 96.5],
    [96.5, 98.0, 95.0, 97.5],
    [97.5, 98.5, 94.0, 94.5],
    [94.5, 95.5, 92.0, 92.5],
    [92.5, 93.5, 90.0, 93.0],
    [93.0, 96.0, 92.5, 95.5],
    [96.5, 97.0, 95.0, 95.5],
    [96.0, 96.2, 94.6, 94.8],
    [94.8, 95.0, 93.4, 93.6],
    [93.6, 93.9, 92.8, 93.0],
    [93.0, 95.5, 92.9, 95.0],
    [95.0, 97.5, 94.8, 97.0],
    [97.0, 100.0, 96.8, 99.5],
    [99.5, 102.0, 99.0, 101.5],
    [101.5, 104.0, 101.0, 103.5],
    [103.5, 104.0, 100.0, 100.5],
    [100.5, 101.0, 96.0, 96.5],
    [96.5, 97.0, 93.5, 94.0],
    [94.0, 95.0, 93.8, 94.8],
  ];
  return candlesFromRows(rows);
}

/// A series with exactly one three-candle gap in it -- candles 4, 5 and 6.
///
/// Every other window of three fails the rule, and the candles after the gap
/// stay above it, so the band is still fresh.
function gapCandles() {
  const rows = [
    [100.0, 100.6, 99.2, 99.8],
    [99.8, 100.4, 99.0, 99.6],
    [99.6, 100.8, 99.1, 100.2],
    [100.2, 101.0, 99.9, 100.4],
    [100.4, 100.5, 99.5, 100.0],
    [100.0, 106.0, 100.6, 105.5],
    [105.5, 105.6, 103.0, 104.0],
    [104.0, 105.5, 103.5, 105.0],
    [105.0, 106.0, 104.0, 105.5],
    [105.5, 106.2, 104.8, 106.0],
    [106.0, 106.5, 105.0, 105.8],
    [105.8, 106.4, 104.9, 105.2],
  ];
  return candlesFromRows(rows);
}

function candlesFromRows(rows) {
  return rows.map(([open, high, low, close], i) => ({
    symbol: "BTCUSDT",
    timeframe: "5m",
    open_time: i * 300_000_000_000,
    open, high, low, close,
    volume: 10, buy_volume: 6, sell_volume: 4,
  }));
}

// Off by default. Detection is a real cost on a long window, and an overlay
// nobody asked for is noise on top of the candles.
//
// The presence check comes first and is not redundant: against a *stale* wasm
// the key is absent, and `(scene.regions ?? []).length === 0` would then pass
// while the shell's draw loop threw on `undefined.length`. An empty overlay and
// a missing one are different facts, so they are asserted separately.
check(
  "the scene always carries a `regions` array",
  Array.isArray(scene.regions),
  `got ${JSON.stringify(scene.regions)}`
);
check(
  "zones are off unless asked for",
  Array.isArray(scene.regions) && scene.regions.length === 0,
  `${scene.regions && scene.regions.length} regions in a request that never mentioned them`
);

const zoned = build({
  candles: zonedCandles(),
  width: 900,
  height: 420,
  mode: "candles",
  zones: true,
});

check(
  "a zone comes back",
  Array.isArray(zoned.regions) && zoned.regions.length > 0,
  `got ${JSON.stringify(zoned.regions)}`
);

const zone = (zoned.regions ?? []).find((r) => r.name === "demand");
check("and it is a demand zone", Boolean(zone), JSON.stringify(zoned.regions.map((r) => r.name)));

if (zone) {
  // Every key the shell reads. A rename is not a compile error anywhere -- it is
  // an overlay that silently stops appearing, which looks like "no zones in this
  // window" rather than like a bug.
  for (const key of [
    "name", "side", "x", "w", "y_top", "h", "price_low", "price_high",
    "mitigated", "fresh", "origin", "label",
  ]) {
    check(`the region carries \`${key}\``, zone[key] !== undefined && zone[key] !== null);
  }
  // `name` is the colour key and `side` the fallback, so both are checked for
  // the *value* and not just for presence: a `Side` that leaked its own
  // `"Buy"` onto the wire would still be a non-null string.
  check(
    "the name is snake_case on the wire",
    zone.name === "demand",
    zone.name
  );
  check("the side is snake_case on the wire", zone.side === "buy", zone.side);
  check(
    "the band is an area, not a line",
    zone.h > 0 && zone.w > 0 && zone.price_high > zone.price_low,
    `w=${zone.w} h=${zone.h} ${zone.price_low}..${zone.price_high}`
  );
  check(
    "the band is inside the plot",
    zone.x >= zoned.plot.x - 1 &&
      zone.x + zone.w <= zoned.plot.x + zoned.plot.w + 1 &&
      zone.y_top >= zoned.plot.y - 1 &&
      zone.y_top + zone.h <= zoned.plot.y + zoned.plot.h + 1,
    `x=${zone.x} w=${zone.w} y=${zone.y_top} h=${zone.h}`
  );
  check(
    "the engine formatted the label",
    typeof zone.label === "string" && zone.label.startsWith("demand"),
    zone.label
  );
  check(
    "the band carries the prices it was detected from",
    zone.price_low === 92.8 && zone.price_high === 96.2,
    `${zone.price_low}..${zone.price_high}`
  );
  check(
    "it says which break put it there",
    zone.origin &&
      zone.origin.source === "structure_break" &&
      zone.origin.kind === "bos" &&
      zone.origin.level === 97.0,
    JSON.stringify(zone.origin)
  );
}

// --- regions: a concept the client wrote ------------------------------------
//
// The feature this whole layer exists for. Nothing in this workspace knows what
// a fair value gap is -- there is no detector for it, no enum variant, no field.
// This document is the entire definition, and it travels in the request as
// data: `crates/sandbox` never compiles untrusted code, and a concept document
// is how a client adds a word to the vocabulary without adding code.

/// A fair value gap: three candles, the first candle's high left behind below
/// the third candle's low.
function gapConcept() {
  return {
    name: "bullish_gap",
    label: "bullish gap",
    side: "buy",
    window: 3,
    lower: { high: 0 },
    upper: { low: 2 },
    require: [{ left: { high: 0 }, op: "below", right: { low: 2 } }],
  };
}

const conceived = build({
  candles: gapCandles(),
  width: 900,
  height: 420,
  mode: "candles",
  concepts: [gapConcept()],
});

const gap = (conceived.regions ?? []).find((r) => r.name === "bullish gap");
check(
  "a concept the client wrote comes back as a band",
  Boolean(gap),
  `got ${JSON.stringify(conceived.regions)}`
);

if (gap) {
  check(
    "the band is exactly what the document asked for",
    gap.price_low === 100.5 && gap.price_high === 103.0,
    `${gap.price_low}..${gap.price_high}`
  );
  check("the client's side survives the trip", gap.side === "buy", gap.side);
  check(
    "a client's band says it came from a pattern, not a break",
    gap.origin && gap.origin.source === "pattern",
    JSON.stringify(gap.origin)
  );
  check(
    "the label is the client's, formatted by the engine",
    gap.label === "bullish gap (fresh)",
    gap.label
  );
  check(
    "and it is a real rectangle inside the plot",
    gap.w > 0 &&
      gap.h > 0 &&
      gap.x >= conceived.plot.x - 1 &&
      gap.x + gap.w <= conceived.plot.x + conceived.plot.w + 1,
    `x=${gap.x} w=${gap.w} h=${gap.h}`
  );
  check("nothing was refused", !conceived.note, conceived.note ?? "(no note)");
}

// A document that cannot mean anything is refused *and* says why. The ratio is
// the interesting refusal: detection alone would happily draw it -- every band
// is at least -50% of its window's range -- so the band being absent is the
// validator doing its job rather than the detector failing to match.
const refused = build({
  candles: gapCandles(),
  width: 900,
  height: 420,
  mode: "candles",
  concepts: [{ ...gapConcept(), min_band_ratio: -0.5 }],
});

check(
  "a refused concept is not drawn",
  Array.isArray(refused.regions) && refused.regions.length === 0,
  JSON.stringify(refused.regions)
);
check(
  "and the refusal names the document and the reason",
  typeof refused.note === "string" &&
    refused.note.includes("bullish_gap") &&
    refused.note.includes("refused") &&
    refused.note.includes("-0.5"),
  refused.note ?? "(no note)"
);

// --- the levels default -----------------------------------------------------
//
// `#[serde(default)]` on a `Vec` fills in an *empty* one, so a request that
// never mentions `lines` would draw nothing while `Request::default()` promised
// four. This check is where that was found, so it stays.

const noLines = build({ candles: candles(60), width: 900, height: 420 });
check(
  "omitting `lines` does not mean no levels",
  noLines.levels.length === 4,
  `got ${noLines.levels.length}`
);
const emptyLines = build({ candles: candles(60), width: 900, height: 420, lines: [] });
check(
  "an explicit empty list does mean no levels",
  emptyLines.levels.length === 0,
  `got ${emptyLines.levels.length}`
);

// --- the viewport and the gestures ------------------------------------------
//
// The host tests cover `scene::build` thoroughly, but they build `Viewport` and
// `Gesture` *in Rust* -- so they cannot see the JSON the shell actually sends. A
// mis-tagged variant (`zoomTime` for `zoom_time`) or a field renamed from
// `fraction` to `time` passes every Rust test and fails in the browser with a
// chart that will not move. That is the gap this section exists to close, and it
// is the same shape as the `lines` default found above.

const all = build({ candles: candles(200), width: 900, height: 420 });
check(
  "a request that never mentions a viewport shows everything",
  all.candles.length === 200 && all.viewport.from === 0,
  `drew ${all.candles.length} of 200, viewport ${JSON.stringify(all.viewport)}`
);
check(
  "and it reports `count` as null, so echoing it keeps following the market",
  all.viewport.count === null,
  JSON.stringify(all.viewport)
);
check(
  "the reported viewport carries no `total` -- the shell never learns the bar count",
  !("total" in all.viewport),
  JSON.stringify(all.viewport)
);

// A window, exactly as the shell echoes one back.
const windowed = build({
  candles: candles(200),
  width: 900,
  height: 420,
  viewport: { from: 50, count: 25, price: null },
});
check(
  "a viewport narrows what is drawn",
  windowed.candles.length === 25,
  `drew ${windowed.candles.length}`
);
check(
  "and the bars grow, because the slot is the plot over the window",
  windowed.candles[0].w > all.candles[0].w,
  `${windowed.candles[0].w} vs ${all.candles[0].w}`
);
check(
  "and it comes back as the window that was asked for",
  windowed.viewport.from === 50 && windowed.viewport.count === 25,
  JSON.stringify(windowed.viewport)
);
check(
  "the drawn bars come from the window, not from the start of the series",
  windowed.price_min > all.price_min,
  `window starts at ${windowed.price_min}, series at ${all.price_min}`
);

// The gestures, by the exact tags the shell sends.
const zoomed = build({
  candles: candles(200),
  width: 900,
  height: 420,
  gesture: { kind: "zoom_time", factor: 2, anchor: 0.5 },
});
check(
  "`zoom_time` is the tag the engine knows",
  zoomed.candles.length === 100,
  `drew ${zoomed.candles.length}, expected 100`
);
check(
  "and the bar under the anchor held still",
  Math.abs(zoomed.viewport.from - 50) <= 1,
  `from ${zoomed.viewport.from}, expected about 50`
);

const panned = build({
  candles: candles(200),
  width: 900,
  height: 420,
  viewport: { from: 50, count: 100, price: null },
  gesture: { kind: "pan", time: 0.5, price: 0 },
});
check(
  "`pan` carries both axes and the time half moves",
  panned.viewport.from === 100,
  `from ${panned.viewport.from}, expected 100`
);
check(
  "and panning did not change the zoom",
  panned.viewport.count === 100,
  `count ${panned.viewport.count}`
);

const priceZoomed = build({
  candles: candles(200),
  width: 900,
  height: 420,
  gesture: { kind: "zoom_price", factor: 2, anchor: 0.5 },
});
check(
  "`zoom_price` comes back as an explicit range",
  priceZoomed.viewport.price !== null &&
    priceZoomed.viewport.price.max > priceZoomed.viewport.price.min,
  JSON.stringify(priceZoomed.viewport.price)
);
check(
  "and the axis really is narrower than the fitted one",
  priceZoomed.price_max - priceZoomed.price_min < all.price_max - all.price_min,
  `${priceZoomed.price_max - priceZoomed.price_min} vs ${all.price_max - all.price_min}`
);

const refitted = build({
  candles: candles(200),
  width: 900,
  height: 420,
  viewport: { from: 50, count: 25, price: { min: 150, max: 160 } },
  gesture: { kind: "fit" },
});
check(
  "`fit` hands back the whole series and the fitted price axis",
  refitted.candles.length === 200 &&
    refitted.viewport.price === null &&
    refitted.viewport.count === null,
  `drew ${refitted.candles.length}, viewport ${JSON.stringify(refitted.viewport)}`
);

// An unknown gesture is a refusal with a message, not a trap: the shell can show
// the message and the page survives.
let unknownGesture = null;
try {
  build({
    candles: candles(20),
    width: 900,
    height: 420,
    gesture: { kind: "zoomDiagonal", factor: 2 },
  });
} catch (e) {
  unknownGesture = e.message;
}
check(
  "an unknown gesture kind is refused with a message",
  typeof unknownGesture === "string" && unknownGesture.includes("zoomDiagonal"),
  unknownGesture ?? "it was accepted"
);

// Hostile numbers, through the real boundary rather than through Rust.
//
// JSON has no `NaN` or `Infinity` literal, so what a client can actually send is
// a huge finite number or an out-of-range exponent. Whether `serde_json` turns
// `1e999` into infinity or refuses it outright is a detail of the parser rather
// than of this engine, so both outcomes are acceptable here -- and a blank chart
// or a trap is not. The Rust sweep proves the arithmetic; this proves the JSON
// survives the trip.
for (const factor of [1e308, 1e999, -1e308, 0, -1, -0]) {
  let refused = null;
  let drawn = null;
  try {
    drawn = build({
      candles: candles(200),
      width: 900,
      height: 420,
      gesture: { kind: "zoom_time", factor, anchor: 0.5 },
    }).candles.length;
  } catch (e) {
    refused = e.message;
  }
  check(
    `a factor of ${factor} is refused or usable, never blank`,
    refused !== null || (drawn >= 10 && drawn <= 200),
    refused ?? `drew ${drawn} bars`
  );
}

for (const anchor of [1e308, 1e999, -1e308]) {
  let refused = null;
  let usable = false;
  try {
    const scene = build({
      candles: candles(200),
      width: 900,
      height: 420,
      gesture: { kind: "zoom_price", factor: 2, anchor },
    });
    usable = scene.viewport.price !== null && scene.price_max > scene.price_min;
  } catch (e) {
    refused = e.message;
  }
  check(
    `an anchor of ${anchor} is refused or usable, never blank`,
    refused !== null || usable,
    refused ?? `price axis ${usable ? "ok" : "collapsed"}`
  );
}

// --- drawings ---------------------------------------------------------------
//
// The shell sends this JSON and reads this JSON back, and the two ends of the
// contract are in different languages. The Rust tests build a `Drawing` in Rust,
// so they cannot see a mis-tagged anchor or a renamed key -- and both of those
// are a drawing that silently never appears, which reads as "the tool does
// nothing" rather than as a bug.

check(
  "the scene always carries a `drawings` array",
  Array.isArray(scene.drawings),
  `got ${JSON.stringify(scene.drawings)}`
);

const drawn = build({
  candles: candles(120),
  width: 900,
  height: 420,
  drawings: [
    {
      id: "d1",
      kind: "trendline",
      a1: { unit: "fraction", x: 0.2, y: 0.7 },
      a2: { unit: "fraction", x: 0.6, y: 0.3 },
      label: null,
      selected: false,
    },
  ],
});

check(
  "a drawing placed by fraction comes back",
  Array.isArray(drawn.drawings) && drawn.drawings.length === 1,
  JSON.stringify(drawn.drawings)
);

const segment = (drawing) => drawing.parts.find((part) => part.shape === "segment");
const handles = (drawing) => drawing.parts.filter((part) => part.shape === "handle");
const line = (drawn.drawings ?? [])[0];

if (line) {
  check("the id is echoed", line.id === "d1", line.id);
  check("the kind is snake_case on the wire", line.kind === "trendline", line.kind);
  check(
    "the anchors come back absolute, ready to store",
    line.a1.unit === "absolute" &&
      Number.isFinite(line.a1.time) &&
      Number.isFinite(line.a1.price) &&
      line.a2.unit === "absolute",
    JSON.stringify([line.a1, line.a2])
  );
  // The drag base. Not an `Anchor`: it is only ever a fraction, so it is only
  // ever two numbers, and a `unit` here would be a field the shell can read and
  // can never find to be anything else.
  check(
    "and the fractions a body drag adds to are a plain pair",
    Number.isFinite(line.a1_fraction?.x) &&
      Number.isFinite(line.a1_fraction?.y) &&
      Number.isFinite(line.a2_fraction?.x) &&
      line.a1_fraction.unit === undefined,
    JSON.stringify([line.a1_fraction, line.a2_fraction])
  );

  const before = segment(line);

  // Place with a fraction, store what came back, reload: the pixels must be the
  // same. This is the property that stops a drawing drifting a little further
  // every time it is dragged.
  const reloaded = build({
    candles: candles(120),
    width: 900,
    height: 420,
    drawings: [
      { id: "d1", kind: "trendline", a1: line.a1, a2: line.a2, label: null, selected: false },
    ],
  });
  const after = segment(reloaded.drawings[0]);
  check(
    "what was stored puts the drawing back on the same pixels",
    Boolean(before && after) &&
      Math.abs(before.x1 - after.x1) < 1e-9 &&
      Math.abs(before.y1 - after.y1) < 1e-9 &&
      Math.abs(before.x2 - after.x2) < 1e-9 &&
      Math.abs(before.y2 - after.y2) < 1e-9,
    `${JSON.stringify(before)} vs ${JSON.stringify(after)}`
  );

  // The body drag, in the shell's own arithmetic: the pointer's offset over the
  // plot's size, added to both fractions. Both ends have to move by the same
  // pixels, or a trendline dragged by its middle changes slope -- which is what
  // the shell used to do by sending one anchor to the pointer.
  const dx = 0.1;
  const dy = -0.05;
  const shift = (f) => ({ unit: "fraction", x: f.x + dx, y: f.y + dy });
  const moved = build({
    candles: candles(120),
    width: 900,
    height: 420,
    drawings: [
      {
        id: "d1",
        kind: "trendline",
        a1: shift(line.a1_fraction),
        a2: shift(line.a2_fraction),
        label: null,
        selected: false,
      },
    ],
  });
  const shifted = segment(moved.drawings[0]);
  check(
    "a body drag moves both ends by the same pixels",
    Boolean(shifted) &&
      Math.abs(shifted.x1 - before.x1 - dx * drawn.plot.w) < 1e-6 &&
      Math.abs(shifted.x2 - before.x2 - dx * drawn.plot.w) < 1e-6 &&
      Math.abs(shifted.y1 - before.y1 - dy * drawn.plot.h) < 1e-6 &&
      Math.abs(shifted.y2 - before.y2 - dy * drawn.plot.h) < 1e-6,
    `moved ${JSON.stringify(shifted)} from ${JSON.stringify(before)}`
  );
}

// Handles belong to the selected drawing and to no other. They are what the
// shell hit-tests, so a handle on an unselected drawing is a grab point for
// something the user never picked.
const pair = build({
  candles: candles(120),
  width: 900,
  height: 420,
  drawings: [
    { id: "a", kind: "trendline", a1: { unit: "fraction", x: 0.1, y: 0.1 }, a2: { unit: "fraction", x: 0.2, y: 0.2 }, label: null, selected: false },
    { id: "b", kind: "trendline", a1: { unit: "fraction", x: 0.3, y: 0.3 }, a2: { unit: "fraction", x: 0.4, y: 0.4 }, label: null, selected: true },
  ],
});
check(
  "only the selected drawing offers handles",
  handles(pair.drawings[0]).length === 0 && handles(pair.drawings[1]).length === 2,
  `${handles(pair.drawings[0]).length} and ${handles(pair.drawings[1]).length}`
);
check(
  "and they are numbered the way the shell reads them",
  handles(pair.drawings[1])
    .map((h) => h.anchor)
    .join(",") === "0,1",
  JSON.stringify(handles(pair.drawings[1]).map((h) => h.anchor))
);

// A horizontal line takes one anchor, and a stored second one is not an error --
// the column permits it. It must still offer a single handle, because the
// drawing does not read the second anchor, and a grab point the engine would not
// honour is a drag that does nothing.
const horizontal = build({
  candles: candles(120),
  width: 900,
  height: 420,
  drawings: [
    { id: "h", kind: "hline", a1: { unit: "fraction", x: 0.5, y: 0.4 }, a2: { unit: "fraction", x: 0.9, y: 0.4 }, label: null, selected: true },
  ],
});
check(
  "a horizontal line offers one handle even with a stored second anchor",
  handles(horizontal.drawings[0]).length === 1,
  JSON.stringify(handles(horizontal.drawings[0]))
);

// A refusal is a note and an absent drawing -- not a trap, and not a blank shape
// sitting on the chart looking like one that was drawn.
const halfDrawn = build({
  candles: candles(120),
  width: 900,
  height: 420,
  drawings: [
    { id: "r", kind: "rect", a1: { unit: "fraction", x: 0.2, y: 0.2 }, label: null, selected: false },
  ],
});
check(
  "a two-anchor kind with one anchor is not drawn",
  Array.isArray(halfDrawn.drawings) && halfDrawn.drawings.length === 0,
  JSON.stringify(halfDrawn.drawings)
);
check(
  "and the note names the drawing and the reason",
  typeof halfDrawn.note === "string" &&
    halfDrawn.note.includes("rect") &&
    halfDrawn.note.includes("two anchors"),
  halfDrawn.note ?? "(no note)"
);

// A click with no drag on a tool that needs two points. This is the one refusal
// the shell cannot pre-empt: it does not know whether the pointer moved, and it
// should not have to -- the engine compares the two anchors it resolved.
const clicked = build({
  candles: candles(120),
  width: 900,
  height: 420,
  drawings: [
    {
      id: "c",
      kind: "trendline",
      a1: { unit: "fraction", x: 0.3, y: 0.3 },
      a2: { unit: "fraction", x: 0.3, y: 0.3 },
      label: null,
      selected: false,
    },
  ],
});
check(
  "a click with no drag is not stored as a shape with no extent",
  Array.isArray(clicked.drawings) && clicked.drawings.length === 0,
  JSON.stringify(clicked.drawings)
);
check(
  "and it says so rather than silently doing nothing",
  typeof clicked.note === "string" && clicked.note.includes("no extent"),
  clicked.note ?? "(no note)"
);

// An unknown kind is refused rather than drawn as something else. This is the
// check that makes "the database and the engine agree on the vocabulary" true
// from the browser's side.
let unknownKind = null;
try {
  build({
    candles: candles(20),
    width: 900,
    height: 420,
    drawings: [
      { id: "x", kind: "gann_fan", a1: { unit: "fraction", x: 0.5, y: 0.5 }, label: null, selected: false },
    ],
  });
} catch (e) {
  unknownKind = e.message;
}
check(
  "an unknown drawing kind is refused with a message",
  typeof unknownKind === "string" && unknownKind.includes("gann_fan"),
  unknownKind ?? "it was accepted"
);

// JSON has no `Infinity`, so a client that computes one sends `null`. That is
// not a hypothetical: it is what a drag produces the moment a plot has no width.
// The refusal has to be a message rather than a trap.
let nullAnchor = null;
try {
  build({
    candles: candles(20),
    width: 900,
    height: 420,
    drawings: [
      {
        id: "n",
        kind: "trendline",
        a1: { unit: "fraction", x: 0.5, y: null },
        a2: { unit: "fraction", x: 0.6, y: 0.6 },
        label: null,
        selected: false,
      },
    ],
  });
} catch (e) {
  nullAnchor = e.message;
}
check(
  "an anchor of `null` is refused rather than drawn",
  typeof nullAnchor === "string" && nullAnchor.length > 0,
  nullAnchor ?? "it was accepted"
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

// --- the answer's levels cross the boundary as prices ----------------------
//
// This is the one place a rename is invisible: serde takes a `default` for a
// field it does not recognise, so a misspelled key is not a compile error in the
// shell, not an error in Rust, and not a failed test -- it is a level that never
// appears. The request below is written by hand, in the shell's spelling, for
// exactly that reason.

{
  const levels = build({
    candles: candles(10),
    width: 900,
    height: 420,
    overlays: [
      { price: 105.0, label: "stop", role: "stop", band_to: null },
      { price: 108.0, label: "entry", role: "entry", band_to: 112.0, filled: true },
      { price: 112.0, label: "target", role: "target" },
    ],
  });

  check(
    "overlays sent as prices come back positioned",
    Array.isArray(levels.overlays) &&
      levels.overlays.length === 3 &&
      levels.overlays.every((o) => typeof o.y === "number"),
    JSON.stringify(levels.overlays)
  );
  check(
    "and the price is echoed, so the shell can label without re-deriving it",
    levels.overlays[0]?.price === 105.0,
    JSON.stringify(levels.overlays[0])
  );
  check(
    "and the role survives, because it is the shell's colour key",
    levels.overlays.map((o) => o.role).join(",") === "stop,entry,target",
    levels.overlays.map((o) => o.role).join(",")
  );
  check(
    "and the band's far edge is a position, not the price again",
    typeof levels.overlays[1]?.band_y === "number" && levels.overlays[1]?.filled === true,
    JSON.stringify(levels.overlays[1])
  );
  check(
    "and a level with no band reports none, rather than a zero-height band",
    levels.overlays[0]?.band_y === null,
    JSON.stringify(levels.overlays[0]?.band_y)
  );

  // A level that cannot be placed is refused, and the refusal is *loud*. Two
  // layers, deliberately:
  //
  //   * `null` is not a price, so serde rejects the whole request. That is the
  //     right call -- a request built by hand with a missing field is a bug in
  //     the caller, and building a scene from it would draw an answer that was
  //     never fully made.
  //   * a *finite but unplaceable* price (a huge number) is inside the type and
  //     is refused per-overlay with a note, so one bad level does not cost the
  //     chart.
  //
  // Asserting both, because "refused" and "refused with the chart intact" are
  // different promises and the shell behaves differently around each.
  let serdeRefused = "";
  try {
    build({
      candles: candles(10),
      width: 900,
      height: 420,
      overlays: [{ price: null, label: "stop", role: "stop" }],
    });
  } catch (e) {
    serdeRefused = String(e.message);
  }
  check(
    "a level whose price is null is refused rather than drawn at zero",
    serdeRefused.includes("expected f64"),
    serdeRefused || "(the request was accepted, which would draw a stop at the origin)"
  );

  // `1e308` is finite, so serde accepts it and the mapping places it far off the
  // plot. The canvas clips it, which is the correct rendering of "a price this
  // chart cannot show" -- and the candles are untouched.
  const offScale = build({
    candles: candles(10),
    width: 900,
    height: 420,
    overlays: [
      { price: 1e308, label: "absurd", role: "other" },
      { price: 108.0, label: "entry", role: "entry" },
    ],
  });
  check(
    "and a price too large to draw is placed off-plot without costing the chart",
    offScale.overlays.length === 2 && offScale.candles.length === 10,
    `overlays ${offScale.overlays.length}, candles ${offScale.candles.length}, note ${JSON.stringify(offScale.note ?? null)}`
  );
}

console.log(
  failures === 0
    ? `\nall checks passed (${path})`
    : `\n${failures} check(s) FAILED (${path})`
);
process.exit(failures === 0 ? 0 : 1);
