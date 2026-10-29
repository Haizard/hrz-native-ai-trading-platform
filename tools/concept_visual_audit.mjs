#!/usr/bin/env node
/*
 * The concept visual fidelity audit's driver.
 *
 * The spec it serves (`docs/superpowers/specs/2026-10-29-concept-visual-fidelity-audit-design.md`)
 * asks for a photograph of what our engine draws today, per concept, next to a cited
 * description of what the wider world draws. So this tool holds **no rendering rules of
 * its own**: the shape routing and the painting come from `concept_shapes.js`, the
 * family table from `concept_taxonomy.js`, and both are the bytes the page loads. A tool
 * that re-implemented either would photograph the tool instead of the chart, which is
 * the one way this audit could be wrong and still look tidy.
 *
 * Subcommands:
 *   taxonomy  -- the twelve-pass table against the live registry (no browser, no ledger)
 *   seed      -- write one ledger shard per family, every row at its seeded value
 *   render    -- one page per concept, built by the real wasm from real events
 *   check    -- the completeness gate over the shards
 *
 * Usage: node tools/concept_visual_audit.mjs <subcommand> [family] [--symbol S --timeframe T --limit N]
 */
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";

const require = createRequire(import.meta.url);
const Taxonomy = require("../frontend/app/concept_taxonomy.js");
const ConceptShapes = require("../frontend/app/concept_shapes.js");

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const gateway = process.env.HRZ_GATEWAY ?? "http://127.0.0.1:8080";
const ledgerDir = join(root, "reports/concept-visual-audit/ledger");

/// The spec's twelve-pass table, copied verbatim.
///
/// It lives here rather than in prose because a family count that moves without the
/// spec moving with it means the audit is disagreeing with itself -- and the passes are
/// scheduled by these numbers. `more` must be empty: a concept the panel cannot name is
/// a gap in the taxonomy, and the pass that notices it says so instead of inventing a
/// family for the run.
const EXPECTED = {
  smc: 32, sd: 4, pa: 11, session: 14, fib: 6, chart: 15, harmonics: 9,
  candles: 34, "volume-profile": 9, "order-flow": 1, trend: 12, momentum: 10,
  volatility: 4, volume: 5, statistical: 10, econ: 6, more: 0,
};

async function concepts() {
  const response = await fetch(`${gateway}/concepts`);
  if (!response.ok) throw new Error(`GET /concepts -> ${response.status}; is the gateway up on ${gateway}?`);
  return response.json();
}

async function taxonomy() {
  const entries = await concepts();
  const counts = new Map(Object.keys(EXPECTED).map((id) => [id, 0]));
  const unexpected = [];
  for (const entry of entries) {
    const family = Taxonomy.familyOf(entry.id);
    if (!counts.has(family)) unexpected.push(`${entry.id} -> ${family}`);
    else counts.set(family, counts.get(family) + 1);
  }
  const drift = Object.entries(EXPECTED).filter(([id, want]) => counts.get(id) !== want);
  console.log(`${entries.length} concepts across ${counts.size} families`);
  for (const [id, want] of Object.entries(EXPECTED)) {
    const got = counts.get(id);
    if (got !== want) console.log(`  ${id}: expected ${want}, the registry says ${got}`);
  }
  for (const line of unexpected) console.log(`  unmapped ${line}`);
  if (drift.length || unexpected.length) {
    console.error("FAIL the taxonomy and the audit's pass table disagree");
    process.exit(1);
  }
  console.log("ok   every concept lands in the family the audit will write it under");
}

const [, , subcommand, ...rest] = process.argv;

function flag(name, fallback) {
  const at = rest.indexOf(`--${name}`);
  return at >= 0 && rest[at + 1] !== undefined ? rest[at + 1] : fallback;
}

/// The real engine, loaded once.
///
/// `chart_preview.mjs` opened to show this exact thing: a scene is not a
/// screenshot, it is what `build_scene` returns for a request, and the audit's
/// photographs have to come from the same wasm the browser loads.
async function loadEngine() {
  const bytes = await readFile(join(root, "frontend/app/chart_engine.wasm"));
  const { instance } = await WebAssembly.instantiate(bytes, {});
  const wasm = instance.exports;
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  return function build(request) {
    const json = encoder.encode(JSON.stringify(request));
    const pointer = wasm.alloc(json.length);
    new Uint8Array(wasm.memory.buffer, pointer, json.length).set(json);
    const status = wasm.build_scene(pointer, json.length);
    wasm.dealloc(pointer, json.length);
    if (status !== 0) {
      const start = wasm.last_error_ptr();
      throw new Error(decoder.decode(new Uint8Array(wasm.memory.buffer, start, wasm.last_error_len()))
        || "the engine refused the request");
    }
    const start = wasm.scene_ptr();
    const length = wasm.scene_len();
    return JSON.parse(decoder.decode(new Uint8Array(wasm.memory.buffer, start, length).slice()));
  };
}

async function candlesFor(symbol, timeframe, limit) {
  const url = `${gateway}/candles?symbol=${encodeURIComponent(symbol)}`
    + `&timeframe=${encodeURIComponent(timeframe)}&limit=${limit}`;
  const response = await fetch(url);
  if (!response.ok) throw new Error(`GET /candles -> ${response.status} for ${symbol} ${timeframe}`);
  const body = await response.json();
  return body.candles || [];
}

async function eventsFor(symbol, timeframe, limit) {
  const url = `${gateway}/concept-events/scan?symbol=${encodeURIComponent(symbol)}`
    + `&timeframe=${encodeURIComponent(timeframe)}&limit=${limit}`;
  const response = await fetch(url);
  if (!response.ok) throw new Error(`GET /concept-events/scan -> ${response.status} for ${symbol} ${timeframe}`);
  const body = await response.json();
  return body.events || [];
}

/// The page a pass reads.
///
/// It carries no drawing rules either. What it does carry is `concept_shapes.js`
/// itself -- the bytes the browser loads, inlined so the file opens from disk and
/// survives being emailed -- and the same two-phase call `app.js` makes, `"under"`
/// before the candles and `"over"` after them. A photograph taken with a painter of
/// the tool's own would flatter the chart, which is the failure mode this whole
/// audit was specified to avoid.
function renderPage({ entry, family, shapeList, scene, events, build: buildInfo, shapesSource }) {
  const shapes = shapeList.length;
  const draws = scene.concept_shapes.length;
  // What the engine is expected to hand back for these shapes: a zone and a
  // segment each lower to a body plus a label, a candle mark to a label alone.
  // Fewer than that means a shape was refused, which is a third kind of empty and
  // worth naming in the page rather than leaving for someone to count pixels over.
  const expected = shapeList.reduce((n, s) => n + (s.type === "candle_mark" ? 1 : 2), 0);
  let note = "";
  if (events === 0) {
    note = `<p class="empty">No events. The scan window for <code>${escapeHtml(buildInfo.symbol)} ${escapeHtml(buildInfo.timeframe)}</code> ` +
      `produced nothing for this concept, so there is nothing here to judge. Either the detector never ` +
      `fires on this instrument, or the window is too short to hold one.</p>`;
  } else if (shapes === 0) {
    note = `<p class="empty"><strong>${events} event(s) routed to zero shapes.</strong> This is a vocabulary gap, ` +
      `on camera: the detector fires, and the shape router has no way to draw what it found.</p>`;
  } else if (draws < expected) {
    note = `<p class="empty">${shapes} shape(s) should have lowered to ${expected} positioned draw(s); the engine ` +
      `returned ${draws}. It refused some of them, and its note reads <code>${escapeHtml(scene.note || "(none)")}</code>.</p>`;
  }
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>${escapeHtml(entry.id)} — what our engine draws today</title>
<style>
  :root {
    --bg: #0d1117; --panel: #161b22; --line: #2a313c;
    --text: #e6edf3; --muted: #8b949e; --accent: #58a6ff; --warn: #d29922;
    --mono: ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace;
  }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: var(--text);
         font: 13px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; padding: 20px; }
  h1 { font-size: 15px; margin: 0 0 2px; font-weight: 600; }
  h1 code { color: var(--accent); }
  p.lede { color: var(--muted); margin: 0 0 14px; max-width: 92ch; }
  .wrap { position: relative; border: 1px solid var(--line); border-radius: 8px;
          background: #0b0e14; overflow: hidden; }
  canvas { display: block; width: 100%; }
  dl { display: flex; flex-wrap: wrap; gap: 6px 22px; margin: 14px 0 0; padding: 0; }
  div { display: flex; gap: 6px; }
  dt { color: var(--muted); font-family: var(--mono); font-size: 11px; }
  dd { margin: 0; font-family: var(--mono); font-size: 11px; }
  .empty { color: var(--warn); border: 1px solid var(--warn); border-radius: 6px;
           padding: 8px 12px; margin: 14px 0 0; max-width: 92ch; }
  code { font-family: var(--mono); color: var(--accent); }
  pre { background: var(--panel); border: 1px solid var(--line); border-radius: 8px;
        padding: 12px; overflow: auto; font-size: 11px; color: var(--muted); max-height: 320px; }
  summary { color: var(--muted); font-size: 12px; cursor: pointer; margin-top: 14px; }
  nav { margin-top: 16px; font-size: 12px; }
  nav a { color: var(--accent); }
</style>
</head>
<body>
<h1><code>${escapeHtml(entry.id)}</code> — ${escapeHtml(entry.name)}</h1>
<p class="lede">
  What our engine draws for this concept <em>today</em>, from real events through real
  wasm. The request carried <code>${shapes}</code> semantic shape(s); the engine
  positioned them and the page paints them with the app's own
  <code>ConceptShapes.paint</code>, <code>"under"</code> before the candles and
  <code>"over"</code> after them. Nothing here re-imagines the drawing.
</p>
${note}
<div class="wrap"><canvas id="c"></canvas></div>
<dl>
  <div><dt>family</dt><dd>${escapeHtml(family)}</dd></div>
  <div><dt>instrument</dt><dd>${escapeHtml(buildInfo.symbol)} ${escapeHtml(buildInfo.timeframe)}</dd></div>
  <div><dt>window</dt><dd>${buildInfo.candles} candle(s)</dd></div>
  <div><dt>events</dt><dd>${events}</dd></div>
  <div><dt>shapes out</dt><dd>${shapes}</dd></div>
  <div><dt>draws back</dt><dd>${draws} of ${expected}</dd></div>
  <div><dt>registry</dt><dd>pane=${entry.pane} overlay=${entry.overlay} ${escapeHtml((entry.required_channels || []).join(","))}</dd></div>
</dl>
<details>
  <summary>the request and the scene, as bytes</summary>
  <pre>${escapeHtml(JSON.stringify({ concept_shapes: shapeList }, null, 2))}</pre>
  <pre>${escapeHtml(JSON.stringify(scene.concept_shapes, null, 2))}</pre>
</details>
<nav>Concept <code>${escapeHtml(entry.id)}</code> of family <code>${escapeHtml(family)}</code>, rendered ${escapeHtml(buildInfo.at)} by <code>tools/concept_visual_audit.mjs render</code>.</nav>
<script>
${shapesSource}
</script>
<script>
// A closing tag sequence inside a JSON string would end this script early; only a
// label or a note can carry one, and neither is worth a broken page.
const scene = ${JSON.stringify(scene).replaceAll("</", "<\\/")};
// The chart's own dark palette. Not a second opinion about concepts -- a backdrop,
// so the shapes are read against candles the way they are on the page.
const COLORS = { up: "#2ebd85", down: "#f6465d", grid: "#161b26", text: "#8a93a6" };

const canvas = document.getElementById("c");
const ratio = window.devicePixelRatio || 1;
canvas.width = scene.width * ratio;
canvas.height = scene.height * ratio;
canvas.style.height = scene.height + "px";
const ctx = canvas.getContext("2d");
ctx.setTransform(ratio, 0, 0, ratio, 0, 0);

for (const tick of scene.ticks) {
  ctx.strokeStyle = COLORS.grid;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(scene.plot.x, tick.y + 0.5);
  ctx.lineTo(scene.plot.x + scene.plot.w, tick.y + 0.5);
  ctx.stroke();
}

// The order the app's draw() uses: zones and segments UNDER the candles, so a
// fill is a backdrop the price is read against rather than a curtain over it.
window.ConceptShapes.paint(ctx, scene, "under");

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

ctx.fillStyle = COLORS.text;
ctx.font = "10px ui-monospace, monospace";
for (const tick of scene.ticks) {
  ctx.fillText(tick.price.toFixed(2), scene.plot.x + scene.plot.w + 6, tick.y + 3);
}

// ...and the labels over them: a zone's name under a candle is a name nobody reads.
window.ConceptShapes.paint(ctx, scene, "over");
</script>
</body>
</html>
`;
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

async function render(family) {
  if (!family || family.startsWith("--")) {
    console.error("usage: node tools/concept_visual_audit.mjs render <family> [--symbol S --timeframe T --limit N]");
    process.exit(2);
  }
  if (!(family in EXPECTED)) {
    console.error(`FAIL "${family}" is not a family the audit knows: ${Object.keys(EXPECTED).join(", ")}`);
    process.exit(1);
  }
  const symbol = flag("symbol", "BTCUSDT");
  const timeframe = flag("timeframe", "15m");
  const limit = Number(flag("limit", "500"));

  const entries = await concepts();
  const familyEntries = entries.filter((entry) => Taxonomy.familyOf(entry.id) === family);
  if (!familyEntries.length) {
    console.error(`FAIL the registry has no concept in family "${family}"`);
    process.exit(1);
  }

  // One window, fetched once for the whole family: thirty-two pages built from
  // thirty-two different windows would compare paintings rather than renders.
  const candles = await candlesFor(symbol, timeframe, limit);
  if (!candles.length) {
    console.error(`FAIL ${symbol} ${timeframe} has no candles on ${gateway}; the photographs would be empty`);
    process.exit(1);
  }
  const events = await eventsFor(symbol, timeframe, limit);
  const build = await loadEngine();
  // The painter the page runs is the painter the browser runs -- the same bytes,
  // read from disk and inlined, not a copy this tool maintains.
  const shapesSource = await readFile(join(root, "frontend/app/concept_shapes.js"), "utf8");
  const outDir = join(root, "reports/concept-visual-audit", family);
  await mkdir(outDir, { recursive: true });
  const at = new Date().toISOString().slice(0, 16).replace("T", " ");
  const buildInfo = { symbol, timeframe, candles: candles.length, at };

  let photographed = 0;
  let silent = 0;
  let noEvents = 0;
  console.log(`${family}: ${familyEntries.length} concept(s) on ${symbol} ${timeframe}, ` +
    `${candles.length} candle(s), ${events.length} event(s)`);
  for (const entry of familyEntries) {
    // The router, called exactly the way the pane calls it -- one enabled concept,
    // this one's events, no shape invented for the photograph.
    const eventsForConcept = events.filter((event) => event.concept_id === entry.id);
    const shapeList = ConceptShapes.from(eventsForConcept, new Set([entry.id]));
    const scene = build({ candles, width: 1180, height: 520, mode: "candles", concept_shapes: shapeList });
    const draws = scene.concept_shapes || [];
    await writeFile(join(outDir, `${entry.id}.html`), renderPage({
      entry, family, shapeList, scene: { ...scene, concept_shapes: draws },
      events: eventsForConcept.length, build: buildInfo, shapesSource,
    }));
    if (!eventsForConcept.length) noEvents += 1;
    else if (!shapeList.length) silent += 1;
    else photographed += 1;
    console.log(`  ${entry.id.padEnd(34)} ${String(eventsForConcept.length).padStart(4)} event(s)  ` +
      `${String(shapeList.length).padStart(4)} shape(s)  ${String(draws.length).padStart(4)} draw(s)`);
  }
  console.log(`ok   ${outDir}`);
  console.log(`     ${photographed} drawn, ${silent} fired-but-routed-nothing, ${noEvents} silent on this window`);
  console.log(`     pages are photographs, not source -- gitignored. The ledger row and`);
  console.log(`     the family section carry what the picture showed; rerun this command`);
  console.log(`     to look again (a comparable window, not the identical one).`);
  if (silent) {
    console.log("     the fired-but-routed-nothing ones are the vocabulary gap; they are a finding, not a failure");
  }
}

/// The ledger's own vocabulary, copied from the spec's row table.
///
/// A pass is free to be wrong about a concept; it is not free to invent a category,
/// because the next pass -- and the state architecture that reads these rows -- counts
/// them. So an unknown verdict is a gate failure, not a style note.
const KINDS = ["event", "series", "interval", "readout"];
const DRAW_VERDICTS = ["matches", "differs_minor", "differs_major", "impossible_in_vocabulary"];
const FIRES_VERDICTS = ["same_bars", "different_rule", "no_external_rule", "continuous"];
const RULE_FORMS = ["predicate", "recursion"];
const SHAPE_STATUSES = ["wiring", "new_rendering"];
const SHAPE_SCALES = ["price", "own_pane", "time_only"];
const PREVIEW_VERDICTS = ["agrees", "differs", "not_rendered"];

// Fields with a rule of their own: the general emptiness scan stands down on them so a
// row reports one problem per field, not two, and a pass can work the list.
const OWNED_FIELDS = new Set([
  "reference", "kind", "draw.verdict", "fires.verdict", "rule_as_data.form",
  "rule_as_data.inputs", "target_shape.status", "target_shape.scale",
  "preview_agrees", "parameters",
]);

/// The names `DataChannels` actually serves, read from the Rust rather than copied.
///
/// A hardcoded list here is a second source of truth that drifts the first time the
/// broker gains a channel -- and rule 6's whole job is to keep a pass from naming a
/// feed the venue does not deliver.
async function channelNames() {
  const source = await readFile(join(root, "crates/market-data/src/broker.rs"), "utf8");
  const names = [...source.matchAll(/pub const ([A-Z0-9_]+): Self = Self\(\s*\d+ << \d+\s*\);/g)]
    .map((match) => match[1].toLowerCase());
  if (!names.length) {
    console.error("FAIL no DataChannels constants parsed out of crates/market-data/src/broker.rs");
    console.error("     the channel regex is stale -- fix it here rather than dropping rule 6");
    process.exit(1);
  }
  return new Set(names);
}

const isText = (value) => typeof value === "string" && value.trim().length > 0;

/// One row, every rule that fires on it.
///
/// Each problem names the concept's field and the rule, and says what would satisfy it:
/// a gate nobody can read their way out of is a gate that gets edited rather than fixed.
function rowProblems(row, ctx) {
  const problems = [];
  const add = (rule, field, why) =>
    problems.push({ rule, field, why, message: `rule ${rule} (${field}): ${why}` });

  // Rule 2 -- nothing left at its seeded value.
  const scan = (value, path) => {
    if (OWNED_FIELDS.has(path)) return;
    if (value === null || value === undefined) add(2, path, "left unset by the seed; a pass has to answer it");
    else if (typeof value === "string") { if (!isText(value)) add(2, path, "empty -- still the value the seed wrote"); }
    else if (Array.isArray(value)) value.forEach((item, i) => scan(item, `${path}[${i}]`));
    else if (typeof value === "object") {
      for (const [key, item] of Object.entries(value)) scan(item, path ? `${path}.${key}` : key);
    }
  };
  scan(row, "");

  const oneOf = (value, list, field, rule) => {
    if (!isText(value)) add(rule, field, "empty -- still the value the seed wrote");
    else if (!list.includes(value)) add(rule, field, `"${value}" is not in the ledger's vocabulary (${list.join(" | ")})`);
  };
  oneOf(row.kind, KINDS, "kind", 2);
  oneOf(row.draw && row.draw.verdict, DRAW_VERDICTS, "draw.verdict", 2);
  oneOf(row.fires && row.fires.verdict, FIRES_VERDICTS, "fires.verdict", 2);
  oneOf(row.rule_as_data && row.rule_as_data.form, RULE_FORMS, "rule_as_data.form", 2);
  oneOf(row.target_shape && row.target_shape.status, SHAPE_STATUSES, "target_shape.status", 2);
  oneOf(row.target_shape && row.target_shape.scale, SHAPE_SCALES, "target_shape.scale", 2);

  // Rule 3 -- a citation, or an explicit statement that there is no convention to cite.
  const ref = row.reference;
  if (!ref || typeof ref !== "object") {
    add(3, "reference", "a row with no reference is this audit's opinion wearing a citation");
  } else if (ref.no_convention === true) {
    if (!isText(ref.reason)) {
      add(3, "reference.reason", 'no external convention is a legal answer, but it has to say why -- and "none found" is not why');
    }
  } else {
    for (const field of ["source", "url", "quote", "checked"]) {
      if (!isText(ref[field])) {
        add(3, `reference.${field}`, "a reference is source, url, one sentence quoted from it, and the date it was read");
      }
    }
    if (isText(ref.checked) && !/^\d{4}-\d{2}-\d{2}$/.test(ref.checked)) {
      add(3, "reference.checked", `"${ref.checked}" is not a date -- the row has to age, and "today" does not`);
    }
  }

  // Rule 4 -- a series may fire continuously; an event that does is not an event.
  if (row.fires && row.fires.verdict === "continuous" && row.kind === "event") {
    add(4, "fires.verdict", 'an "event" that fires on every bar is a series wearing an event\'s row -- set kind to "series" or pick a real verdict');
  }

  // Rule 5 -- the registry already knows whether a series has a scale; do not disagree silently.
  if (row.kind === "series") {
    const reg = row.registry || {};
    const scale = row.target_shape && row.target_shape.scale;
    if (!reg.pane && !reg.overlay) {
      add(5, "registry", "the registry gives this concept no pane and no overlay, so calling it a series is reporting a defect -- record it in the section, do not route around it here");
    } else if (reg.pane && scale && scale !== "own_pane") {
      add(5, "target_shape.scale", `pane is true, so the shape belongs on its own scale, not "${scale}"`);
    } else if (!reg.pane && reg.overlay && scale === "own_pane") {
      add(5, "target_shape.scale", "overlay is true and pane is false, so the shape sits on the price frame -- own_pane contradicts the registry");
    }
  }

  // Rule 6 -- the rule names its inputs, and an input names a channel the venue serves.
  const inputs = row.rule_as_data && row.rule_as_data.inputs;
  const channelList = [...ctx.channels].sort().join(", ");
  if (!Array.isArray(inputs) || !inputs.length) {
    add(6, "rule_as_data.inputs", "a rule that does not name its inputs is prose, not data -- the combined-concept spec reads this field");
  } else {
    inputs.forEach((input, i) => {
      const at = `rule_as_data.inputs[${i}]`;
      if (!input || typeof input !== "object") { add(6, at, "each input is { name, channel }"); return; }
      if (!isText(input.name)) add(6, `${at}.name`, "an input has to be named");
      if (!isText(input.channel)) add(6, `${at}.channel`, `the channel is one DataChannels names (${channelList})`);
      // Either spelling the spec asks for: the registry's lowercase `names()` output and
      // the bit name it cites in prose ("TRADES_AGGRESSOR", not "the tape") are the same
      // channel, and a row should not fail for quoting the source instead of the JSON.
      else if (!ctx.channels.has(String(input.channel).toLowerCase())) {
        add(6, `${at}.channel`, `"${input.channel}" is not a channel DataChannels names (${channelList})`);
      }
    });
  }

  // Rule 7 -- the judgement is anchored to a photograph that exists.
  if (!isText(row.render)) {
    add(7, "render", "the row has to point at the picture it was judged from");
  } else if (!ctx.hasFile(row.render)) {
    add(7, "render", `${row.render} does not exist -- \`node tools/concept_visual_audit.mjs render ${row.family || "<family>"}\` builds it`);
  }

  // Rule 8 -- the card in the Indicators face is a second renderer for the same shape.
  oneOf(row.preview_agrees, PREVIEW_VERDICTS, "preview_agrees", 8);

  // Rule 9 -- the parameter gap is counted, not asserted in prose.
  const declared = (row.registry && row.registry.parameters) || [];
  const params = row.parameters;
  if (!params || !Array.isArray(params.honoured)) {
    add(9, "parameters.honoured", params ? "unset -- an empty list is the honest answer when the detector reads no parameter; an absent one is a row nobody judged" : "the row has no parameters block; re-seed it");
  } else {
    if (!Array.isArray(params.declared) || params.declared.join("|") !== declared.join("|")) {
      add(9, "parameters.declared", `the registry declares [${declared.join(", ")}]; the row must carry that list, not a shorter one`);
    }
    for (const name of params.honoured) {
      if (!declared.includes(name)) add(9, "parameters.honoured", `"${name}" is not a parameter the registry declares`);
    }
  }

  return problems;
}

async function readShard(family) {
  const path = join(ledgerDir, `${family}.json`);
  if (!existsSync(path)) return null;
  const parsed = JSON.parse(await readFile(path, "utf8"));
  return Array.isArray(parsed) ? parsed : parsed.rows;
}

/// The completeness gate: the ledger against the registry, and every row against the
/// rules above. `--family` scopes one pass; no flag sweeps the union.
async function check(family) {
  if (family && !(family in EXPECTED)) {
    console.error(`FAIL "${family}" is not a family the audit knows: ${Object.keys(EXPECTED).join(", ")}`);
    process.exit(1);
  }
  let entries;
  try {
    entries = await concepts();
  } catch (error) {
    console.error(`FAIL the gate counts rows against the live registry, and it cannot reach it: ${error.message}`);
    console.error(`     start the gateway on ${gateway}, or point the gate at it with HRZ_GATEWAY=`);
    process.exit(1);
  }
  const channels = await channelNames();
  const ctx = { channels, hasFile: (path) => existsSync(join(root, path)) };
  const byId = new Map(entries.map((entry) => [entry.id, entry]));
  const families = family ? [family] : Object.keys(EXPECTED).filter((id) => EXPECTED[id] > 0);

  // The kinds are declared once, in the vocabulary chapter, and the gate reads that
  // chapter: a fifth kind has to appear there before a row may use it, so the decision
  // is made in the open instead of typed into a field a reviewer skips.
  const vocabRel = "docs/superpowers/specs/concept-visual-audit/00-vocabulary.md";
  const vocabPath = join(root, vocabRel);
  if (!existsSync(vocabPath)) {
    console.error(`FAIL ${vocabRel} is missing -- the gate's kinds have no declaration to be checked against`);
    process.exit(1);
  }
  const vocab = await readFile(vocabPath, "utf8");
  for (const kind of KINDS) {
    if (!vocab.includes(`\`${kind}\``)) {
      console.error(`FAIL the vocabulary chapter no longer names the kind "${kind}" the gate accepts -- declare it there, or drop it from both in the same commit`);
      process.exit(1);
    }
  }

  const tally = new Map();
  let rows = 0;
  let incomplete = 0;
  let missingShards = 0;

  for (const id of families) {
    const shardRows = await readShard(id);
    if (!shardRows) {
      console.log(`FAIL ${id}: no shard at reports/concept-visual-audit/ledger/${id}.json -- run \`node tools/concept_visual_audit.mjs seed\``);
      missingShards += 1;
      continue;
    }
    const want = entries.filter((entry) => Taxonomy.familyOf(entry.id) === id).map((entry) => entry.id);
    const got = shardRows.map((row) => row.concept_id);
    // Rule 1 -- exactly one row per registry concept, in the shard familyOf(id) puts it in.
    for (const absent of want.filter((conceptId) => !got.includes(conceptId))) {
      console.log(`  rule 1: ${absent} has no row in ${id}.json -- the registry answers it`);
      tally.set(1, (tally.get(1) || 0) + 1);
      incomplete += 1;
    }
    for (const stray of got.filter((conceptId) => !want.includes(conceptId))) {
      const home = Taxonomy.familyOf(stray);
      console.log(`  rule 1: ${stray} has a row in ${id}.json but the taxonomy puts it in ${home}${byId.has(stray) ? "" : ", which the registry no longer answers"}`);
      tally.set(1, (tally.get(1) || 0) + 1);
      incomplete += 1;
    }

    let bad = 0;
    for (const row of shardRows) {
      rows += 1;
      const problems = rowProblems(row, ctx);
      if (row.family !== id) problems.push({ rule: 1, field: "family", why: `the row says ${row.family}, the shard is ${id}`, message: "" });
      const entry = byId.get(row.concept_id);
      if (entry && row.version !== entry.version) {
        problems.push({ rule: 1, field: "version", why: `the registry is at ${entry.version}, the row was judged at ${row.version} -- re-seed and re-judge`, message: "" });
      }
      if (!problems.length) continue;
      bad += 1;
      for (const problem of problems) tally.set(problem.rule, (tally.get(problem.rule) || 0) + 1);
      const grouped = new Map();
      for (const problem of problems) {
        if (!grouped.has(problem.rule)) grouped.set(problem.rule, []);
        grouped.get(problem.rule).push(problem);
      }
      console.log(`  ${row.concept_id}`);
      for (const [rule, list] of [...grouped.entries()].sort((a, b) => a[0] - b[0])) {
        const fields = list.slice(0, 5).map((problem) => problem.field).join(", ");
        console.log(`     rule ${rule}: ${fields}${list.length > 5 ? ` +${list.length - 5} more` : ""}`);
        console.log(`        ${list[0].why}`);
      }
    }
    console.log(`${id}: ${shardRows.length} row(s), ${bad} incomplete`);
    incomplete += bad;
  }

  const counts = [...tally.entries()].sort((a, b) => a[0] - b[0]).map(([rule, n]) => `${rule}:${n}`);
  if (incomplete || missingShards) {
    console.log(`\nFAIL ${incomplete} problem row(s) of ${rows} checked${missingShards ? `, ${missingShards} shard(s) missing` : ""}`);
    console.log(`     problems per rule: ${counts.join("  ")}`);
    console.log(`     the rules are the spec's ledger row table; a field that cannot be answered honestly is a finding to write in the section, not a field to delete`);
    process.exit(1);
  }
  console.log(`\nok   ${rows} row(s) complete across ${families.length} famil${families.length === 1 ? "y" : "ies"}`);
}

/// The row as the registry seeds it: everything the platform already knows, filled
/// from the live answer, and everything a pass must decide left at its empty value.
///
/// The empty fields are not a placeholder to be embarrassed about -- they are what
/// `check` fails on, which is how the ledger stays complete as the registry grows. A
/// concept registered tomorrow appears here as a row nobody has judged, so
/// completeness cannot rot.
function seedRow(entry) {
  const family = Taxonomy.familyOf(entry.id);
  return {
    concept_id: entry.id,
    family,
    version: entry.version,
    registry: {
      pane: entry.pane,
      overlay: entry.overlay,
      channels: entry.required_channels,
      parameters: (entry.parameters || []).map((p) => p.name),
    },
    kind: "",
    reference: { source: "", url: "", quote: "", checked: "" },
    draw: { today: "", canonical: "", verdict: "" },
    fires: { our_rule: "", their_rule: "", verdict: "" },
    rule_as_data: { form: "predicate", rule: "", inputs: [] },
    warmup: { bars: 0, before: "" },
    target_shape: { name: "", status: "wiring", scale: "price" },
    detector: "",
    parameters: {
      declared: (entry.parameters || []).map((p) => p.name),
      honoured: null,
    },
    render: `reports/concept-visual-audit/${family}/${entry.id}.html`,
    preview_agrees: "",
  };
}

async function seed() {
  const entries = await concepts();
  const want = Object.values(EXPECTED).reduce((n, c) => n + c, 0);
  if (entries.length !== want) {
    console.error(`FAIL the registry answers ${entries.length} concepts; the pass table sums to ${want}`);
    process.exit(1);
  }

  const shards = new Map();
  for (const entry of entries) {
    const family = Taxonomy.familyOf(entry.id);
    if (!shards.has(family)) shards.set(family, []);
    shards.get(family).push(seedRow(entry));
  }

  for (const [family, rows] of Object.entries(EXPECTED)) {
    const got = (shards.get(family) || []).length;
    if (got !== rows) console.log(`  ${family}: the pass table says ${rows}, the registry says ${got}`);
  }
  const unmapped = [...shards.keys()].filter((f) => !(f in EXPECTED));
  if (unmapped.length) {
    for (const family of unmapped) console.log(`  unmapped family ${family}: ${shards.get(family).length} concept(s)`);
    console.error("FAIL the taxonomy grew a family the audit has no pass for");
    process.exit(1);
  }
  if (shards.has("more")) {
    console.error(`FAIL ${shards.get("more").length} concept(s) the panel cannot name: ` +
      shards.get("more").map((r) => r.concept_id).join(", "));
    process.exit(1);
  }

  await mkdir(ledgerDir, { recursive: true });
  const force = rest.includes("--force");
  let written = 0;
  let kept = 0;
  for (const [family, rows] of shards) {
    const path = join(ledgerDir, `${family}.json`);
    const next = JSON.stringify({ family, rows }, null, 2) + "\n";
    let existing = null;
    try {
      existing = await readFile(path, "utf8");
    } catch {
      /* the common case: no shard yet */
    }
    if (existing !== null && existing !== next) {
      // A pass writes its verdicts into this file. Re-seeding over them would be the
      // quietest way to lose twelve passes of research, so it takes an explicit order.
      if (!force) {
        console.error(`FAIL ${family}.json holds work the seed did not write. ` +
          `Re-seed with --force only if you mean to throw those rows away.`);
        process.exit(1);
      }
      console.log(`  ${family}: re-seeded over a shard that had been written into (--force)`);
    }
    if (existing === next) {
      kept += 1;
      continue;
    }
    await writeFile(path, next);
    written += 1;
  }
  console.log(`${entries.length} rows across ${shards.size} shard(s): ${written} written, ${kept} already exact`);
  console.log("ok   every registry concept has a row, and every judgement in it is empty for `check` to fail on");
}

const here = fileURLToPath(import.meta.url);
// Task 5's fixture imports `seedRow` and `EXPECTED` from this file, and a top-level
// dispatch would answer it with "unknown subcommand" and `process.exit(2)`. Run the
// CLI only when this file is the one Node was pointed at.
const invokedDirectly = process.argv[1] ? resolve(process.argv[1]) === here : false;

if (invokedDirectly) {
  if (subcommand === "taxonomy") {
    await taxonomy();
  } else if (subcommand === "seed") {
    await seed();
  } else if (subcommand === "render") {
    await render(rest.find((arg) => !arg.startsWith("--")));
  } else if (subcommand === "check") {
    await check(rest.find((arg) => !arg.startsWith("--")));
  } else {
    console.error(`unknown subcommand "${subcommand ?? ""}" -- expected: taxonomy | seed | render | check`);
    process.exit(2);
  }
}

export {
  EXPECTED, Taxonomy, ConceptShapes, root, gateway, concepts,
  ledgerDir, seedRow, loadEngine, candlesFor, eventsFor, renderPage, escapeHtml,
  rowProblems, channelNames, readShard,
  KINDS, DRAW_VERDICTS, FIRES_VERDICTS, RULE_FORMS, SHAPE_STATUSES, SHAPE_SCALES, PREVIEW_VERDICTS,
};
