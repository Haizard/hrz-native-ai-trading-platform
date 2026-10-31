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

// One script scope, no modules: a second top-level `function name()` in `app.js`
// is not a redeclaration error, it is a *replacement* -- hoisting lets the last
// one win, and the first one's callers silently get the second one's body. That
// is invisible everywhere else: the builder's concept editor and the indicator
// library card were both named `conceptCard`, and the page threw nothing while
// Studio rendered library cards into a form it used to build by hand.
{
  const counts = new Map();
  for (const [, name] of shell.matchAll(/^function\s+([A-Za-z_$][\w$]*)\s*\(/gm)) {
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  const shadowed = [...counts].filter(([, n]) => n > 1).map(([name]) => name);
  check(
    "no name in the app is declared twice, because the second one would erase the first",
    shadowed.length === 0,
    shadowed.join(", ")
  );
}

// jsdom does not fetch external scripts (there is no server), so the files the
// page loads before `app.js` are read here and injected below in the same order
// the browser would run them.
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

// --- the modal the browser gives you, which jsdom does not ------------------

// jsdom 30 parses `<dialog>` and its `open` attribute but ships neither
// `showModal` nor `close`, so every panel, drawer and popover in this harness
// would throw the first time the page asked to be a modal. What the platform
// actually provides -- the top layer, the scrim, the focus trap, inert
// background -- is not implementable here and is checked by hand in a browser.
// This stands in for the two methods and the two events, which is enough to
// drive the shell's own decisions: which host is showing, what the address says,
// and where focus goes when the box closes.
{
  const proto = window.HTMLDialogElement && window.HTMLDialogElement.prototype;
  if (proto && !proto.showModal) {
    proto.showModal = function showModal() {
      this.open = true;
    };
    proto.show = function show() {
      this.open = true;
    };
    proto.close = function close(returnValue) {
      if (!this.open) return;
      this.open = false;
      if (returnValue !== undefined) this.returnValue = String(returnValue);
      this.dispatchEvent(new window.Event("close"));
    };
  }
}

// --- what the shell is allowed to believe about its surroundings -------------

// A 2D context that records. `getContext` is called once per frame per pane by
// `draw()`, and the shell asks for `2d` and nothing else.
//
// One shared context for every canvas, which is fine because the shell never
// reads `ctx.canvas` -- the canvas it draws on is the one it asked. It did used
// to answer that property, and the branch was removed rather than left here: a
// stub that supports something nothing calls is a claim about the shell that
// nothing checks.
const painted = { ops: [], text: [], styles: [] };
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
    // Assignments matter too: a canvas paints `var(--up)` as nothing, so
    // "nothing hands the context a token" is a claim about these.
    set(_target, prop, value) {
      painted.styles.push(`${String(prop)}=${String(value)}`);
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

/// A click on the **global** toolbar, the one above the chart grid. Same
/// gesture as `pickTool`, different surface: the checks that care about the
/// two toolbars agreeing need a way to press each of them.
function globalPick(name) {
  document
    .querySelector(`#globalTools button[data-tool="${name}"]`)
    .click();
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
// One box for every canvas is what the chart engine needs, but it is not what a
// trader sees: a library of 182 cards has twelve on screen and 170 below the
// fold, and the difference is the whole question of what a card costs. So an
// indicator card reports its real position only once this file says the
// platform has put it there -- `__cardOnScreen`, by concept id, because a render
// builds new canvas nodes and a check that marked the old one would be marking
// nothing.
const BELOW_FOLD = {
  x: 0, y: 100000, top: 100000, left: 0, bottom: 100064, right: 240, width: 240, height: 64,
};
const cardOnScreen = new Set();
window.__cardOnScreen = (id, value) => {
  if (value) cardOnScreen.add(id);
  else cardOnScreen.delete(id);
};
Object.defineProperty(window.HTMLCanvasElement.prototype, "getBoundingClientRect", {
  value() {
    const card = this.closest && this.closest(".conceptCard[data-concept]");
    return card && !cardOnScreen.has(card.dataset.concept) ? BELOW_FOLD : RECT;
  },
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

// `toDataURL` is not implemented in jsdom either -- it needs the native `canvas`
// package, which this harness deliberately does not depend on.
//
// This one is *not* a "just do not throw" stub, and the difference matters. The
// shell's `captureChart` downscales through an offscreen canvas and then calls
// `toDataURL`, so a stub that throws would exercise only the failure path and
// leave the success path -- the allowlist check, the byte arithmetic, the
// `screenshot` field on the packet -- completely unverified. That is the half of
// the feature a user actually depends on.
//
// So it returns a real base64 `image/png` header instead: small, valid, and
// shaped exactly like what a browser hands back, which is enough for every check
// downstream of the pixels. The 1x1 transparent PNG below is the standard one.
//
// `width`/`height` are honoured so the downscale can be *observed*: a shell that
// forgot to cap the long edge would hand back a payload as large as the canvas
// it started from, and one of the checks reads the size it produced.
const ONE_PIXEL_PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
window.HTMLCanvasElement.prototype.toDataURL = function (type) {
  const mediaType = typeof type === "string" && type ? type : "image/png";
  return `data:${mediaType};base64,${ONE_PIXEL_PNG}`;
};

// The socket. `connectLive` and `connectBook` both open one and both tolerate
// it never saying anything, so a stub that stays silent is the honest double:
// nothing about this file depends on a frame arriving.
//
// It does record *what it was opened for*, because a channel is the only part of
// the page that says out loud which chart it belongs to: `/ws/market/BTCUSDT/5m`
// is a per-pane claim and `/ws/orderbook/BTCUSDT` is a page-level one, and the
// difference between those two is the whole of "which chart is the panel about".
const sockets = [];
/// Additions to the socket mock, so a check can drive a refusal.
///
/// Declared before the class that reads them, because the class body runs at
/// assignment time and a `const` after it would be in its temporal dead zone.
/// `refuse` is a predicate on the URL: a refused socket fires `onerror` instead
/// of `onopen`, which is what a browser does when the handshake gets a non-101
/// -- and per spec it hands script no status, which is the whole reason the
/// shell has to go and ask.
const socketBehaviour = { refuse: null };
window.WebSocket = class {
  constructor(url) {
    this.url = String(url);
    this.readyState = 1;
    this.closed = false;
    // Everything this socket was told, in order. Recorded rather than ignored
    // because the request *body* is the only place several contracts live: the
    // agent message's `chart` packet exists nowhere else, and a stub that
    // swallowed it made "the shell sends the viewport" untestable -- so an
    // `ask()` that dropped it on the floor would have passed every check here.
    this.sent = [];
    sockets.push(this);
    setTimeout(() => {
      if (socketBehaviour.refuse && socketBehaviour.refuse(this.url)) {
        // A refusal, then a close -- the order a browser uses. `onerror` first
        // is what makes the shell's reason-probe run.
        if (this.onerror) this.onerror({ type: "error" });
        if (this.onclose) this.onclose({ code: 1006 });
        return;
      }
      if (this.onopen) this.onopen();
    }, 0);
  }
  send(payload) {
    this.sent.push(String(payload));
  }
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
  // The AI-drawn row (`docs/21` phase 3) is injected later, just before the
  // symbol round-trip returns to BTCUSDT -- injecting it here would inflate
  // every storage-count check between this point and the clear-all, and the
  // clear-all itself would delete it before the layer section could use it.
  drawings: [],
  nextId: 1,
  requests: [],
  failCreate: null, // a status code, to exercise the failure path
  calls: [],
  // The dashboard's one batch, `GET /tickers?limit=1000`: ten majors and sixty
  // alts, volumes descending (which is the order the route really returns),
  // changes chosen to land in every heat bucket. BTC and ETH keep the
  // directions Task 12's rail checks expect (BTC up, ETH down) so whichever
  // of the two batches last answered, the rail's tint is the same.
  markets: (() => {
    const majors = [
      ["BTCUSDT", 67412.5, 2.5],
      ["ETHUSDT", 2481.1, -2.25],
      ["SOLUSDT", 148.2, 8.1],
      ["BNBUSDT", 592.4, 0.4],
      ["XRPUSDT", 0.6123, -0.9],
      ["ADAUSDT", 0.4412, -2.6],
      ["DOGEUSDT", 0.1284, -4.4],
      ["AVAXUSDT", 34.2, 0],
      ["LINKUSDT", 17.8, 1.2],
      ["DOTUSDT", 7.05, -1.4],
    ];
    const rows = majors.map(([symbol, last, change], index) => ({
      symbol,
      last_price: last,
      price_change_percent: change,
      high_price: last * 1.05,
      low_price: last * 0.95,
      quote_volume: 2_000_000_000 - index * 100_000_000,
    }));
    for (let i = 0; i < 60; i += 1) {
      rows.push({
        symbol: `ALT${String(i).padStart(2, "0")}USDT`,
        last_price: 500 + i * 10,
        price_change_percent: i % 2 === 0 ? 1.4 : -1.6,
        high_price: 600 + i * 10,
        low_price: 400 + i * 10,
        quote_volume: 900_000_000 - i * 10_000_000,
      });
    }
    return rows;
  })(),
  // What `/capabilities` reports for the agent, and what `/auth/me` answers.
  //
  // Mutable so a check can drive the two refusal causes the shell must tell
  // apart. Default `ready` + `200`: a harness whose default was "not
  // configured" would make every other check's agent socket fail for a reason
  // nobody chose.
  agentCapability: "ready",
  meStatus: 200,
  // The dock's three sources. Two monitors -- one enabled, one paused -- so a
  // check can tell "Pause" from "Resume" without first pressing Pause; one
  // event, so the touches list has a row and the dock's two sections are both
  // non-empty at once.
  monitors: [
    { id: "mon-1", symbol: "BTCUSDT", timeframe: "5m", enabled: true, created_at: "2026-10-27T09:00:00Z" },
    { id: "mon-2", symbol: "ETHUSDT", timeframe: "1h", enabled: false, created_at: "2026-10-26T09:00:00Z" },
  ],
  watchEvents: [
    {
      id: "ev-1", symbol: "BTCUSDT", timeframe: "5m",
      reason: "price touched the lower demand zone", status: "done",
      summary: "the agent re-read the chart and held", created_at: "2026-10-27T09:30:00Z",
    },
  ],
  // The Indicators panel's catalog: eight real ids from eight different rail
  // groups, chosen so every shape a concept can take is on screen at once -- one
  // that marks bars, one that measures in its own sub-pane with a fixed scale,
  // one that draws over the price, one that needs a second instrument, and one
  // this instrument cannot feed at all.
  //
  // Nine momentum oscillators would have let the rail, the kind chips, the pair
  // field, the greyed card and the "needs" line all pass while none of them was
  // exercised, which is the failure mode this fixture exists to avoid.
  concepts: [
    {
      id: "smc.liquidity_sweep", version: "1.0.0", library: "smc",
      name: "Liquidity Sweep", parameters: [], required_channels: ["candles"],
      description: "price ran beyond a liquidity level and closed back inside it -- the pool was taken without acceptance",
      available: true, pane: false, overlay: false,
    },
    {
      id: "candles.doji", version: "1.0.0", library: "candles",
      name: "Doji", parameters: [], required_channels: ["candles"],
      description: "open and close nearly equal -- the bar could not decide",
      available: true, pane: false, overlay: false,
    },
    {
      id: "econ.fomc", version: "1.0.0", library: "econ",
      name: "FOMC Decision", parameters: [], required_channels: ["candles"],
      description: "the funds target's own change -- the decision IS the surprise",
      available: true, pane: false, overlay: false,
    },
    {
      id: "indicators.rsi", version: "1.0.0", library: "indicators",
      name: "RSI", parameters: [], required_channels: ["candles"],
      description: "Wilder's relative strength index over 14 bars -- in its own pane with the 30/70 bands",
      available: true, pane: true, overlay: false,
    },
    {
      id: "indicators.macd", version: "1.0.0", library: "indicators",
      name: "MACD", parameters: [], required_channels: ["candles"],
      description: "the 12/26 EMA difference, its 9-bar signal, and the histogram between them",
      available: true, pane: true, overlay: false,
    },
    {
      id: "indicators.bollinger", version: "1.0.0", library: "indicators",
      name: "Bollinger Bands", parameters: [], required_channels: ["candles"],
      description: "the 20-bar average with its two-sigma bands on the price plot",
      available: true, pane: false, overlay: true,
    },
    {
      id: "indicators.pairs", version: "1.0.0", library: "indicators",
      name: "Pairs (Cointegration)", parameters: [], required_channels: ["candles"],
      description: "the Engle-Granger spread's z-score with the mean-reversion gate in the label",
      available: true, pane: true, overlay: false,
    },
    {
      id: "order_flow.delta_shift", version: "1.0.0", library: "order_flow",
      name: "Delta Shift", parameters: [], required_channels: ["trades_aggressor"],
      description: "bar delta at a multiple of its recent average absolute value -- aggressors leaned one way",
      available: false, pane: false, overlay: false,
    },
  ],
  // The answer `GET /scan` gives. Two ranked rows, one failure and a note, which
  // is the shape that makes the panel's three regions all visible at once --
  // ranked first, the unmeasured in their own fold, and the caveat below. A
  // fixture with only ranked rows would let a shell that dropped `failures`
  // entirely still look correct.
  scan: {
    metric: "rsi",
    timeframe: "1h",
    // The order and the values deliberately **disagree**, and that is the point:
    // the server ranks by the metric's own direction over a set the caller cannot
    // see (the full universe, then truncated), so "highest number first" is not
    // the same as "server's order". A client that re-sorted would look correct
    // against a fixture whose values happened to descend, and wrong here.
    rows: [
      { symbol: "BTCUSDT", value: 43.5, as_of_ms: null, bars: 300, error: null },
      { symbol: "ETHUSDT", value: 71.234, as_of_ms: null, bars: 300, error: null },
    ],
    failures: [
      { symbol: "XRPUSDT", value: null, as_of_ms: null, bars: 0, error: "no candles for 1h" },
    ],
    skipped: 0,
    requested: 3,
    complete: false,
    summary: "3 instruments measured on 1h by RSI; 2 could be ranked.",
    universe: "venue",
    universe_size: 842,
    note: "the ranking covers the 50 most liquid instruments",
  },
  // Two instruments, because a chart per instrument is what a second pane is for
  // and one symbol cannot tell two panes apart.
  //
  // The order and the counts matter, and each is adversarial on purpose.
  //
  // The order is **alphabetical** -- `15m, 1d, 1h, 1m, 4h, 5m` -- which is the
  // order the live deployment used to report before the server moved to its
  // standard ladder, and it is what made the default timeframe regress to the
  // *thinnest* series once the selects started coming from `/symbols` instead of
  // from the markup. It is kept here even though the server no longer sends it,
  // because a fixture already in ladder order would let the shell's sort pass
  // without ever having run -- the harness would be asserting the server's
  // answer rather than the page's.
  //
  // The counts are what `deepestFrame` reads to pick the series a chart opens
  // on and what `nextFrame` reads to decide a timeframe cannot fill a chart. A
  // fixture tidied into `5m, 15m, 1h` would have hidden both.
  //
  // `1d` is the rung the ladder gained when `STANDARD_TIMEFRAMES` became one
  // shared constant (`docs/19` row 23), and this fixture did not have it, so the
  // harness could not see it at all. Its counts are representative rather than
  // captured: a daily series is the one a fresh buffer has least of.
  symbols: [
    {
      symbol: "BTCUSDT",
      coverage_note: "",
      timeframes: [
        { timeframe: "15m", candles: 3, expected: 3, missing: 0, first: 0, last: 0 },
        { timeframe: "1d", candles: 1180, expected: 1200, missing: 20, first: 0, last: 0 },
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

/// The Indicators panel's two preview reads, as the routes answer them.
///
/// Both are built from `candlesFor`, so the timestamps line up with the bars the
/// pane is holding -- and deliberately not *exactly*: the series starts at bar
/// 100 while the card's crop is the last 96 bars of the window, so four of its
/// points fall outside what the card shows. That is the case the alignment is for.
/// Likewise one event sits at bar 40, off the end of the crop, because "a concept
/// whose history is outside the window draws nothing" has to be distinguishable
/// from a preview that is simply broken.

/// The answer `GET /concept-events/scan` gives: every concept's events at once,
/// which is why the whole grid of cards costs one request.
function conceptScanEvents(symbol, timeframe) {
  const bars = candlesFor(200, symbol, timeframe);
  const sweep = bars[150];
  const fomc = bars[180];
  const stale = bars[40];
  const rsiBar = bars[170];
  return [
    {
      concept_id: "smc.liquidity_sweep", concept_version: "1.0.0", symbol, timeframe,
      direction: "bearish", status: "confirmed", event_time_ns: sweep.open_time,
      price_level: sweep.high, zone_low: sweep.high - 0.8, zone_high: sweep.high + 0.5,
      evidence: "swept the equal highs and closed back inside",
      bars: [bars[148].open_time, sweep.open_time],
    },
    {
      concept_id: "econ.fomc", concept_version: "1.0.0", symbol, timeframe,
      direction: "neutral", status: "confirmed", event_time_ns: fomc.open_time,
      price_level: fomc.close, evidence: "25bp cut",
      bars: [bars[176].open_time, fomc.open_time],
    },
    {
      // A pane concept's event: its `price_level` is the bar's close, and the
      // reading it is about (28.4) is the magnitude. The chart must place the
      // mark at the pane's own line at this bar, not at the close.
      concept_id: "indicators.rsi", concept_version: "1.0.0", symbol, timeframe,
      direction: "bullish", status: "confirmed", event_time_ns: rsiBar.open_time,
      price_level: rsiBar.close, zone_low: rsiBar.low, zone_high: rsiBar.high,
      evidence: { magnitude: 28.4 }, bars: [rsiBar.open_time, rsiBar.open_time],
    },
    {
      concept_id: "smc.liquidity_sweep", concept_version: "1.0.0", symbol, timeframe,
      direction: "bullish", status: "expired", event_time_ns: stale.open_time,
      price_level: stale.low, zone_low: stale.low - 0.5, zone_high: stale.low + 0.3,
      evidence: "took the equal lows", bars: [bars[38].open_time, stale.open_time],
    },
  ];
}

/// The answer `GET /indicator-series` gives for one concept.
///
/// The four shapes the engine emits -- a fixed-scale line, bars and lines on an
/// auto scale, an overlay with a fill -- are all here, keyed by the concept the
/// shell asked for, so a card that asked for RSI and drew MACD's data would fail.
function conceptSeriesFixture(concept, symbol, timeframe) {
  const bars = candlesFor(200, symbol, timeframe).slice(100);
  const times = bars.map((b) => b.open_time);
  const closes = bars.map((b) => b.close);
  const wave = (offset) => bars.map((_, i) => Math.sin((i + offset) / 7));
  if (concept === "indicators.rsi") {
    return {
      symbol, times,
      spec: {
        label: "RSI(14)", scale: { mode: "fixed", min: 0, max: 100 }, levels: [30, 70],
        lines: [{ name: "RSI", color: 0x8b5cf6, values: closes.map((_, i) => 50 + 22 * Math.sin(i / 6)) }],
        segments: [], markers: [], bars: null, fills: [], target: "pane",
      },
    };
  }
  if (concept === "indicators.macd") {
    const line = wave(0).map((v) => v * 1.5);
    return {
      symbol, times,
      spec: {
        label: "MACD(12,26,9)", scale: { mode: "auto" }, levels: [0],
        lines: [
          { name: "MACD", color: 0x4c8dff, values: line },
          { name: "signal", color: 0xf59e0b, values: wave(3).map((v) => v * 1.1) },
        ],
        segments: [], markers: [],
        bars: { name: "histogram", color_up: 0x26a69a, color_down: 0xef5350, values: line.map((v, i) => v - wave(3)[i] * 1.1) },
        fills: [], target: "pane",
      },
    };
  }
  if (concept === "indicators.bollinger") {
    const mid = closes.map((_, i) => 100 + (i + 100) * 0.5);
    return {
      symbol, times,
      spec: {
        label: "BB(20,2)", scale: { mode: "auto" }, levels: [],
        lines: [
          { name: "upper", color: 0x60a5fa, values: mid.map((v) => v + 2) },
          { name: "mid", color: 0x9ca3af, values: mid },
          { name: "lower", color: 0x60a5fa, values: mid.map((v) => v - 2) },
        ],
        segments: [], markers: [], bars: null,
        fills: [{ name: "band", color: 0x60a5fa, upper: mid.map((v) => v + 2), lower: mid.map((v) => v - 2) }],
        target: "overlay",
      },
    };
  }
  return {
    symbol, times,
    spec: {
      label: concept || "series", scale: { mode: "auto" }, levels: [],
      lines: [{ name: "spread", color: 0x4c8dff, values: wave(1) }],
      segments: [], markers: [], bars: null, fills: [], target: "pane",
    },
  };
}

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

  // The instrument bar's search. Ahead of `/symbols` on purpose: `startsWith`
  // does not know about path segments, and `/symbols/search` starts with
  // `/symbols` -- so a mock below this one would swallow every search.
  if (url.startsWith("/symbols/search")) {
    const query = (queryOf(url).get("q") ?? "").toUpperCase();
    const all = backend.symbols.map((entry) => ({
      symbol: entry.symbol,
      base: entry.symbol.replace(/USDT$/, ""),
      quote: "USDT",
      trading: true,
    }));
    return json(200, {
      query,
      results: all.filter((entry) => entry.symbol.includes(query)),
      indexed: all.length,
      fetched_at: FIXTURE_NOW_MS,
      stale: false,
    });
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

  // One rising and one falling, so the rail's edge tint is exercised in both
  // directions by a single render.
  if (url.startsWith("/tickers")) {
    const query = queryOf(url);
    // The venue's most active list: what the markets table, the heatmap and
    // the watchlist panel read on the dashboard.
    if (query.get("limit")) return json(200, { tickers: backend.markets });
    const asked = (query.get("symbols") ?? "BTCUSDT,ETHUSDT").split(",").filter(Boolean);
    return json(200, {
      tickers: asked.map((symbol, index) => ({
        symbol,
        last_price: basePriceFor(symbol) + 10,
        price_change_percent: index % 2 === 0 ? 1.5 : -2.25,
      })),
    });
  }

  // The indicator workspace the Inspector tab projects. The revision is the
  // longer prefix, so it is matched first -- `startsWith` does not know about
  // path segments, and `/indicator-workspaces/ws-1` is a prefix of it.
  //
  // The preview is shaped like the real thing rather than like the three numbers
  // the checks read: a live layer's `concepts` go into the scene request, and the
  // engine deserializes them with `deny_unknown_fields`. A `{name}`-only concept
  // was the first version of this fixture and the attach failed with "missing
  // field `side`" -- the fixture, not the page, was the thing that was wrong.
  if (url === "/indicator-workspaces/ws-1/revisions/rev-1") {
    return json(200, {
      preview: {
        name: "Liquidity Sweep",
        concepts: [
          {
            name: "bullish_gap",
            label: "bullish gap",
            side: "buy",
            window: 3,
            lower: { high: 0 },
            upper: { low: 2 },
            require: [{ left: { high: 0 }, op: "below", right: { low: 2 } }],
          },
        ],
        zones: [
          {
            id: "z1",
            start_time: 1_789_679_400_000 * 1e6,
            end_time: 1_789_679_700_000 * 1e6,
            price_low: 100.5,
            price_high: 103,
            label: "bullish gap",
            state: "active",
          },
          {
            id: "z2",
            start_time: 1_789_680_000_000 * 1e6,
            end_time: 1_789_680_300_000 * 1e6,
            price_low: 104,
            price_high: 106.5,
            label: "bullish gap",
            state: "tapped",
          },
        ],
        markers: [
          {
            id: "m1",
            evidence_id: "e1",
            time: 1_789_679_400_000 * 1e6,
            price: 101,
            label: "sweep",
            kind: "bullish",
          },
        ],
      },
    });
  }
  if (url === "/indicator-workspaces/ws-1") {
    return json(200, {
      id: "ws-1",
      name: "Liquidity Sweep",
      symbol: "BTCUSDT",
      active_revision_id: "rev-1",
    });
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
        // The list, in the order the shell sent it: a stub that re-paired the
        // points into named columns could not tell a channel from a line that
        // lost its width.
        points: body.points,
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

  // What the agent channel reports when a handshake is refused.
  //
  // The socket itself cannot say why: a non-101 response is opaque to script, so
  // the shell goes and asks. These two stubs are the answers it can get, and the
  // point of the checks below is that the shell distinguishes them rather than
  // printing one generic sentence for both.
  if (url.startsWith("/scan")) {
    // Echoes back what the shell *asked*, rather than returning a canned answer
    // for every query. The parameters are the contract: a shell that sent the
    // wrong metric name or dropped the explicit symbols would still render a
    // plausible table against a fixture that ignored them.
    const asked = new URLSearchParams(url.slice(url.indexOf("?") + 1));
    const scan = { ...backend.scan };
    scan.metric = asked.get("metric") || "rsi";
    scan.timeframe = asked.get("timeframe") || "1h";
    scan.universe = asked.get("symbols") ? "explicit" : "venue";
    scan.universe_size = asked.get("symbols") ? null : backend.scan.universe_size;
    // A real `as_of_ms` on the ranked rows, so the panel's age column has a
    // number to render and a check on it is not asserting the formatting of null.
    scan.rows = backend.scan.rows.map((r) => ({ ...r, as_of_ms: Date.now() - 120000 }));
    return json(200, scan);
  }

  if (url.startsWith("/concepts")) {
    // The catalog the Indicators panel renders, and that a pane consults once a
    // concept is attached. The route's contract is an *array* -- the generic `{}`
    // fallback made the first attach crash the shell, which stopped every section
    // after the drawing checks from running at all.
    return json(200, backend.concepts);
  }

  // The Indicators panel's previews. One scan answers for every event card on the
  // page, which is the whole reason a grid of them costs one request; the series
  // route is per concept and per instrument.
  if (url.startsWith("/concept-events/scan")) {
    const asked = queryOf(url);
    return json(200, {
      events: conceptScanEvents(asked.get("symbol") ?? "BTCUSDT", asked.get("timeframe") ?? "5m"),
    });
  }
  if (url.startsWith("/indicator-series")) {
    const asked = queryOf(url);
    return json(200, conceptSeriesFixture(
      asked.get("concept"),
      asked.get("symbol") ?? "BTCUSDT",
      asked.get("timeframe") ?? "5m"
    ));
  }

  // The dock's Alerts tab. `PATCH` and `DELETE` store the change rather than
  // answering a fixed list: a stub that ignored either would let "Pause" pass
  // while the row still said Watching, which is the one thing this tab is for.
  if (url.startsWith("/watch-monitors")) {
    const id = url.split("/")[2];
    if (method === "PATCH") {
      const body = JSON.parse(options.body || "{}");
      const monitor = backend.monitors.find((m) => m.id === id);
      if (monitor) monitor.enabled = Boolean(body.enabled);
      return json(200, monitor || {});
    }
    if (method === "DELETE") {
      backend.monitors = backend.monitors.filter((m) => m.id !== id);
      return json(204, null);
    }
    return json(200, { monitors: backend.monitors });
  }
  if (url.startsWith("/watch-events")) {
    return json(200, { events: backend.watchEvents });
  }
  // The dashboard's Automation pulse. Two running, one paused, one killed --
  // one of each status the counts distinguish.
  if (url === "/bots") {
    return json(200, [
      { id: "bot-1", status: "running", mode: "paper", venue: "binance" },
      { id: "bot-2", status: "running", mode: "paper", venue: "binance" },
      { id: "bot-3", status: "paused", mode: "paper", venue: "binance" },
      { id: "bot-4", status: "killed", mode: "paper", venue: "binance" },
    ]);
  }
  // The dock's Orders tab reads the local ledger; this route is the ticket's
  // server-side contract list, which the tab only links to.
  if (url.startsWith("/trade/portfolio")) {
    return json(200, []);
  }

  if (url.startsWith("/capabilities")) {
    const agent = backend.agentCapability;
    return json(200, {
      capabilities: [
        agent === "ready"
          ? { name: "agent", readiness: "ready", verified: true, depends_on: "AWS_BEDROCK_*" }
          : {
              name: "agent",
              readiness: agent,
              verified: false,
              depends_on: "AWS_BEDROCK_REGION",
              detail: "not configured",
            },
      ],
      data: [],
      instruments: { indexed: 0, fetched_at_ns: null, stale: true },
      feeds: { route: 0, bot: 0, max_active: 32, symbols: [] },
      all_ready: agent === "ready",
    });
  }

  if (url.startsWith("/auth/me")) {
    // The token the harness wrote is accepted unless a check says otherwise --
    // this is the "the agent is configured, so the refusal was the credential"
    // half of the pair.
    if (backend.meStatus !== 200) {
      return json(backend.meStatus, {
        error: {
          code: "UNAUTHORIZED",
          message: "the session token is missing, malformed or expired",
        },
      });
    }
    return json(200, { user_id: "harness", email: "harness@example.test" });
  }

  // The plan matrix and the harness user's entitlement. Both are read on load,
  // and the shell dereferences their fields rather than tolerating `{}`: an
  // empty body here crashed the page before any check ran, so the fixture has
  // to have the route's real shape.
  //
  // The harness user is on Desk, and Desk's `max_panes` is served as unlimited
  // (`-1`) rather than its catalogue value. The wall checks drive the page to
  // a hundred cells, and a finite cap would stop the add control at the plan's
  // number instead -- turning "the wall stops at a hundred" into a test of
  // billing. The caps themselves are covered by `billing_flow.rs`; this file
  // is about the shell.
  if (url.startsWith("/billing/plans")) {
    const limits = {
      max_panes: -1,
      max_layouts: 500,
      max_alerts: 500,
      max_watchers: 50,
      max_ai_credits: 20_000,
      max_backtests: 2_000,
      history_days: -1,
      max_paper_bots: 20,
      max_live_bots: 10,
      max_concurrent: 100,
    };
    const features = {
      orderflow: true,
      orderflow_advanced: true,
      byo_model_key: true,
      priority_support: true,
      dedicated_onboarding: true,
      community_indicators: "priority",
    };
    return json(200, {
      plans: [
        {
          id: "free",
          name: "Free",
          tagline: "Chart, paper trade, and try the agent.",
          pricing: { monthly_cents: 0, annual_cents: 0, annual_effective_monthly_cents: 0 },
          limits: { ...limits, max_panes: 4, max_layouts: 3, max_alerts: 5, max_watchers: 1 },
          features: { ...features, orderflow: false, orderflow_advanced: false },
        },
        {
          id: "desk",
          name: "Desk",
          tagline: "Multiple live books and the highest allowances.",
          pricing: {
            monthly_cents: 9_900,
            annual_cents: 99_000,
            annual_effective_monthly_cents: 8_250,
          },
          limits,
          features,
        },
      ],
      trial_days: 14,
      mode: "internal",
      billing_available: true,
      community_indicators_available: true,
      credit_costs: {},
    });
  }

  if (url.startsWith("/billing/subscription")) {
    const period_start = new Date(FIXTURE_NOW_MS - 10 * 86_400_000).toISOString();
    const period_end = new Date(FIXTURE_NOW_MS + 20 * 86_400_000).toISOString();
    return json(200, {
      plan: "desk",
      plan_name: "Desk",
      limits: {
        max_panes: -1,
        max_layouts: 500,
        max_alerts: 500,
        max_watchers: 50,
        max_ai_credits: 20_000,
        max_backtests: 2_000,
        history_days: -1,
        max_paper_bots: 20,
        max_live_bots: 10,
        max_concurrent: 100,
      },
      features: {
        orderflow: true,
        orderflow_advanced: true,
        byo_model_key: true,
        priority_support: true,
        dedicated_onboarding: true,
        community_indicators: "priority",
      },
      subscription: {
        status: "active",
        cycle: "monthly",
        period_start,
        period_end,
        cancel_at_period_end: false,
        trial_used: true,
        provider: "internal",
      },
      usage: {
        period_start,
        resets_at: period_end,
        ai_credits: { used: 120, limit: 20_000, remaining: 19_880 },
        backtests: { used: 3, limit: 2_000, remaining: 1_997 },
      },
      community_indicators_available: true,
    });
  }

  if (url.startsWith("/indicator-workspaces")) {
    return json(200, [
      { id: "ws-liquidity", name: "Liquidity sweep", symbol: "BTCUSDT", timeframe: "15m" },
    ]);
  }

  // The account panel's Brokers tab, in both halves: the venues a deployment
  // offers and the accounts this user has connected. Both are lists, because
  // both are iterated -- an empty object from here is what the panel's own check
  // in the harness used to trip over.
  if (url.startsWith("/brokers/available")) {
    return json(200, [
      { venue: "binance", name: "Binance" },
      { venue: "bybit", name: "Bybit" },
    ]);
  }
  if (url.startsWith("/brokers")) {
    return json(200, [
      { venue: "binance", label: "main", status: "connected", last_error: "" },
    ]);
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
// JWT-shaped, not opaque: `HRZApp.email()` reads the payload's `sub`, and an
// opaque token makes every email read empty -- which is how the Details tab's
// account row would sit at "—" and nobody would notice. Nothing checks the
// signature, in the page or in the mocks.
const payload = Buffer.from(JSON.stringify({ sub: "harness@example.test" })).toString("base64");
window.localStorage.setItem(tokenKey, `header.${payload}.signature`);

// --- the page's clocks -------------------------------------------------------
//
// The dashboard's tick is a five-second interval, and jsdom will not run one
// inside a test. Recording what the page registered -- by the callback's name,
// which is a function declaration's name -- is how "the dashboard's clock runs
// there and nowhere else" is checked without waiting five seconds.
const intervals = new Map();
const realSetInterval = window.setInterval;
const realClearInterval = window.clearInterval;
window.setInterval = (callback, ms, ...rest) => {
  const id = realSetInterval(callback, ms, ...rest);
  intervals.set(id, { ms, name: callback && callback.name });
  return id;
};
window.clearInterval = (id) => {
  intervals.delete(id);
  realClearInterval(id);
};
/// How many live clocks were registered by a function with this name.
const clocksNamed = (name) =>
  [...intervals.values()].filter((clock) => clock.name === name).length;

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

// --- what the page may believe about visibility -------------------------------
//
// jsdom implements no `IntersectionObserver`, and the shell deliberately treats
// a DOM without one as "every pane is on screen". That fallback is right for a
// browser missing the API, but under this harness it would leave the wall's
// pause/resume path -- the reason a hundred charts do not mean a hundred
// sockets -- completely unexercised.
//
// So this stub is the API's shape and nothing more. `observe` records the node
// and, on a later task, answers that it is on screen -- which is what a real
// observer does for every element it is handed, and what a page that assumes it
// cannot otherwise learn here. `__intersect` plays the platform's part and
// delivers a *change* to every observer watching the node. It reports how many
// heard, so a check can tell "the shell never observed this pane" apart from
// "the shell ignored the change".
const observers = [];
// Whether `observe` plays the platform's initial delivery. The indicator library
// asks which cards are on screen before it asks for their pictures, so this flag
// is how a check can say "the platform has not told it yet" and see that the
// page therefore asks for nothing.
let silentObserve = false;
window.__observeSilently = (value) => { silentObserve = value; };
window.IntersectionObserver = class IntersectionObserver {
  constructor(callback, options) {
    this.callback = callback;
    this.options = options;
    this.targets = new Set();
    observers.push(this);
  }
  observe(target) {
    this.targets.add(target);
    if (silentObserve) return;
    // A real observer answers on a later task, not inside `observe` -- a page
    // that was written to a synchronous answer would work here and not in Chrome.
    queueMicrotask(() => {
      if (this.targets.has(target)) this.callback([{ target, isIntersecting: true }], this);
    });
  }
  unobserve(target) { this.targets.delete(target); }
  disconnect() { this.targets.clear(); }
  takeRecords() { return []; }
};
window.__intersect = (target, isIntersecting) => {
  let heard = 0;
  for (const observer of observers) {
    if (!observer.targets.has(target)) continue;
    heard += 1;
    observer.callback([{ target, isIntersecting }], observer);
  }
  return heard;
};

// jsdom reports every media query as false. The shell's defaults (watchlist
// and inspector open) hang off min-width queries, so pin a 1440px viewport.
// This must be in place before `theme.js` loads -- it reads the OS preference
// as it defines `Theme`.
window.matchMedia = (query) => {
  const min = /min-width:\s*(\d+)px/.exec(query);
  return {
    matches: min ? 1440 >= Number(min[1]) : false,
    media: query,
    onchange: null,
    addEventListener() {}, removeEventListener() {},
    addListener() {}, removeListener() {},
    dispatchEvent() { return false; },
  };
};

// The page's scripts, in the page's order, then `app.js`. `app.js` paints the
// icon slots and reads `Theme` on load, so a run that skipped these would fail
// before the first check -- and it would fail on the harness, not the page.
//
// jsdom fetches nothing, so the order has to come from somewhere, and the only
// honest source is `index.html` itself. A script the page loads and the harness
// forgot is not a skipped feature: `app.js` evaluates with a global missing, and
// the run dies much later as "timed out waiting for the engine to load" -- a
// failure that looks like the page's fault and is entirely the harness's.
//
// `views/shell.js` calls `HRZUI.drawer` and `app.js` calls `Shell.init`, so a
// missing file is a boot failure rather than a silent gap. The palette and the
// instrument bar bind listeners as they load and reach `HRZApp` only inside them,
// so they may sit before the app defines the facade.
const pageScripts = [...html.matchAll(/<script src="([^"]+)"[^>]*>/g)].map(
  (match) => `frontend/app/${match[1].replace(/^\/assets\//, "")}`
);
// The builder is a concept editor with a harness of its own, `check_builder.mjs`;
// the shell reaches it only from inside a handler, so it needs no slot here.
const harnessGap = new Set(["frontend/app/builder.js", "frontend/app/app.js"]);

const inject = async (file) => {
  const node = document.createElement("script");
  node.textContent = await readFile(join(root, file), "utf8");
  document.head.appendChild(node);
};

for (const file of pageScripts) {
  if (!harnessGap.has(file)) await inject(file);
}

// These checks are about the chart wall and the panels beside it. A fresh
// account has no snapshot, so without a route the shell opens on Dashboard
// and every chart pane is behind a hidden section.
window.history.replaceState(null, "", "#/charts");

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
// The toolbar's expected content is the engine's registry, read out of the page
// the same way the painter reads it. A literal list of tool names here is a
// second vocabulary: it goes stale the moment a kind lands, and a stale list
// tests nothing except that nobody changed anything.
const registry = () => (window.HRZApp && window.HRZApp.toolRegistry()) || [];
const registryKinds = () => registry().map((t) => t.kind);
const registryGroups = () => {
  const seen = [];
  for (const t of registry()) if (!seen.includes(t.group)) seen.push(t.group);
  return seen;
};

check(
  "the engine handed the shell a tool registry",
  registry().length > 0 &&
    registry().every(
      (t) =>
        typeof t.kind === "string" &&
        typeof t.label === "string" &&
        typeof t.title === "string" &&
        typeof t.group === "string" &&
        typeof t.group_label === "string" &&
        (t.anchors?.form === "fixed" ? Number.isInteger(t.anchors.points) : t.anchors?.form === "many" && Number.isInteger(t.anchors.min))
    ),
  `${registry().length} tools`
);
check(
  "the drawing toolbar is on the page",
  paneNode().querySelectorAll(".tools button[data-tool]").length === registry().length + 1,
  `${paneNode().querySelectorAll(".tools button[data-tool]").length} tools for ${registry().length} registry rows (+ cursor)`
);
check(
  "every tool the engine's registry declares has a button, and no button it does not",
  (() => {
    const toolbar = paneNode().querySelector(".tools");
    const names = [...toolbar.querySelectorAll("button[data-tool]")].map((b) => b.dataset.tool);
    const expected = ["cursor", ...registryKinds()];
    const missing = expected.filter((name) => !names.includes(name));
    const extra = names.filter((name) => !expected.includes(name));
    if (missing.length || extra.length) return false;
    return true;
  })(),
  (() => {
    const toolbar = paneNode().querySelector(".tools");
    const names = [...toolbar.querySelectorAll("button[data-tool]")].map((b) => b.dataset.tool);
    const expected = ["cursor", ...registryKinds()];
    return `missing ${expected.filter((n) => !names.includes(n)).join(",")} extra ${names.filter((n) => !expected.includes(n)).join(",")}`;
  })()
);
check(
  "the tools are grouped into the registry's flyouts",
  paneNode().querySelectorAll(".tools .toolGroup .toolFlyout button[data-tool]").length ===
    registry().length,
  `${paneNode().querySelectorAll(".tools .toolGroup .toolFlyout button[data-tool]").length} flyout tools`
);
check(
  "every registry tool is stroked in both themes",
  (() => {
    const dark = window.HRZApp.chartPaletteFor("dark");
    const light = window.HRZApp.chartPaletteFor("light");
    return registryKinds().every((kind) => dark[kind] && light[kind]);
  })(),
  (() => {
    const dark = window.HRZApp.chartPaletteFor("dark");
    const light = window.HRZApp.chartPaletteFor("light");
    return registryKinds()
      .filter((kind) => !dark[kind] || !light[kind])
      .map((kind) => `${kind}:${dark[kind] ? "" : "no-dark"}${light[kind] ? "" : " no-light"}`)
      .join(", ") || "all keyed";
  })()
);
check(
  "the magnet, AI layer, undo and redo controls are on the toolbar",
  ["[data-magnet]", "[data-ai-layer]", "[data-undo]", "[data-redo]"].every((sel) =>
    paneNode().querySelector(`.tools ${sel}`)
  )
);
check(
  "undo starts disabled and enables once a drawing is commanded",
  paneNode().querySelector(".tools [data-undo]").disabled === true,
  `disabled=${paneNode().querySelector(".tools [data-undo]")?.disabled}`
);

// The series a chart opens on comes from `GET /symbols`, and the fixture lists
// its timeframes alphabetically -- `15m, 1d, 1h, 1m, 4h, 5m` -- which is the
// order the deployment actually sent. Taking the first option therefore opened
// the chart on the *thinnest* series -- three bars -- which is the "a thin chart
// is telling the truth" case the bar count in the label was added for, arriving
// as the default instead of as a warning. The markup it replaced had
// `5m selected`, so this was a regression, and the fixture had been tidied into
// `5m, 15m, 1h` so the harness could not see it.
check(
  "the chart opens on the series with the most bars, not the first one listed",
  selectIn(PANE, "timeframe") === "5m",
  `opened on ${selectIn(PANE, "timeframe")}`
);
check(
  "and the timeframe options read as a ladder rather than in the server's order",
  [...paneNode().querySelector(".timeframe").options].map((o) => o.value).join(",") ===
    "1m,5m,15m,1h,4h,1d",
  [...paneNode().querySelector(".timeframe").options].map((o) => o.value).join(",")
);

// --- the instrument bar ------------------------------------------------------
//
// The bar is a projection of the active pane, not a second set of controls. The
// checks drive it the way a user does -- press a frame, pick a symbol -- and
// then read the *pane's* own selects back, because "the bar says 1h" and "the
// chart is on 1h" are different claims and only the second one matters.

console.log("\nthe instrument bar");

const barNode = () => document.getElementById("instrumentBar");

// The bar repaints on the next animation frame after the chart changes under it
// -- one coalesced frame, so a pane that fills its symbol, its ladder and its
// overlay in three steps paints once. jsdom does not guarantee a frame inside
// the 30ms these checks otherwise settle for, so anything that reads a value the
// *bar* painted waits for that value; a bar that never projects the chart is a
// timeout, not a skip.
await waitFor(
  () => document.getElementById("barSymbolName").textContent === "BTCUSDT",
  "the instrument bar to project the chart it opened on"
);

check(
  "the instrument bar sits at the top of the chart column",
  barNode() && barNode().parentElement.id === "chartColumn" &&
    barNode().nextElementSibling && barNode().nextElementSibling.id === "chartStage",
  barNode() ? `${barNode().parentElement.id} / ${barNode().nextElementSibling?.id}` : "missing"
);
check(
  "it names the active chart's symbol",
  document.getElementById("barSymbolName").textContent === "BTCUSDT",
  document.getElementById("barSymbolName").textContent
);
check(
  "and offers the ladder the pane actually has",
  [...document.querySelectorAll("#barFrames [data-frame]")].map((b) => b.dataset.frame).join(",") ===
    "1m,5m,15m,1h,4h",
  [...document.querySelectorAll("#barFrames [data-frame]")].map((b) => b.dataset.frame).join(",")
);
check(
  "with the pane's own frame pressed",
  document.querySelector('#barFrames [data-frame="5m"]').getAttribute("aria-pressed") === "true",
  document.querySelector('#barFrames [data-frame="5m"]').getAttribute("aria-pressed")
);
check(
  "and the chart type segmented from the pane's own select",
  [...document.querySelectorAll("#barModes [data-mode]")].map((b) => b.dataset.mode).join(",") ===
    "candles,heikin_ashi,bars,line,area,footprint",
  [...document.querySelectorAll("#barModes [data-mode]")].map((b) => b.dataset.mode).join(",")
);

document.querySelector('#barFrames [data-frame="1h"]').click();
await settle();
check(
  "pressing a frame moves the pane's own timeframe select",
  selectIn(PANE, "timeframe") === "1h",
  selectIn(PANE, "timeframe")
);
await waitFor(
  () =>
    document.querySelector('#barFrames [data-frame="1h"]').getAttribute("aria-pressed") === "true",
  "the bar to follow the pane's frame back"
);
check(
  "and the bar follows the pane back",
  document.querySelector('#barFrames [data-frame="1h"]').getAttribute("aria-pressed") === "true",
  document.querySelector('#barFrames [data-frame="1h"]').getAttribute("aria-pressed")
);

document.querySelector('#barModes [data-mode="bars"]').click();
await settle();
check(
  "pressing a chart type moves the pane's own select",
  selectIn(PANE, "mode") === "bars",
  selectIn(PANE, "mode")
);

document.getElementById("barZones").click();
await settle();
check(
  "the bar's Zones toggle drives the pane's own toggle",
  paneNode().querySelector(".zones").getAttribute("aria-pressed") === "true",
  paneNode().querySelector(".zones").getAttribute("aria-pressed")
);
document.getElementById("barZones").click();
await settle();

document.getElementById("barSymbol").click();
const searchInput = document.getElementById("symbolPopInput");
check(
  "the symbol panel opens with the caret in its field",
  document.getElementById("symbolPop").hidden === false && document.activeElement === searchInput,
  `hidden=${document.getElementById("symbolPop").hidden} focused=${document.activeElement && document.activeElement.id}`
);
searchInput.value = "eth";
searchInput.dispatchEvent(new window.Event("input", { bubbles: true }));
await waitFor(
  () => document.querySelector('#symbolPopList [data-symbol="ETHUSDT"]'),
  "the symbol search results"
);
check(
  "the search lists the matching instrument",
  document.querySelectorAll("#symbolPopList [data-symbol]").length === 1,
  `${document.querySelectorAll("#symbolPopList [data-symbol]").length} results`
);
document.querySelector('#symbolPopList [data-symbol="ETHUSDT"]').click();
await settle();
check(
  "picking it moves the active chart",
  selectIn(PANE, "symbol") === "ETHUSDT",
  selectIn(PANE, "symbol")
);
check("and the panel closes behind it", document.getElementById("symbolPop").hidden === true);
// The field the search was typed into went away with the panel, so the caret
// has to go back to the button that opened it -- otherwise the page holds focus
// on a hidden input, and the window-level shortcuts that ask "is the user
// typing?" stop working for the rest of the session.
check(
  "and picking hands the caret back to the button that opened the panel",
  document.activeElement === document.getElementById("barSymbol"),
  document.activeElement && document.activeElement.id
);

// The strip narrows by moving groups into More, never by cloning them: `#split`
// and the layout selects are the app's nodes and carry its listeners, so a copy
// would be a control that looks live and does nothing.
const columnWidth = (width) => {
  // The bar measures its *own* box, so that is what gets pinned; the column is
  // pinned too because it is the thing the user drags, and a headless DOM gives
  // both zero.
  for (const id of ["instrumentBar", "chartColumn"]) {
    Object.defineProperty(document.getElementById(id), "clientWidth", {
      value: width,
      configurable: true,
    });
  }
  window.InstrumentBar.measure();
};

columnWidth(800);
check(
  "at 800px the indicator and pane groups move into More",
  document.getElementById("moreGroups").querySelector('[data-bar-group="indicators"]') &&
    document.getElementById("moreGroups").querySelector('[data-bar-group="panes"]') &&
    document.querySelector('#instrumentBar [data-bar-group="view"]') &&
    document.querySelector('#instrumentBar [data-bar-group="instrument"]'),
  [...document.querySelectorAll("#instrumentBar > .barGroup")].map((g) => g.dataset.barGroup).join(",")
);
check(
  "and the moved group is the same node, not a copy",
  document.getElementById("moreGroups").querySelector("#split") === document.getElementById("split")
);

columnWidth(700);
check(
  "at 700px the view group joins them, and the symbol stays",
  document.getElementById("moreGroups").querySelector('[data-bar-group="view"]') &&
    document.querySelector('#instrumentBar [data-bar-group="instrument"]') &&
    document.querySelector('#instrumentBar [data-bar-group="chrome"]'),
  [...document.querySelectorAll("#instrumentBar > .barGroup")].map((g) => g.dataset.barGroup).join(",")
);

columnWidth(1400);
check(
  "widening again puts every group back in its own place",
  [...document.querySelectorAll("#instrumentBar > .barGroup")].map((g) => g.dataset.barGroup).join(",") ===
    "instrument,view,indicators,panes,chrome",
  [...document.querySelectorAll("#instrumentBar > .barGroup")].map((g) => g.dataset.barGroup).join(",")
);
check("and More stops offering moved controls", document.getElementById("moreSection").hidden === true);

// Back to the state the rest of the file expects: the checks after this one
// assume the first chart on 5m, candles, zones off.
document.querySelector('#barFrames [data-frame="5m"]').click();
await settle();
document.querySelector('#barModes [data-mode="candles"]').click();
await settle();
check(
  "and the bar restored the chart the later checks assume",
  selectIn(PANE, "timeframe") === "5m" && selectIn(PANE, "mode") === "candles",
  `${selectIn(PANE, "timeframe")} ${selectIn(PANE, "mode")}`
);

// The symbol check above moved the active pane to ETHUSDT, and the live-feed,
// drawing and book checks after this point are written against BTCUSDT -- so the
// block leaves the chart on the instrument it was found on, through the same
// panel a user would.
document.getElementById("barSymbol").click();
searchInput.value = "btc";
searchInput.dispatchEvent(new window.Event("input", { bubbles: true }));
await waitFor(
  () => document.querySelector('#symbolPopList [data-symbol="BTCUSDT"]'),
  "the symbol search to find BTCUSDT again"
);
document.querySelector('#symbolPopList [data-symbol="BTCUSDT"]').click();
await settle();
check(
  "and the bar leaves the chart on the symbol the rest of the file assumes",
  selectIn(PANE, "symbol") === "BTCUSDT",
  selectIn(PANE, "symbol")
);

// --- the watchlist and the inspector -----------------------------------------
//
// Both are projections of the active chart. The checks drive the real controls
// and read the pane back, for the same reason the bar checks do.

console.log("\nthe watchlist rail");

const watchRows = () => [...document.querySelectorAll("#wlRows .watchRow")];
check(
  "the rail lists the venue, priced",
  watchRows().length === 2 &&
    document.getElementById("wlCount").textContent.includes("whole venue"),
  `${watchRows().length} rows · ${document.getElementById("wlCount").textContent}`
);
check(
  "a rising row is tinted up and a falling row down",
  watchRows()[0].dataset.dir === "up" && watchRows()[1].dataset.dir === "down",
  watchRows().map((row) => row.dataset.dir).join(",")
);
check(
  "and the source line says how old the prices are",
  /^prices \d+s ago$/.test(document.getElementById("wlSource").textContent),
  document.getElementById("wlSource").textContent
);

check(
  "the Library starts open",
  document.getElementById("libToggle").getAttribute("aria-expanded") === "true" &&
    document.getElementById("libBody").hidden === false,
  `expanded=${document.getElementById("libToggle").getAttribute("aria-expanded")}`
);
document.getElementById("libToggle").click();
check(
  "collapsing it keeps the header and the count",
  document.getElementById("libBody").hidden === true &&
    document.getElementById("libToggle").getAttribute("aria-expanded") === "false" &&
    document.getElementById("libToggle").textContent.includes("Library"),
  `hidden=${document.getElementById("libBody").hidden}`
);
document.getElementById("libToggle").click();

// Alt-click is "beside, not instead": the chart being read stays put.
const panesBefore = paneNodes().length;
const ethRow = watchRows().find(
  (row) => row.querySelector(".wlSym").textContent === "ETHUSDT"
);
ethRow.querySelector(".watchRowMain").dispatchEvent(
  new window.MouseEvent("click", { bubbles: true, altKey: true })
);
await waitFor(() => paneNodes().length === panesBefore + 1, "the pane Alt-click opens");
check(
  "Alt-click on a row opens that symbol in a new pane",
  selectIn(paneNodes().length - 1, "symbol") === "ETHUSDT",
  selectIn(paneNodes().length - 1, "symbol")
);
check(
  "and the new pane is the active one",
  paneNodes()[paneNodes().length - 1].classList.contains("active")
);
paneNodes()[paneNodes().length - 1].querySelector(".close").click();
await settle();
check(
  "closing it returns to the first chart",
  paneNodes().length === panesBefore && paneNodes()[0].classList.contains("active"),
  `${paneNodes().length} panes`
);

console.log("\nthe inspector projections");

document.querySelector('#inspectorTabs [data-inspector="indicator"]').click();
await settle();
check(
  "the Indicator tab opens and says nothing is attached",
  document.getElementById("inspectorIndicator").hidden === false &&
    document.getElementById("indicatorProjectionEmpty").hidden === false &&
    document.getElementById("indicatorProjection").hidden === true,
  `panel=${document.getElementById("inspectorIndicator").hidden} empty=${document.getElementById("indicatorProjectionEmpty").hidden}`
);

const attached = await window.HRZApp.attachWorkspace("ws-1");
await settle();
check(
  "attaching projects the indicator into the tab",
  document.getElementById("indicatorProjection").hidden === false &&
    document.getElementById("indicatorName").textContent === "Liquidity Sweep" &&
    document.getElementById("indicatorConcepts").textContent === "1" &&
    document.getElementById("indicatorZones").textContent === "2" &&
    document.getElementById("indicatorMarkers").textContent === "1" &&
    document.getElementById("barIndicator").hidden === false,
  `${attached} · ${document.getElementById("indicatorName").textContent} / ${document.getElementById("indicatorZones").textContent} zones`
);

document.getElementById("indicatorDetach").click();
await settle();
check(
  "Detach removes it from the pane, the bar and the tab together",
  document.getElementById("indicatorProjection").hidden === true &&
    document.getElementById("indicatorProjectionEmpty").hidden === false &&
    document.getElementById("barIndicator").hidden === true &&
    paneNode().querySelector(".indicatorChip").hidden === true,
  `bar hidden=${document.getElementById("barIndicator").hidden} projection hidden=${document.getElementById("indicatorProjection").hidden}`
);

document.querySelector('#inspectorTabs [data-inspector="details"]').click();
await settle();
check(
  "the Details tab states the chart, its bars and its account",
  document.getElementById("dfChart").textContent === "BTCUSDT · 5m" &&
    document.getElementById("dfBars").textContent === "500 bars" &&
    document.getElementById("dfAccount").textContent === "harness@example.test",
  `${document.getElementById("dfChart").textContent} · ${document.getElementById("dfBars").textContent} · ${document.getElementById("dfAccount").textContent}`
);

// --- the dock ----------------------------------------------------------------
//
// The tab bar is the dock when it is shut, which is what makes the Console's
// unread dot worth having. Every check below drives a real control and reads
// the panel back; none of them calls the paint functions directly.

console.log("\nthe dock");

const dockNode = () => document.getElementById("dock");
const dockTab = (name) => document.querySelector(`#dock [data-dock="${name}"]`);
// The dot is a count, not a flag, and the session already logged lines before
// this section -- so the check reads the number the tab carries rather than
// assuming this warning is the first thing ever logged.
const consoleBadge = () => {
  const match = /(\d+) new/.exec(dockTab("console").getAttribute("aria-label") || "");
  return match ? Number(match[1]) : 0;
};

check(
  "the dock's tab bar is on screen on Charts, with its body shut",
  dockNode().hidden === false && dockNode().dataset.open === "false" &&
    document.getElementById("dockBody").hidden === true,
  `hidden=${dockNode().hidden} open=${dockNode().dataset.open} body=${document.getElementById("dockBody").hidden}`
);

const badgeBefore = consoleBadge();
window.HRZLog.warn("a warning from the harness");
check(
  "a line logged while the console is shut marks its tab",
  dockTab("console").dataset.unread === "true" && consoleBadge() === badgeBefore + 1,
  `${dockTab("console").getAttribute("aria-label")} (was ${badgeBefore})`
);

dockTab("alerts").click();
await waitFor(
  () => document.querySelectorAll("#dockMonitorRows .dockRow").length === 2,
  "the monitors load into the dock"
);
check(
  "the Alerts tab opens the dock and lists the server's monitors",
  dockNode().dataset.open === "true" &&
    document.getElementById("dockAlerts").hidden === false &&
    document.getElementById("dockMonitorCount").textContent === "2" &&
    document.querySelectorAll("#dockEventRows .dockRow").length === 1,
  `${document.querySelectorAll("#dockMonitorRows .dockRow").length} monitors · ${document.querySelectorAll("#dockEventRows .dockRow").length} events`
);

document.querySelector('#dockMonitorRows [data-monitor-toggle="mon-1"]').click();
await waitFor(
  () => document.querySelector('#dockMonitorRows [data-monitor-toggle="mon-1"]').textContent.trim() === "Resume",
  "the pause reaches the row"
);
check(
  "Pause sends the PATCH and the row follows the server's answer",
  backend.calls.some((call) => call.method === "PATCH" && call.url === "/watch-monitors/mon-1") &&
    document.querySelector('#dockMonitorRows [data-monitor-toggle="mon-1"]').dataset.enabled === "false"
);

document.querySelector('#dockMonitorRows [data-monitor-delete="mon-2"]').click();
await waitFor(
  () => document.querySelectorAll("#dockMonitorRows .dockRow").length === 1,
  "the deleted monitor leaves the list"
);
check(
  "Delete sends the DELETE and the deleted scope leaves the list",
  backend.calls.some((call) => call.method === "DELETE" && call.url === "/watch-monitors/mon-2")
);

// The ledger is localStorage, so seeding it is the same act as opening a
// position -- except that no toast fires and no mark is needed for the row.
window.localStorage.setItem("paper.positions.v1", JSON.stringify([
  { id: "p1", symbol: "BTCUSDT", side: "buy", size: 1, entry: 100, leverage: 5, liq: 82, at: 1 },
]));
window.localStorage.setItem("paper.orders.v1", JSON.stringify([
  { symbol: "ETHUSDT", side: "sell", size: 2, limit: 1000000, leverage: 10, at: 2 },
]));
dockTab("orders").click();
await settle();
check(
  "the Orders tab reads the practice ledger back",
  document.getElementById("dockOrders").hidden === false &&
    document.querySelectorAll("#dockPosRows .dockRow").length === 1 &&
    document.querySelectorAll("#dockOrdRows .dockRow").length === 1 &&
    document.getElementById("dockPosCount").textContent.includes("1"),
  `${document.querySelectorAll("#dockPosRows .dockRow").length} positions · ${document.querySelectorAll("#dockOrdRows .dockRow").length} orders`
);

document.querySelector("#dockPosRows [data-close-pos]").click();
await settle();
check(
  "Close on a dock row closes the same position the ticket owns",
  document.querySelectorAll("#dockPosRows .dockRow").length === 0 &&
    document.getElementById("dockPosEmpty").hidden === false &&
    JSON.parse(window.localStorage.getItem("paper.positions.v1") || "[]").length === 0
);

document.querySelector("#dockOrdRows [data-cancel-order]").click();
await settle();
check(
  "Cancel removes the resting order",
  document.querySelectorAll("#dockOrdRows .dockRow").length === 0 &&
    JSON.parse(window.localStorage.getItem("paper.orders.v1") || "[]").length === 0
);

dockTab("console").click();
await waitFor(
  () => document.querySelectorAll("#consoleRows .consoleRow").length >= 2,
  "the console prints what was logged"
);
// The lines in between come from the dock's own actions -- closing a position
// toasts, and a toast is a log line. So the check reads the ring the page
// actually holds: the console prints all of it, in order, and the harness's
// warning is still somewhere in the middle of it.
const logged = window.HRZLog.entries();
check(
  "the Console prints the session's log, newest last, and clears the unread mark",
  dockTab("console").dataset.unread === "false" &&
    document.getElementById("dockConsole").hidden === false &&
    document.querySelectorAll("#consoleRows .consoleRow").length === logged.length &&
    document.getElementById("consoleRows").lastElementChild.textContent.includes(
      logged[logged.length - 1].message
    ) &&
    [...document.querySelectorAll("#consoleRows .consoleMsg")].some(
      (node) => node.textContent === "a warning from the harness"
    ),
  `${document.querySelectorAll("#consoleRows .consoleRow").length} lines / ${logged.length} logged`
);

document.getElementById("consoleClear").click();
await settle();
check(
  "Clear empties the console and the empty state takes over",
  window.HRZLog.entries().length === 0 &&
    document.querySelectorAll("#consoleRows .consoleRow").length === 0 &&
    document.getElementById("consoleEmpty").hidden === false
);

for (let line = 0; line < 205; line += 1) window.HRZLog.info(`line ${line}`);
check(
  "the ring keeps the newest 200 lines and drops the oldest",
  window.HRZLog.entries().length === 200 &&
    window.HRZLog.entries()[199].message === "line 204"
);

document.getElementById("dockCollapse").click();
await settle();
check(
  "the chevron shuts the body and leaves the tab bar",
  dockNode().dataset.open === "false" &&
    document.getElementById("dockBody").hidden === true &&
    dockNode().hidden === false
);

window.HRZLog.warn("logged while the dock is shut");
check(
  "a line logged after the dock shuts marks the Console tab",
  dockTab("console").dataset.unread === "true"
);

if (window.Shell) window.Shell.go("dashboard");
await settle();
check(
  "leaving Charts takes the dock off screen",
  dockNode().hidden === true
);

if (window.Shell) window.Shell.go("charts");
await settle();
check(
  "returning restores the tab bar, still shut",
  dockNode().hidden === false && dockNode().dataset.open === "false"
);

// --- the chart's own chrome --------------------------------------------------
//
// The rail, the theme bridge and the two settings surfaces. The palette checks
// read `HRZApp.chartPalette()` -- the same function the panes are painted from
// -- rather than a canvas pixel, because the harness's 2D context records calls
// and not colours.

console.log("\nthe chart rail, theme and settings");

const paletteNow = () =>
  window.HRZApp && window.HRZApp.chartPalette ? window.HRZApp.chartPalette() : {};
const railNode = () => document.getElementById("globalTools");

check(
  "the rail and the wall share a row inside the chart column",
  railNode().parentElement.id === "chartStage" &&
    document.getElementById("charts").parentElement.id === "chartStage" &&
    document.getElementById("chartStage").parentElement.id === "chartColumn",
  `${railNode().parentElement.id} / ${document.getElementById("chartStage").parentElement.id}`
);
check(
  "the rail is one button per tool group, each named but none lettered",
  document.querySelectorAll("#globalTools .toolGroupTrigger").length === registryGroups().length &&
    [...document.querySelectorAll("#globalTools .toolGroupTrigger")].map((b) => b.dataset.group).join(
      ","
    ) === registryGroups().join(",") &&
    [...document.querySelectorAll("#globalTools .toolGroupTrigger")].every(
      (b) => b.getAttribute("aria-label") && b.textContent.trim() === ""
    ) &&
    document.querySelector("#globalTools button[data-tool='cursor']").textContent.trim() === "" &&
    document.querySelector("#globalTools button[data-tool='cursor']").getAttribute("aria-label") === "Cursor",
  [...document.querySelectorAll("#globalTools .toolGroupTrigger")].map((b) => b.getAttribute("aria-label")).join(",")
);
check(
  "and the rail names the chart it will draw on",
  document.getElementById("globalToolsWhich").textContent.includes("BTCUSDT") &&
    railNode().title === document.getElementById("globalToolsWhich").textContent &&
    railNode().getAttribute("aria-label").includes("BTCUSDT"),
  railNode().title
);
check(
  "a group mark still opens its flyout, beside the rail rather than below it",
  (() => {
    const trigger = document.querySelector("#globalTools .toolGroupTrigger");
    const flyout = trigger.parentElement.querySelector(".toolFlyout");
    trigger.click();
    const opened = flyout.hidden === false;
    trigger.click();
    return opened && flyout.hidden === true;
  })()
);

const darkUp = paletteNow().up;
window.Theme.set("light");
await settle();
check(
  "the chart palette follows the theme instead of staying dark on a white page",
  document.documentElement.dataset.theme === "light" &&
    paletteNow().up === "#067a4d" &&
    paletteNow().text === "#5f6b7a" &&
    paletteNow().grid === "#e7ecf2",
  `${document.documentElement.dataset.theme} · ${paletteNow().up} · ${paletteNow().grid}`
);
check("and the dark palette is the one the chart shipped with", darkUp === "#2ebd85", darkUp);

document.getElementById("moreChartSettings").click();
await settle();
check(
  "Chart settings opens as a dialog from the More panel",
  document.getElementById("chartSettings").open === true,
  `open=${document.getElementById("chartSettings").open}`
);

document.querySelector('#chartSettings [data-cvd="true"]').click();
await settle();
check(
  "Blue / orange swaps the direction colours, and both surfaces know",
  paletteNow().up === "#2962ff" &&
    paletteNow().down === "#ff6d00" &&
    paletteNow().entry === "#2563eb" &&
    JSON.parse(window.localStorage.getItem("hrz.chart")).cvd === true &&
    document.querySelector('#chartSettings [data-cvd="true"]').getAttribute("aria-pressed") === "true" &&
    document.getElementById("moreCvd").getAttribute("aria-pressed") === "true",
  `${paletteNow().up} / ${paletteNow().down} · entry ${paletteNow().entry}`
);

const gridToggle = document.getElementById("csGrid");
gridToggle.checked = false;
gridToggle.dispatchEvent(new window.Event("change"));
await settle();
check(
  "unchecking Grid lines takes the grid out of the palette",
  paletteNow().grid === "rgba(0, 0, 0, 0)" &&
    JSON.parse(window.localStorage.getItem("hrz.chart")).grid === false,
  paletteNow().grid
);

document.getElementById("csReset").click();
await settle();
check(
  "Reset forgets both choices and both come back",
  paletteNow().grid !== "rgba(0, 0, 0, 0)" &&
    paletteNow().up === "#067a4d" &&
    window.localStorage.getItem("hrz.chart") === null &&
    document.getElementById("csGrid").checked === true
);
document.getElementById("chartSettings").open = false;
window.Theme.set("dark");
await settle();
check("going back to dark restores the dark palette", paletteNow().up === "#2ebd85", paletteNow().up);

check(
  "the gesture coach is on screen until it is dismissed",
  document.getElementById("chartCoach").hidden === false
);
document.getElementById("coachAgree").click();
await settle();
check(
  "Got it hides the coach and remembers the choice",
  document.getElementById("chartCoach").hidden === true &&
    window.localStorage.getItem("hrz.coach.charts") === "1"
);
if (window.Shell) window.Shell.go("dashboard");
await settle();
if (window.Shell) window.Shell.go("charts");
await settle();
check(
  "and it does not come back on the next visit",
  document.getElementById("chartCoach").hidden === true
);

check(
  "the shortcut sheet documents the gestures the coach names",
  document.querySelectorAll("#shortcuts .shortcutGroup").length === 3 &&
    document.getElementById("shortcutGestures") !== null,
  `${document.querySelectorAll("#shortcuts .shortcutGroup").length} groups`
);

// --- the dashboard's structure ------------------------------------------------
//
// Task 16 fills these panels with data. These checks are the ones that survive
// both halves of the task: the structure a reader navigates, and the fact that
// the placeholder it replaced is gone.

console.log("\nthe dashboard");

if (window.Shell) window.Shell.go("dashboard");
await settle();

check(
  "Dashboard is a page of its own, not a placeholder",
  document.querySelector("#dest-dashboard .dashInner") !== null &&
    document.getElementById("dest-dashboard").hidden === false &&
    document.getElementById("dest-dashboard").textContent.includes("Markets") &&
    !document.getElementById("dest-dashboard").textContent.includes("land in M5"),
  document.getElementById("dest-dashboard").textContent.slice(0, 60).trim()
);
check(
  "four pulse cards, each named for the question it answers",
  [...document.querySelectorAll("#pulseRow .pulseCard")].map((card) => card.querySelector("h2").textContent).join(",") ===
    "Market,Watchlist,Automation,Plan",
  [...document.querySelectorAll("#pulseRow .pulseCard h2")].map((head) => head.textContent).join(",")
);
check(
  "the markets table offers four lists and four sortable columns",
  document.querySelectorAll("#marketTabs [role='tab']").length === 4 &&
    document.querySelectorAll(".marketTable thead th[data-sort]").length === 4 &&
    document.querySelector(".marketTable th[data-sort='volume']").getAttribute("aria-sort") === "descending" &&
    document.getElementById("marketRows") !== null &&
    document.getElementById("marketMore") !== null,
  `${document.querySelectorAll("#marketTabs [role='tab']").length} tabs · ${document.querySelectorAll(".marketTable thead th[data-sort]").length} columns`
);
check(
  "the watchlist adds if asked and the heatmap has both views",
  document.getElementById("dashWatchAdd").getAttribute("aria-expanded") === "false" &&
    document.getElementById("dashWatchSearch").hidden === true &&
    document.querySelector("#dest-dashboard a[href='#/charts']") !== null &&
    document.getElementById("heatTableToggle").getAttribute("aria-pressed") === "false" &&
    document.getElementById("heatTableWrap").hidden === true &&
    document.getElementById("heatGrid").hidden === false,
  `add=${document.getElementById("dashWatchAdd").getAttribute("aria-expanded")} table=${document.getElementById("heatTableWrap").hidden}`
);

// --- the dashboard's data ----------------------------------------------------
//
// One clock, four surfaces. Every check drives a control or reads a pane back;
// none of them calls a paint function directly.

// The empty-state checks below need a known starting list, and the rail's
// section above leaves whatever it starred behind. The block adds and removes a
// favorite of its own, so it ends where it began.
window.localStorage.setItem("watchlist.favorites", "[]");

if (window.Shell) window.Shell.go("dashboard");
await waitFor(
  () => document.querySelectorAll("#marketRows tr[data-symbol]").length > 0,
  "the markets table to fill"
);

const marketRows = () => [...document.querySelectorAll("#marketRows tr[data-symbol]")];
const marketSymbols = () => marketRows().map((row) => row.dataset.symbol);

check(
  "the page leads with its date and a market line",
  document.getElementById("dashDate").textContent.length > 0 &&
    document.getElementById("dashMarket").textContent.includes("BTCUSDT") &&
    document.getElementById("dashMarket").textContent.includes("67,412.5"),
  document.getElementById("dashMarket").textContent
);
check(
  "the market pulse reads BTC and draws the 24-hour line",
  document.getElementById("pulseMarket").textContent.includes("67,412.5") &&
    document.querySelectorAll("#pulseMarket .hrz-price--up").length === 1 &&
    document.querySelector("#pulseMarket svg.pulseSpark polyline") !== null,
  document.getElementById("pulseMarket").textContent.trim()
);
check(
  "with no favorites the watchlist pulse and panel both say so",
  document.getElementById("pulseWatch").textContent.includes("No favorites") &&
    document.getElementById("dashWatchEmpty").hidden === false &&
    document.querySelectorAll("#dashWatchRows .dashWatchRow").length === 0
);
check(
  "the automation pulse counts every status and ages the last signal",
  document.getElementById("pulseAuto").textContent.includes("2 running · 1 paused · 1 stopped") &&
    /last signal .+ ago/.test(document.getElementById("pulseAuto").textContent),
  document.getElementById("pulseAuto").textContent.trim()
);
// The harness user is on Desk and Desk meters both quotas, so the card names
// the plan and shows the credits the fixture reports -- 120 of 20,000.
await waitFor(
  () => document.querySelector("#pulsePlan [role='progressbar']") !== null,
  "the plan card to meter"
);
check(
  "the plan pulse names the plan and meters the credits",
  document.getElementById("pulsePlan").textContent.includes("Desk") &&
    document.getElementById("pulsePlan").querySelector('[role="progressbar"]').getAttribute("aria-valuenow") === "120" &&
    document.getElementById("pulsePlan").textContent.includes("Backtests"),
  document.getElementById("pulsePlan").textContent.trim()
);

check(
  "the table shows the first fifty of seventy, sorted by volume",
  marketRows().length === 50 &&
    document.getElementById("marketCount").textContent === "50 of 70" &&
    document.getElementById("marketMore").hidden === false &&
    marketSymbols()[0] === "BTCUSDT",
  `${marketRows().length} rows · ${document.getElementById("marketCount").textContent}`
);
document.getElementById("marketMore").click();
await settle();
check(
  "Show 50 more appends the rest and goes away",
  marketRows().length === 70 && document.getElementById("marketMore").hidden === true,
  `${marketRows().length} rows`
);

document.querySelector("#marketTabs [data-market='majors']").click();
await settle();
check(
  "Majors is the ten liquid pairs",
  marketRows().length === 10 && marketSymbols().includes("DOGEUSDT") && !marketSymbols().includes("ALT00USDT"),
  `${marketRows().length} rows`
);
document.querySelector("#marketTabs [data-market='alts']").click();
await settle();
check(
  "Alts is everything else, paged at fifty",
  marketRows().length === 50 &&
    document.getElementById("marketCount").textContent === "50 of 60" &&
    !marketSymbols().includes("BTCUSDT"),
  `${marketRows().length} rows · ${document.getElementById("marketCount").textContent}`
);
document.querySelector("#marketTabs [data-market='all']").click();
await settle();

const lastHead = document.querySelector(".marketTable th[data-sort='last']");
lastHead.querySelector(".hrz-table__sort").click();
await settle();
check(
  "sorting by last starts biggest-first and says so",
  lastHead.getAttribute("aria-sort") === "descending" && marketSymbols()[0] === "BTCUSDT",
  `${lastHead.getAttribute("aria-sort")} · ${marketSymbols()[0]}`
);
lastHead.querySelector(".hrz-table__sort").click();
await settle();
check(
  "a second press flips the column",
  lastHead.getAttribute("aria-sort") === "ascending" && marketSymbols()[0] === "DOGEUSDT",
  `${lastHead.getAttribute("aria-sort")} · ${marketSymbols()[0]}`
);
document.querySelector(".marketTable th[data-sort='volume'] .hrz-table__sort").click();
await settle();

const filter = document.getElementById("marketFilter");
filter.value = "btc";
filter.dispatchEvent(new window.Event("input"));
await settle();
check(
  "the filter narrows before the page does",
  marketRows().length === 1 && marketSymbols()[0] === "BTCUSDT" &&
    document.getElementById("marketCount").textContent === "1 instrument",
  `${marketRows().length} rows · ${document.getElementById("marketCount").textContent}`
);
filter.value = "";
filter.dispatchEvent(new window.Event("input"));
await settle();

// --- the watchlist panel -----------------------------------------------------
//
// The panel writes the same key the Charts rail reads, so the check that
// matters is the one that leaves through the panel and reads the rail.

document.getElementById("dashWatchAdd").click();
check(
  "the add control opens a search and says so",
  document.getElementById("dashWatchSearch").hidden === false &&
    document.getElementById("dashWatchAdd").getAttribute("aria-expanded") === "true"
);
const addInput = document.getElementById("dashWatchInput");
addInput.value = "ETH";
addInput.dispatchEvent(new window.Event("input"));
await waitFor(
  () => document.querySelectorAll("#dashWatchResults [data-symbol]").length > 0,
  "the symbol search to answer"
);
document.querySelector("#dashWatchResults [data-symbol='ETHUSDT']").click();
await settle();
check(
  "adding a symbol pins it, prices it and marks it in the results",
  document.querySelectorAll("#dashWatchRows .dashWatchRow").length === 1 &&
    document.getElementById("dashWatchRows").textContent.includes("ETH") &&
    document.getElementById("dashWatchEmpty").hidden === true &&
    JSON.parse(window.localStorage.getItem("watchlist.favorites")).includes("ETHUSDT") &&
    document.querySelector("#dashWatchResults [data-symbol='ETHUSDT'] .muted").textContent === "★",
  document.getElementById("dashWatchRows").textContent.trim()
);
document.getElementById("dashWatchAdd").click();
check(
  "and Done puts the search away",
  document.getElementById("dashWatchSearch").hidden === true &&
    document.getElementById("dashWatchAdd").getAttribute("aria-expanded") === "false"
);
document.querySelector("#dashWatchRows [data-star]").click();
await settle();
check(
  "the row's star removes it from both lists",
  document.querySelectorAll("#dashWatchRows .dashWatchRow").length === 0 &&
    document.getElementById("dashWatchEmpty").hidden === false &&
    JSON.parse(window.localStorage.getItem("watchlist.favorites")).length === 0 &&
    !document.querySelector("#wlRows .watchRow[data-symbol='ETHUSDT']"),
  document.getElementById("dashWatchRows").textContent.trim()
);

// --- the heatmap -------------------------------------------------------------

const tiles = () => [...document.querySelectorAll("#heatGrid .heatTile")];
check(
  "the map is the top sixty, sized by volume and coloured by change",
  tiles().length === 60 &&
    tiles()[0].dataset.size === "4" &&
    tiles()[59].dataset.size === "1" &&
    document.querySelector("#heatGrid .heatTile[data-bucket='3']") !== null &&
    document.querySelector("#heatGrid .heatTile[data-bucket='-3']") !== null &&
    document.querySelector("#heatGrid .heatTile[data-bucket='0']") !== null,
  `${tiles().length} tiles · ${tiles()[0].dataset.size}/${tiles()[59].dataset.size}`
);
check(
  "a tile names itself for a screen reader",
  tiles()[0].getAttribute("aria-label") === "BTC, up 2.5 percent",
  tiles()[0].getAttribute("aria-label")
);
document.getElementById("heatTableToggle").click();
await settle();
check(
  "Show as table swaps in the same sixty rows and remembers the choice",
  document.getElementById("heatGrid").hidden === true &&
    document.getElementById("heatTableWrap").hidden === false &&
    document.querySelectorAll("#heatRows tr").length === 60 &&
    window.localStorage.getItem("hrz.heat.table") === "true",
  `${document.querySelectorAll("#heatRows tr").length} rows`
);
document.getElementById("heatTableToggle").click();
await settle();
check(
  "and back to the map",
  document.getElementById("heatGrid").hidden === false &&
    document.getElementById("heatTableWrap").hidden === true &&
    window.localStorage.getItem("hrz.heat.table") === "false"
);

// --- the clock ---------------------------------------------------------------
//
// The dashboard's tick is live here, and the Charts rail's is not: one clock,
// not two. Leaving hands the wall its own clock back.

check(
  "the dashboard's clock is the only one running",
  clocksNamed("dashTick") === 1 && clocksNamed("fetchWatchlistTickers") === 0,
  `dash=${clocksNamed("dashTick")} rail=${clocksNamed("fetchWatchlistTickers")}`
);
document.querySelector("#heatGrid .heatTile[data-symbol='SOLUSDT']").click();
await waitFor(
  () => document.getElementById("dest-charts").hidden === false,
  "the heatmap tile to open Charts"
);
await settle();
check(
  "a heat tile opens that symbol in Charts",
  selectIn(0, "symbol") === "SOLUSDT",
  selectIn(0, "symbol")
);
check(
  "and leaving stops the dashboard's clock and starts the rail's",
  clocksNamed("dashTick") === 0 && clocksNamed("fetchWatchlistTickers") === 1,
  `dash=${clocksNamed("dashTick")} rail=${clocksNamed("fetchWatchlistTickers")}`
);

// --- the accessibility contract ----------------------------------------------
//
// The parts of Spec 10 that are a property of the DOM: the skip link, named
// landmarks, one heading per destination, one Tab stop per strip, a region that
// speaks the series and not the tick, and a canvas that names itself. Contrast,
// 200% zoom, reduced motion and a real screen reader are the manual pass in
// Step 6 -- a renderer is what they need, and jsdom is not one.

console.log("\nthe accessibility contract");

check(
  "a skip link is the first thing Tab reaches, and its target can take focus",
  document.body.firstElementChild.classList.contains("skipLink") &&
    document.body.firstElementChild.getAttribute("href") === "#mainView" &&
    document.getElementById("mainView").tabIndex === -1,
  document.body.firstElementChild.className
);
// The skip link is a fragment, and a fragment is not a route. Taken as one it
// would throw the keyboard user off the chart they asked to skip into.
window.location.hash = "#mainView";
await settle();
check(
  "and taking it does not move the user off the destination they were on",
  window.location.hash === "#/charts" && document.getElementById("dest-charts").hidden === false,
  `${window.location.hash} hidden=${document.getElementById("dest-charts").hidden}`
);
check(
  "the landmarks are named: main, views, panels, inspector, status",
  document.getElementById("mainView").tagName === "MAIN" &&
    document.getElementById("topNav").getAttribute("aria-label") === "Views" &&
    document.getElementById("panelNav").getAttribute("aria-label") === "Panels" &&
    document.getElementById("inspector").getAttribute("aria-label") === "Inspector" &&
    document.getElementById("statusbar").tagName === "FOOTER"
);
// Two reads in a row say the same thing, so nothing may be written: a
// `role="status"` node announces every text change, and the rail re-reads the
// status every few seconds -- a write per read would read the connection aloud
// forever. The observer is the only way to see a write that changes nothing.
// The first read runs unobserved, so a stale label left by an earlier tick is
// written here and is not mistaken for a re-write below.
window.Shell.refreshStatus();
const a11yWrites = [];
const a11yWatch = new window.MutationObserver((records) => a11yWrites.push(...records));
for (const id of ["sbConnLabel", "sbSave"]) {
  a11yWatch.observe(document.getElementById(id), { childList: true, characterData: true, subtree: true });
}
window.Shell.refreshStatus();
window.Shell.refreshStatus();
await settle();
a11yWatch.disconnect();
check(
  "connection and save state announce, and are written only when they change",
  a11yWrites.length === 0 &&
    document.getElementById("sbConnLabel").getAttribute("role") === "status" &&
    document.getElementById("sbSave").getAttribute("role") === "status",
  `${a11yWrites.length} write(s)`
);
const a11yDestinations = [...document.querySelectorAll("#mainView .destination")];
check(
  "every view has exactly one h1",
  a11yDestinations.length === 3 &&
    a11yDestinations.every((section) => section.querySelectorAll("h1").length === 1),
  a11yDestinations
    .map((section) => `${section.id}:${section.querySelectorAll("h1").length}`)
    .join(" ")
);
// A panel floats over a page that already owns the heading, so it cannot claim a
// second h1 -- it is a section of the screen the user is on, titled by its own h2.
check(
  "the panel is titled by an h2 and carries no h1",
  document.querySelectorAll("#workPanel h1").length === 0 &&
    document.querySelectorAll("#workPanel .hrz-dialog__head h2").length === 1,
  `${document.querySelectorAll("#workPanel h1").length} h1 · ${document.querySelectorAll("#workPanel .hrz-dialog__head h2").length} h2`
);
const a11yPositive = [...document.querySelectorAll("[tabindex]")].filter(
  (node) => Number(node.getAttribute("tabindex")) > 0
);
check(
  "nothing asks for a positive tabindex",
  a11yPositive.length === 0,
  a11yPositive.map((node) => node.id || node.className).join(" ")
);

const a11yStrip = document.getElementById("inspectorTabs");
const a11yStops = () => [...a11yStrip.querySelectorAll('[role="tab"]')].filter((tab) => tab.tabIndex === 0);
const a11yChosen = a11yStrip.querySelector('[aria-selected="true"]');
check(
  "a strip is one Tab stop, and it is the selected tab",
  a11yStops().length === 1 && a11yStops()[0] === a11yChosen,
  `${a11yStops().length} stop(s)`
);
const a11yTabsBefore = [...a11yStrip.querySelectorAll('[role="tab"]')];
const a11yAt = a11yTabsBefore.indexOf(a11yChosen);
a11yChosen.focus();
a11yChosen.dispatchEvent(new window.KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
await settle();
const a11yTabsAfter = [...a11yStrip.querySelectorAll('[role="tab"]')];
const a11yNext = a11yTabsAfter[(a11yAt + 1) % a11yTabsAfter.length];
check(
  "the right arrow moves selection, focus and the Tab stop together",
  a11yNext.getAttribute("aria-selected") === "true" &&
    document.activeElement === a11yNext &&
    a11yNext.tabIndex === 0 &&
    a11yChosen.tabIndex === -1,
  `${a11yNext.dataset.inspector} selected=${a11yNext.getAttribute("aria-selected")} focus=${document.activeElement === a11yNext}`
);
a11yChosen.click();
await settle();
check(
  "and clicking the first tab puts the selection back",
  a11yChosen.getAttribute("aria-selected") === "true" && a11yChosen.tabIndex === 0
);
check(
  "the dashboard's strip is one Tab stop too",
  [...document.querySelectorAll("#marketTabs [role='tab']")].filter((tab) => tab.tabIndex === 0).length === 1,
  `${[...document.querySelectorAll("#marketTabs [role='tab']")].filter((tab) => tab.tabIndex === 0).length} stop(s)`
);

// The canvas check comes first: it reads the series the page opened with, not
// the one the next check asks for.
const a11yCanvas = canvasOf(0);
check(
  "the chart canvas names its own series for a reader",
  a11yCanvas.getAttribute("role") === "img" &&
    (a11yCanvas.getAttribute("aria-label") || "").startsWith(selectIn(0, "symbol")),
  a11yCanvas.getAttribute("aria-label")
);

const a11yAnnounce = document.getElementById("chartAnnounce");
check(
  "the chart's live region is polite",
  a11yAnnounce.getAttribute("role") === "status" &&
    a11yAnnounce.getAttribute("aria-live") === "polite",
  `${a11yAnnounce.getAttribute("role")} ${a11yAnnounce.getAttribute("aria-live")}`
);
// Change the series through the pane's own control, the way a person does.
// The options come from that pane's select, so this works whatever series the
// heat tile left open, and `/candles` synthesises a window for any symbol. By
// this point in the file the page has been alive long enough that the region
// already holds a series, so the check is "a change speaks, a tick does not",
// not "it starts empty".
const a11ySymbol = paneNode(0).querySelector(".symbol");
const a11ySymbolWas = a11ySymbol.value;
a11ySymbol.value = [...a11ySymbol.options]
  .map((option) => option.value)
  .find((value) => value !== a11ySymbolWas);
a11ySymbol.dispatchEvent(new window.Event("change", { bubbles: true }));
await waitFor(
  () => a11yAnnounce.textContent.includes(a11ySymbol.value),
  "the live region to name the series the user asked for"
);
check(
  "and it names the series that was chosen, with no price",
  a11yAnnounce.textContent.startsWith("Chart on ") &&
    !/\d+\.\d{2}/.test(a11yAnnounce.textContent),
  a11yAnnounce.textContent
);
const a11ySpoken = a11yAnnounce.textContent;
await window.fetchWatchlistTickers();
await settle();
check(
  "a price tick never reaches the live region",
  a11yAnnounce.textContent === a11ySpoken,
  a11yAnnounce.textContent
);
// Put the pane back on the series it started on: every section after this one
// assumes the page is as Task 16 left it.
a11ySymbol.value = a11ySymbolWas;
a11ySymbol.dispatchEvent(new window.Event("change", { bubbles: true }));
await settle();

const a11ySpark = document.createElement("canvas");
document.body.appendChild(a11ySpark);
const a11yStylesBefore = painted.styles.length;
window.HRZUI.sparkline(a11ySpark, [1, 2, 1.5, 3], { trend: "down", label: "XRP" });
const a11yStyles = painted.styles.slice(a11yStylesBefore);
check(
  "a sparkline hands the canvas a colour, not a custom property",
  a11yStyles.some((entry) => entry.startsWith("strokeStyle=")) &&
    !a11yStyles.some((entry) => entry.includes("var(")),
  a11yStyles.filter((entry) => entry.includes("Style=")).join(" ")
);
a11ySpark.remove();

// --- idle cost ---------------------------------------------------------------
//
// jsdom's `document.hidden` is a getter on the prototype; an own property
// shadows it for the length of this check, which is the only way to ask "what
// does this tab do when nobody is looking" without a real browser.

Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
const a11yCalls = backend.calls.length;
await window.fetchWatchlistTickers();
window.dashTick();
await settle();
check(
  "a tab nobody is looking at does not poll the venue",
  backend.calls.length === a11yCalls,
  `${backend.calls.length - a11yCalls} call(s)`
);
// Deleting the own property hands `document.hidden` back to jsdom's prototype.
delete document.hidden;
window.Shell.go("charts");
await settle();

// --- put the chart back ------------------------------------------------------
//
// The last check above charted SOLUSDT 15m on the first pane, and the drawing,
// badge and layer sections below read BTCUSDT 5m off that same pane. This is the
// dashboard's visit ending, not a dashboard behaviour, so it restores rather
// than asserting.

const backSymbol = paneNode(0).querySelector(".symbol");
backSymbol.value = "BTCUSDT";
change(backSymbol);
await waitFor(() => selectIn(0, "symbol") === "BTCUSDT", "the pane to return to BTCUSDT");
const backFrame = paneNode(0).querySelector(".timeframe");
backFrame.value = "5m";
change(backFrame);
await waitFor(() => selectIn(0, "timeframe") === "5m", "the pane to return to 5m");
await settle();

// --- the frame at every width -------------------------------------------------
//
// jsdom lays nothing out, so the breakpoint checks read responsive.css the way
// the old 1100/900 checks read the inline block. They are not "a media query
// exists": each one asserts the declaration that makes the width work. The DOM
// half is the phone's navigation, which is a second route to the same
// destinations and must not be a second router.

console.log("\nthe frame at every width");

const styleFiles = [
  "tokens", "base", "components", "shell", "charts", "dashboard",
  "inspector", "studio", "community", "account", "public", "responsive",
];
const styleText = (
  await Promise.all(
    styleFiles.map((name) =>
      readFile(join(root, `frontend/app/styles/${name}.css`), "utf8")
    )
  )
).join("\n");
const responsiveText = await readFile(
  join(root, "frontend/app/styles/responsive.css"),
  "utf8"
);
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
const responsiveBlock = (width) => {
  const at = responsiveText.indexOf(`@media (max-width: ${width}px)`);
  if (at < 0) return "";
  const next = responsiveText.indexOf("@media", at + 1);
  return responsiveText.slice(at, next < 0 ? responsiveText.length : next);
};

check(
  "the old 1100 and 900 breakpoints are gone",
  !styleText.includes("@media (max-width: 1100px)") &&
    !styleText.includes("@media (max-width: 900px)"),
  `1100=${styleText.indexOf("@media (max-width: 1100px)")} 900=${styleText.indexOf("@media (max-width: 900px)")}`
);
check(
  "three breakpoints, widest first",
  responsiveText.indexOf("@media (max-width: 1279px)") >= 0 &&
    responsiveText.indexOf("@media (max-width: 1279px)") <
      responsiveText.indexOf("@media (max-width: 1023px)") &&
    responsiveText.indexOf("@media (max-width: 1023px)") <
      responsiveText.indexOf("@media (max-width: 767px)"),
  `${responsiveText.indexOf("@media (max-width: 1279px)")} < ${responsiveText.indexOf("@media (max-width: 1023px)")} < ${responsiveText.indexOf("@media (max-width: 767px)")}`
);
check(
  "at 1279 the inspector narrows through the property the splitter writes",
  /--aside-w:\s*320px/.test(ruleIn(responsiveBlock(1279), ":root")),
  ruleIn(responsiveBlock(1279), ":root").trim() || "(no :root rule)"
);
check(
  "at 1023 the panels leave the row and the controls grow to a finger",
  /position:\s*fixed/.test(ruleIn(responsiveBlock(1023), "#sideWatch")) &&
    /position:\s*fixed/.test(ruleIn(responsiveBlock(1023), "#inspectorHost")) &&
    /position:\s*fixed/.test(ruleIn(responsiveBlock(1023), "#dock")) &&
    /min-height:\s*44px/.test(ruleIn(responsiveBlock(1023), ".hrz-tab")) &&
    /--bar-h:\s*52px/.test(ruleIn(responsiveBlock(1023), ":root")),
  ruleIn(responsiveBlock(1023), "#sideWatch").trim().slice(0, 60) || "(no drawer)"
);
check(
  "at 767 the views and the status bar give way to the bottom nav",
  /display:\s*none/.test(ruleIn(responsiveBlock(767), "#topNav")) &&
    /display:\s*none/.test(ruleIn(responsiveBlock(767), "#statusbar")) &&
    /display:\s*flex/.test(ruleIn(responsiveBlock(767), ".phoneNav")) &&
    /--topbar-h:\s*48px/.test(ruleIn(responsiveBlock(767), ":root")),
  ruleIn(responsiveBlock(767), ":root").trim() || "(no :root rule)"
);
// The panel is the one surface that is wider on a phone than on a desktop: it
// has the whole screen and needs it. Three widths, one sheet -- the size the
// route asked for cannot make a phone show a 520px column with 40px of chart.
const panelSheet = [...responsiveBlock(767).matchAll(/([^{}]+)\{([^}]*)\}/g)]
  .map(([, selector, body]) => ({ selector, body }))
  .find((rule) => rule.selector.includes(".hrz-dialog--panel")) || { selector: "", body: "" };
check(
  "and the panel is a sheet at every width it can be asked for",
  /width:\s*100%/.test(panelSheet.body) &&
    /max-width:\s*100%/.test(panelSheet.body) &&
    ["", '[data-width="narrow"]', '[data-width="medium"]', '[data-width="wide"]']
      .every((suffix) => panelSheet.selector.includes(`.hrz-dialog--panel${suffix}`)),
  panelSheet.selector.replace(/\s+/g, " ").trim() || "(no panel rule)"
);
check(
  "and the wall stacks one chart per screen",
  /display:\s*block/.test(ruleIn(responsiveBlock(767), ".chartRow")) &&
    /100dvh/.test(ruleIn(responsiveBlock(767), ".chartRow .chartPane")),
  ruleIn(responsiveBlock(767), ".chartRow .chartPane").trim().slice(0, 60) ||
    "(no .chartPane rule)"
);
check(
  "page padding follows one token, 24 then 16 then 12",
  /--page-pad:\s*var\(--space-6\)/.test(styleText) &&
    /--page-pad:\s*var\(--space-4\)/.test(responsiveBlock(1023)) &&
    /--page-pad:\s*var\(--space-3\)/.test(responsiveBlock(767)),
  `24=${/--page-pad:\s*var\(--space-6\)/.test(styleText)} 16=${/--page-pad:\s*var\(--space-4\)/.test(responsiveBlock(1023))} 12=${/--page-pad:\s*var\(--space-3\)/.test(responsiveBlock(767))}`
);

check(
  "the bottom nav is the view list on a phone",
  document.querySelectorAll("#phoneNav [data-dest]").length === 3 &&
    document.getElementById("phoneNav").hidden === false &&
    document.querySelectorAll("#phoneNav [data-open-panel]").length === 0,
  `${document.querySelectorAll("#phoneNav [data-dest]").length} views · hidden=${document.getElementById("phoneNav").hidden}`
);
document.querySelector("#phoneNav [data-dest='dashboard']").click();
await settle();
check(
  "and its buttons are the router, not a second one",
  document.getElementById("dest-dashboard").hidden === false &&
    document.querySelector("#phoneNav [data-dest='dashboard']").getAttribute("aria-current") === "page" &&
    document.querySelector("#topNav [data-dest='dashboard']").getAttribute("aria-current") === "page",
  `phone=${document.querySelector("#phoneNav [data-dest='dashboard']").getAttribute("aria-current")} top=${document.querySelector("#topNav [data-dest='dashboard']").getAttribute("aria-current")}`
);
document.querySelector("#phoneNav [data-dest='charts']").click();
await settle();

// --- the panels that float over the frame ------------------------------------
//
// Community, Bots and Account are not destinations any more: they are one modal
// that the address bar is allowed to name. Everything the shell promises about
// that modal is decided here -- which host is on screen, what the address says,
// and whether the two can ever disagree.
//
// What this file cannot check is the modal's own furniture: the top layer, the
// scrim, the focus trap. jsdom has no `showModal`, and the shim above stands in
// for the two methods and the two events the shell calls. The primitive's own
// behaviour is asserted in `ui_check`; what follows is the shell's use of it.

console.log("\nthe panels that float over the frame");

const panelEl = () => document.getElementById("workPanel");
const panelShown = (name) => {
  const body = document.getElementById(`panelBody-${name}`);
  return Boolean(body) && body.hidden === false;
};
const panelTrigger = (name) => document.querySelector(`[data-open-panel="${name}"]`);
const panelCalls = (prefix) => backend.calls.filter((call) => call.url.startsWith(prefix)).length;

const communityCalls = panelCalls("/community");
check(
  "a panel nobody opened asked for nothing",
  communityCalls === 0,
  `${communityCalls} /community call(s)`
);

panelTrigger("community").focus();
panelTrigger("community").click();
await settle();
check(
  "the trigger floats a modal over the page it is on",
  panelEl().open === true && panelShown("community") && !panelShown("bots") &&
    document.getElementById("dest-charts").hidden === false,
  `open=${panelEl().open} community=${panelShown("community")}`
);
check(
  "and the address names both the page and the panel",
  window.location.hash === "#/charts/community",
  window.location.hash
);
check(
  "the icon that opened it says the door is open",
  panelTrigger("community").getAttribute("aria-expanded") === "true" &&
    panelTrigger("account").getAttribute("aria-expanded") === "false",
  `community=${panelTrigger("community").getAttribute("aria-expanded")} account=${panelTrigger("account").getAttribute("aria-expanded")}`
);
check(
  "the head carries the panel's name, width and selection",
  document.getElementById("workPanelTitle").textContent === "Community" &&
    panelEl().dataset.width === "wide" &&
    document.getElementById("panelTab-community").getAttribute("aria-selected") === "true",
  `${document.getElementById("workPanelTitle").textContent} ${panelEl().dataset.width}`
);
check(
  "and opening it is the first time the library is fetched",
  panelCalls("/community") > communityCalls,
  `${panelCalls("/community") - communityCalls} call(s)`
);

document.getElementById("panelTab-bots").click();
await settle();
check(
  "a head tab is a route change, not a second modal",
  panelEl().open === true && panelShown("bots") && !panelShown("community") &&
    window.location.hash === "#/charts/bots",
  `${panelEl().open} ${window.location.hash}`
);

const closeBox = document.getElementById("workPanelClose");
closeBox.click();
await settle();
check(
  "the close button asks the route, which puts the panel away",
  panelEl().open === false && window.location.hash === "#/charts" &&
    document.getElementById("dest-charts").hidden === false,
  `open=${panelEl().open} ${window.location.hash}`
);
check(
  "and focus goes back to the icon of the face that was showing",
  document.activeElement === panelTrigger("bots"),
  document.activeElement.getAttribute("data-open-panel") ||
    document.activeElement.id || document.activeElement.tagName
);

panelTrigger("bots").click();
await settle();
const escapeAsk = new window.Event("cancel", { cancelable: true });
panelEl().dispatchEvent(escapeAsk);
await settle();
check(
  "Escape is answered rather than taken, so the address never keeps a panel",
  escapeAsk.defaultPrevented === true && panelEl().open === false &&
    window.location.hash === "#/charts",
  `prevented=${escapeAsk.defaultPrevented} ${window.location.hash}`
);

panelTrigger("community").click();
await settle();
window.location.hash = "#/charts";
await settle();
check(
  "a Back press leaves the screen it came from, panel and all",
  panelEl().open === false && window.location.hash === "#/charts" &&
    document.getElementById("dest-charts").hidden === false,
  `open=${panelEl().open} ${window.location.hash}`
);

// The address the bar had when these three were destinations is still a link to
// the same content, and is rewritten to the shape the shell writes on reload.
window.location.hash = "#/community";
await settle();
check(
  "the old shape of the address still opens the panel",
  panelShown("community") && window.location.hash === "#/charts/community",
  `${panelShown("community")} ${window.location.hash}`
);

// Nothing pointed at that panel -- an address bar did. The icon is still the
// door it came through, so it is the door closing hands focus back to.
document.getElementById("workPanelClose").click();
await settle();
check(
  "a panel the address opened hands focus to its icon",
  panelEl().open === false && document.activeElement === panelTrigger("community"),
  `${panelEl().open} ${document.activeElement.getAttribute("data-open-panel")}`
);

panelTrigger("account").click();
await settle();
check(
  "the avatar opens the account panel on its first section",
  panelShown("account") && window.location.hash === "#/charts/account/plan",
  window.location.hash
);
document.querySelector('#accountTabs [data-account="brokers"]').click();
await settle();
check(
  "and a section is a deeper address, not a second panel",
  panelShown("account") && !panelShown("bots") &&
    window.location.hash === "#/charts/account/brokers" &&
    document.querySelector('#accountTabs [data-account="brokers"]').getAttribute("aria-selected") === "true",
  window.location.hash
);

const tickerBefore = clocksNamed("fetchWatchlistTickers");
check(
  "the rail under the modal keeps its clock",
  tickerBefore === 1,
  `${tickerBefore} watchlist ticker(s)`
);

document.querySelector("#topNav [data-dest='dashboard']").click();
await settle();
check(
  "switching view puts the panel away and leaves the address a page",
  panelEl().open === false && window.location.hash === "#/dashboard" &&
    document.getElementById("dest-dashboard").hidden === false,
  `${panelEl().open} ${window.location.hash}`
);

// --- the fourth face: the indicator library ----------------------------------
//
// The library the chart used to carry as a block of checkboxes inside the
// right-click menu. It is a panel face now, so the shell's promises apply to it:
// one address, one host, one render pass -- plus two that belong to a panel of
// cards rather than a pane of rows, that every card asks for its own picture and
// that the whole grid costs one event read.

console.log("\nthe indicators face");

const conceptCards = () => [...document.querySelectorAll("#conceptGrid [data-concept]")];
const conceptCard = (id) => document.querySelector(`#conceptGrid [data-concept="${id}"]`);
const conceptRail = () => [...document.querySelectorAll("#conceptGroups [data-concept-group]")];
const routeCalls = (prefix) => backend.calls.filter((c) => c.url.startsWith(prefix));
/// Let every queued preview fetch and draw land. The queue is four deep and each
/// card is a promise chain, so a single 30ms settle is not the whole flush.
const drain = async () => { for (let i = 0; i < 8; i += 1) await settle(); };

check(
  "the fourth door on the bar is the library, and it is always there",
  panelTrigger("concepts") !== null &&
    panelTrigger("concepts").disabled === false &&
    panelTrigger("concepts").getAttribute("aria-label") === "Indicators" &&
    document.getElementById("panelTab-concepts") !== null,
  panelTrigger("concepts") ? panelTrigger("concepts").getAttribute("aria-label") : "no trigger"
);

const scanBefore = routeCalls("/concept-events/scan").length;
const seriesBefore = routeCalls("/indicator-series").length;
check(
  "a library nobody opened asked for nothing",
  scanBefore === 0 && seriesBefore === 0,
  `${scanBefore} scan(s) ${seriesBefore} series(s)`
);

// The library this page ships against is 182 concepts wide and the first screen
// shows twelve, so what a card costs is not "is it in the library" but "has the
// platform put it on screen". Opening the face here happens with the observer
// kept silent -- a grid the platform has said nothing about yet -- and the only
// right answer to that is no request at all.
window.__observeSilently(true);
panelTrigger("concepts").click();
await settle();
check(
  "from the dashboard it lands on a chart first, because an indicator needs one",
  panelShown("concepts") && window.location.hash === "#/charts/concepts" &&
    document.getElementById("dest-charts").hidden === false &&
    document.getElementById("dest-dashboard").hidden === true,
  `${panelEl().open} ${window.location.hash}`
);
check(
  "the head names the face and the tab says it is the one showing",
  document.getElementById("workPanelTitle").textContent === "Indicators" &&
    document.getElementById("panelTab-concepts").getAttribute("aria-selected") === "true" &&
    panelTrigger("concepts").getAttribute("aria-expanded") === "true",
  `${document.getElementById("workPanelTitle").textContent}`
);

const coverage = document.getElementById("conceptCoverage").textContent;
check(
  "the rail counts what the instrument can actually run",
  coverage.includes("7 of 8") && coverage.includes("BTCUSDT") && coverage.includes("5m"),
  coverage
);
check(
  "and one row per family this instrument has members in -- no empty zeros",
  conceptRail().map((row) => row.dataset.conceptGroup).join(",") ===
    "all,smc,candles,order-flow,trend,momentum,volatility,statistical,econ",
  conceptRail().map((row) => row.dataset.conceptGroup).join(",")
);
check(
  "each row carries its own touched-over-total, not just a size",
  conceptRail().find((row) => row.dataset.conceptGroup === "trend")
    .textContent.includes("0/1"),
  conceptRail().find((row) => row.dataset.conceptGroup === "trend")?.textContent
);
check(
  "the whole library is eight cards, each with a canvas of its own",
  conceptCards().length === 8 &&
    conceptCards().every((card) => card.querySelector(".conceptCard__canvas")),
  `${conceptCards().length} card(s)`
);

const chipText = (id) => [...conceptCard(id).querySelectorAll(".conceptCard__chip")]
  .map((node) => node.textContent).join("+");
check(
  "a card says what it draws: a band on the price, a line in a sub-pane, a mark on a bar",
  chipText("indicators.bollinger") === "on chart" &&
    chipText("indicators.rsi") === "sub-pane" &&
    chipText("candles.doji") === "events",
  `${chipText("indicators.bollinger")} ${chipText("indicators.rsi")} ${chipText("candles.doji")}`
);
const starved = conceptCard("order_flow.delta_shift");
check(
  "and one the instrument cannot feed is greyed, unpressable, and says what it needs",
  starved.classList.contains("conceptCard--unavailable") &&
    starved.querySelector(".conceptCard__main").disabled === true &&
    starved.querySelector(".conceptCard__needs").textContent.includes("trades_aggressor"),
  starved.querySelector(".conceptCard__needs")?.textContent || "no needs line"
);

// The library this page ships against is 182 concepts wide and the first screen
// shows twelve, so what a card costs is not "is it in the library" but "has the
// platform put it on screen". Nothing has been put on screen here -- and the only
// right answer to that is no request at all.
await drain();
check(
  "a card the platform has not placed on screen costs no request",
  routeCalls("/indicator-series").length === seriesBefore &&
    routeCalls("/concept-events/scan").length === scanBefore,
  `${routeCalls("/indicator-series").length - seriesBefore} series and ` +
    `${routeCalls("/concept-events/scan").length - scanBefore} scan call(s) with nothing on screen`
);

// One card comes up, and it is the only thing worth asking for. The face then
// repaints on the chart's clock while that read is still open -- the same card
// at the same instrument, so the same picture must not go on the wire twice.
const previewCanvas = (id) => conceptCard(id).querySelector(".conceptCard__canvas");
const repaintFace = () => {
  document.dispatchEvent(new window.CustomEvent("hrz:concepts"));
  document.dispatchEvent(new window.CustomEvent("hrz:pane"));
};
window.__cardOnScreen("indicators.rsi", true);
repaintFace();
repaintFace();
await drain();
check(
  "the card that comes into view is asked for once, even across a repaint while its read is open",
  routeCalls("/indicator-series").length - seriesBefore === 1 &&
    routeCalls("/indicator-series").slice(seriesBefore)
      .every((call) => call.url.includes("concept=indicators.rsi")),
  routeCalls("/indicator-series").slice(seriesBefore).map((call) => call.url.split("concept=")[1]).join(" ") || "(none)"
);

// The rest of the first screen, plus one card the scroll brings -- which is the
// other half of the signal, and arrives as an observer record rather than as a
// box the page measured while it was building.
for (const id of ["indicators.macd", "indicators.bollinger", "indicators.pairs", "candles.doji", "econ.fomc"]) {
  window.__cardOnScreen(id, true);
}
repaintFace();
window.__intersect(previewCanvas("smc.liquidity_sweep"), true);
await drain();
check(
  "every event card's picture comes from one scan, not one request per card",
  routeCalls("/concept-events/scan").length - scanBefore === 1,
  `${routeCalls("/concept-events/scan").length - scanBefore} scan call(s) for 3 event cards`
);
check(
  "and each series card asked for its own concept on this instrument",
  routeCalls("/indicator-series").length - seriesBefore === 4 &&
    ["indicators.rsi", "indicators.macd", "indicators.bollinger", "indicators.pairs"]
      .every((id) => routeCalls("/indicator-series")
        .some((call) => call.url === `/indicator-series?symbol=BTCUSDT&timeframe=5m&concept=${id}` ||
          call.url === `/indicator-series?symbol=BTCUSDT&timeframe=5m&concept=${id}&symbol2=ETHUSDT`)),
  routeCalls("/indicator-series").map((call) => call.url.split("concept=")[1]).join(" ")
);
check(
  "the one concept with a second instrument sends the one the pane is on",
  routeCalls("/indicator-series").some((call) =>
    call.url.includes("concept=indicators.pairs") && call.url.includes("symbol2=")),
  routeCalls("/indicator-series").map((call) => call.url).find((url) => url.includes("pairs")) || "(none)"
);
check(
  "the greyed card was not fetched for, and the pairs card offers its second instrument",
  !routeCalls("/indicator-series").some((call) => call.url.includes("delta_shift")) &&
    conceptCard("indicators.pairs").querySelector(".conceptCard__pairInput") !== null,
  ""
);
check(
  "a previewed card is drawn into, not left shimmering",
  (() => {
    const canvas = conceptCard("indicators.rsi").querySelector(".conceptCard__canvas");
    return canvas.width > 0 && !canvas.classList.contains("conceptCard__canvas--wait");
  })(),
  `width=${conceptCard("indicators.rsi").querySelector(".conceptCard__canvas").width}`
);

// The second render of the same cards is the same data: re-asking for a picture
// the face already holds is how a panel of sixty cards becomes sixty requests
// every time the trader presses an arrow key.
const seriesAfterFirst = routeCalls("/indicator-series").length;
const scanAfterFirst = routeCalls("/concept-events/scan").length;
conceptRail().find((row) => row.dataset.conceptGroup === "smc").click();
await settle();
conceptRail().find((row) => row.dataset.conceptGroup === "all").click();
await settle();
await drain();
check(
  "and a group is a filter over cached answers, not a second fetch",
  routeCalls("/indicator-series").length === seriesAfterFirst &&
    routeCalls("/concept-events/scan").length === scanAfterFirst,
  `+${routeCalls("/indicator-series").length - seriesAfterFirst} series, +${routeCalls("/concept-events/scan").length - scanAfterFirst} scan(s)`
);

check(
  "a family row is an address, and the address is the only truth about it",
  panelShown("concepts") && window.location.hash === "#/charts/concepts" &&
    conceptCards().length === 8 &&
    conceptRail().find((row) => row.dataset.conceptGroup === "all")
      .getAttribute("aria-current") === "true",
  `${window.location.hash} · ${conceptCards().length} card(s)`
);

document.getElementById("conceptActiveRow").click();
await settle();
check(
  "the chart's own row is the third segment too, and says so when it is empty",
  window.location.hash === "#/charts/concepts/active" &&
    conceptCards().length === 0 &&
    document.getElementById("conceptEmpty").hidden === false &&
    document.getElementById("conceptEmpty").textContent.includes("Nothing on this chart yet"),
  `${window.location.hash} · ${document.getElementById("conceptEmpty").textContent.slice(0, 40)}`
);

window.location.hash = "#/charts/concepts/smc";
await settle();
check(
  "typing the family into the address bar opens it directly",
  panelShown("concepts") && conceptCards().length === 1 &&
    conceptCards()[0].dataset.concept === "smc.liquidity_sweep" &&
    document.getElementById("conceptHeadTitle").textContent === "Smart Money Concepts",
  `${window.location.hash} · ${conceptCards().map((c) => c.dataset.concept).join(",")}`
);

const search = document.getElementById("conceptSearch");
window.location.hash = "#/charts/concepts";
await settle();
search.value = "bollinger";
search.dispatchEvent(new window.Event("input", { bubbles: true }));
await settle();
check(
  "the search filters across families and the head counts what it found",
  conceptCards().length === 1 &&
    document.getElementById("conceptHeadNote").textContent.includes("1 match") &&
    document.getElementById("conceptHeadNote").textContent.includes("bollinger"),
  `${conceptCards().length} card(s) · ${document.getElementById("conceptHeadNote").textContent}`
);
search.value = "liquidity";
search.dispatchEvent(new window.Event("input", { bubbles: true }));
await settle();
check(
  "a query crosses families rather than navigating to one: on the whole library it finds the SMC card",
  window.location.hash === "#/charts/concepts" && conceptCards().length === 1 &&
    conceptCards()[0].dataset.concept === "smc.liquidity_sweep",
  `${window.location.hash} · ${conceptCards().map((c) => c.dataset.concept).join(",")}`
);
conceptRail().find((row) => row.dataset.conceptGroup === "smc").click();
await settle();
check(
  "and a group change is a filter change, not a reset of what was typed",
  window.location.hash === "#/charts/concepts/smc" && search.value === "liquidity" &&
    conceptCards().length === 1,
  `${window.location.hash} · ${search.value} · ${conceptCards().length}`
);
search.value = "qqqq";
search.dispatchEvent(new window.Event("input", { bubbles: true }));
await settle();
check(
  "and nothing matching is said, not shown as an empty box",
  conceptCards().length === 0 && document.getElementById("conceptEmpty").hidden === false,
  ""
);
search.value = "";
search.dispatchEvent(new window.Event("input", { bubbles: true }));
await settle();

// --- picking one -------------------------------------------------------------

window.location.hash = "#/charts/concepts";
await settle();
const rsiChipRow = paneNode().querySelector(".conceptChips");
check(
  "with nothing on the chart, the head has no concept chips to show",
  conceptCards().length === 8 && rsiChipRow && rsiChipRow.hidden === true,
  `${conceptCards().length} card(s) · chips hidden=${rsiChipRow?.hidden}`
);

conceptCard("indicators.rsi").querySelector(".conceptCard__main").click();
await settle();
check(
  "adding one dismisses the panel, so the trader sees the thing they added",
  panelEl().open === false && window.location.hash === "#/charts" &&
    document.getElementById("dest-charts").hidden === false,
  `open=${panelEl().open} ${window.location.hash}`
);
const chipRow = () => paneNode().querySelector(".conceptChips");
const chipsOnPane = () => [...chipRow().querySelectorAll(".conceptChip")];
const chipRemoves = () => [...chipRow().querySelectorAll("button[data-concept]")];
check(
  "and the chart names it: a chip on the pane's own head, removable where it sits",
  chipsOnPane().length === 1 &&
    chipRemoves()[0].dataset.concept === "indicators.rsi" &&
    chipRow().hidden === false &&
    chipsOnPane()[0].textContent.includes("RSI"),
  chipsOnPane().map((chip) => chip.textContent).join(",") || "no chips"
);

panelTrigger("concepts").click();
await settle();
await drain();
check(
  "reopened, the card remembers: pressed, counted, and its family row counts it",
  conceptCard("indicators.rsi").classList.contains("conceptCard--on") &&
    conceptCard("indicators.rsi").querySelector(".conceptCard__main")
      .getAttribute("aria-checked") === "true" &&
    document.getElementById("conceptActiveCount").textContent === "1" &&
    conceptRail().find((row) => row.dataset.conceptGroup === "momentum")
      .textContent.includes("1/1"),
  `${conceptCard("indicators.rsi")?.className} count=${document.getElementById("conceptActiveCount").textContent}`
);

conceptCard("indicators.macd").querySelector(".conceptCard__main").click();
await settle();
check(
  "a second concept follows the same rule: added, dismissed, two chips on the chart",
  panelEl().open === false && window.location.hash === "#/charts" &&
    chipRemoves().map((button) => button.dataset.concept).join(",") === "indicators.rsi,indicators.macd",
  chipRemoves().map((button) => button.dataset.concept).join(",") || "no chips"
);

// The other door: the chip on the chart's own head, reached without opening
// anything. This is what makes staying open on a switch-off cost nothing.
chipRemoves()[0].click();
await settle();
check(
  "and a chip's own cross takes it off the chart with the panel shut",
  chipsOnPane().length === 1 && chipRemoves()[0].dataset.concept === "indicators.macd",
  chipRemoves().map((button) => button.dataset.concept).join(",") || "none"
);

panelTrigger("concepts").click();
await settle();
await drain();
document.getElementById("conceptActiveRow").click();
await settle();
check(
  "the chart's own row is the third segment too, and holds what is on the chart",
  window.location.hash === "#/charts/concepts/active" && conceptCards().length === 1 &&
    conceptCards()[0].dataset.concept === "indicators.macd" &&
    document.getElementById("conceptActiveCount").textContent === "1",
  `${window.location.hash} · ${conceptCards().length} card(s)`
);

conceptCards()[0].querySelector(".conceptCard__main").click();
await settle();
check(
  "switching one off keeps the panel open -- a trader who is pruning is still choosing",
  panelEl().open === true && window.location.hash === "#/charts/concepts/active" &&
    conceptCards().length === 0 &&
    document.getElementById("conceptActiveCount").textContent === "0",
  `open=${panelEl().open} ${conceptCards().length} card(s)`
);
check(
  "and the chip on the chart goes with it, the row shutting rather than sitting empty",
  chipsOnPane().length === 0 && chipRow().hidden === true,
  `${chipsOnPane().length} chip(s) hidden=${chipRow().hidden}`
);

// The wire the visual fidelity audit photographs. Everything above is about the
// panel: the card pressed, the chip appearing, the counter moving. Nothing asked
// whether the chart then *drew* the concept the trader added -- and that is the
// only claim an indicator exists to keep. An event concept goes on the request as
// semantic shapes, comes back from the engine as positioned draws, and leaves
// strokes on the canvas; take any of the three away and the panel still looks
// perfect.
window.location.hash = "#/charts/concepts";
await settle();
await drain();
conceptCard("smc.liquidity_sweep").querySelector(".conceptCard__main").click();
await settle();
await drain();

const sweepShapes = lastRequest().concept_shapes || [];
check(
  "an enabled event concept is on the wire: a segment from its swing to its break",
  sweepShapes.length === 2 &&
    sweepShapes.every((s) => s.type === "segment" && s.price > 0 && s.to_ns > s.from_ns),
  `${sweepShapes.length} shape(s) ${sweepShapes.map((s) => s.type).join(",")}`
);

const sweepDraws = (lastScene().concept_shapes || []);
const sweepDrawKinds = [...new Set(sweepDraws.map((d) => Object.keys(d)[0]))].sort().join(",");
check(
  "and the engine, not the page, places it: positioned draws come back",
  sweepDraws.length > 0 && sweepDraws.every((d) => d.Line || d.Label || d.Box),
  `${sweepDraws.length} lowered draw(s): ${sweepDrawKinds}`
);

const conceptOpsBefore = painted.ops.length;
const conceptTextBefore = painted.text.length;
window.HRZApp.resizePanes();
await settle();
const conceptLabels = painted.text.slice(conceptTextBefore);
check(
  "the painter strokes it under the candles and prints its name over them",
  painted.ops.length > conceptOpsBefore &&
    painted.ops.slice(conceptOpsBefore).includes("stroke") &&
    conceptLabels.some((t) => t.includes("liquidity_sweep")),
  `+${painted.ops.length - conceptOpsBefore} op(s) · ${conceptLabels.slice(0, 3).join("/") || "no labels"}`
);

panelTrigger("concepts").click();
await settle();
await drain();
conceptCard("smc.liquidity_sweep").querySelector(".conceptCard__main").click();
await settle();
await drain();
check(
  "and switching it off takes it off the wire, not just off the card",
  (lastRequest().concept_shapes || []).length === 0,
  `${(lastRequest().concept_shapes || []).length} shape(s) after switching off`
);

// Clear one family, not the whole chart: the button is in a group's head, and a
// trader who means the family must not lose the four other things they attached.
const conceptClear = document.getElementById("conceptClearGroup");
window.location.hash = "#/charts/concepts";
await settle();
conceptCard("indicators.rsi").querySelector(".conceptCard__main").click();
await settle();
panelTrigger("concepts").click();
await settle();
await drain();
conceptCard("indicators.macd").querySelector(".conceptCard__main").click();
await settle();
// The scale rule the audit recorded as ignored: a pane concept's reading is
// not a price. With RSI and MACD on -- both pane concepts -- the price frame
// must receive no shapes at all, and the RSI crossing must arrive as a mark on
// the RSI pane's own line, at the value the line draws.
const paneRequest = lastRequest();
const rsiPane = (paneRequest.sub_panes || [])
  .find((p) => p.kind === "series" && p.label === "RSI(14)");
const rsiMark = rsiPane && (rsiPane.markers || [])[0];
const rsiFixtureValue = 50 + 22 * Math.sin(70 / 6);
check(
  "a pane concept marks its own pane, not the price chart",
  (paneRequest.concept_shapes || []).length === 0 &&
    rsiPane && (rsiPane.markers || []).length === 1 &&
    rsiMark.kind === "bullish" && rsiMark.at === 170 &&
    Math.abs(rsiMark.value - rsiFixtureValue) < 1e-9,
  `${(paneRequest.concept_shapes || []).length} price shape(s) · ` +
    `${(rsiPane?.markers || []).length} pane mark(s)` +
    (rsiMark ? ` at bar ${rsiMark.at}, value ${rsiMark.value.toFixed(2)}` : "")
);
panelTrigger("concepts").click();
await settle();
await drain();
conceptRail().find((row) => row.dataset.conceptGroup === "trend").click();
await settle();
check(
  "a family with something in it offers to clear that family",
  window.location.hash === "#/charts/concepts/trend" &&
    conceptCards().map((c) => c.dataset.concept).join(",") === "indicators.macd" &&
    conceptClear.hidden === false,
  `${window.location.hash} · ${conceptCards().map((c) => c.dataset.concept).join(",")} · hidden=${conceptClear.hidden}`
);
conceptClear.click();
await settle();
check(
  "and it stops at the family: the trend card is off but still there, the momentum one untouched",
  conceptCards().length === 1 &&
    !conceptCard("indicators.macd").classList.contains("conceptCard--on") &&
    conceptCard("indicators.macd").querySelector(".conceptCard__main")
      .getAttribute("aria-checked") === "false" &&
    chipsOnPane().length === 1 &&
    chipRemoves()[0].dataset.concept === "indicators.rsi" &&
    document.getElementById("conceptActiveCount").textContent === "1" &&
    conceptClear.hidden === true,
  `cards=${conceptCards().length} macdOn=${conceptCard("indicators.macd")?.classList.contains("conceptCard--on")} ` +
    `chips=${chipsOnPane().length} count=${document.getElementById("conceptActiveCount").textContent}`
);
chipRemoves()[0].click();
await settle();

// --- the keyboard ------------------------------------------------------------

document.getElementById("workPanelClose").click();
await settle();
document.dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "/", bubbles: true, cancelable: true,
}));
await settle();
check(
  "the slash opens the library from anywhere on the page, with no modifier held",
  panelShown("concepts") && window.location.hash === "#/charts/concepts",
  `${panelShown("concepts")} ${window.location.hash}`
);
document.getElementById("panelBody-concepts").dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "/", bubbles: true, cancelable: true,
}));
await settle();
check(
  "and inside the face the same key means let me type, not open the face again",
  document.activeElement === document.getElementById("conceptSearch"),
  document.activeElement.id || document.activeElement.tagName
);

const firstCard = conceptCards()[0].querySelector(".conceptCard__main");
check(
  "one Tab stop for the whole grid, at the card the arrows were last on",
  conceptCards().filter((c) => c.querySelector(".conceptCard__main").tabIndex === 0).length === 1,
  `${conceptCards().filter((c) => c.querySelector(".conceptCard__main").tabIndex === 0).length} stop(s)`
);
firstCard.focus();
conceptCards()[0].querySelector(".conceptCard__main")
  .dispatchEvent(new window.KeyboardEvent("keydown", {
    key: "ArrowRight", bubbles: true, cancelable: true,
  }));
await settle();
check(
  "and the arrows walk it, moving the stop with the point",
  document.activeElement === conceptCards()[1].querySelector(".conceptCard__main") &&
    conceptCards()[1].querySelector(".conceptCard__main").tabIndex === 0 &&
    firstCard.tabIndex === -1,
  `${document.activeElement.textContent?.slice(0, 12)} tabIndex=${firstCard.tabIndex}`
);

document.getElementById("workPanelClose").click();
await settle();
check(
  "closed, the face hands focus back to its own door",
  panelEl().open === false && document.activeElement === panelTrigger("concepts"),
  `${panelEl().open} ${document.activeElement.getAttribute("data-open-panel")}`
);

// The chart keeps its clock under a modal, so the queue that fetches card
// pictures has to stop with the modal -- otherwise a panel closed twenty minutes
// ago is still asking for sixty previews nobody will look at. Measured against the
// moment of closing, because the pane's own event read is a different request for
// a different reason and this is not about it.
const scanOnClose = routeCalls("/concept-events/scan").length;
const seriesOnClose = routeCalls("/indicator-series").length;
document.dispatchEvent(new window.CustomEvent("hrz:concepts"));
document.dispatchEvent(new window.CustomEvent("hrz:pane"));
await settle();
await drain();
check(
  "a closed face fetches nothing",
  routeCalls("/indicator-series").length === seriesOnClose &&
    routeCalls("/concept-events/scan").length === scanOnClose,
  `+${routeCalls("/indicator-series").length - seriesOnClose} series, ` +
    `+${routeCalls("/concept-events/scan").length - scanOnClose} scan(s) while closed`
);

// Back to the platform's own answer for every section after this one: what the
// wall does with a pane it is told to look at is a different promise from what a
// card costs, and it is asserted where the wall is asserted.
window.__observeSilently(false);

document.querySelector("#topNav [data-dest='dashboard']").click();
await settle();

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
  "with one absolute point per anchor the kind takes",
  stored &&
    Array.isArray(stored.points) &&
    stored.points.length === 2 &&
    stored.points.every((p) => p.unit === "absolute"),
  stored && JSON.stringify(stored.points)
);
check(
  "and two points that are not the same place",
  stored &&
    (stored.points[0].time !== stored.points[1].time ||
      stored.points[0].price !== stored.points[1].price),
  stored && JSON.stringify(stored.points)
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
// The workspace keeps its own autosave PUT (`/workspace`), which can land at any
// point inside a gesture window. These checks are about the drawing store, so
// they count that one route and nothing else.
const drawingPuts = () =>
  backend.calls.filter((c) => c.method === "PUT" && c.url.startsWith("/drawings/"));
const putBefore = drawingPuts().length;

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

const puts = drawingPuts();
check(
  "a body drag stores the move",
  puts.length === putBefore + 1,
  `${puts.length - putBefore} PUTs`
);

if (puts.length > putBefore) {
  const body = JSON.parse(puts[puts.length - 1].body);
  const dt = body.points[1].time - body.points[0].time;
  const dp = body.points[1].price - body.points[0].price;
  const dtBefore = stored.points[1].time - stored.points[0].time;
  const dpBefore = stored.points[1].price - stored.points[0].price;
  // Equal *differences* is exactly "the shape moved rather than deformed", and
  // it is checkable without knowing which way the axes run.
  check(
    "and the shape kept its extent, so it translated rather than stretched",
    Math.abs(dt - dtBefore) < 1e-6 && Math.abs(dp - dpBefore) < 1e-6,
    `span ${dtBefore}/${dpBefore} -> ${dt}/${dp}`
  );
  check(
    "both ends moved",
    body.points[0].time !== stored.points[0].time ||
      body.points[0].price !== stored.points[0].price,
    `${JSON.stringify(stored.points[0])} -> ${JSON.stringify(body.points[0])}`
  );
  check(
    "and the scene agrees with what was stored",
    Math.abs(lastScene().drawings[0].points[0].absolute.time - body.points[0].time) < 1e-6,
    `${lastScene().drawings[0].points[0].absolute.time} vs ${body.points[0].time}`
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

const putsNow = drawingPuts;
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
      dragged.points[0].time === afterBody.points[0].time &&
        dragged.points[0].price === afterBody.points[0].price,
      `${JSON.stringify(afterBody.points[0])} -> ${JSON.stringify(dragged.points[0])}`
    );
    check(
      "while the one that was grabbed did, so the slope changed",
      dragged.points[1].time !== afterBody.points[1].time ||
        dragged.points[1].price !== afterBody.points[1].price,
      `${JSON.stringify(afterBody.points[1])} -> ${JSON.stringify(dragged.points[1])}`
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
// The refused click above finished a placement, and a finished placement
// disarms the tool (single-shot, like TradingView). Without this the next drag
// is a cursor pan and the save path is never entered -- which is how these two
// checks stayed red while the shell was fine.
pickTool("trendline");
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
// Same re-arm: the failed save also finished a placement.
pickTool("trendline");
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
// A placed shape disarms the tool, so the second shape needs the same arming
// as the first -- otherwise "two shapes" is one shape and a pan.
pickTool("trendline");
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

  // A notice is not a promise that the silence lasts. The server says "no feed is
  // configured", which means *this gateway* opens none -- not that nothing will
  // ever publish into the bus -- and it does not close the socket for exactly
  // that reason. So a frame arriving afterwards has to clear the notice, or the
  // badge would go on saying "no feed" while its own age ticked, contradicting
  // the evidence it is carrying.
  deliver(liveSocket, {
    type: "notice",
    message: "no market feed is configured (MARKET_FEED is not `binance`)",
  });
  await settle();
  check(
    "a notice turns the badge into a statement about the feed",
    badgeState() === "nofeed",
    `${badgeState()}: ${badge().textContent}`
  );

  deliver(liveSocket, frame(0));
  await settle();
  check(
    "and a candle published by something else outranks the notice",
    badgeState() === "live",
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
  // Zero of *this symbol's user rows*: the AI row is BTCUSDT's, so its absence
  // here is the assertion, and the layer filter is irrelevant at count zero.
  lastScene().drawings.length === 0,
  `${lastScene().drawings.length} drawings`
);
check(
  "and it was not deleted, only left behind",
  backend.drawings.length === 1,
  `${backend.drawings.length} stored`
);

// The agent has drawn, before the user looked again: an AI-created row lands in
// storage the same way a hand-drawn one does (`docs/21` phase 2), and the next
// load is what brings it onto the chart. Injected here rather than seeded at
// the top so every count above stays about the harness's own gestures. The
// price is inside the fixture candles' range (BTCUSDT hashes to 827), so the
// engine actually places the line on the plot.
backend.drawings.push({
  id: "ai-seeded-1",
  symbol: "BTCUSDT",
  kind: "hline",
  points: [{ unit: "absolute", time: FIXTURE_NOW_MS - 600_000, price: 830 }],
  label: "AI resistance",
  created_by: "ai",
  confidence: 0.87,
  reason: "three rejections in the last 80 bars",
});

symbolSelect.value = "BTCUSDT";
change(symbolSelect);
await settle();
await settle();
await settle();

check(
  "and it comes back on the symbol it belongs to",
  // The user's trendline, and only that: the AI row is in storage but the layer
  // is off, so the scene shows one row. That is exactly the default the layer
  // section below asserts on.
  lastScene().drawings.length === 1,
  `${lastScene().drawings.length} drawings`
);

// --- the AI analysis layer (`docs/21` phase 3) --------------------------------
//
// The agent can draw (`create_drawing`), and what it draws lands in the same
// table with `created_by: "ai"`. Presentation is the shell's half of the deal:
// the layer is OFF by default -- an annotation the agent inserted between the
// user's marks and the candles is a claim that needs opting into -- and the
// filter is presentation only, because undo and delete must not care what is
// being shown. The seeded backend row is what makes every check below a
// statement about a real object rather than about an empty list.

console.log("\nthe AI analysis layer");

const aiLayerButton = () => paneNode().querySelector(".tools [data-ai-layer]");
const aiRowCount = (scene) => scene.drawings.filter((d) => d.id === "ai-seeded-1").length;

check(
  "the AI layer toggle is on the toolbar",
  Boolean(aiLayerButton()),
  aiLayerButton()?.textContent
);
check(
  "it starts off",
  aiLayerButton().getAttribute("aria-pressed") === "false",
  aiLayerButton().getAttribute("aria-pressed")
);
check(
  "and the seeded AI drawing is hidden while it is off",
  aiRowCount(lastScene()) === 0,
  `${aiRowCount(lastScene())} AI rows in the scene`
);

aiLayerButton().click();
await settle();

check(
  "turning it on reports the layer as on",
  aiLayerButton().getAttribute("aria-pressed") === "true",
  aiLayerButton().getAttribute("aria-pressed")
);
check(
  "and the AI drawing appears",
  aiRowCount(lastScene()) === 1,
  `${aiRowCount(lastScene())} AI rows in the scene`
);
check(
  "and the user's own drawings were not filtered out with it",
  lastScene().drawings.length >= 1,
  `${lastScene().drawings.length} rows total`
);

// Selecting the AI drawing says why it is there -- the reason the agent stated
// at draw time, from storage, without re-asking the model. An unselected drawing
// emits no handles, so the grab is on the hline's *body*: its segment spans the
// whole plot at one y, and clicking anywhere on it selects the drawing.
const aiSegment = lastScene()
  .drawings.find((d) => d.id === "ai-seeded-1")
  ?.parts.find((part) => part.shape === "segment");
if (aiSegment) {
  pickTool("cursor");
  const midX = (aiSegment.x1 + aiSegment.x2) / 2;
  const midY = (aiSegment.y1 + aiSegment.y2) / 2;
  pointer("pointerdown", midX, midY);
  await settle();
  pointer("pointerup", midX, midY);
  await settle();
  await settle();
  check(
    "selecting the AI drawing surfaces its stored reason",
    note().includes("three rejections"),
    note() || "(empty)"
  );
} else {
  check("selecting the AI drawing surfaces its stored reason", false, "the AI drawing offered no segment");
}

aiLayerButton().click();
await settle();
check(
  "turning it off hides the AI drawing again",
  aiRowCount(lastScene()) === 0,
  `${aiRowCount(lastScene())} AI rows in the scene`
);
// Presentation only: the row is in storage and in the shell's list, hidden not
// gone. A filter that deleted what it hid would make the toggle destructive.
check(
  "hiding did not delete it",
  backend.drawings.some((d) => d.id === "ai-seeded-1"),
  `${backend.drawings.length} stored`
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
  // Three panes in this fixture share ETHUSDT/5m, so the stream is not the
  // closed pane's to take: the hub counts readers, and the survivors are still
  // readers. "One socket per pane" would fail here by closing a channel the
  // remaining charts are drawing from.
  check(
    "a chart that shared its series leaves the shared channel running",
    openSocketsFor("/ws/market/").length === marketBeforeClose,
    `${marketBeforeClose} open before, ${openSocketsFor("/ws/market/").length} after`
  );
  check(
    "and the add button comes back once there is room again",
    splitButton.disabled === false,
    splitButton.disabled ? "still disabled" : "enabled"
  );

  // The other half of the refcount: the first pane is the page's only BTCUSDT
  // chart, so closing it is the last reader leaving and the channel goes.
  const uniqueBefore = openSocketsFor("/ws/market/").length;
  paneNode(0).querySelector(".close").click();
  await settle();
  await settle();
  check(
    "and a chart whose series no one else held closes that channel",
    openSocketsFor("/ws/market/").length === uniqueBefore - 1,
    `${uniqueBefore} open before, ${openSocketsFor("/ws/market/").length} after`
  );

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

// --- the agent's answer ------------------------------------------------------
//
// No check in this file had ever opened the agent channel, so the one path that
// *draws* an answer had never run under any guard. It shipped calling `draw()` --
// which is one pane's own function -- from page scope, so every answer threw a
// `ReferenceError` at the moment it arrived: the transcript was written and then
// the chart was never touched. Found in a browser console, not by a test.

console.log("\nthe agent's answer");

const questionBox = document.getElementById("question");
const askButton = document.getElementById("ask");
const agentTextBefore = painted.text.length;

questionBox.value = "find me a long setup";
askButton.click();
await settle();

const agentSocket = socketsFor("/ws/agent").at(-1);
check(
  "asking opens the agent channel",
  Boolean(agentSocket),
  `${socketsFor("/ws/agent").length} agent sockets`
);

// The thesis is drawn by the pane whose symbol it is about, so the frame has to
// name the instrument the chart is actually showing.
const askedSymbol = paneNode().querySelector(".symbol").value;
deliver(agentSocket, {
  type: "progress",
  payload: { stage: "tool", name: "analyze_timeframe" },
});

// The same shape `POST /agent/ask` returns -- `Frame::Data` carries
// `to_response(answer)`, so `payload.thesis` is at the top level and not under
// an `answer` key. A frame shaped the other way is a second thing for the shell
// and the route to disagree about.
// Caught rather than left to propagate: a throw inside `onmessage` escapes
// `deliver` and takes the whole harness down, so an unguarded version of this
// check aborts the run instead of failing it. Naming the bug is the point.
let agentThrew = null;
try {
  deliver(agentSocket, {
    type: "data",
    payload: {
    thesis: {
      symbol: askedSymbol,
      timeframe: "5m",
      direction: "long",
      confidence_pct: 61,
      higher_timeframe_checks: [],
      order_flow_checks: [],
      entry_price: 100,
      stop_price: 99,
      target_price: 102,
      risk_reward: 2,
      // Every field `thesisHtml` reads, because a card that throws on a missing
      // one is a card the transcript cannot render at all.
      invalidation: "a close below the sweep low",
      narrative: "a long from the sweep",
      },
    },
  });
} catch (e) {
  agentThrew = e;
}

check(
  "an answer reaches the chart without throwing",
  !agentThrew,
  agentThrew ? String(agentThrew.message) : ""
);

// The answer's levels are positioned by the engine, so the frame that draws them
// is a *rebuild* -- and the pane schedules that on `requestAnimationFrame`. The
// read below has to wait for it, or it inspects the canvas as it was before the
// answer arrived. That is not a detail of the harness: it is the shape of the
// feature, and a check that skipped this wait would pass against a shell that
// never redrew at all. A fixed 30ms nap raced the rAF whenever the machine was
// loaded -- the redraw is scheduled, not immediate -- so this waits for the
// drawn labels themselves, with a generous cap that only a genuinely broken
// redraw can reach.
for (
  let waited = 0;
  waited < 2000 &&
  !painted.text.slice(agentTextBefore).some((line) => line.startsWith("stop"));
  waited += 25
) {
  await new Promise((resolve) => setTimeout(resolve, 25));
}

const drawn = painted.text.slice(agentTextBefore);
const activePaneSymbol = () => paneNode().querySelector(".symbol").value;
check(
  "the answer's levels are drawn on the chart, not only written in the panel",
  drawn.some((line) => line.startsWith("stop")),
  `drew ${drawn.length} labels: ${drawn.join(" / ")}`
);
// All three, at the prices the answer gave. The engine positions them, so this
// also proves the shell sent prices rather than pixels: a shell that mapped them
// itself would have to reproduce the engine's price scale exactly, and the
// numbers below would drift away from the axis ticks on the next resize.
check(
  "and it is the engine that put them there, at the prices the answer gave",
  ["stop 99.00", "entry 100.00", "target 102.00"].every((label) =>
    drawn.includes(label)
  ),
  `looking for the answer's three levels; drew ${drawn.join(" / ")}`
);
// The thesis is drawn by the pane whose symbol it is about, and by no other. With
// a second chart on the page this is the difference between a level and a level
// that means nothing -- a BTCUSDT entry is not a price on an ETHUSDT chart.
check(
  "and it is drawn by the pane the answer was about, not by every pane",
  activePaneSymbol() === askedSymbol,
  `the pane reads ${activePaneSymbol()}, the answer was about ${askedSymbol}`
);
// The architectural half: what the shell *sends* is prices, and the engine
// answers with pixels. Asserted on the request and the scene rather than on the
// drawn labels, because the labels are the same two numbers either way -- a
// shell that inverted the price scale itself would look identical on the canvas
// and diverge the moment the axis moved.
{
  const sentOverlays = engine.requests.at(-1)?.overlays ?? null;
  const sceneOverlays = engine.frames.at(-1)?.scene?.overlays ?? null;
  check(
    "and the shell sends prices, because it has no price scale of its own",
    Array.isArray(sentOverlays) &&
      sentOverlays.length > 0 &&
      sentOverlays.every((o) => typeof o.price === "number" && !("y" in o)),
    JSON.stringify(sentOverlays)
  );
  check(
    "and the scene answers with canvas coordinates",
    Array.isArray(sceneOverlays) &&
      sceneOverlays.length > 0 &&
      sceneOverlays.every((o) => typeof o.y === "number" && typeof o.price === "number"),
    JSON.stringify(sceneOverlays)
  );
  // The band: the engine orders the two edges, so the shell never has to know
  // whether a short's stop is above or below its entry.
  const banded = (sceneOverlays ?? []).filter((o) => o.band_y !== null && o.band_y !== undefined);
  check(
    "and the stop-to-target band arrives with both edges resolved",
    banded.length === 1,
    `${banded.length} of ${(sceneOverlays ?? []).length} carry a band`
  );
}

const thesisPanel = document.getElementById("thesis");
check(
  "and the panel says what the answer was",
  thesisPanel.textContent.includes("long"),
  thesisPanel.textContent.slice(0, 60)
);

check(
  "and the working state is over, so the button can be used again",
  !askButton.disabled
);

// --- the chart the question was asked about ---------------------------------

console.log("\nthe chart attached to a question");

/// The shell's own screenshot budget, read out of its source.
///
/// Read rather than repeated: a second copy of the number here would agree with
/// itself forever while the shell and the server drifted apart, which is the one
/// thing this check exists to notice. Falls back to the server's value so a
/// rename in the shell shows up as a failed check rather than a crash.
const MAX_SCREENSHOT_BYTES_JS = (() => {
  const match = /const MAX_SCREENSHOT_BYTES = ([0-9 *]+);/.exec(shell);
  if (!match) return 4 * 1024 * 1024;
  // eslint-disable-next-line no-eval -- a literal this file just matched.
  return eval(match[1]);
})();

/// What the shell last put on the agent socket, parsed.
const lastAsk = () => {
  const socket = socketsFor("/ws/agent").at(-1);
  const frames = (socket?.sent ?? []).map((raw) => {
    try { return JSON.parse(raw); } catch { return null; }
  });
  // The last *question*, not the last frame. A socket carries the discussion and
  // the shell reuses it, so reading by index would answer with a previous
  // question's packet the moment a second message is sent -- which is exactly
  // what the "switched off" check below needs to be able to tell apart.
  return frames.filter((frame) => frame && typeof frame.question === "string").at(-1) ?? null;
};

// Off by default. Attaching a picture of somebody's screen is a thing the user
// asks for, not a thing that happens because they typed a question.
check(
  "the attach control starts off",
  document.getElementById("attachChart").getAttribute("aria-pressed") === "false",
  document.getElementById("attachChart").getAttribute("aria-pressed")
);
check(
  "and a plain question carries no chart context at all",
  lastAsk() !== null && !("chart" in lastAsk()),
  JSON.stringify(lastAsk()?.chart ?? null)
);

// jsdom has no layout engine, so there are no pixels -- but `toDataURL` is
// stubbed above, which means the shell's *capture path* runs for real: the
// downscale decision, the `data:` URL parse, the allowlist and the byte
// arithmetic all execute, and everything after them is a plain value the packet
// carries. So this section asserts the packet from the frame the shell actually
// sent, and asserts the capture from the field that frame now carries.
document.getElementById("attachChart").click();
check(
  "switching it on reads as pressed",
  document.getElementById("attachChart").getAttribute("aria-pressed") === "true",
  document.getElementById("attachChart").getAttribute("aria-pressed")
);
check(
  "and it says what it is about to send",
  document.getElementById("attachNote").textContent.includes("viewport"),
  document.getElementById("attachNote").textContent ||
    "(nothing -- a control that sends a screenshot without saying so is the problem)"
);

questionBox.value = "what is this level?";
askButton.click();
await settle();

const withChart = lastAsk();
check(
  "a question asked with it on carries the chart context",
  withChart !== null && Boolean(withChart.chart),
  JSON.stringify(withChart?.chart ?? null)
);

// The window comes from the engine, so it is the same one the chart is drawn
// from. Compared against the *active pane's* own scene rather than
// `lastScene()`: with more than one chart on the page the recorder's last frame
// belongs to whichever pane redrew most recently, and comparing against it made
// this check fail on two correct numbers from two different charts.
const sentChart = withChart?.chart ?? {};
const activeSeries = `${selectIn(PANE, "symbol")}/${selectIn(PANE, "timeframe")}`;
const paneScene = sceneOf(activeSeries);
check(
  "the harness found the active pane's own scene",
  Boolean(paneScene),
  `looked for ${activeSeries}; frames: ${engine.frames.map((f) => f.series).join(", ")}`
);
check(
  "and the window it sends is the window the engine resolved",
  paneScene !== null &&
    sentChart.visible_from_ns === paneScene.from &&
    sentChart.visible_to_ns === paneScene.to,
  `sent ${sentChart.visible_from_ns}..${sentChart.visible_to_ns}, engine resolved ${paneScene?.from}..${paneScene?.to}`
);
check(
  "and it sends the visible price axis",
  paneScene !== null &&
    sentChart.price_low === paneScene.price_min &&
    sentChart.price_high === paneScene.price_max,
  `sent ${sentChart.price_low}..${sentChart.price_high}, engine drew ${paneScene?.price_min}..${paneScene?.price_max}`
);
check(
  "and the resolution on screen",
  sentChart.timeframe === selectIn(PANE, "timeframe"),
  `sent ${sentChart.timeframe}, the select reads ${selectIn(PANE, "timeframe")}`
);
// The whole design rule: the agent reads the window itself through the same
// service the chart is drawn from. Sending candles here would be a second source
// for one fact -- and the two would differ at the newest bar, which is the bar
// a question about "now" is about.
check(
  "and no candle data rides along",
  !("candles" in sentChart) && !("bars" in sentChart),
  `keys: ${Object.keys(sentChart).join(", ")}`
);

// The capture, asserted from the frame rather than described at the source.
// The stub above is what makes this possible: `captureChart` runs its real
// downscale-and-encode path and `screenshotFromUrl` runs its real parse, so a
// field arriving here means both of them did their job.
check(
  "and a picture of the chart goes with it",
  Boolean(sentChart.screenshot),
  JSON.stringify(sentChart.screenshot?.media_type ?? null)
);
check(
  "and the picture is one the model can actually read",
  ["image/png", "image/jpeg", "image/webp"].includes(sentChart.screenshot?.media_type),
  `${sentChart.screenshot?.media_type} -- anything else is a 422 the client could have avoided`
);
check(
  "and it carries bytes, not a data URL",
  typeof sentChart.screenshot?.data === "string" &&
    /^[A-Za-z0-9+/]+=*$/.test(sentChart.screenshot.data),
  "the API wants the base64 payload alone; sending the whole `data:` URL would be a decode error at the provider"
);
check(
  "and the packet never sends the picture as a page-sized blob",
  // The budget is in image bytes. A stub returns something tiny, so this checks
  // the *direction* of the arithmetic rather than the limit itself: a shell that
  // compared base64 characters against the byte budget, or skipped the check
  // entirely, would still pass here -- which is why the unit itself is asserted
  // separately below.
  (sentChart.screenshot?.data?.length ?? 0) * 0.75 <= MAX_SCREENSHOT_BYTES_JS,
  `${Math.round(((sentChart.screenshot?.data?.length ?? 0) * 3) / 4)} bytes against a ${MAX_SCREENSHOT_BYTES_JS} byte budget`
);

// Switching it off must actually stop it. A toggle whose off state still sends
// is worse than no toggle: the user believes their screen is private.
//
// The turn is closed first. `ask()` refuses while `asking` is true -- a second
// question during a running one would be queued against an answer nobody
// matched -- so without delivering the answer above, the click below is dropped
// and this section silently re-reads the *previous* question's frame. That is
// what it did: two questions sent, the third never left the page, and the check
// still passed by looking at the old one.
deliver(socketsFor("/ws/agent").at(-1), {
  type: "data",
  payload: { thesis: { symbol: askedSymbol, direction: "long" } },
});
await settle();

document.getElementById("attachChart").click();
check(
  "switching it off reads as unpressed",
  document.getElementById("attachChart").getAttribute("aria-pressed") === "false",
  document.getElementById("attachChart").getAttribute("aria-pressed")
);
questionBox.value = "and now?";
askButton.click();
await settle();
const asks = (socketsFor("/ws/agent").at(-1)?.sent ?? [])
  .map((raw) => {
    try { return JSON.parse(raw); } catch { return null; }
  })
  .filter((frame) => frame && typeof frame.question === "string");
check(
  "and the next question carries no chart context again",
  asks.length >= 2 && !("chart" in asks.at(-1)),
  `questions sent: ${asks.length}; last has chart: ${"chart" in (asks.at(-1) ?? {})}; ` +
    `questions: ${asks.map((f) => f.question).join(" | ")}`
);

// The capture half, which the DOM here cannot perform. These are source checks,
// and they are about units and allowlists rather than logic -- the two ways a
// capture silently stops working.
check(
  "the shell downscales a capture before sending it",
  shell.includes("MAX_SCREENSHOT_EDGE") && shell.includes("drawImage"),
  "the capture must resize rather than send the pane at its native size"
);
check(
  "and it measures the payload in image bytes, not base64 characters",
  /data\.length \* 3\) \/ 4/.test(shell) || /length \* 3\)\s*\/\s*4/.test(shell),
  "base64 is ~33% longer than the image; comparing the encoded length to a byte budget rejects a legal image"
);
check(
  "and the media types it will send are the three the server accepts",
  ["image/png", "image/jpeg", "image/webp"].every((type) => shell.includes(type)),
  "an allowlist that drifts from `SCREENSHOT_MEDIA_TYPES` is a 422 the client could have avoided"
);

// An answer the page cannot render must say so on the page. A throw inside a
// socket handler is otherwise invisible: the panel sits on "Working…" or shows
// an answer with no chart, and the only place it appears is a console nobody
// was told to open. This is the failure `draw()` had.
questionBox.value = "and now a broken one";
askButton.click();
await settle();
const brokenSocket = socketsFor("/ws/agent").at(-1);
// Caught for the same reason as above: without it, a page that throws here
// takes the harness down and the run aborts instead of failing one check.
try {
  deliver(brokenSocket, {
    type: "data",
    payload: { thesis: { symbol: askedSymbol, direction: "long" } },
  });
} catch {
  // Recorded by the check below, which is looking for the page having said so.
}
// The shell's frame handlers are async (`decodeFrame` reads Blob bodies), so a
// delivered frame lands one microtask after `deliver` returns -- the same
// settle every other delivered frame above waits for.
await settle();

check(
  "an answer the page cannot render says so, instead of hanging on Working…",
  thesisPanel.textContent.includes("could not be shown"),
  thesisPanel.textContent.slice(-90)
);
check(
  "and it does not leave the Ask button stuck",
  !askButton.disabled
);

// --- the scanner ---------------------------------------------------------------
//
// `GET /scan` shipped with eight route tests and no reader. Nothing in this file
// had ever opened the panel, so the whole surface -- the ranking, the unmeasured
// instruments, the universe the answer came from -- was unobserved, and the route
// was reachable only by hand.
//
// The checks below are about the things the response carries that a panel could
// plausibly drop without any Rust test noticing: that the ranking is drawn in the
// order given, that a symbol which could not be measured is *not* drawn as one
// that ranked last, that the summary is shown as written, and that the metric and
// timeframe the user picked are what actually went on the wire.

console.log("\nthe scanner");

const scanOut = document.getElementById("scanOut");
const scanGo = document.getElementById("scanGo");

// The tab row no longer carries Scan: the six agent tabs retired and the chat's
// environments became the front door, leaving this pane in the DOM as the
// surface that renders the last `/scan` answer. There is therefore no tab to
// click -- but the pane and its controls are the reader this section is about,
// and `selectPane` is the shell's own reveal path, so the checks drive that.
// Asserting a tab existed would assert against a design already retired.
window.selectPane("scan");
await settle();

check(
  "the scan pane exists and can be shown",
  document.getElementById("pane-scan").hidden === false,
  "the chat's Scan environment is its front door; the pane still renders the answer"
);
check(
  "and it opens saying what to do, not showing an empty table",
  scanOut.textContent.includes("Pick a measurement"),
  scanOut.textContent.slice(0, 60)
);

// The metric and timeframe the user chose are the ones that have to go out. Both
// are read from the DOM rather than set from the fixture, because a shell that
// hardcoded either -- or that sent the label instead of the wire name -- would
// render a correct-looking table under a heading that is a lie.
document.getElementById("scanMetric").value = "atr_percent";
document.getElementById("scanTimeframe").value = "4h";
document.getElementById("scanSymbols").value = "BTCUSDT,ETHUSDT";
scanGo.click();
await settle();
await settle();

const scanCall = backend.calls.filter((c) => c.url.startsWith("/scan")).at(-1);
const scanQuery = new URLSearchParams(scanCall ? scanCall.url.split("?")[1] : "");
check(
  "the metric and timeframe the user picked are what is asked for",
  scanQuery.get("metric") === "atr_percent" && scanQuery.get("timeframe") === "4h",
  scanCall ? scanCall.url : "(no scan request)"
);
check(
  "and the instruments typed into the box are sent as an explicit universe",
  scanQuery.get("symbols") === "BTCUSDT,ETHUSDT",
  scanQuery.get("symbols") ?? "(absent)"
);

// And an *empty* box must send no `symbols` at all, rather than an empty one.
//
// `symbols=` present-but-blank is a third request: `Some("")` at the route, which
// parses to an explicit universe of nothing rather than falling back to the
// venue's instruments -- so the user who cleared the box would get an empty
// ranking instead of everything, and the panel would report `universe: explicit`
// while showing none.
document.getElementById("scanSymbols").value = "";
scanGo.click();
await settle();
await settle();
const cleared = backend.calls.filter((c) => c.url.startsWith("/scan")).at(-1);
check(
  "and an empty box asks about the venue, sending no symbols at all",
  cleared && !new URLSearchParams(cleared.url.split("?")[1]).has("symbols"),
  cleared ? cleared.url : "(no scan request)"
);
check(
  "and the server's own summary is shown, not a sentence invented here",
  scanOut.textContent.includes("3 instruments measured on 1h by RSI"),
  scanOut.textContent.slice(0, 90)
);

// The *ranked* table only. `table.scan` matches the unmeasured one too, and a
// selector that caught both would make the check below pass while the panel drew
// a symbol it could not measure in rank order -- which is the exact defect it
// names. `table.scan` outside the `details` is the ranking.
const scanRows = () => {
  const ranked = [...scanOut.querySelectorAll("table.scan")].find((t) => !t.closest("details"));
  return ranked ? [...ranked.querySelectorAll("tbody tr")] : [];
};
check(
  "the ranking is drawn, one row per measured instrument",
  scanRows().length >= backend.scan.rows.length,
  `${scanRows().length} rows`
);
check(
  "and in the order the server ranked them, not re-sorted here",
  scanRows().length > 0 && scanRows()[0].textContent.includes("BTCUSDT"),
  scanRows().length ? scanRows()[0].textContent.trim().slice(0, 40) : "(no rows)"
);
check(
  "and the value under it is that instrument's own, in the server's order",
  scanRows().length > 0 && scanRows()[0].textContent.includes("43.50"),
  scanRows().length ? scanRows()[0].textContent.trim().slice(0, 40) : "(no rows)"
);

// The failure case, which is what the fixture exists for. A symbol that could not
// be measured must not appear as a ranked row: "we could not measure this" and
// "this ranked last" are different claims, and drawing the first as the second
// states something false.
check(
  "a symbol that could not be measured is not shown as one that ranked last",
  !scanRows().some((r) => r.textContent.includes("XRPUSDT")),
  scanRows().map((r) => r.textContent.slice(0, 12).trim()).join(" | ")
);
check(
  "and it is kept, with its own reason, rather than dropped",
  scanOut.textContent.includes("XRPUSDT") && scanOut.textContent.includes("no candles for 1h"),
  "an instrument the scan could not measure is the answer to why it is not on the list"
);

// --- the order book -----------------------------------------------------------
//
// Nothing in this file had ever looked at the book panel, so the three ways its
// channel can end were all unobserved -- and the live log showed they matter:
// the feed is healthy and the books sync, yet a user can still see "WebSocket is
// closed before the connection is established" over an empty ladder.
//
// The closes are (a) we aborted it by moving panes, (b) the server closed it
// after DEPTH_GRACE with a notice explaining why, and (c) it failed outright.
// They must produce three different outcomes, because one generic sentence for
// all of them is how a healthy book reads as a broken one.
//
// Driven through the DOM like every other check here: `bookMsg` is the panel's
// own element, and the pane's symbol select is how a user moves the book.

console.log("\nthe order book");

const bookMsg = document.getElementById("bookMsg");
/// Every message the book panel was made to show, in order.
///
/// Read by `inOne` below rather than read after the fact, because a post-hoc read
/// cannot see an abort: a later close overwrites the panel, and the state the
/// guard protects is gone by the time a check looks at it. Captured here, at the
/// instant of the write, is the only place the abort is observable at all.
const __log = [];
// `window.MutationObserver`, not a bare `MutationObserver`: this file is an ES
// module, and jsdom's globals live on the `window` it was given rather than in
// this scope. A bare reference threw `ReferenceError` -- which the guard below
// turned into a silent skip, so the observer simply never ran and every check
// reading `__log` failed against an empty array while the panel clearly held the
// right text. The guard is now on the thing that actually exists.
if (typeof window.MutationObserver === "function") {
  new window.MutationObserver(() => __log.push(bookMsg.textContent)).observe(bookMsg, {
    childList: true,
    characterData: true,
    subtree: true,
  });
}

/// Every book socket opened so far, oldest first.
const bookSockets = () => socketsFor("/ws/orderbook");
/// The sockets still open for the book channel.
const openBooks = () => openSocketsFor("/ws/orderbook");

/// Fail unless `run()` produces exactly the `want` list, in order.
///
/// Every check in this section asserts on *every* close, not on the last one, and
/// the reason is the harness itself: a socket's `onclose` is synchronous here but
/// a browser's is a task, and a socket opened microseconds before its close can
/// therefore land in either order. "The last message is right" does not survive
/// that. "These two messages were produced, each exactly once, and neither is the
/// wrong one" does -- because a duplicate is the only shape the reordering could
/// hide, and it is what an unguarded shell actually produces.
const inOne = async (what, run, want) => {
  __log.length = 0;
  await run();
  // The observer is a callback, not a synchronous hook: jsdom delivers it on the
  // microtask queue, so reading `__log` on the line after `run()` returns would
  // read it before the write that `run()` just made. This is the same trap
  // `settle` exists for on the socket side.
  await Promise.resolve();
  const seen = [...__log];
  const missing = want.filter((w) => !seen.includes(w));
  const extra = seen.filter((s) => !want.includes(s));
  check(
    what,
    missing.length === 0 && extra.length === 0,
    `wanted [${want.join(", ")}]; got [${seen.join(", ")}]`
  );
};

/// Reopen the book channel from a known state, and *leave a socket open behind
/// it* for the next check to abort.
///
/// The book follows the active pane, and the two gestures that put a pane in the
/// book's path are a pointer press (which makes that pane active) and a `change`
/// on that pane's own symbol select (which is what `onSymbolChange` is wired to).
///
/// Both are needed, and each was learned from a failing check:
///
/// * Without the press, the select belongs to a pane that is not active, so
///   `onSymbolChange`'s `which === activePane` is false and `connectBook` never
///   runs -- the whole section would have been inert.
/// * Without the select changing to a *different* instrument, `setActive` skips
///   the reconnect (`moved` is false for a same-symbol move), so a check that
///   wanted a superseded socket would find the same one still open and pass
///   without the guard ever being read.
///
/// `setActive` runs on a capture-phase listener, so the press has taken effect by
/// the time the dispatch returns -- no waiting between the two.
///
/// One pane, because the multi-chart section closes back down to one before this
/// runs: there is no second select to move, and the press has to address the pane
/// that exists rather than the one the earlier section happened to end on.
async function reset() {
  pickTool("cursor", 0);
  pointer("pointerdown", 400, 200, 0);
  pointer("pointerup", 400, 200, 0);
  const select = paneNode(0).querySelector(".symbol");
  const to = select.value === "BTCUSDT" ? "ETHUSDT" : "BTCUSDT";
  if (![...select.options].some((o) => o.value === to)) {
    const option = document.createElement("option");
    option.value = to;
    select.appendChild(option);
  }
  select.value = to;
  change(select);
  await settle();
  // Open, and still connecting. Its `onclose` is either the abort below or the
  // harness's shutdown -- never one the server sent, which is the whole
  // distinction this section is about.
  return openBooks().at(-1);
}

// (c) A refused handshake. The panel must not sit on "Connecting…" forever.
//
// This is the one close the shell has no message for: no `notice` arrived, and
// nothing asked for the abort, so the honest reading is "the book disconnected"
// -- a sentence that is unhelpful but true. What it must *not* be is silence.
socketBehaviour.refuse = (url) => url.includes("/ws/orderbook/");
await inOne(
  "a refused book socket does not leave the panel on Connecting…",
  async () => {
    await reset();
    await settle();
  },
  ["The book disconnected."]
);
socketBehaviour.refuse = null;

// (a) A close we asked for is not news.
//
// Moving the book off a socket aborts it, and that abort is the literal cause of
// the browser error this section exists for: "WebSocket is closed before the
// connection is established" is what a socket that never finished its handshake
// reports when something closes it. An unguarded shell answers that by telling
// the user their book disconnected -- on every instrument change, over a book
// that is perfectly fine.
//
// The new socket is left *connecting* (no frame delivered), so it stays silent in
// the log: this read is the abort and nothing else.
const superseded = await reset();
await inOne(
  "a close we asked for is reported as no close at all",
  async () => {
    await reset();
  },
  []
);
check(
  "and the socket that was moved off really was closed",
  superseded.closed === true && !openBooks().includes(superseded),
  superseded.closed ? "closed" : "still open"
);

// (b) The server closing with an explanation. `/ws/orderbook` gives up after
// `DEPTH_GRACE` and says why, and that notice is the only sentence naming the real
// cause -- either `MARKET_FEED` is unset or the book has not finished syncing.
// Overwriting it with "disconnected" throws away the one useful thing the
// exchange carried and leaves the user with a healthy feed and no explanation.
await reset();
const noticeText =
  "no order book for BTCUSDT arrived within 5s. The feed subscribes depth alongside " +
  "trades, so this means either no market feed is configured (MARKET_FEED) or the " +
  "book has not finished syncing.";
await inOne(
  "a book the server closed keeps the server's own explanation",
  async () => {
    const noticeSocket = openBooks().at(-1);
    deliver(noticeSocket, { type: "notice", message: noticeText });
    // The frame first, then the close -- the order a browser delivers them in.
    // Delivered and closed in one synchronous block, both writes land in the
    // same MutationObserver batch and only the last text is ever seen, which
    // made an unguarded `onclose` look correct.
    await settle();
    noticeSocket.close();
    await settle();
  },
  [noticeText]
);

// The retry. `/ws/orderbook` closes after its grace period whenever a book has
// not synced yet, and the live log shows books arriving 8-29 diffs later -- so a
// book that was merely late has to come back on its own rather than needing a
// page reload. Nothing else in this file would notice if it never did: every
// other check here is about a message, and this one is about a socket that
// reappears with no gesture behind it.
await reset();
await settle();
const retrySymbol = openBooks().at(-1).url.split("/").pop();
const retryBefore = bookSockets().length;
openBooks().at(-1).close();
await settle();
// Past `BOOK_RETRY_MS` (1000ms in the shell). The retry re-checks that the pane
// has not moved, which is why nothing may touch the symbol select in between.
await new Promise((resolve) => setTimeout(resolve, 1400));
const reopened = openBooks().at(-1);
check(
  "a book channel that closed is re-opened on its own",
  bookSockets().length > retryBefore && Boolean(reopened) && reopened.url.endsWith("/" + retrySymbol),
  `${retryBefore} -> ${bookSockets().length} sockets, ${
    reopened ? reopened.url.split("/").pop() : "none open"
  }`
);

// --- why the agent channel refused -------------------------------------------
//
// A refused WebSocket handshake is opaque to script -- per spec the response
// status is not exposed -- so `onerror` cannot tell an expired token from an
// agent that was never configured, and the shell said one generic sentence for
// both. They have opposite fixes (sign in again, versus tell the operator to set
// `AWS_BEDROCK_*`), so reporting one as the other sends the user at the wrong
// problem.
//
// The cached socket has to go before a refusal can be observed at all.
//
// `ensureAgentSocket` returns the *same* promise once it has opened one, so
// everything above has left a live agent socket behind and a new question would
// reuse it -- the refuse predicate would never be consulted, no `onerror` would
// fire, and the checks below would read a panel that just says "Working…"
// forever while passing for the wrong reason. Closing the live socket is what
// the shell's own `onclose` treats as "this one is gone": it nulls `agentReady`.
//
// This is also the only way to make the refusal path reachable twice in one run,
// which matters because the two checks below are the same failure mode with two
// different causes.
const dropAgentSocket = async () => {
  const live = socketsFor("/ws/agent").filter((s) => !s.closed).at(-1);
  if (live) live.close();
  // `asking` may still be true from the close above; clear it by finishing that
  // turn, or the next `ask()` returns early and never opens anything.
  await new Promise((resolve) => setTimeout(resolve, 30));
};

/// The text the panel ended up showing for the last turn.
///
/// Read from the last `.turn` rather than from the whole panel: the transcript
/// keeps every earlier question, so `textContent.includes(...)` over the lot
/// would match an answer from several sections ago and pass against a shell that
/// said nothing about this one. The last turn is the only one that can be the
/// answer to the question just asked.
const lastTurnText = () => {
  const cards = thesisPanel.querySelectorAll(".turn");
  return cards.length ? cards[cards.length - 1].textContent : "";
};

// Not configured. The capabilities route answers this without a valid token,
// which is why the shell asks it first -- and the deployment-level answer beats
// the credential one: a user who reads "sign in again" when the server has no
// Bedrock credentials will sign in forever.
await dropAgentSocket();
backend.agentCapability = "not_configured";
socketBehaviour.refuse = (url) => url.includes("/ws/agent/");
questionBox.value = "is the agent there?";
askButton.click();
await new Promise((resolve) => setTimeout(resolve, 80));

check(
  "an agent that is not configured says so, rather than blaming the connection",
  lastTurnText().includes("not configured"),
  lastTurnText().slice(-90)
);
check(
  "and it asks the server before answering, instead of guessing from the socket",
  backend.calls.some((c) => c.url.startsWith("/capabilities")),
  "a refused handshake carries no status, so the reason has to come from somewhere"
);

// Configured, and the credential is the thing that failed. The same refusal must
// produce the *other* message -- otherwise "not configured" is just a second
// generic sentence wearing the first one's words, and the shell has learned
// nothing about why the handshake was refused.
await dropAgentSocket();
backend.agentCapability = "ready";
backend.meStatus = 401;
questionBox.value = "and now?";
askButton.click();
await new Promise((resolve) => setTimeout(resolve, 80));

check(
  "a refused credential is reported as a credential, not as a missing agent",
  lastTurnText().includes("refused") && !lastTurnText().includes("not configured"),
  lastTurnText().slice(-90)
);

socketBehaviour.refuse = null;
backend.meStatus = 200;

// --- the parity tools and the global toolbar ---------------------------------
//
// The two features this session added to the shell, checked the way the rest
// of this file checks things: through the page's own DOM and its own network
// stub, never by reaching into internals.

console.log("\nthe global toolbar and the parity tools");

check(
  "a standalone toolbar exists outside the panes",
  Boolean(document.getElementById("globalTools")),
  document.getElementById("globalTools") ? "present" : "absent"
);
check(
  "and it is built from the engine's registry, not empty",
  document.querySelectorAll("#globalTools button[data-tool]").length === registry().length + 1,
  `${document.querySelectorAll("#globalTools button[data-tool]").length} buttons for ${registry().length} rows (+ cursor)`
);
check(
  "and it names the chart it acts on",
  document.getElementById("globalToolsWhich")?.textContent.includes("BTCUSDT"),
  document.getElementById("globalToolsWhich")?.textContent ?? "(no label)"
);
check(
  "and its pressed state mirrors the active pane's tool",
  (() => {
    const panePressed = paneNode(0).querySelector('.tools button[aria-pressed="true"]');
    const globalPressed = document.querySelector('#globalTools button[aria-pressed="true"]');
    return panePressed && globalPressed && panePressed.dataset.tool === globalPressed.dataset.tool;
  })(),
  `${paneNode(0).querySelector('.tools button[aria-pressed="true"]')?.dataset.tool} vs ${
    document.querySelector('#globalTools button[aria-pressed="true"]')?.dataset.tool
  }`
);

// A click on the *global* row arms the tool on the active pane -- that is the
// whole point of a second toolbar: two surfaces, one state.
globalPick("rect");
check(
  "picking a tool on the global row arms it on the active pane",
  paneNode(0).querySelector('.tools button[data-tool="rect"]').getAttribute("aria-pressed") ===
    "true",
  paneNode(0).querySelector('.tools button[data-tool="rect"]').getAttribute("aria-pressed")
);

// A three-point tool takes three clicks, and the stored row carries a third
// point the engine resolved. This is the parity gesture end to end -- and the
// click count comes from the registry's `anchors`, not from a number the shell
// remembers, so a tool the engine changed cannot be placed with the wrong count.
const threePointKind = registry().find((row) => row.anchors?.form === "fixed" && row.anchors.points === 3)?.kind;
check(
  "the registry declares a three-point tool to place",
  Boolean(threePointKind),
  registry().map((row) => `${row.kind}:${JSON.stringify(row.anchors)}`).join(" ")
);
const drawingsBeforeThree = backend.drawings.length;
pickTool(threePointKind);
pointer("pointerdown", 300, 200);
pointer("pointerup", 300, 200);
pointer("pointermove", 420, 200);
pointer("pointerdown", 420, 200);
pointer("pointerup", 420, 200);
pointer("pointermove", 360, 120);
pointer("pointerdown", 360, 120);
pointer("pointerup", 360, 120);
await settle();
await settle();
const triangle = backend.drawings[backend.drawings.length - 1];
check(
  "a three-point tool stores exactly one drawing after three clicks",
  backend.drawings.length === drawingsBeforeThree + 1,
  `${backend.drawings.length - drawingsBeforeThree} stored`
);
check(
  "and the stored row carries all three points, absolute",
  triangle &&
    triangle.points.length === 3 &&
    triangle.points.every((p) => p.unit === "absolute") &&
    triangle.points[0].time !== triangle.points[1].time &&
    triangle.points[1].time !== triangle.points[2].time,
  triangle && JSON.stringify(triangle.points)
);

// A two-point drag must still work -- and must send exactly two points, because
// the engine refuses a kind given a point it does not take.
const drawingsBeforeTwo = backend.drawings.length;
pickTool("trendline");
pointer("pointerdown", 180, 280);
pointer("pointermove", 520, 130);
pointer("pointerup", 520, 130);
await settle();
await settle();
const trendline = backend.drawings[backend.drawings.length - 1];
check(
  "a two-point drag still stores, with no third point sent",
  backend.drawings.length === drawingsBeforeTwo + 1 &&
    trendline && trendline.points.length === 2,
  trendline && `points: ${JSON.stringify(trendline.points)}`
);

// Back to a neutral state for whatever reads the page after this file.
pointer("pointerdown", 400, 300, 0);
pointer("pointerup", 400, 300, 0);
await settle();

// --- the wall -----------------------------------------------------------------
//
// The row layout stops at four because two rows of two is what stays readable.
// The wall is the other answer to the same question: the user picks the column
// count and the rows accumulate, scrolling both ways, up to a hundred charts.
//
// Three claims make it a wall and not just a bigger split: the cells are direct
// children of one grid (no row wrappers, no row splitters), the column count is
// the user's and survives a reload, and the cap is a hundred counted by the
// control rather than a number in a comment. The fourth is the one a hundred
// charts makes necessary -- offscreen panes must give up their streams, or the
// wall is a hundred live channels.

console.log("\nthe wall");

const MAX_GRID = 100;
const layoutModeSelect = document.getElementById("layoutMode");
const columnsSelect = document.getElementById("chartCols");
const addChart = document.getElementById("split");
const chartsWall = document.getElementById("charts");
const splitOption = layoutModeSelect.querySelector('option[value="split"]');

check(
  "the page ships in the row layout, with each chart inside a row wrapper",
  chartsWall.classList.contains("gridLayout") === false &&
    paneNode(0).parentElement.classList.contains("chartRow"),
  paneNode(0).parentElement.className
);

layoutModeSelect.value = "grid";
change(layoutModeSelect);
await settle();

check(
  "choosing Grid turns the chart area into one wall",
  chartsWall.classList.contains("gridLayout") === true,
  chartsWall.className
);
check(
  "and its cells are direct children, with no row wrappers left over",
  paneNodes().length === 1 &&
    paneNode(0).parentElement === chartsWall &&
    chartsWall.querySelectorAll(".chartRow").length === 0 &&
    chartsWall.querySelectorAll(".row-splitter").length === 0,
  `${chartsWall.querySelectorAll(".chartRow").length} rows, ` +
    `${chartsWall.querySelectorAll(".row-splitter").length} splitters`
);
check(
  "and the column control appears, because a wall has columns to choose",
  columnsSelect.hidden === false,
  columnsSelect.hidden ? "hidden" : "shown"
);

// The column count is a custom property on the container rather than a class per
// count: the CSS rule consumes the number, so the only observable is the number.
columnsSelect.value = "4";
change(columnsSelect);
await settle();
check(
  "the wall takes the column count the user picks",
  chartsWall.style.getPropertyValue("--chart-cols") === "4",
  `--chart-cols: ${chartsWall.style.getPropertyValue("--chart-cols") || "(unset)"}`
);
check(
  "and the choice is written down, so a reload opens the same wall",
  JSON.parse(window.localStorage.getItem("atp.chartLayout") || "{}").columns === 4,
  window.localStorage.getItem("atp.chartLayout")
);

// Five charts: one more than the row layout can hold. In the wall that is a
// fifth cell; in split it was a disabled button and a stopped page.
for (let i = 0; i < 4; i += 1) {
  addChart.click();
  await settle();
}
await settle();

check(
  "adding in the wall appends a cell, not a row",
  paneNodes().length === 5 && paneNodes().every((p) => p.parentElement === chartsWall),
  `${paneNodes().length} panes, ${chartsWall.querySelectorAll(".chartRow").length} rows`
);
check(
  "and five charts fit where the row layout stops at four",
  paneNodes().length === 5 && addChart.disabled === false,
  `${paneNodes().length} panes, add ${addChart.disabled ? "disabled" : "enabled"}`
);
check(
  "and the split option says why it cannot take them, instead of dropping four",
  splitOption.disabled === true,
  splitOption.disabled ? "disabled" : "still offered"
);
check(
  "and the add control names the wall's own cap, not the row layout's",
  addChart.title.includes(`(5/${MAX_GRID})`),
  addChart.title
);

layoutModeSelect.value = "split";
change(layoutModeSelect);
await settle();
check(
  "switching back to rows is refused while the wall holds more than four",
  chartsWall.classList.contains("gridLayout") === true && paneNodes().length === 5,
  `${paneNodes().length} panes, grid ${chartsWall.classList.contains("gridLayout")}`
);

// Closing back to four is the one change that makes the rows legal again.
// Guarded, not asserted: under the "wall caps at four" mutation the fifth pane
// never exists, and reaching into `paneNode(4)` would throw and end the run --
// a crash reports less than a failing check, and the cap check below is the one
// that names that defect.
const fifthPane = paneNode(4);
if (fifthPane) fifthPane.querySelector(".close").click();
await settle();
check(
  "closing one chart re-offers the layout that can hold the rest",
  splitOption.disabled === false,
  splitOption.disabled ? "still disabled" : "offered"
);

layoutModeSelect.value = "split";
change(layoutModeSelect);
await settle();
check(
  "and taking it rebuilds two-column rows around the charts that remain",
  chartsWall.classList.contains("gridLayout") === false &&
    chartsWall.querySelectorAll(".chartRow").length === 2 &&
    paneNodes().length === 4 &&
    paneNodes().every((p) => p.parentElement.classList.contains("chartRow")),
  `${chartsWall.querySelectorAll(".chartRow").length} rows, ${paneNodes().length} panes`
);
check(
  "and the column control goes away with the grid it describes",
  columnsSelect.hidden === true,
  columnsSelect.hidden ? "hidden" : "shown"
);

// --- what a hundred charts may not cost --------------------------------------
//
// Back to the wall for the two claims that only matter once it is big. The
// visibility gate first: a pane that scrolls out of view gives up its market
// channel and its per-frame rebuild, and comes back with both when it returns.
// The stream is page-level, so this is observable as one socket closing -- the
// pane itself and its DOM stay exactly where they were.

layoutModeSelect.value = "grid";
change(layoutModeSelect);
await settle();

const watchedPane = paneNode(3);
const watchedSymbol = watchedPane.querySelector(".symbol");
// ETHUSDT is the fixture's second instrument, and every other cell here reads
// BTCUSDT, so this pane is the only reader of its series -- which is what makes
// the channel closing a statement about *this* pane rather than about a
// neighbour still holding it.
watchedSymbol.value = "ETHUSDT";
change(watchedSymbol);
await settle();
await settle();
await settle();

const watchedKey = "/ws/market/ETHUSDT/5m";
const watchedOpen = () => openSocketsFor(watchedKey).length;
check(
  "the pane chosen to leave the screen holds a market channel of its own",
  watchedOpen() === 1,
  `${watchedOpen()} open for ${watchedKey}`
);

const heard = window.__intersect(watchedPane, false);
await settle();
await settle();
check(
  "the page's observer is watching the wall, so leaving the screen is a fact it knows",
  heard >= 1,
  `${heard} observer(s) heard the change`
);
check(
  "a pane that scrolls off screen keeps its DOM but gives up its channel",
  paneNodes().length === 4 && watchedOpen() === 0,
  `${paneNodes().length} panes, ${watchedOpen()} open for ${watchedKey}`
);
const pausedBadge = watchedPane.querySelector(".feedStatus");
check(
  "and it says it is paused rather than calling a deliberate close a failure",
  pausedBadge.textContent.includes("paused") && pausedBadge.dataset.state === "idle",
  `${pausedBadge.dataset.state}: ${pausedBadge.textContent}`
);

// The other half of "gives up": no scene is rebuilt for a cell nobody sees. A
// wheel is the strongest gesture to try, because it always repaints when visible.
const framesBeforePausedWheel = engine.frames.length;
wheel(3, 120);
await settle();
await settle();
check(
  "and a gesture on it does not rebuild a scene nobody can see",
  engine.frames.length === framesBeforePausedWheel,
  `${engine.frames.length - framesBeforePausedWheel} scene(s) built`
);

window.__intersect(watchedPane, true);
await settle();
await settle();
await settle();
check(
  "scrolling back into view reopens a channel of its own",
  watchedOpen() === 1,
  `${watchedOpen()} open for ${watchedKey}`
);
check(
  "and the badge stops saying paused",
  !watchedPane.querySelector(".feedStatus").textContent.includes("paused"),
  watchedPane.querySelector(".feedStatus").textContent
);

// The cap. Driving to it is the only way to see the control stop; a number in a
// title proves what the page *says*, not what it does. The loop is bounded by
// the cap itself, so a page that never disables the button fails the run rather
// than hanging it.
let guard = 0;
while (!addChart.disabled && paneNodes().length < MAX_GRID && guard < MAX_GRID + 10) {
  addChart.click();
  guard += 1;
}
await settle();
await settle();
await settle();

check(
  "the wall stops at a hundred charts",
  paneNodes().length === MAX_GRID && addChart.disabled === true,
  `${paneNodes().length} panes, add ${addChart.disabled ? "disabled" : "enabled"}`
);
check(
  "and the control says so rather than quietly doing nothing",
  addChart.title.includes(`(${MAX_GRID}/${MAX_GRID})`),
  addChart.title
);
const pastCap = window.addPane();
check(
  "and the guard refuses a hundred-and-first even when called directly",
  pastCap === null && paneNodes().length === MAX_GRID,
  `${paneNodes().length} panes`
);

// Every cell added in the wall cloned the pane the user was on, so they share
// its series. The hub counts readers, not panes: a hundred charts on a handful
// of series is a handful of sockets, and the whole wall costs what the series
// cost, not what the cells cost.
const streamsAtCap = openSocketsFor("/ws/market/").length;
check(
  "a hundred charts do not open a hundred market streams",
  streamsAtCap > 0 && streamsAtCap <= 5,
  `${streamsAtCap} stream(s) for ${paneNodes().length} charts`
);
check(
  "and the wall still has its cells as direct children at the cap",
  paneNodes().every((p) => p.parentElement === chartsWall) &&
    chartsWall.querySelectorAll(".chartRow").length === 0,
  `${chartsWall.querySelectorAll(".chartRow").length} rows`
);

// --- the command palette -----------------------------------------------------
//
// The palette is the shell's keyboard door: the only way to reach a symbol or
// a saved chat without walking the frame. It is driven here the way a keyboard
// drives it (a document-level event, a typed query, Enter), because a click
// would pass even if the key handler were never bound.

console.log("\nthe command palette");

document.dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "k",
  metaKey: true,
  bubbles: true,
  cancelable: true,
}));

const palette = document.getElementById("palette");
check(
  "the palette opens on its shortcut",
  Boolean(palette && palette.hasAttribute("open")),
  palette ? `open=${palette.hasAttribute("open")}` : "no #palette"
);

const paletteTitles = () =>
  [...document.querySelectorAll("#paletteList .paletteGroup__title")].map((node) => node.textContent);
const openingTitles = paletteTitles();
check(
  "and opens as a launcher, not a search",
  openingTitles.join(",") === "Navigate,Actions",
  openingTitles.join(", ") || "(none)"
);
check(
  "with the first destination selected",
  (document.querySelector('#paletteList [role="option"][aria-selected="true"]')?.textContent || "")
    .includes("Dashboard"),
  document.querySelector('#paletteList [role="option"][aria-selected="true"]')?.textContent || "(none)"
);
check(
  "the count is announced",
  (document.getElementById("paletteStatus").textContent || "").endsWith("commands"),
  document.getElementById("paletteStatus").textContent
);

const paletteInput = document.getElementById("paletteInput");
paletteInput.value = "liquidity";
paletteInput.dispatchEvent(new window.Event("input", { bubbles: true }));
await settle();

const filteredTitles = paletteTitles();
check(
  "typing filters and offers the user's saved chats",
  filteredTitles.includes("Chats"),
  filteredTitles.join(", ") || "(none)"
);

const chatOption = [...document.querySelectorAll('#paletteList [role="option"]')]
  .find((node) => node.textContent.includes("Liquidity sweep"));
check(
  "with the workspace the API listed",
  Boolean(chatOption),
  chatOption ? chatOption.textContent : "(none)"
);
if (chatOption) {
  chatOption.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  await settle();
  check(
    "and selecting one deep-links into Studio",
    window.location.hash === "#/studio/ws-liquidity",
    window.location.hash
  );
}

document.dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "/",
  metaKey: true,
  bubbles: true,
  cancelable: true,
}));
const sheet = document.getElementById("shortcuts");
check(
  "the shortcut sheet opens on its shortcut",
  Boolean(sheet && sheet.hasAttribute("open")),
  sheet ? `open=${sheet.hasAttribute("open")}` : "no #shortcuts"
);
const sheetClose = sheet && sheet.querySelector("[data-close]");
if (sheetClose) sheetClose.click();
check(
  "and closes from its own button",
  Boolean(sheet) && !sheet.hasAttribute("open"),
  sheet ? `open=${sheet.hasAttribute("open")}` : "no #shortcuts"
);

document.dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "1",
  code: "Digit1",
  altKey: true,
  bubbles: true,
  cancelable: true,
}));
check("Alt+1 goes to Dashboard", window.location.hash === "#/dashboard", window.location.hash);

document.dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "2",
  code: "Digit2",
  altKey: true,
  bubbles: true,
  cancelable: true,
}));
check("and Alt+2 comes back to Charts", window.location.hash === "#/charts", window.location.hash);

document.dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "4",
  code: "Digit4",
  altKey: true,
  bubbles: true,
  cancelable: true,
}));
await settle();
check(
  "and Alt+4 is the fourth thing on the bar: a panel, over the view it lands on",
  panelEl().open === true && panelShown("community") &&
    window.location.hash === "#/charts/community",
  `open=${panelEl().open} ${window.location.hash}`
);

document.dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "6",
  code: "Digit6",
  altKey: true,
  bubbles: true,
  cancelable: true,
}));
await settle();
check(
  "and Alt+6 is the indicator library, the sixth thing on the same bar",
  panelEl().open === true && panelShown("concepts") && !panelShown("community") &&
    window.location.hash === "#/charts/concepts",
  `open=${panelEl().open} concepts=${panelShown("concepts")} ${window.location.hash}`
);

// Last, so it covers the panels above as well: a session that ends must not
// leave a modal hanging over the home page a signed-out visitor is handed.
// The token goes rather than only the event, because `signedIn()` reads the
// token -- so the chrome answers to the session and not to the message.
const liveToken = window.localStorage.getItem(tokenKey);
window.localStorage.removeItem(tokenKey);
document.dispatchEvent(new window.CustomEvent("hrz:auth", { detail: { signedIn: false } }));
await settle();
check(
  "signing out closes the panel and every door to it",
  panelEl().open === false &&
    [...document.querySelectorAll("[data-open-panel]")].every((button) => button.disabled),
  `open=${panelEl().open} ${[...document.querySelectorAll("[data-open-panel]")]
    .map((button) => `${button.dataset.openPanel}=${button.disabled}`)
    .join(" ")}`
);
window.localStorage.setItem(tokenKey, liveToken);
document.dispatchEvent(new window.CustomEvent("hrz:auth", { detail: { signedIn: true } }));
await settle();

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
