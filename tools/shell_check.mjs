// Exercise the shell the way a browser does.
//
// ## Why this exists
//
// `chart-engine` is covered on the host by `cargo test -p chart-engine`, and
// `wasm_abi_check.mjs` covers the ABI between it and JavaScript. Neither touches
// `app.js` -- and `app.js` is where four defects in the drawing tools lived, none
// of them visible from outside: the toolbar rendered, the buttons pressed, and
// nothing happened. A gesture cannot be unit-tested without a DOM, so this
// supplies one and then performs the gestures.
//
// The engine here is the **real** one: `fetch` serves the bytes of the checked-in
// artifact, so a scene built in this file is a scene the browser would get. What
// is faked is everything around it -- the network, the socket, and the 2D
// context, which records instead of rasterising.
//
// That recording is the point. The shell's job is to decide *what* to draw and
// *what to store*; this asserts on both, from outside, without reading the
// shell's own variables.
//
// ## Running it
//
// jsdom is not a dependency of the workspace -- nothing here is built by `cargo`
// -- so it is installed beside the managed Node runtime and pointed at with
// `NODE_PATH`, which is the only thing this file needs beyond Node itself:
//
//     cd "$WORKBUDDY_NODE_WORKSPACE" && npm install jsdom
//     NODE_PATH="$WORKBUDDY_NODE_WORKSPACE/node_modules" node tools/shell_check.mjs
//
// An optional argument overrides which wasm is loaded, which is how the same
// gestures can be run against a freshly built artifact before it is checked in:
//
//     ... node tools/shell_check.mjs target/engine/chart_engine.wasm
//
// Exits 0 when every check passes, 1 when one fails, and **2 when jsdom is
// missing** -- a skipped tool rather than a broken shell, so a machine without
// it does not read as a regression.

import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const wasmPath = process.argv[2] ?? join(root, "frontend/app/chart_engine.wasm");

let failures = 0;
const check = (name, ok, detail = "") => {
  console.log(`${ok ? "  ok  " : " FAIL "}  ${name}${detail ? ` -- ${detail}` : ""}`);
  if (!ok) failures += 1;
};

// --- jsdom ------------------------------------------------------------------
//
// Resolved through `require` rather than `import`, because `NODE_PATH` is
// honoured by CommonJS resolution and ignored by ESM. A missing jsdom is a
// *skipped* tool, not a failing one, so it exits 2 rather than 1 -- otherwise a
// machine without it would look like a broken shell.

const require = createRequire(import.meta.url);
let JSDOM;
let VirtualConsole;
try {
  ({ JSDOM, VirtualConsole } = require("jsdom"));
} catch {
  console.error(
    "jsdom is not installed, so the shell cannot be exercised.\n" +
      "  npm install jsdom   (into the managed Node workspace)\n" +
      "  then run this with NODE_PATH pointing at its node_modules.\n"
  );
  process.exit(2);
}

const html = await readFile(join(root, "frontend/app/index.html"), "utf8");
const shell = await readFile(join(root, "frontend/app/app.js"), "utf8");
const wasmBytes = await readFile(wasmPath);

// The page's own console, forwarded. Without this a shell that throws on load
// is a timeout with no explanation -- which is the least useful failure mode a
// test can have.
const virtualConsole = new VirtualConsole();
virtualConsole.on("jsdomError", (e) =>
  console.error("  [page error]", e.message, e.detail && e.detail.stack)
);
virtualConsole.on("error", (...args) => console.error("  [page console.error]", ...args));
virtualConsole.on("warn", (...args) => console.error("  [page warn]", ...args));

const dom = new JSDOM(html, {
  url: "http://localhost:8080/",
  runScripts: "dangerously",
  pretendToBeVisual: true,
  virtualConsole,
});
const { window } = dom;
const { document } = window;

// --- what the shell is allowed to believe about its surroundings -------------

// A 2D context that records. `getContext` is called once per frame by `draw()`,
// and the shell asks for `2d` and nothing else.
const painted = { ops: [], text: [] };
const context = new Proxy(
  {},
  {
    get(_target, prop) {
      if (prop === "canvas") return document.getElementById("chart");
      if (prop === "measureText") return () => ({ width: 10 });
      // Every drawing call lands here, so a check can ask "did it stroke
      // anything at all" without knowing which call it was.
      return (...args) => {
        painted.ops.push(String(prop));
        if (prop === "fillText") painted.text.push(String(args[0]));
        return undefined;
      };
    },
    set() {
      return true;
    },
  }
);

// jsdom lays nothing out, so the numbers the shell reads are all zero and the
// plot would be a degenerate rectangle. These are the only dimensions it needs.
const VIEW = { width: 900, height: 420 };
window.devicePixelRatio = 1;
const wrap = document.getElementById("chartWrap");
const canvas = document.getElementById("chart");
for (const [name, value] of [
  ["clientWidth", VIEW.width],
  ["clientHeight", VIEW.height],
]) {
  Object.defineProperty(wrap, name, { value, configurable: true });
}
Object.defineProperty(canvas, "getContext", {
  value: () => context,
  configurable: true,
});
Object.defineProperty(canvas, "getBoundingClientRect", {
  value: () => ({
    x: 0,
    y: 0,
    left: 0,
    top: 0,
    right: VIEW.width,
    bottom: VIEW.height,
    width: VIEW.width,
    height: VIEW.height,
  }),
  configurable: true,
});
// Pointer capture is not implemented in jsdom, and the shell calls it on every
// drag. Capturing is a browser nicety here -- the events are dispatched directly
// at the canvas either way -- so the stubs only have to not throw.
const captured = new Set();
canvas.setPointerCapture = (id) => captured.add(id);
canvas.releasePointerCapture = (id) => captured.delete(id);
canvas.hasPointerCapture = (id) => captured.has(id);

// The socket. `connectLive` and `connectBook` both open one and both tolerate
// it never saying anything, so a stub that stays silent is the honest double:
// nothing about this file depends on a frame arriving.
window.WebSocket = class {
  constructor() {
    this.readyState = 1;
    setTimeout(() => this.onopen && this.onopen(), 0);
  }
  send() {}
  close() {
    this.readyState = 3;
    if (this.onclose) this.onclose();
  }
};

// --- the API ----------------------------------------------------------------
//
// A small in-memory backend. It stores what it is given and echoes it back the
// way the real route does, because the shell takes its next state from the
// *response* rather than from what it sent -- which is the contract that makes
// `createDrawing` replace the local drawing with the stored one.

const backend = {
  drawings: [],
  nextId: 1,
  requests: [],
  failCreate: null, // a status code, to exercise the failure path
  calls: [],
};

const json = (status, body) => ({
  ok: status >= 200 && status < 300,
  status,
  statusText: status === 200 ? "OK" : "Error",
  async text() {
    return JSON.stringify(body);
  },
  async arrayBuffer() {
    return new ArrayBuffer(0);
  },
});

/// A base price that depends on the symbol.
///
/// So two panes on two instruments cannot be mistaken for one another in a
/// check: the same fixture on both would make "pane B asked for its own bars"
/// indistinguishable from "pane B drew pane A's bars twice".
function basePriceFor(symbol) {
  let hash = 0;
  for (const ch of symbol) hash = (hash * 31 + ch.charCodeAt(0)) % 1000;
  return 100 + hash;
}

function candlesFor(count, symbol = "BTCUSDT") {
  const base0 = basePriceFor(symbol);
  return Array.from({ length: count }, (_, i) => {
    const base = base0 + i * 0.5;
    const open = base;
    const close = base + (i % 2 === 0 ? 0.4 : -0.2);
    return {
      symbol,
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

/// The query string of a request, as an object.
const queryOf = (url) => new URLSearchParams(url.split("?")[1] ?? "");

window.fetch = async (path, options = {}) => {
  const url = String(path);
  const method = (options.method || "GET").toUpperCase();
  backend.calls.push({ method, url, body: options.body });

  if (url.endsWith(".wasm")) {
    return {
      ok: true,
      status: 200,
      statusText: "OK",
      async arrayBuffer() {
        return wasmBytes.buffer.slice(
          wasmBytes.byteOffset,
          wasmBytes.byteOffset + wasmBytes.byteLength
        );
      },
      async text() {
        return "";
      },
    };
  }

  if (url.startsWith("/candles")) {
    const query = queryOf(url);
    const symbol = (query.get("symbol") ?? "BTCUSDT").toUpperCase();
    const timeframe = query.get("timeframe") ?? "5m";
    // Echoed rather than hardcoded. The shell stamps the *response's* symbol and
    // timeframe onto the scene, so a stub that always answered "BTCUSDT/5m"
    // would make two panes on two instruments indistinguishable -- and telling
    // them apart is what the second pane has to be checked for.
    return json(200, { symbol, timeframe, candles: candlesFor(200, symbol) });
  }

  if (url.startsWith("/drawings")) {
    const symbol = (queryOf(url).get("symbol") ?? "BTCUSDT").toUpperCase();
    if (method === "GET") {
      // Scoped by symbol and echoing it, like the route. `loadDrawings` discards
      // a reply whose symbol is not the one it asked about, so a stub that
      // answered every request with every drawing would hide that rule.
      return json(200, {
        symbol,
        drawings: backend.drawings.filter((d) => d.symbol === symbol),
      });
    }
    if (method === "POST") {
      if (backend.failCreate) {
        const status = backend.failCreate;
        return json(status, { error: { code: "NOPE", message: "the database said no" } });
      }
      const body = JSON.parse(options.body);
      const stored = {
        id: `server-${backend.nextId++}`,
        symbol: (body.symbol ?? symbol).toUpperCase(),
        kind: body.kind,
        a1: body.a1,
        a2: body.a2 ?? null,
        label: body.label ?? null,
      };
      backend.drawings.push(stored);
      return json(201, stored);
    }
    if (method === "PUT") {
      const id = url.split("/").pop();
      const body = JSON.parse(options.body);
      const at = backend.drawings.findIndex((d) => d.id === id);
      if (at >= 0) backend.drawings[at] = { ...backend.drawings[at], ...body };
      return json(200, backend.drawings[at] ?? {});
    }
    if (method === "DELETE") {
      const id = url.split("/").pop();
      backend.drawings = backend.drawings.filter((d) => d.id !== id);
      return json(200, { id, deleted: true });
    }
  }

  // Anything else the shell asks for: an empty object is what an endpoint with
  // nothing to say returns, and the shell already tolerates it.
  return json(200, {});
};

// A signed-in session. `loadDrawings` returns early without a token -- it is how
// the shell avoids asking for a stranger's drawings -- so without this the whole
// read path is skipped and the checks below pass vacuously.
//
// The key is read out of the shell rather than written here, because writing it
// here was wrong and nothing noticed. The first version of this file set
// `"token"`; the shell uses `"atp.token"`. `loadDrawings` therefore returned at
// its guard on every run, no drawings were ever fetched, and every check still
// passed -- the precondition this comment describes was the one thing the file
// did not check. Reading it from the source means a rename in `app.js` fails
// here, loudly, instead of turning the read path off in silence.
const tokenKey = /\bTOKEN_KEY\s*=\s*"([^"]+)"/.exec(shell)?.[1];
if (!tokenKey) {
  console.error(
    "\nthe harness could not find TOKEN_KEY in frontend/app/app.js.\n" +
      "It signs in by writing that key, so it cannot run without it -- and a run\n" +
      "without a session silently skips every drawings read.\n"
  );
  process.exit(1);
}
window.localStorage.setItem(tokenKey, "a-token-for-the-harness");

// --- the real engine, with a recorder around it ------------------------------
//
// Wrapping `WebAssembly.instantiate` rather than the shell is deliberate: the
// shell then holds the proxy and every call it makes is observed, including the
// ones made by code this file never calls directly.

const engine = { requests: [], scenes: [] };
const RealWebAssembly = window.WebAssembly;
// `defineProperty` rather than assignment: the global is an accessor on the
// window in jsdom, and a plain assignment to it fails silently -- which would
// leave the recorder unwired and every check below passing for the wrong reason.
Object.defineProperty(window, "WebAssembly", {
  configurable: true,
  writable: true,
  value: {
    ...RealWebAssembly,
    async instantiate(bytes, imports) {
      const { instance } = await RealWebAssembly.instantiate(bytes, imports);
      const real = instance.exports;
      // A copy, not a Proxy. A wasm module's exports are non-configurable
      // read-only data properties, and a Proxy that returns anything other than
      // the real value for one of them throws -- so `build_scene` cannot be
      // wrapped in place. Copying is safe here because the exports are plain
      // functions that ignore their receiver, and `memory` is copied by value.
      const wrapped = {};
      for (const key of Object.keys(real)) wrapped[key] = real[key];

      const build = real.build_scene;
      wrapped.build_scene = (pointer, length) => {
        const request = new TextDecoder().decode(
          new Uint8Array(real.memory.buffer, pointer, length).slice()
        );
        engine.requests.push(JSON.parse(request));
        const status = build(pointer, length);
        if (status === 0) {
          const start = real.scene_ptr();
          const size = real.scene_len();
          engine.scenes.push(
            JSON.parse(
              new TextDecoder().decode(new Uint8Array(real.memory.buffer, start, size).slice())
            )
          );
        }
        return status;
      };

      return { instance: { exports: wrapped } };
    },
  },
});

// --- drive it ----------------------------------------------------------------

const waitFor = async (predicate, label, timeoutMs = 5000) => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  // Say what the page did instead, rather than only what it failed to do.
  console.error(
    `\ntimed out waiting for ${label}\n` +
      `  chartMsg: ${JSON.stringify(document.getElementById("chartMsg")?.textContent)}\n` +
      `  fetch calls: ${backend.calls.length} ${JSON.stringify(backend.calls.map((c) => c.url).slice(0, 5))}\n` +
      `  scenes built: ${engine.scenes.length}\n`
  );
  process.exit(1);
};

const script = document.createElement("script");
script.textContent = shell;
document.head.appendChild(script);

await waitFor(
  () => document.getElementById("chartMsg").textContent === "",
  "the engine to load"
);
await waitFor(() => engine.scenes.length > 0, "the first scene");

const note = () => document.getElementById("chartNote").textContent;
const lastScene = () => engine.scenes[engine.scenes.length - 1];
const lastRequest = () => engine.requests[engine.requests.length - 1];

/// One pointer event at a canvas coordinate.
///
/// A `MouseEvent` with a `pointerId` bolted on. jsdom does not implement
/// `PointerEvent`, and the shell reads only `clientX`, `clientY`, `button` and
/// `pointerId` -- so this is the same information the browser would deliver.
function pointer(type, x, y, id = 1) {
  const event = new window.MouseEvent(type, {
    bubbles: true,
    cancelable: true,
    clientX: x,
    clientY: y,
    button: 0,
  });
  Object.defineProperty(event, "pointerId", { value: id });
  canvas.dispatchEvent(event);
}

/// A click on a toolbar button, by the name the shell gave it.
function pickTool(name) {
  document.querySelector(`#tools button[data-tool="${name}"]`).click();
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 30));

// --- the page ---------------------------------------------------------------

console.log("\nthe page");

check(
  "the engine loaded and cleared its message",
  document.getElementById("chartMsg").textContent === "",
  document.getElementById("chartMsg").textContent
);
// The precondition for every drawing check below, asserted rather than assumed.
// `loadDrawings` returns at its guard without a session, so a harness that failed
// to sign in would report a clean run while never having fetched a drawing --
// which is exactly what this file did until the check was added.
check(
  "the shell signed in and asked for this symbol's drawings",
  backend.calls.some((c) => c.method === "GET" && c.url.startsWith("/drawings?")),
  backend.calls.map((c) => `${c.method} ${c.url}`).slice(0, 6).join(" | ") || "(no calls)"
);
check(
  "a scene came back with candles in it",
  lastScene().candles.length > 0,
  `${lastScene().candles.length} bars`
);
check(
  "the shell stroked something",
  painted.ops.length > 0,
  `${painted.ops.length} canvas calls`
);
check(
  "the drawing toolbar is on the page",
  document.querySelectorAll("#tools button[data-tool]").length === 5,
  `${document.querySelectorAll("#tools button[data-tool]").length} tools`
);
check(
  "every tool the engine knows has a button",
  ["cursor", "trendline", "hline", "rect", "fib"].every((name) =>
    document.querySelector(`#tools button[data-tool="${name}"]`)
  )
);
check(
  "the page has no media query yet, so this is the layout it ships",
  !/ @media /.test(html),
  "checked so the responsive work has to change this line"
);

// --- placing a drawing -------------------------------------------------------
//
// The first defect this file was written for: `startPlacing` set `placing` but
// never `drag`, so `onPointerUp` returned at its guard and no drawing was ever
// stored. Every check in this section fails against that code.

console.log("\nplacing a drawing");

pickTool("trendline");
check(
  "the tool button reports itself pressed",
  document.querySelector('#tools button[data-tool="trendline"]').getAttribute("aria-pressed") ===
    "true"
);

const created = backend.drawings.length;
pointer("pointerdown", 180, 280);
await settle();
// On the *move*, not on the press. At the press both anchors are the same point
// and the engine refuses a shape with no extent, so the drawing appears when it
// first has one -- which is also the first moment it could be seen. Asserting it
// at the press was the first version of this check, and it failed for that
// reason rather than for a defect.
pointer("pointermove", 520, 130);
await settle();
check(
  "the shape is on the chart while it is being drawn",
  lastScene().drawings.length === 1,
  `${lastScene().drawings.length} drawings`
);
pointer("pointerup", 520, 130);
await settle();
await settle();

check(
  "a drag stores exactly one drawing",
  backend.drawings.length === created + 1,
  `${backend.drawings.length - created} stored`
);
const stored = backend.drawings[backend.drawings.length - 1];
check(
  "and it is the kind that was selected",
  stored && stored.kind === "trendline",
  stored && stored.kind
);
check(
  "with absolute anchors, which is the only form storage keeps",
  stored && stored.a1.unit === "absolute" && stored.a2.unit === "absolute",
  stored && JSON.stringify([stored.a1, stored.a2])
);
check(
  "and two anchors that are not the same point",
  stored &&
    (stored.a1.time !== stored.a2.time || stored.a1.price !== stored.a2.price),
  stored && JSON.stringify([stored.a1, stored.a2])
);

// The fourth defect: the saved drawing replaced the local one with a row that
// had `selected: false`, so it offered no handles and could not be grabbed. The
// scene request is where that is visible -- `selected` is what makes the engine
// emit them.
check(
  "the drawing just made is selected, so the engine offers handles",
  lastRequest().drawings.some((d) => d.selected),
  JSON.stringify(lastRequest().drawings.map((d) => d.selected))
);
check(
  "and the scene carries a handle for it",
  lastScene().drawings[0].parts.some((part) => part.shape === "handle"),
  JSON.stringify(lastScene().drawings[0].parts.map((p) => p.shape))
);

// --- moving a drawing --------------------------------------------------------
//
// The third defect: a body drag sent one anchor to the pointer instead of
// translating the drawing, so a trendline dragged by its middle changed slope.
// The property is that both ends move by the *same* amount.

console.log("\nmoving a drawing");

// Back to the cursor first. `selectTool` keeps the tool active so several shapes
// can be drawn in a row, which means a press with the trendline still selected
// starts a *second* line rather than grabbing the first one. Leaving this out
// made the two checks below fail against a shell that was behaving correctly.
pickTool("cursor");

const segmentOf = (scene) =>
  scene.drawings[0].parts.find((part) => part.shape === "segment");
const before = segmentOf(lastScene());
const putBefore = backend.calls.filter((c) => c.method === "PUT").length;

// The middle of the line, which is what a body grab means.
const midX = (before.x1 + before.x2) / 2;
const midY = (before.y1 + before.y2) / 2;
pointer("pointerdown", midX, midY);
await settle();
pointer("pointermove", midX + 60, midY + 30);
await settle();
pointer("pointerup", midX + 60, midY + 30);
await settle();
await settle();

const puts = backend.calls.filter((c) => c.method === "PUT");
check(
  "a body drag stores the move",
  puts.length === putBefore + 1,
  `${puts.length - putBefore} PUTs`
);

if (puts.length > putBefore) {
  const body = JSON.parse(puts[puts.length - 1].body);
  const dt = body.a2.time - body.a1.time;
  const dp = body.a2.price - body.a1.price;
  const dtBefore = stored.a2.time - stored.a1.time;
  const dpBefore = stored.a2.price - stored.a1.price;
  // Equal *differences* is exactly "the shape moved rather than deformed", and
  // it is checkable without knowing which way the axes run.
  check(
    "and the shape kept its extent, so it translated rather than stretched",
    Math.abs(dt - dtBefore) < 1e-6 && Math.abs(dp - dpBefore) < 1e-6,
    `span ${dtBefore}/${dpBefore} -> ${dt}/${dp}`
  );
  check(
    "both ends moved",
    body.a1.time !== stored.a1.time || body.a1.price !== stored.a1.price,
    `${JSON.stringify(stored.a1)} -> ${JSON.stringify(body.a1)}`
  );
  check(
    "and the scene agrees with what was stored",
    Math.abs(lastScene().drawings[0].a1.time - body.a1.time) < 1e-6,
    `${lastScene().drawings[0].a1.time} vs ${body.a1.time}`
  );
}

// --- grabbing an anchor ------------------------------------------------------
//
// The other half of the same defect. A body drag moves both ends by one delta; a
// handle drag moves one end and leaves the other exactly where it was. Both are
// asserted because each is satisfied by the other's bug: a shell that always
// drags the body passes this section's sibling, and one that always drags a
// single anchor passes this one. Only a shell that picks between them by what
// was grabbed passes both.

console.log("\ngrabbing an anchor");

const putsNow = () => backend.calls.filter((c) => c.method === "PUT");
const lastBody = () => {
  const puts = putsNow();
  return puts.length ? JSON.parse(puts[puts.length - 1].body) : null;
};
// Where the shape sits after the body drag -- which is the state this gesture
// starts from. Falls back to what was originally stored, so the section still
// reports something useful if the body drag above is the thing that broke.
const afterBody = lastBody() ?? stored;

const anchorOne = lastScene().drawings[0].parts.find(
  (part) => part.shape === "handle" && part.anchor === 1
);
check(
  "the selected drawing still offers a handle to grab",
  Boolean(anchorOne),
  JSON.stringify(lastScene().drawings[0].parts.map((p) => p.shape))
);

if (anchorOne) {
  const putBeforeHandle = putsNow().length;
  pointer("pointerdown", anchorOne.x, anchorOne.y);
  await settle();
  pointer("pointermove", anchorOne.x + 50, anchorOne.y - 40);
  await settle();
  pointer("pointerup", anchorOne.x + 50, anchorOne.y - 40);
  await settle();
  await settle();

  check(
    "a handle drag stores the move",
    putsNow().length === putBeforeHandle + 1,
    `${putsNow().length - putBeforeHandle} PUTs`
  );

  if (putsNow().length > putBeforeHandle) {
    const dragged = lastBody();
    check(
      "and the end that was not grabbed did not move",
      dragged.a1.time === afterBody.a1.time && dragged.a1.price === afterBody.a1.price,
      `${JSON.stringify(afterBody.a1)} -> ${JSON.stringify(dragged.a1)}`
    );
    check(
      "while the one that was grabbed did, so the slope changed",
      dragged.a2.time !== afterBody.a2.time || dragged.a2.price !== afterBody.a2.price,
      `${JSON.stringify(afterBody.a2)} -> ${JSON.stringify(dragged.a2)}`
    );
  }
}

// --- a click with no drag ----------------------------------------------------
//
// The tool is a drag, so a click puts both anchors at one point. That is a shape
// with no extent: invisible, and stored anyway unless the engine refuses it.

console.log("\na click with no drag");

// Back to the trendline: a press on empty canvas with the cursor selects nothing
// and pans, which is a different refusal from the one this section is about.
pickTool("trendline");

const beforeClick = backend.drawings.length;
pointer("pointerdown", 300, 200);
await settle();
pointer("pointerup", 300, 200);
await settle();
await settle();

check(
  "nothing is stored",
  backend.drawings.length === beforeClick,
  `${backend.drawings.length - beforeClick} stored`
);
check(
  "and the engine's reason reaches the strip the user reads",
  note().includes("no extent"),
  note() || "(empty)"
);

// --- a save that fails -------------------------------------------------------
//
// The second defect: `createDrawing` wrote the note and then re-rendered, and
// `render()` owns the note strip -- so the reason was wiped by the frame that
// was meant to show it. A message nobody can read is a silent failure.

console.log("\na save that fails");

backend.failCreate = 500;
const beforeFailure = backend.drawings.length;
pointer("pointerdown", 200, 150);
await settle();
pointer("pointermove", 600, 320);
await settle();
pointer("pointerup", 600, 320);
await settle();
await settle();

check(
  "the drawing is not left on the chart looking saved",
  lastScene().drawings.length === beforeFailure,
  `${lastScene().drawings.length} drawings`
);
check(
  "and the reason survives the render that follows it",
  note().includes("not saved"),
  note() || "(empty)"
);

backend.failCreate = null;

// --- deleting ----------------------------------------------------------------
//
// There is no click-to-select gesture: a drawing becomes selected by being made
// or by being grabbed, which is why the failed save above leaves nothing
// selected -- it removed the drawing it had selected. So this draws one first,
// which is also exactly the flow the Delete key exists for: draw a shape, press
// Delete, it is gone.

console.log("\ndeleting");

const beforeDelete = backend.drawings.length;
pointer("pointerdown", 260, 160);
await settle();
pointer("pointermove", 470, 300);
await settle();
pointer("pointerup", 470, 300);
await settle();
await settle();
const afterDraw = backend.drawings.length;
check(
  "a freshly drawn shape is stored and selected",
  afterDraw === beforeDelete + 1 && lastRequest().drawings.some((d) => d.selected),
  `${afterDraw - beforeDelete} stored`
);

window.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Delete" }));
await settle();
await settle();

// Measured against `afterDraw`, not `beforeDelete`: the delete removes the shape
// this section just drew, so the count returns to where it started. Subtracting
// the other way round printed "0 removed" on a passing check, which is a message
// that cannot be true about the thing it claims to measure.
const afterDelete = backend.drawings.length;
check(
  "Delete removes the selected drawing from the store",
  afterDelete === beforeDelete,
  `${afterDraw - afterDelete} removed`
);
check(
  "and it is off the chart as well as out of the database",
  !lastScene().drawings.some((d) => d.selected),
  JSON.stringify(lastScene().drawings.map((d) => d.selected))
);

document.getElementById("clearDrawings").click();
await settle();
await settle();
check("Clear empties the store", backend.drawings.length === 0, `${backend.drawings.length} left`);

// --- a drawing belongs to a symbol -------------------------------------------
//
// The user's decision was "persisted per user + symbol", and this is the half of
// it the shell is responsible for: it must ask for the current symbol's drawings
// and it must not draw another instrument's shapes on this chart. It is also the
// property the second pane depends on -- two panes showing two instruments have
// to keep two sets of drawings apart.

console.log("\na drawing belongs to a symbol");

// A second instrument. The page ships one option, so a check that wants to
// change symbol has to add the one it is changing to -- which is also what the
// real page does when the symbol list grows.
const symbolSelect = document.getElementById("symbol");
const other = document.createElement("option");
other.value = "ETHUSDT";
symbolSelect.appendChild(other);

pickTool("trendline");
pointer("pointerdown", 220, 200);
await settle();
pointer("pointermove", 560, 260);
await settle();
pointer("pointerup", 560, 260);
await settle();
await settle();

check(
  "the shape is stored against the symbol that was on screen",
  backend.drawings.length === 1 && backend.drawings[0].symbol === "BTCUSDT",
  JSON.stringify(backend.drawings.map((d) => d.symbol))
);

symbolSelect.value = "ETHUSDT";
symbolSelect.dispatchEvent(new window.Event("change"));
await settle();
await settle();
await settle();

check(
  "changing symbol asks for that symbol's drawings",
  backend.calls.some((c) => c.url.startsWith("/drawings?") && c.url.includes("ETHUSDT")),
  backend.calls.filter((c) => c.url.startsWith("/drawings?")).slice(-1)[0]?.url ?? "(none)"
);
check(
  "and the other instrument's shape is not drawn on this chart",
  lastScene().drawings.length === 0,
  `${lastScene().drawings.length} drawings`
);
check(
  "and it was not deleted, only left behind",
  backend.drawings.length === 1,
  `${backend.drawings.length} stored`
);

symbolSelect.value = "BTCUSDT";
symbolSelect.dispatchEvent(new window.Event("change"));
await settle();
await settle();
await settle();

check(
  "and it comes back on the symbol it belongs to",
  lastScene().drawings.length === 1,
  `${lastScene().drawings.length} drawings`
);

console.log(
  failures === 0
    ? `\nall shell checks passed (${wasmPath})`
    : `\n${failures} shell check(s) FAILED`
);
process.exit(failures === 0 ? 0 : 1);
