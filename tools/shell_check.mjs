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
//
// Uncaught exceptions are also *counted*, and asserted on at the end. A listener
// that throws is invisible from outside: the page keeps working, the console
// fills up, and every check still passes. That is exactly how a stale resize
// handler referencing a variable that no longer existed survived a green run --
// this file printed the error and reported success in the same breath.
const pageErrors = [];
const virtualConsole = new VirtualConsole();
virtualConsole.on("jsdomError", (e) => {
  pageErrors.push(e.message);
  console.error("  [page error]", e.message, e.detail && e.detail.stack);
});
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

// A 2D context that records. `getContext` is called once per frame per pane by
// `draw()`, and the shell asks for `2d` and nothing else.
//
// One shared context for every canvas, which is fine because the shell never
// reads `ctx.canvas` -- the canvas it draws on is the one it asked. It did used
// to answer that property, and the branch was removed rather than left here: a
// stub that supports something nothing calls is a claim about the shell that
// nothing checks.
const painted = { ops: [], text: [] };
const context = new Proxy(
  {},
  {
    get(_target, prop) {
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

/// The panes, in the order they are on the page.
const paneNodes = () => [...document.querySelectorAll(".chartPane")];
/// One pane's canvas.
const canvasOf = (index = 0) => paneNodes()[index].querySelector(".chart");

/// The pane these checks are about unless they say otherwise.
///
/// Almost every check here is about one chart, and the one the page ships is the
/// first. The pane-independence section names its own panes explicitly.
const PANE = 0;
const paneNode = (index = PANE) => paneNodes()[index];
const note = (index = PANE) => paneNode(index).querySelector(".chartNote").textContent;
/// One pane's live-feed badge, and the state it is claiming.
///
/// The state is a `data-` attribute rather than only words, because the words
/// carry a live age and a check that matched them with a regex would pass on a
/// badge that had stopped updating. `dataset.state` is the claim; the text is the
/// evidence for it, and both are asserted.
const badge = (index = PANE) => paneNode(index).querySelector(".feedStatus");
const badgeState = (index = PANE) => badge(index).dataset.state;
const lastScene = () => engine.scenes[engine.scenes.length - 1];
const lastRequest = () => engine.requests[engine.requests.length - 1];
/// Which instrument a scene request is for.
///
/// The recorder sees every pane's requests, so a check that is about one chart
/// has to be able to say which one a request came from -- and the candles are
/// what carry that, because the request is otherwise only numbers.
const symbolOf = (request) => (request.candles[0] ? request.candles[0].symbol : "");

/// One pointer event at a canvas coordinate, on one pane's canvas.
///
/// A `MouseEvent` with a `pointerId` bolted on. jsdom does not implement
/// `PointerEvent`, and the shell reads only `clientX`, `clientY`, `button` and
/// `pointerId` -- so this is the same information the browser would deliver.
function pointer(type, x, y, index = PANE, id = 1) {
  const event = new window.MouseEvent(type, {
    bubbles: true,
    cancelable: true,
    clientX: x,
    clientY: y,
    button: 0,
  });
  Object.defineProperty(event, "pointerId", { value: id });
  canvasOf(index).dispatchEvent(event);
}

/// A click on one pane's toolbar button, by the name the shell gave it.
function pickTool(name, index = PANE) {
  paneNode(index).querySelector(`.tools button[data-tool="${name}"]`).click();
}

/// A `change` event the way a `<select>` fires one: bubbling.
///
/// `new Event("change")` defaults to `bubbles: false`, and the page listens for
/// `change` on the container so that a pane added later needs no wiring. A
/// non-bubbling event therefore never arrives -- which went unnoticed because the
/// check that needed it was passing for another reason entirely: the pane it was
/// about happened to be the active one already, so a *different* code path
/// reconnected the book. The event is now the one a browser sends, and the check
/// fails when the delegated listener is the only thing that could have worked.
const change = (node) => node.dispatchEvent(new window.Event("change", { bubbles: true }));

/// One wheel notch in the middle of one pane's canvas.
///
/// jsdom implements `WheelEvent`, so this is the same event a browser delivers --
/// `deltaY`, `deltaMode` and the shift modifier all read back. The pointer is at
/// the centre of the plot so the zoom has somewhere to anchor.
function wheel(index = PANE, deltaY = 120, shiftKey = false) {
  canvasOf(index).dispatchEvent(
    new window.WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      clientX: VIEW.width / 2,
      clientY: VIEW.height / 2,
      deltaY,
      deltaMode: 0,
      shiftKey,
    })
  );
}

/// One pane's control value, by the class the markup gave it.
const selectIn = (index, name) => paneNode(index).querySelector(`.${name}`).value;
/// How many scenes the engine has built for one series.
const framesFor = (series) => engine.frames.filter((f) => f.series === series).length;
/// Every series the engine has been asked for, once each.
const seriesSeen = () => [...new Set(engine.frames.map((f) => f.series))];
/// How many drawings one series' latest scene carries, or -1 if it never drew.
///
/// `-1` rather than a throw: a series that never drew is a *failing check*, and a
/// check that takes the process down with it reports nothing at all -- including
/// the failure it was written to find.
const drawingsIn = (series) => {
  const scene = sceneOf(series);
  return scene ? scene.drawings.length : -1;
};
/// Which panes carry the active mark, as a string like "0,2" or "none".
const activeIndexes = () =>
  paneNodes()
    .map((pane, index) => (pane.classList.contains("active") ? String(index) : ""))
    .filter((s) => s !== "")
    .join(",") || "none";

// Every stub below is on a *prototype*, not on an element, because a pane added
// at runtime is a new canvas and a new `.chartWrap` -- and a harness that had to
// be told when one appeared would be unable to check the thing this file exists
// to check. The page ships one pane in the markup and clones it, so nothing here
// can be attached to "the" canvas.
//
// A canvas reports a rectangle anchored at (0, 0). That is not a simplification
// to be embarrassed about: the shell turns a pointer position into a canvas
// position with `event.clientX - rect.left`, so a rect at the origin makes the
// coordinates this file dispatches *pane-local*, which is what a check wants to
// talk about anyway.
const RECT = {
  x: 0,
  y: 0,
  left: 0,
  top: 0,
  right: VIEW.width,
  bottom: VIEW.height,
  width: VIEW.width,
  height: VIEW.height,
};
Object.defineProperty(window.HTMLCanvasElement.prototype, "getContext", {
  value: () => context,
  configurable: true,
});
Object.defineProperty(window.HTMLCanvasElement.prototype, "getBoundingClientRect", {
  value: () => RECT,
  configurable: true,
});

// `clientWidth`/`clientHeight` are what `draw()` sizes the canvas from. A pane's
// wrapper is the only element on the page that has a size.
for (const [name, value] of [
  ["clientWidth", VIEW.width],
  ["clientHeight", VIEW.height],
]) {
  Object.defineProperty(window.Element.prototype, name, {
    get() {
      return this.classList && this.classList.contains("chartWrap") ? value : 0;
    },
    configurable: true,
  });
}

// Pointer capture is not implemented in jsdom, and the shell calls it on every
// drag. Capturing is a browser nicety here -- the events are dispatched directly
// at the canvas either way -- so the stubs only have to not throw.
const captured = new Set();
window.HTMLCanvasElement.prototype.setPointerCapture = (id) => captured.add(id);
window.HTMLCanvasElement.prototype.releasePointerCapture = (id) => captured.delete(id);
window.HTMLCanvasElement.prototype.hasPointerCapture = (id) => captured.has(id);

// The socket. `connectLive` and `connectBook` both open one and both tolerate
// it never saying anything, so a stub that stays silent is the honest double:
// nothing about this file depends on a frame arriving.
//
// It does record *what it was opened for*, because a channel is the only part of
// the page that says out loud which chart it belongs to: `/ws/market/BTCUSDT/5m`
// is a per-pane claim and `/ws/orderbook/BTCUSDT` is a page-level one, and the
// difference between those two is the whole of "which chart is the panel about".
const sockets = [];
window.WebSocket = class {
  constructor(url) {
    this.url = String(url);
    this.readyState = 1;
    this.closed = false;
    sockets.push(this);
    setTimeout(() => this.onopen && this.onopen(), 0);
  }
  send() {}
  close() {
    this.readyState = 3;
    this.closed = true;
    if (this.onclose) this.onclose();
  }
};
/// Every socket opened for one path prefix, oldest first.
const socketsFor = (prefix) => sockets.filter((s) => s.url.includes(prefix));
/// The sockets still open for one path prefix.
const openSocketsFor = (prefix) => socketsFor(prefix).filter((s) => !s.closed);

/// Hand a socket a frame, the way the server would.
///
/// The one thing a silent stub cannot exercise is the *content* of a frame, and
/// content is the whole point of the market channel's notice: a channel that says
/// nothing is indistinguishable from one with nothing to say, which is exactly
/// how a chart sat on seven-hour-old candles with no explanation anywhere.
const deliver = (socket, frame) => {
  if (socket.onmessage) socket.onmessage({ data: JSON.stringify(frame) });
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
  // Two instruments, because a chart per instrument is what a second pane is for
  // and one symbol cannot tell two panes apart.
  //
  // The order and the counts are the ones the live deployment reports, and both
  // matter. The order is alphabetical -- `15m, 1h, 1m, 4h, 5m` -- which is what
  // made the default timeframe regress to the *thinnest* series once the selects
  // started coming from `/symbols` instead of from the markup, and the counts are
  // what `nextFrame` reads to decide a timeframe cannot fill a chart. A fixture
  // tidied into `5m, 15m, 1h` would have hidden both.
  symbols: [
    {
      symbol: "BTCUSDT",
      coverage_note: "",
      timeframes: [
        { timeframe: "15m", candles: 3, expected: 3, missing: 0, first: 0, last: 0 },
        { timeframe: "1h", candles: 4441, expected: 4520, missing: 79, first: 0, last: 0 },
        { timeframe: "1m", candles: 2930, expected: 10565, missing: 7635, first: 0, last: 0 },
        { timeframe: "4h", candles: 1111, expected: 1130, missing: 19, first: 0, last: 0 },
        { timeframe: "5m", candles: 53182, expected: 54241, missing: 1059, first: 0, last: 0 },
      ],
    },
    {
      symbol: "ETHUSDT",
      coverage_note: "",
      timeframes: [
        { timeframe: "5m", candles: 900, expected: 900, missing: 0, first: 0, last: 0 },
      ],
    },
  ],
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

function candlesFor(count, symbol = "BTCUSDT", timeframe = "5m") {
  const base0 = basePriceFor(symbol);
  return Array.from({ length: count }, (_, i) => {
    const base = base0 + i * 0.5;
    const open = base;
    const close = base + (i % 2 === 0 ? 0.4 : -0.2);
    return {
      symbol,
      timeframe,
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

/// Milliseconds per bar, matching `BAR_MS` in `app.js`.
///
/// The coverage and footprint stubs need it: the shell asks for a window in
/// milliseconds and steps back one bar at a time, so a stub that answered in any
/// other unit would hand it an empty window.
const BAR_MS = { "1m": 60_000, "5m": 300_000, "1h": 3_600_000, "4h": 14_400_000 };

/// When the fixture's data ends, in milliseconds.
///
/// A real timestamp rather than a small number near zero. The footprint fixture
/// used to start at the epoch, which was tidy and wrong in a way that mattered as
/// soon as anything compared stored data against the live feed: the badge's whole
/// job is that comparison, and against an epoch-relative ladder every reading
/// would have been "thirteen thousand hours behind" -- a fixture that could not
/// tell a working chart from a broken one.
const FIXTURE_NOW_MS = 1_789_679_400_000;

/// A window of trade-level ladders, in the shape `/footprint` returns.
///
/// Twenty levels on a half-point step, which is what a real window looks like
/// after the route has sized the bucket. The width matters as much as the count:
/// at twenty columns across this viewport a cell is about the 54px the shell
/// sizes for, so the engine has to bring its font down to what the cell can hold.
/// A tidier fixture -- three levels, four columns -- would have made the ladder
/// legible for free and hidden the whole question.
function footprintColumns(count, bar = BAR_MS["5m"]) {
  return Array.from({ length: count }, (_, i) => {
    const base = 100 + i * 0.5;
    const cells = Array.from({ length: 12 }, (_, k) => {
      const price = base + k * 0.5;
      const bid = 0.4 + ((i + k) % 5) * 0.3;
      const ask = 0.5 + ((i * 2 + k) % 6) * 0.35;
      // A diagonal imbalance on every fourth level, alternating side, so a check
      // can tell an outlined cell from a filled one.
      const imbalance =
        (i + k) % 4 === 0
          ? { side: k % 2 === 0 ? "buy" : "sell", ratio: 2.5, stacked: 2 }
          : null;
      return { price, bid, ask, delta: ask - bid, imbalance };
    });
    const bid_volume = cells.reduce((sum, c) => sum + c.bid, 0);
    const ask_volume = cells.reduce((sum, c) => sum + c.ask, 0);
    const poc = cells.reduce(
      (best, c) => (c.bid + c.ask > best.bid + best.ask ? c : best),
      cells[0]
    );
    return {
      // The newest ladder ends one bar before the fixture's clock, which is what
      // a stored series looks like next to a feed that has just closed a bar.
      open_time: (FIXTURE_NOW_MS - (count - i) * bar) * 1e6,
      open: base,
      high: base + 6,
      low: base - 1,
      close: base + 3,
      volume: bid_volume + ask_volume,
      bid_volume,
      ask_volume,
      delta: ask_volume - bid_volume,
      poc: poc.price,
      cells,
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

  if (url.startsWith("/symbols")) {
    // The list the page builds every pane's selects from. An instrument the page
    // cannot chart, or a timeframe it has no bars for, would be a selector option
    // that draws nothing -- which is worse than no option, because the user
    // concludes the chart is broken rather than the setting.
    return json(200, backend.symbols);
  }

  if (url.startsWith("/candles")) {
    const query = queryOf(url);
    const symbol = (query.get("symbol") ?? "BTCUSDT").toUpperCase();
    const timeframe = query.get("timeframe") ?? "5m";
    // Echoed rather than hardcoded. The shell stamps the *response's* symbol and
    // timeframe onto the scene, so a stub that always answered "BTCUSDT/5m"
    // would make two panes on two instruments indistinguishable -- and telling
    // them apart is what the second pane has to be checked for.
    return json(200, { symbol, timeframe, candles: candlesFor(200, symbol, timeframe) });
  }

  if (url.startsWith("/footprint/coverage")) {
    // Milliseconds, because that is the unit the shell's `BAR_MS` is in and it
    // does `to - columns * bar` on this number. A window in nanoseconds would put
    // `from` in the future and the request would come out empty.
    return json(200, {
      symbol: (queryOf(url).get("symbol") ?? "BTCUSDT").toUpperCase(),
      from: FIXTURE_NOW_MS - 400 * BAR_MS["5m"],
      to: FIXTURE_NOW_MS,
    });
  }

  if (url.startsWith("/footprint?")) {
    const query = queryOf(url);
    const timeframe = query.get("timeframe") ?? "5m";
    const bar = BAR_MS[timeframe] ?? BAR_MS["5m"];
    // The *requested* window, not a number invented here. The shell sizes its
    // column count from the viewport and then asks for that many bars, so a stub
    // that always answered with twenty would hand the engine twenty ladders in a
    // fourteen-column plot -- narrower cells than the shell sized for, and the
    // engine would correctly say the numbers no longer fit. A fixture that
    // ignores the request tests a different program.
    const span = Number(query.get("to")) - Number(query.get("from"));
    const count = Math.max(1, Math.round(span / bar));
    return json(200, {
      symbol: (query.get("symbol") ?? "BTCUSDT").toUpperCase(),
      timeframe,
      trades: 1_234,
      bucket_size: 0.5,
      // The full ladders, in `candles`. That is the route's own shape and the
      // shell depends on both halves of it: it maps this array down to OHLC for
      // the axis *and* hands the same array back as the request's `footprint`
      // field. A fixture that returned a tidied candle list here produced
      // "missing field `delta`" from the engine.
      candles: footprintColumns(count, bar),
    });
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

const engine = { requests: [], scenes: [], frames: [] };

/// Which series a scene request is for, as `SYMBOL/timeframe`.
///
/// The recorder sees every pane's requests, and `lastScene()` is only ever one of
/// them. The candles are what carry the identity -- the request is otherwise only
/// numbers -- and they carry both halves, because two panes on one instrument at
/// two timeframes is the case this file most needs to tell apart.
const seriesOf = (request) => {
  const candle = request.candles[0];
  return candle ? `${candle.symbol}/${candle.timeframe}` : "?";
};
/// The last scene built for one series.
const sceneOf = (series) => {
  for (let i = engine.frames.length - 1; i >= 0; i -= 1) {
    if (engine.frames[i].series === series) return engine.frames[i].scene;
  }
  return null;
};

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
        const decoded = new TextDecoder().decode(
          new Uint8Array(real.memory.buffer, pointer, length).slice()
        );
        const request = JSON.parse(decoded);
        engine.requests.push(request);
        const status = build(pointer, length);
        if (status === 0) {
          const start = real.scene_ptr();
          const size = real.scene_len();
          const scene = JSON.parse(
            new TextDecoder().decode(new Uint8Array(real.memory.buffer, start, size).slice())
          );
          engine.scenes.push(scene);
          // The same scene, filed under the series that produced it, so a check
          // can ask what one pane's chart looks like *now* without depending on
          // that pane having been the last to draw.
          engine.frames.push({ series: seriesOf(request), scene });
        }
        return status;
      };

      return { instance: { exports: wrapped } };
    },
  },
});

// --- drive it ----------------------------------------------------------------
//
// The helpers below are defined before the script is injected, because `waitFor`
// uses them: the first thing this file does is wait for the page's own load to
// finish, and a helper declared after that point would be in its temporal dead
// zone at exactly the moment it is needed.

const waitFor = async (predicate, label, timeoutMs = 5000) => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  // Say what the page did instead, rather than only what it failed to do.
  console.error(
    `\ntimed out waiting for ${label}\n` +
      `  chartMsg: ${JSON.stringify(paneNode()?.querySelector(".chartMsg").textContent)}\n` +
      `  fetch calls: ${backend.calls.length} ${JSON.stringify(backend.calls.map((c) => c.url).slice(0, 5))}\n` +
      `  scenes built: ${engine.scenes.length}\n`
  );
  process.exit(1);
};

// --- the clock the page reads ------------------------------------------------
//
// The live badge's whole claim is about *age*, and the age it goes quiet at is a
// bar and a half -- 120 seconds on a 1m chart. Waiting that out is not a test, it
// is a delay, so the checks move the page's clock rather than sleeping through it.
//
// Only the page's. This replaces `window.Date.now`, so the deadlines in `waitFor`
// above keep using the real clock -- which matters, because a check that moved
// time forward and also moved the timeout would prove nothing about either.

const realPageNow = window.Date.now.bind(window.Date);
const pageClock = { offset: 0 };
window.Date.now = () => realPageNow() + pageClock.offset;

const script = document.createElement("script");
script.textContent = shell;
document.head.appendChild(script);

await waitFor(
  () => paneNode().querySelector(".chartMsg").textContent === "",
  "the engine to load"
);
await waitFor(() => engine.scenes.length > 0, "the first scene");


const settle = () => new Promise((resolve) => setTimeout(resolve, 30));

// --- the page ---------------------------------------------------------------

console.log("\nthe page");

check(
  "the engine loaded and cleared its message",
  paneNode().querySelector(".chartMsg").textContent === "",
  paneNode().querySelector(".chartMsg").textContent
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
  paneNode().querySelectorAll(".tools button[data-tool]").length === 5,
  `${paneNode().querySelectorAll(".tools button[data-tool]").length} tools`
);
check(
  "every tool the engine knows has a button",
  ["cursor", "trendline", "hline", "rect", "fib"].every((name) =>
    paneNode().querySelector(`.tools button[data-tool="${name}"]`)
  )
);

// The series a chart opens on now comes from `GET /symbols`, and the server lists
// its timeframes alphabetically. Taking the first option therefore opened the
// chart on the *thinnest* series on the deployment -- three bars -- which is the
// "a thin chart is telling the truth" case the bar count in the label was added
// for, arriving as the default instead of as a warning. The markup it replaced
// had `5m selected`, so this was a regression, and the fixture had been tidied
// into `5m, 15m, 1h` so the harness could not see it.
check(
  "the chart opens on the series with the most bars, not the first one listed",
  selectIn(PANE, "timeframe") === "5m",
  `opened on ${selectIn(PANE, "timeframe")}`
);
check(
  "and the timeframe options read as a ladder rather than in the server's order",
  [...paneNode().querySelector(".timeframe").options].map((o) => o.value).join(",") ===
    "1m,5m,15m,1h,4h",
  [...paneNode().querySelector(".timeframe").options].map((o) => o.value).join(",")
);

// --- the layout at a narrow width --------------------------------------------
//
// jsdom lays nothing out, so these read the stylesheet rather than measure the
// page -- the same trade `packaging.rs` makes for the Docker image, and for the
// same reason: the alternative is asserting nothing about it at all.
//
// They are not "a media query exists". A media query on its own changes nothing;
// what stops the chart being squeezed to nothing is that `main` stacks *and* the
// chart is given a height of its own, because a column that shares one height
// between a chart and a panel hands the chart whatever the panel leaves.

console.log("\nthe layout at a narrow width");

const styleText = html.slice(html.indexOf("<style>"), html.indexOf("</style>"));
const mediaBlockAt = (width) => {
  const at = styleText.indexOf(`@media (max-width: ${width}px)`);
  if (at < 0) return "";
  const next = styleText.indexOf("@media", at + 1);
  return styleText.slice(at, next < 0 ? styleText.length : next);
};
/// The declarations of one rule inside a block, or "" if it is not there.
///
/// The selector is escaped before it goes into a pattern, because selectors are
/// full of `.` and `#` and an unescaped `.` matches any character -- so a check
/// for `.chartWrap` would also pass on `#chartWrap`, which is the rule it was
/// renamed away from.
const ruleIn = (block, selector) => {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const found = new RegExp(`(^|[\\s,])${escaped}\\s*\\{([^}]*)\\}`).exec(block);
  return found ? found[2] : "";
};

const panelBlock = mediaBlockAt(1100);
const narrowBlock = mediaBlockAt(900);

check(
  "there is a breakpoint that narrows the panel",
  /width:\s*300px/.test(ruleIn(panelBlock, "aside")),
  ruleIn(panelBlock, "aside").trim() || "(no aside rule)"
);
check(
  "and one that puts the panel below the chart",
  /flex-direction:\s*column/.test(ruleIn(narrowBlock, "main")),
  ruleIn(narrowBlock, "main").trim() || "(no main rule)"
);
check(
  "where the chart is given a height of its own, not a share of the column",
  /height:\s*60vh/.test(ruleIn(narrowBlock, ".chartWrap")) &&
    /min-height:/.test(ruleIn(narrowBlock, ".chartWrap")),
  ruleIn(narrowBlock, ".chartWrap").trim() || "(no .chartWrap rule)"
);
check(
  "and the page scrolls rather than holding one viewport",
  /height:\s*auto/.test(ruleIn(narrowBlock, "html, body")),
  ruleIn(narrowBlock, "html, body").trim() || "(no html, body rule)"
);
check(
  "and the panes stack, so two charts are not two 450px charts",
  /flex-direction:\s*column/.test(ruleIn(narrowBlock, "#charts")) &&
    /flex:\s*0 0 auto/.test(ruleIn(narrowBlock, ".chartPane")),
  ruleIn(narrowBlock, "#charts").trim() || "(no #charts rule)"
);
check(
  "and the panel is full width with its border moved to the seam",
  /width:\s*auto/.test(ruleIn(narrowBlock, "aside")) &&
    /border-left:\s*0/.test(ruleIn(narrowBlock, "aside")) &&
    /border-top:/.test(ruleIn(narrowBlock, "aside")),
  ruleIn(narrowBlock, "aside").trim() || "(no aside rule)"
);
// Order is load-bearing, not cosmetic. Both blocks match at 800px, and both set
// `aside { width }`; if the stacking one came first the narrowing one would win
// and the panel would be a 300px column under a full-width chart.
check(
  "and the stacking breakpoint comes second, so it wins at the widths that match both",
  styleText.indexOf("@media (max-width: 900px)") >
    styleText.indexOf("@media (max-width: 1100px)") &&
    styleText.indexOf("@media (max-width: 900px)") > 0,
  `${styleText.indexOf("@media (max-width: 900px)")} > ${styleText.indexOf("@media (max-width: 1100px)")}`
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
  paneNode().querySelector('.tools button[data-tool="trendline"]').getAttribute("aria-pressed") ===
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

// --- deleting one, and clearing all ------------------------------------------
//
// The report that produced this section: "when I click the clear button it
// removes all the attached tools on the chart instead of the one I have
// selected". There were two jobs and one button, labelled `Clear`, doing the
// destructive one. Now there are two controls, each named for what it does, and
// the destructive one asks first.

console.log("\ndeleting one, and clearing all");

const clearButton = () => paneNode().querySelector(".clearDrawings");
const deleteButton = () => paneNode().querySelector(".deleteDrawing");

// The section above leaves one shape behind -- the Delete key removed the shape
// *it* drew, so the count returned to where that section started, and that is not
// zero. Every count below is relative to this, so "one of two left" means what it
// says rather than depending on what ran before.
const alreadyStored = backend.drawings.length;

// Nothing is selected here: the Delete key above removed the shape that was.
// The control has to be *disabled* rather than drawn and inert, because a button
// that is offered and then does nothing is worse than one that is not there.
check(
  "with nothing selected the delete control is disabled, not silently inert",
  deleteButton().disabled === true,
  `disabled=${deleteButton().disabled}`
);

// Two shapes, so "delete the selected one" and "delete everything" give
// different answers and the check can tell them apart.
pickTool("trendline");
pointer("pointerdown", 240, 180);
await settle();
pointer("pointermove", 520, 240);
await settle();
pointer("pointerup", 520, 240);
await settle();
await settle();
pointer("pointerdown", 300, 280);
await settle();
pointer("pointermove", 620, 330);
await settle();
pointer("pointerup", 620, 330);
await settle();
await settle();

const twoStored = backend.drawings.length - alreadyStored;
const twoSelected = lastRequest().drawings.filter((d) => d.selected).length;
check(
  "two shapes on the chart and exactly one of them selected",
  twoStored === 2 && twoSelected === 1,
  `${twoStored} drawn, ${twoSelected} selected`
);
check(
  "and the delete control is offered now that there is something to delete",
  deleteButton().disabled === false,
  `disabled=${deleteButton().disabled}`
);

deleteButton().click();
await settle();
await settle();
await settle();

check(
  "the delete control removes the selection and leaves the rest",
  backend.drawings.length === alreadyStored + 1,
  `${backend.drawings.length - alreadyStored} left of ${twoStored}`
);
check(
  "and the shape it kept is off the chart too, so nothing was left selected",
  lastScene().drawings.length === alreadyStored + 1 &&
    !lastScene().drawings.some((d) => d.selected),
  `${lastScene().drawings.length} drawn`
);

// `Clear all` is the destructive one, so it takes two clicks. The first is the
// question; the second is the answer.
clearButton().click();
await settle();
check(
  "the first click on Clear all only asks",
  backend.drawings.length === alreadyStored + 1 && clearButton().textContent.trim() === "Sure?",
  `${backend.drawings.length - alreadyStored} left, label "${clearButton().textContent.trim()}"`
);

clearButton().click();
await settle();
await settle();
await settle();

check(
  "and the second one clears everything",
  backend.drawings.length === 0,
  `${backend.drawings.length} left`
);
check(
  "and the label goes back to naming what the button does",
  clearButton().textContent.trim() === "Clear all",
  `"${clearButton().textContent.trim()}"`
);

// --- a market channel that cannot carry anything says so ----------------------
//
// The other half of the same report: seven hours of the same candles on every
// timeframe, with nothing anywhere saying why. The cause was `MARKET_FEED` not
// being set, and the only evidence was a line in a startup log. The gateway now
// sends a notice and closes, the way the order-book channel already did -- so the
// shell has to *read* it, and keep it: the socket closes immediately afterwards
// and a render would otherwise wipe the strip.

console.log("\na market channel with no feed");

const market = openSocketsFor("/ws/market/").slice(-1)[0];
check(
  "a market channel is open to deliver the notice on",
  Boolean(market),
  openSocketsFor("/ws/market/").map((s) => s.url.replace(/^.*\/ws/, "/ws")).join(", ") || "(none)"
);

if (market) {
  deliver(market, {
    type: "notice",
    message: "no market feed is configured (MARKET_FEED is not `binance`)",
  });
  await settle();
  check(
    "a notice on the market channel reaches the strip under the chart",
    note().includes("MARKET_FEED"),
    note() || "(empty)"
  );

  // The server closes the channel straight after the notice, and the shell must
  // not treat that as "forget what it said".
  market.close();
  await settle();
  check(
    "and closing the channel does not wipe the reason it closed",
    note().includes("MARKET_FEED"),
    note() || "(empty)"
  );

  // A render owns that strip, so the notice has to outrank the engine's own note
  // rather than being written into it once. A wheel is the cheapest render there
  // is, and it is also what the user was doing while the chart looked frozen.
  wheel(PANE, 120);
  await settle();
  await settle();
  check(
    "and it survives the render a pan would have triggered",
    note().includes("MARKET_FEED"),
    note() || "(empty)"
  );
}

// --- the live badge -----------------------------------------------------------
//
// "I am not sure [the data is real time] and I cannot prove it." That is the
// requirement exactly: a badge that says LIVE is a claim, and the page gave the
// user no way to check one. This one carries the age of the last frame that
// arrived, so it is a reading -- it counts up while nothing comes and resets when
// a bar closes.
//
// The check that matters is the *third* one. A badge that only ever says live is
// the defect rather than the feature, so the feed goes quiet and the badge has to
// stop claiming otherwise.

console.log("\nthe live badge");

// The section above closed its channel after the server said why. That reading
// has to survive the close: "offline" beside a reason already given would be a
// second answer to a question the server answered once.
check(
  "a channel the server refused to feed says so, rather than saying offline",
  badgeState() === "nofeed",
  `${badgeState()}: ${badge().textContent}`
);

// A fresh channel. `Reload` reopens it, which is also what a user does after
// watching the badge go quiet.
paneNode().querySelector(".load").click();
await settle();
await settle();
await settle();

const liveSocket = openSocketsFor("/ws/market/").slice(-1)[0];
check(
  "reloading opens a new market channel to read",
  Boolean(liveSocket) && !liveSocket.closed,
  openSocketsFor("/ws/market/")
    .map((s) => s.url.replace(/^.*\/ws/, "/ws"))
    .join(", ") || "(none)"
);

if (liveSocket) {
  check(
    "and an open channel with no bar yet does not claim to be live",
    badgeState() === "idle",
    `${badgeState()}: ${badge().textContent}`
  );

  // A bar, in the shape the server sends. `open_time` is nanoseconds, as
  // everywhere else on the wire, and it is the fixture's own clock so a badge
  // that compares stored data against the feed has something true to compare.
  const frame = (minutes) => ({
    type: "data",
    payload: {
      symbol: "BTCUSDT",
      timeframe: "5m",
      open_time: (FIXTURE_NOW_MS + minutes * BAR_MS["5m"]) * 1e6,
      open: 76_500,
      high: 76_520,
      low: 76_490,
      close: 76_510,
      volume: 12.5,
      buy_volume: 7,
      sell_volume: 5.5,
    },
  });

  deliver(liveSocket, frame(0));
  await settle();
  check(
    "a bar on the channel makes the badge say live, with the age as the evidence",
    badgeState() === "live" &&
      /live · BTCUSDT 5m · \d\d:\d\d · \d+s ago/.test(badge().textContent),
    `${badgeState()}: ${badge().textContent}`
  );

  // The half that makes it a measurement rather than a label. Frames stop, the
  // page's clock moves past the threshold, and the badge has to stop saying live
  // -- with no gesture and no redraw, because the case that matters is the page
  // sitting still. That is why the badge owns a clock instead of being refreshed
  // by `render`, and why this waits on the clock rather than dispatching anything.
  pageClock.offset += BAR_MS["5m"] * 2 + 60_000;
  await new Promise((resolve) => setTimeout(resolve, 1200));
  check(
    "and a channel that has gone quiet stops claiming to be live",
    badgeState() === "stale",
    `${badgeState()}: ${badge().textContent}`
  );

  // And a bar arriving again resets it, which is what makes the age worth
  // reading: the number the user watches is the one that starts again.
  deliver(liveSocket, frame(1));
  await settle();
  check(
    "and a bar arriving again resets the reading",
    badgeState() === "live" && badge().textContent.includes("0s ago"),
    `${badgeState()}: ${badge().textContent}`
  );

  // A channel that closes with nothing else to say is offline, and the badge says
  // that rather than freezing on the last reading it had.
  liveSocket.close();
  await settle();
  check(
    "and a channel that closes says offline",
    badgeState() === "offline",
    `${badgeState()}: ${badge().textContent}`
  );
}

// --- the footprint ladder -----------------------------------------------------
//
// The chart the user reported as "not well designed and presented". The cause
// was not the drawing: the engine sized its font from the row height alone and
// the shell dropped any text under 7px, so a real window came out as a colour
// grid with no numbers in it at all. Nothing but a real scene can catch that --
// the fixture has to have the row count and the column width of a real one -- so
// this switches a chart to footprint mode and reads what was painted.

console.log("\nthe footprint ladder");

const modeSelect = paneNode().querySelector(".mode");
const textBefore = painted.text.length;
modeSelect.value = "footprint";
change(modeSelect);
await settle();
await settle();
await settle();

const ladder = painted.text.slice(textBefore);
const pairs = ladder.filter((t) => /^[\d.]+( [KM])? x [\d.]+( [KM])?$/.test(t));
check(
  "a footprint draws its ladder as `bid x ask` pairs",
  pairs.length > 0,
  `${pairs.length} pairs of ${ladder.length} strings; e.g. ${pairs.slice(0, 3).join(" | ") || "(none)"}`
);
// The contract, and the bug: the engine sizes the font from the row height *and*
// the cell width and says whether it fits; the shell used to hold its own
// threshold and the two drifted, which is how a real window came out as a colour
// grid with no numbers. This fails if the engine ever hands back a font it cannot
// draw, or if the shell stops believing it.
const grid = lastScene().footprint;
check(
  "and the engine, not the shell, decided that the numbers fit",
  grid.show_text === true && pairs.length > 0,
  `show_text=${grid.show_text}, font=${grid.font_px.toFixed(2)}px, ${grid.columns.length} columns`
);
check(
  "and the window totals reach the strip under the chart",
  paneNode().querySelector(".footprintStats").textContent.includes("1,234"),
  paneNode().querySelector(".footprintStats").textContent.slice(0, 60)
);

// --- how many candles fit -----------------------------------------------------
//
// The report: "you should limit their width according to number size on that
// cell, because now their widths are large, which makes a small number of candles
// appear on the chart". The cell width was a constant in the shell -- 54, then 64
// -- matched by hand against the engine's font floor and glyph ratio, so how many
// candles were on screen was decided by a guess rather than by the numbers.
//
// The engine now reports `min_cell_px`, which is that same arithmetic read
// backwards, and the shell sizes its window from it. Both halves are checked: the
// shell asks for the engine's figure, and the figure is narrower than the constant
// it replaced -- otherwise this would pass while nothing had changed.

console.log("\nhow many candles fit");

/// Every footprint window the shell has asked for.
const footprintCalls = () => backend.calls.filter((c) => c.url.startsWith("/footprint?"));
/// How many columns one request asked for.
///
/// Derived from the window, because that is how the shell expresses it: it picks a
/// column count and then asks for that many bars.
const columnsOf = (call) => {
  const query = queryOf(call.url);
  const bar = BAR_MS[query.get("timeframe")] ?? BAR_MS["5m"];
  return Math.round((Number(query.get("to")) - Number(query.get("from"))) / bar);
};

const learned = Math.ceil(lastScene().footprint.min_cell_px);
const plotW = lastScene().plot.w;
const fitsIn = (cell) => Math.max(8, Math.min(60, Math.floor(plotW / cell)));
const want = fitsIn(learned);
const seeded = fitsIn(64);

check(
  "the numbers in this window need a narrower cell than the constant this replaced",
  Number.isFinite(learned) && want !== seeded,
  // `Number.isFinite` rather than a bare comparison, because `NaN !== 12` is true:
  // a check written as "the two differ" passes on a missing field, which is the
  // shape of every defect this file exists to catch.
  Number.isFinite(learned)
    ? `the engine wants ${learned}px a cell: ${want} columns, against ${seeded} for the old 64px`
    : `the engine reported no usable min_cell_px (${lastScene().footprint.min_cell_px})`
);

// A reload, so the shell has to size a window from what the engine told it rather
// than from the seed it starts with. The seed is the first load's guess and the
// whole point is that it does not stay a guess.
paneNode().querySelector(".load").click();
await settle();
await settle();
await settle();
await settle();

check(
  "and the shell asks for the most candles that cell width allows",
  columnsOf(footprintCalls().slice(-1)[0]) === want,
  `${columnsOf(footprintCalls().slice(-1)[0])} columns for a ${plotW}px plot and a ${learned}px cell (want ${want})`
);

// --- and the half of "live" the badge has to be able to deny -------------------
//
// The reading above is about the *channel*, and a footprint is not drawn from the
// channel: the ladder comes from `/footprint`, which is built from stored trades.
// So the feed can be perfectly healthy while the ladder is hours old, and a badge
// that said "live" there would be the green light that cannot go out -- the defect
// this project keeps finding, and the exact report that produced this badge. So
// the badge has to be able to say which of the two is stale.

const ladderSocket = openSocketsFor("/ws/market/").slice(-1)[0];
if (ladderSocket) {
  const newestLadder = lastScene().footprint.columns.reduce(
    (best, column) => Math.max(best, Number(column.open_time)),
    0
  );
  // A bar thirteen hours after the stored data, which is what the deployment
  // looked like: a live feed, and a ladder frozen at the last backfill.
  deliver(ladderSocket, {
    type: "data",
    payload: {
      symbol: "BTCUSDT",
      timeframe: "5m",
      open_time: newestLadder + 13 * 3_600_000 * 1e6,
      open: 76_500,
      high: 76_520,
      low: 76_490,
      close: 76_510,
      volume: 12.5,
      buy_volume: 7,
      sell_volume: 5.5,
    },
  });
  await settle();
  check(
    "a live feed over a frozen ladder says which of the two is stale",
    badgeState() === "behind" && badge().textContent.includes("13h"),
    `${badgeState()}: ${badge().textContent}`
  );
}

// Back to candles, because the sections below are about the candle chart and a
// pane left in footprint mode would be testing a different one.
modeSelect.value = "candles";
change(modeSelect);
await settle();
await settle();
await settle();

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
const symbolSelect = paneNode().querySelector(".symbol");
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
change(symbolSelect);
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
change(symbolSelect);
await settle();
await settle();
await settle();

check(
  "and it comes back on the symbol it belongs to",
  lastScene().drawings.length === 1,
  `${lastScene().drawings.length} drawings`
);

// --- a resize ----------------------------------------------------------------
//
// Responsiveness is not only the stylesheet. The canvas is sized from
// `.chartWrap`'s `clientWidth`/`clientHeight` and the engine is told the same two,
// so a window that changes shape has to re-measure both or it keeps drawing at
// the old size -- candles fitted to a plot that is no longer there.
//
// jsdom lays nothing out, so the viewport is moved by hand. That is not a
// workaround: those two properties are precisely the input `draw()` reads.

console.log("\na resize");

const rebuildsBefore = engine.scenes.length;
const wrap = paneNode().querySelector(".chartWrap");
Object.defineProperty(wrap, "clientWidth", { value: 640, configurable: true });
Object.defineProperty(wrap, "clientHeight", { value: 320, configurable: true });
window.dispatchEvent(new window.Event("resize"));
await settle();
await settle();

check(
  "a resize re-measures the canvas",
  canvasOf().width === 640 && canvasOf().height === 320,
  `${canvasOf().width}x${canvasOf().height}`
);
check(
  "and rebuilds the scene at the new size rather than the old one",
  lastRequest().width === 640 && lastRequest().height === 320,
  `${lastRequest().width}x${lastRequest().height}`
);

// Dragging a window edge fires a resize per event, dozens a second, and each one
// is a wasm rebuild plus a full repaint. They coalesce into one per frame, the
// same way a wheel does -- so this asserts a *count*, which is the only way to
// tell coalescing from a shell that happened to be fast.
const rebuildsBeforeBurst = engine.scenes.length;
for (let i = 0; i < 5; i += 1) window.dispatchEvent(new window.Event("resize"));
await settle();
await settle();

check(
  "and a burst of them costs one rebuild, not one each",
  engine.scenes.length === rebuildsBeforeBurst + 1,
  `${engine.scenes.length - rebuildsBeforeBurst} rebuilds for 5 events`
);

// --- more than one chart -----------------------------------------------------
//
// The complaint this section exists for: "multiple chart window option ... same
// chart windows but different time zone on each chart window or different chart
// each with its window".
//
// Two charts on a page are only worth anything if they are actually two, and the
// failure is a quiet one: a second pane that shares the first one's state still
// *looks* like two charts, and every check that only counts panes passes. So most
// of what follows is the negative -- a gesture in one chart leaving the other
// exactly as it was -- and the channels, which are the one part of the page that
// says out loud which chart it belongs to.
//
// It is one function rather than a flat run of statements because the first thing
// it does is assert that there are two charts, and everything below that reads
// pane 1. Flat, it would throw on a missing pane and take the rest of the file
// with it -- and a suite that dies instead of reporting is the same defect as a
// listener that throws: from the outside it looks like a pass.

console.log("\nmore than one chart");

async function twoCharts() {
  const scenesBefore = engine.scenes.length;
  document.getElementById("split").click();
  // Not `waitFor`, which ends the run: "the second chart never drew" is a defect
  // this section exists to report, not a reason to stop reporting.
  let drew = false;
  const deadline = Date.now() + 2000;
  while (!drew && Date.now() < deadline) {
    drew = engine.scenes.length > scenesBefore;
    if (!drew) await new Promise((resolve) => setTimeout(resolve, 10));
  }
  await settle();

  check(
    "adding a chart puts a second one beside it",
    paneNodes().length === 2,
    `${paneNodes().length} panes`
  );
  if (paneNodes().length !== 2) return;

  check("and it drew a chart of its own", drew, drew ? "drew" : "never drew");
  check(
    "and the new one is its own canvas, not the first one drawn twice",
    canvasOf(1) !== canvasOf(0) && canvasOf(1).tagName === "CANVAS",
    canvasOf(1) === canvasOf(0) ? "the same node" : "distinct nodes"
  );
  check(
    "and it opens on the same instrument as the chart it came from",
    selectIn(1, "symbol") === "BTCUSDT",
    selectIn(1, "symbol")
  );
  // `1h`, not `15m`: the next one up from `5m` is `15m`, which holds three bars,
  // and a second chart that opens on three candles reads as a bug rather than as
  // a second chart. The threshold is the bar count the pane is already asking
  // for, not a number invented for this check.
  check(
    "and on the next timeframe up that can fill a chart, so it is not a copy",
    selectIn(1, "timeframe") === "1h",
    `${selectIn(1, "timeframe")} (15m holds 3 bars; the pane asks for ${
      paneNode(1).querySelector(".limit").value
    })`
  );
  check(
    "so the two charts have asked the engine for two different series",
    framesFor("BTCUSDT/5m") > 0 && framesFor("BTCUSDT/1h") > 0,
    seriesSeen().join(", ")
  );
  check(
    "and each one holds its own market channel",
    openSocketsFor("/ws/market/").some((s) => s.url.includes("/ws/market/BTCUSDT/5m")) &&
      openSocketsFor("/ws/market/").some((s) => s.url.includes("/ws/market/BTCUSDT/1h")),
    openSocketsFor("/ws/market/").map((s) => s.url.replace(/^.*\/ws/, "/ws")).join(", ")
  );

  // Adding marks the new chart active, so the panel is already about the chart the
  // user asked for. Touching the other one is what has to move it back -- and the
  // *pointer* is what does that, because a chart has no other way of being pointed
  // at. The controls do not count: clicking a button is not a click on a chart.
  check(
    "adding a chart makes it the one the panel follows",
    paneNode(1).classList.contains("active") && !paneNode(0).classList.contains("active"),
    `active: ${activeIndexes()}`
  );
  pickTool("cursor", 0);
  pointer("pointerdown", 400, 200, 0);
  pointer("pointerup", 400, 200, 0);
  await settle();
  check(
    "and touching a chart is what makes it the one the panel follows",
    paneNode(0).classList.contains("active") && !paneNode(1).classList.contains("active"),
    `active: ${activeIndexes()}`
  );

  // Drawings are stored per user and instrument, not per timeframe -- that is the
  // persistence that was chosen -- so two charts of one instrument legitimately
  // show the same shape. Worth asserting rather than assuming, because the opposite
  // reading of this section ("two charts are independent") would make a shared
  // shape look like a bug.
  check(
    "and both charts of one instrument show that instrument's drawings",
    drawingsIn("BTCUSDT/5m") === 1 && drawingsIn("BTCUSDT/1h") === 1,
    `5m ${drawingsIn("BTCUSDT/5m")}, 1h ${drawingsIn("BTCUSDT/1h")}`
  );

  // A wheel in the first chart. The 5m chart is rebuilt and the 1h chart is not
  // touched at all. A pane that shared a viewport, a scene or a render flag with its
  // neighbour would pass every check above this line and fail here.
  const fiveBefore = framesFor("BTCUSDT/5m");
  const hourBefore = framesFor("BTCUSDT/1h");
  const otherScene = sceneOf("BTCUSDT/1h");

  wheel(0, 120);
  await settle();
  await settle();

  check(
    "a wheel in one chart rebuilds that chart and only that one",
    framesFor("BTCUSDT/5m") > fiveBefore && framesFor("BTCUSDT/1h") === hourBefore,
    `5m +${framesFor("BTCUSDT/5m") - fiveBefore}, 1h +${framesFor("BTCUSDT/1h") - hourBefore}`
  );
  check(
    "and the chart beside it is the very same scene object it was",
    otherScene !== null && sceneOf("BTCUSDT/1h") === otherScene,
    sceneOf("BTCUSDT/1h") === otherScene ? "untouched" : "rebuilt"
  );

  // The second chart's instrument. `el("symbol")` resolving to *this* pane is the
  // single line the whole refactor turns on: one call site, two different selects.
  //
  // Counted, not tested for presence: an earlier section already put this
  // instrument on the *first* chart, so `framesFor("ETHUSDT/5m") > 0` was true
  // before the gesture and stayed true when the gesture did nothing at all.
  const fiveBeforeSymbol = framesFor("BTCUSDT/5m");
  const ethBefore = framesFor("ETHUSDT/5m");
  const secondSymbol = paneNode(1).querySelector(".symbol");
  secondSymbol.value = "ETHUSDT";
  change(secondSymbol);
  await settle();
  await settle();
  await settle();

  check(
    "changing one chart's instrument fetches that instrument",
    framesFor("ETHUSDT/5m") > ethBefore,
    `ETHUSDT/5m +${framesFor("ETHUSDT/5m") - ethBefore}`
  );
  check(
    "and does not rebuild the chart beside it",
    framesFor("BTCUSDT/5m") === fiveBeforeSymbol,
    `5m +${framesFor("BTCUSDT/5m") - fiveBeforeSymbol}`
  );
  check(
    "and the panel's book channel moved to that instrument",
    openSocketsFor("/ws/orderbook/").length === 1 &&
      openSocketsFor("/ws/orderbook/")[0].url.includes("/ws/orderbook/ETHUSDT"),
    openSocketsFor("/ws/orderbook/").map((s) => s.url.replace(/^.*\/ws/, "/ws")).join(", ") ||
      "none open"
  );
  check(
    "and the chart whose instrument changed is the one the panel follows",
    paneNode(1).classList.contains("active"),
    `active: ${activeIndexes()}`
  );

  const storedBefore = backend.drawings.length;
  pickTool("trendline", 1);
  pointer("pointerdown", 200, 180, 1);
  await settle();
  pointer("pointermove", 520, 240, 1);
  await settle();
  pointer("pointerup", 520, 240, 1);
  await settle();
  await settle();

  check(
    "a shape drawn in the second chart is stored against the second chart's instrument",
    backend.drawings.length === storedBefore + 1 &&
      backend.drawings[storedBefore].symbol === "ETHUSDT",
    JSON.stringify(backend.drawings.map((d) => d.symbol))
  );
  check(
    "and drawing it did not rebuild the first chart",
    framesFor("BTCUSDT/5m") === fiveBeforeSymbol,
    `5m +${framesFor("BTCUSDT/5m") - fiveBeforeSymbol}`
  );

  // The cap. The button is *disabled* at four rather than refusing on click, for the
  // same reason the last close button is hidden: a control that is drawn and then
  // does nothing teaches the user the page is broken, when the truth is that a limit
  // was reached.
  const splitButton = document.getElementById("split");
  splitButton.click();
  await settle();
  splitButton.click();
  await settle();
  splitButton.click();
  await settle();
  await settle();

  check("the page stops at four charts", paneNodes().length === 4, `${paneNodes().length} panes`);
  check(
    "and the button that would add a fifth says so instead of refusing",
    splitButton.disabled === true,
    splitButton.disabled ? "disabled" : "still enabled"
  );

  // Closing. The last chart is not closable -- an empty page has no way back.
  check(
    "every chart offers a way to close it while there is more than one",
    paneNodes().every((p) => !p.querySelector(".close").hidden),
    paneNodes().map((p) => (p.querySelector(".close").hidden ? "hidden" : "shown")).join(", ")
  );

  // Touch the rightmost chart, so closing it has to move the panel somewhere rather
  // than leaving it pointing at a pane that is gone.
  pointer("pointerdown", 100, 100, 3);
  pointer("pointerup", 100, 100, 3);
  await settle();

  const marketBeforeClose = openSocketsFor("/ws/market/").length;
  paneNode(3).querySelector(".close").click();
  await settle();
  await settle();

  check(
    "closing a chart takes it off the page",
    paneNodes().length === 3,
    `${paneNodes().length} panes`
  );
  check(
    "and the panel moves to the chart beside it, not to nothing",
    paneNode(2).classList.contains("active"),
    `active: ${activeIndexes()}`
  );
  check(
    "and its market channel was closed with it, not left running",
    openSocketsFor("/ws/market/").length === marketBeforeClose - 1,
    `${marketBeforeClose} open before, ${openSocketsFor("/ws/market/").length} after`
  );
  check(
    "and the add button comes back once there is room again",
    splitButton.disabled === false,
    splitButton.disabled ? "still disabled" : "enabled"
  );

  paneNode(1).querySelector(".close").click();
  await settle();
  paneNode(1).querySelector(".close").click();
  await settle();

  check(
    "closing down to one chart leaves it open, with nothing left to close",
    paneNodes().length === 1 && paneNode(0).querySelector(".close").hidden === true,
    `${paneNodes().length} panes, close ${
      paneNode(0).querySelector(".close").hidden ? "hidden" : "shown"
    }`
  );
  // The guard, not the hidden attribute: the last chart cannot be closed even if
  // something clicks the button anyway.
  paneNode(0).querySelector(".close").click();
  await settle();
  check(
    "and clicking it anyway does not empty the page",
    paneNodes().length === 1,
    `${paneNodes().length} panes`
  );
}

await twoCharts();

// --- nothing threw -----------------------------------------------------------
//
// Last, so it covers every gesture above. A thrown listener is the one kind of
// defect that leaves no trace in any other check: the page looks right, the API
// calls are right, and the console is the only place it appears.

console.log("\nnothing threw");

check(
  "no uncaught error on the page, across every gesture above",
  pageErrors.length === 0,
  pageErrors.join(" | ") || "none"
);

console.log(
  failures === 0
    ? `\nall shell checks passed (${wasmPath})`
    : `\n${failures} shell check(s) FAILED`
);
process.exit(failures === 0 ? 0 : 1);
