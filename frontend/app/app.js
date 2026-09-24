// The workstation shell.
//
// docs/14's decision, recorded 2026-09-14: the chart engine is Rust compiled to
// wasm32-unknown-unknown and this file is plain JavaScript. No bundler, no
// framework, no Node in the deployment image.
//
// ## The rule this file keeps
//
// There is no arithmetic over market data here. Not one average, not one level,
// not one scale factor. The engine returns positioned rectangles and this file
// fills them. If something needs calculating it belongs in analytics-core or the
// chart engine, where it is unit-tested natively -- and docs/14 is explicit that
// a second implementation of the trading math in JavaScript must never exist.
//
// The only numbers computed below are layout: how many pixels a device pixel
// ratio needs, and where a label goes.

"use strict";

// ---------------------------------------------------------------------------
// Elements
// ---------------------------------------------------------------------------

const el = (id) => document.getElementById(id);
const TOKEN_KEY = "atp.token";

let wasm = null; // the chart engine instance, loaded once and shared

// The AI's last thesis, drawn as an overlay on a pane's price axis. Page-level
// because the conversation is: `drawThesis` maps the thesis's own prices onto
// whichever pane it is drawn on, so a thesis for one instrument on a chart of
// another would be a level that means nothing -- see `draw()` for the rule.
let thesis = null;

let bookSocket = null; // the order-book channel
let bookRetry = null; // pending re-open of that channel, see `scheduleBookRetry`
let botSocket = null; // the channel for the one bot being watched
let watchedBot = null; // its id, or null when watching none
let bots = []; // the last bot list read from the API
let botLog = []; // frames from `botSocket`, oldest first
let botNotifications = {}; // bot id -> notifications, once asked for
let notificationsOpen = null; // the bot whose notifications are shown
let venues = []; // the last venue list, so opt-in state has one source
let scan = null; // the last `GET /scan`, or null before the first one

let agentSocket = null; // the agent channel, opened on first ask
let agentReady = null; // resolves when it is open
let agentSession = null; // this tab's session id, minted once
let turns = []; // the conversation: question, steps, answer
let asking = false; // a question is in flight

/// How many live bot events to keep. A 1m bot decides once a minute, so this
/// is about an hour of history -- enough to see a pattern, bounded enough that
/// a forgotten tab does not grow forever.
const BOT_LOG_LIMIT = 60;

// ---------------------------------------------------------------------------
// Session
//
// The token lives in localStorage, which is the wrong place for a long-lived
// credential and the right place for a development tool. A production shell
// would keep it in memory with a refresh flow.
// ---------------------------------------------------------------------------

const token = () => localStorage.getItem(TOKEN_KEY) || "";

function setToken(value) {
  if (value) localStorage.setItem(TOKEN_KEY, value);
  else localStorage.removeItem(TOKEN_KEY);
  paintSession();
}

function paintSession() {
  const value = token();
  el("session").textContent = value ? "signed in" : "not signed in";
  el("signinToggle").textContent = value ? "Sign out" : "Sign in";
  applyAuthGate();
}

/// The home page is the door; the workstation is the room. A token opens it:
/// home hides, `main` un-hides. Everything else on the page lives inside
/// `main`, so this one toggle is the whole gate.
function applyAuthGate() {
  const signedIn = Boolean(token());
  const home = document.getElementById("home");
  const workspace = document.querySelector("main");
  const statusbar = document.getElementById("statusbar");
  if (home) home.hidden = signedIn;
  // The status bar belongs to the workstation -- it counts charts and ticks
  // the market's clock, both of which are dashboard furniture.
  if (statusbar) statusbar.hidden = !signedIn;
  if (workspace) {
    const wasHidden = workspace.hidden;
    workspace.hidden = !signedIn;
    // Charts measure their canvas while `main` is hidden read zero, and a
    // zero-sized canvas paints nothing when the room is finally shown. Any
    // pane that already exists re-measures and redraws now that it has size.
    if (wasHidden && signedIn) {
      for (const pane of panes) pane.redraw();
    }
  }
  // The chat hero greets the account by name; the composer chip names the
  // model that will answer. Both are account-level, so both repaint here.
  paintAiHero();
  paintModelChip();
}

/// The greeting at the top of the AI pane, and the two prompt chips under it.
/// The name comes from the signed-in email -- the handle the platform knows --
/// with the part before the @ as the friendly form.
function paintAiHero() {
  const greeting = document.getElementById("aiGreeting");
  if (!greeting) return;
  const value = token();
  if (!value) { greeting.textContent = "Welcome"; return; }
  try {
    const payload = JSON.parse(atob(value.split(".")[1] || ""));
    const email = payload.sub || payload.email || "";
    const name = email.includes("@") ? email.split("@")[0] : email;
    greeting.textContent = name ? `Welcome back, ${name}` : "Welcome back";
  } catch {
    greeting.textContent = "Welcome back";
  }
}

/// The composer's model chip: which provider answers this user's chats. The
/// same source of truth as the AI Model tab -- the stored config when there
/// is one, the platform primary otherwise.
function paintModelChip() {
  const chip = document.getElementById("aiModelChip");
  if (!chip) return;
  const custom = localStorage.getItem("atp.modelChip");
  chip.textContent = custom ? `◇ ${custom}` : "◇ Platform model";
}

/// The bottom bar's clock. Markets run on UTC, so the clock is UTC and says
/// so -- a local wall time with no name would read as an exchange time.
function tickUtcClock() {
  const clock = document.getElementById("utcClock");
  if (!clock || document.getElementById("statusbar")?.hidden) return;
  const now = new Date();
  const pad = (n) => String(n).padStart(2, "0");
  clock.textContent = `${pad(now.getUTCHours())}:${pad(now.getUTCMinutes())}:${pad(now.getUTCSeconds())} UTC`;
}
setInterval(tickUtcClock, 1000);

/// One authenticate used by both doors: the home page's card and the
/// workstation's inline strip. `register` only changes the endpoint; after
/// either succeeds the same token gates the same UI.
async function authenticate(register, emailId, passwordId, msgId) {
  const email = el(emailId).value.trim();
  const password = el(passwordId).value;
  const msg = el(msgId);
  if (!email || !password) { msg.textContent = "email and password, please"; return; }
  msg.textContent = register ? "Registering…" : "Signing in…";
  try {
    const res = await api(register ? "/auth/register" : "/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ email, password }),
    });
    setToken(res.token);
    msg.textContent = `signed in as ${res.user.email}`;
    // The drawings belong to the account, so they arrive with it. Everything
    // else on the page is public market data and was already there.
    refresh();
  } catch (e) {
    msg.textContent = e.message;
  }
}

async function api(path, options = {}) {
  const headers = { ...(options.headers || {}) };
  if (token()) headers.authorization = `Bearer ${token()}`;
  const response = await fetch(path, { ...options, headers });
  const text = await response.text();
  let body = null;
  try { body = text ? JSON.parse(text) : null; } catch { /* not JSON */ }

  if (!response.ok) {
    // docs/12's envelope: { error: { code, message, details } }.
    const error = body && body.error;
    const message = (error && (error.message || error.code)) ||
      (typeof error === "string" ? error : null) ||
      `${response.status} ${response.statusText}`;
    const thrown = new Error(message);
    thrown.code = error && error.code;
    thrown.status = response.status;
    thrown.details = error && error.details;
    throw thrown;
  }
  return body;
}

// The home page's card is the one auth surface now; the workstation's old
// inline strip is gone from the markup.
function homeAuth(register) {
  return authenticate(register, "homeEmail", "homePassword", "homeMsg");
}

// ---------------------------------------------------------------------------
// The chart engine
// ---------------------------------------------------------------------------

const ENGINE_URL = "/chart_engine.wasm";

// The engine's tool registry, read once at startup over its own ABI.
//
// `docs/21`: the registry is the one place a tool is declared. The toolbar is
// **built from this list**, not from the markup's fallback buttons, so a tool
// the engine learns is a button every pane grows and a kind the shell cannot
// draw is not a button at all. `null` until the engine has loaded, and the
// markup's own five buttons carry the toolbar until then.
let toolRegistry = null;

async function loadEngine() {
  const response = await fetch(ENGINE_URL);
  if (!response.ok) {
    throw new Error(
      `${ENGINE_URL} is ${response.status}. Run \`cargo run -p xtask -- build-frontend\` to build it.`
    );
  }
  const bytes = await response.arrayBuffer();
  // No wasm-bindgen: the module exports plain functions, so a plain
  // instantiation is the whole glue.
  const { instance } = await WebAssembly.instantiate(bytes, {});
  const exports = instance.exports;
  if (exports.tool_registry) {
    // The same buffer convention as a scene: the export fills the engine's
    // result buffer, and the shell copies it out.
    if (exports.tool_registry() === 0) {
      const start = exports.scene_ptr();
      const length = exports.scene_len();
      try {
        toolRegistry = JSON.parse(
          new TextDecoder().decode(new Uint8Array(exports.memory.buffer, start, length).slice())
        );
      } catch {
        toolRegistry = null;
      }
    }
  }
  return exports;
}

/// Ask the engine for a scene.
///
/// The contract is four calls: allocate, write, build, read back.
function buildScene(request) {
  const json = new TextEncoder().encode(JSON.stringify(request));
  const pointer = wasm.alloc(json.length);
  new Uint8Array(wasm.memory.buffer, pointer, json.length).set(json);

  const status = wasm.build_scene(pointer, json.length);
  wasm.dealloc(pointer, json.length);

  if (status !== 0) {
    const start = wasm.last_error_ptr();
    const length = wasm.last_error_len();
    const message = new TextDecoder().decode(new Uint8Array(wasm.memory.buffer, start, length));
    throw new Error(message || "the chart engine refused the request");
  }

  const start = wasm.scene_ptr();
  const length = wasm.scene_len();
  const bytes = new Uint8Array(wasm.memory.buffer, start, length).slice();
  return JSON.parse(new TextDecoder().decode(bytes));
}

// ---------------------------------------------------------------------------
// One chart pane
// ---------------------------------------------------------------------------

/// The names that mean "this pane's ...".
///
/// `el` is the shell's one lookup, and every call site in the chart code below
/// was written when there was a single chart -- so the names are the same ones.
/// What changed is *where* they resolve: a name in this set is looked up inside
/// the pane, and every other name is looked up on the document.
///
/// The set is explicit rather than "try the pane, fall back to the page", because
/// that rule would let a pane shadow a page-level id by accident -- and a pane
/// that quietly answered for `#thesis` would be a bug nothing points at.
const PANE_ELS = new Set([
  "chart", "chartWrap", "tools", "chartMsg", "chartHint", "chartNote",
  "footprintStats", "symbol", "timeframe", "limit", "mode", "zones", "fit",
  "feedStatus", "load", "close", "deleteDrawing", "clearDrawings",
  "magnet", "undo", "redo",
  // The pane's own chrome (title, zoom/collapse buttons) and the hidden bar
  // the right-click menu hosts.
  "paneTitle", "zoomIn", "zoomOut", "minBtn", "expBtn", "chartBar",
]);

/// A timeframe's length in minutes, for ordering the options.
///
/// The shell sorts the options itself rather than trusting the server's order.
/// It *used* to have to: `GET /symbols` listed them alphabetically --
/// `15m, 1h, 1m, 4h, 5m` -- and taking the first one opened every chart on the
/// thinnest series on the deployment. The server now sends a proper ladder
/// (`1m, 5m, 15m, 1h, 4h, 1d`), but the sort stays, because "the next timeframe
/// up" is the shell's question and depending on the server to have answered it
/// is one more thing that can be silently wrong. A suffix the server adds later
/// sorts last rather than throwing: a new unit should cost an odd-looking
/// position, not a blank chart.
const FRAME_UNITS = { s: 1 / 60, m: 1, h: 60, d: 1440, w: 10080 };
function frameMinutes(frame) {
  const parts = /^(\d+)([smhdw])$/.exec(String(frame));
  if (!parts) return Number.POSITIVE_INFINITY;
  return Number(parts[1]) * FRAME_UNITS[parts[2]];
}

/// One chart, and everything that belongs to it.
///
/// A factory rather than a set of module-level functions, because a chart is not
/// a page. Two charts on one page share a session, an aside and an engine, and
/// share nothing else: `scene`, the viewport, the drawings, the tool, the
/// selection, the candles and the live channel are all per pane. They are
/// closures here rather than fields on an object, which is what lets the body of
/// this function stay identical to the code that ran when there was one chart.
///
/// `root` is the pane element. `hooks.onSymbolChange` is called with the pane when
/// its instrument changes: the book is page-level and follows the *active* pane,
/// so only the page can decide whether a given change matters to it.
function createChartPane(root, hooks = {}) {
  // The id this pane stamps the shared context menu with while hosting its
  // controls. Created per call, so clones never share one.
  const paneId = nextPaneId++;

  // This pane's own controls, under the names the chart code already used.
  // Shadowing one function is the whole of the boundary -- every call site below
  // reads exactly as it did when there was a single chart, and the one line that
  // decides what "the chart" means is here.
  //
  // The one wrinkle: while the shared right-click menu hosts this pane's
  // controls, they are inside `#chartMenu`, not inside `root` -- so a plain
  // `root.querySelector` misses them exactly when the user is interacting with
  // them. If this pane owns the open menu, look in both and prefer the menu.
  const el = (name) => {
    if (!PANE_ELS.has(name)) return document.getElementById(name);
    const hosted = menuHosted && document.getElementById("chartMenu");
    if (hosted) {
      const inMenu = hosted.querySelector(`.${name}`);
      if (inMenu) return inMenu;
    }
    return root.querySelector(`.${name}`);
  };
  // True while this pane's controls are in the shared menu. Set in `openMenu`,
  // cleared in `closeMenu`, read by `el`.
  let menuHosted = false;

  // State that used to be module-level. Each of these was a latent bug the moment
  // there could be more than one chart: a shared `viewport` is one chart drawn
  // twice, and a shared `drawings` puts one instrument's trendline on another's.
  let scene = null; // the last scene the engine produced for this pane
  // The window the engine resolved last frame, echoed back with the next request.
  // This is the pane's *entire* zoom state: it never computes one, it only holds
  // this and returns it. See "the interaction model" in `docs/14`.
  let viewport = null;
  // The drawings on this symbol, in the engine's own shape. Loaded from the API on
  // a symbol change and never derived from the canvas: they are the user's
  // analysis, and the shell is a viewer of them rather than their author.
  let drawings = [];
  // The active drawing tool, or `"cursor"`.
  let tool = "cursor";
  // The magnet. On, every fraction anchor a placement sends is snapped by the
  // **engine** to the nearest open/high/low/close/value price within its
  // radius -- the shell sends where the pointer is and never computes a price.
  let magnet = false;
  // Undo/redo, as command pairs. A command is one reversible edit to this
  // pane's drawing document: `{ do(), undo(), label }`. Two stacks, and they
  // are the whole mechanism (`docs/21`): an undo is `undo()` plus a push to
  // the redo stack, not a snapshot to replay.
  let undoStack = [];
  let redoStack = [];
  // The id of the selected drawing, or null. Sent to the engine, which decides
  // which handles exist -- so a drawing cannot look selected here while offering
  // nothing to grab.
  let selectedDrawing = null;
  // The drawing being placed, before it has been saved. Held *outside* `drawings`
  // so the list only ever holds shapes that exist: a refusal, or a save that
  // failed, cannot leave a half-drawn one in it.
  let placing = null;
  let socket = null; // this pane's live candle channel
  // The instruments this pane may show, as the page last read them. Held here
  // because the pane owns its selects and has to rebuild them when the symbol
  // changes -- but the page reads the list once, since four panes asking four
  // times is the same answer for four round trips.
  let instruments = [];
  // The active, server-validated generated revision. It is an opaque market
  // coordinate payload; only the Rust engine maps it into the scene.
  let indicator = null;

  // ---------------------------------------------------------------------------
  // Drawing
  //
  // Every coordinate below comes from the scene. The only arithmetic is the
  // device-pixel-ratio scale, which is a display concern rather than a market one.
  // ---------------------------------------------------------------------------
  // The canvas palette, matched to the LuxAlgo-grade shell in index.html:
  // TV-standard candle colours, a #131722 plot on #171b26 panels, and the
  // same blue accent the chrome uses -- the chart and its frame are one
  // product, not two palettes that happen to share a screen.
  const COLORS = {
    up: "#089981",
    down: "#f23645",
    wick: "#6b7280",
    profile: "#2a2e39",
    value: "#2962ff",
    grid: "#1e222d",
    text: "#787b86",
    vwap: "#e3b341",
    poc: "#d1d4dc",
    vah: "#787b86",
    val: "#787b86",
    entry: "#2962ff",
    stop: "#f23645",
    target: "#089981",
    // The rest of the engine's overlay vocabulary. A level an answer *cited*
    // rather than one of the three trade prices is deliberately the quiet
    // grey-blue the other reference levels use -- it is context, not a plan --
    // and `other` is the text colour so a role the palette does not cover draws
    // as an annotation rather than as something with a meaning it does not have.
    level: "#787b86",
    other: "#787b86",
    // The drawing tools, one colour per kind so two shapes on the same chart are
    // told apart by what they are rather than by which was drawn first.
    trendline: "#2962ff",
    hline: "#e3b341",
    rect: "#9564e2",
    fib: "#4caf8e",
    // The kinds the registry added. Distinct hues, one per kind, so a ray and
    // a trendline on the same chart are told apart by what they are; the
    // ruler is the value-area blue because it measures, like the fib.
    vline: "#d1d4dc",
    ray: "#e3b341",
    extended: "#787b86",
    measure: "#2962ff",
    // The footprint ladder. Buy-aggressed volume is the ask side winning and
    // sell-aggressed is the bid side winning, which is the same convention the
    // level colours above already follow -- a level drawn green means the same
    // thing in both panels. `footValue` is the value area, and is the profile
    // blue rather than a new colour, because it is the same concept.
    footBuy: "#9564e2",
    footSell: "#2962ff",
    footValue: "#2962ff",
    // The two bands the engine ships with. A concept a client defined has no
    // entry here -- it cannot, we have never heard of it -- which is what the
    // side fallback below is for.
    demand: "#089981",
    supply: "#f23645",
    // Keyed by the region's `side`, the direction expected to react from the
    // band. Every region carries one, so an unfamiliar band reads as a direction
    // instead of as grey.
    buy: "#089981",
    sell: "#f23645",
  };

  function draw() {
    const canvas = el("chart");
    const wrap = canvas.parentElement;
    const ratio = window.devicePixelRatio || 1;
    const width = wrap.clientWidth;
    const height = wrap.clientHeight;
    canvas.width = Math.max(1, Math.floor(width * ratio));
    canvas.height = Math.max(1, Math.floor(height * ratio));

    const ctx = canvas.getContext("2d");
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, width, height);
    if (!scene) return;

    drawGrid(ctx, scene);

    // Zones go under everything, before the candles: a supply/demand band is a
    // backdrop the price is read against, not a mark on top of it.
    drawRegions(ctx, scene);
    drawIndicatorZones(ctx, scene);

    // The engine says what to draw, so this is a dispatch rather than a decision.
    // Adding a chart type means adding a case here and a variant in Rust -- not
    // teaching JavaScript what a Heikin-Ashi candle is.
    if (scene.footprint) drawFootprintGrid(ctx, scene);
    if (scene.profile.length) drawProfile(ctx, scene);
    switch (scene.style) {
      case "heikin_ashi":
      case "candles":
        drawCandles(ctx, scene);
        break;
      case "bars":
        drawOhlcBars(ctx, scene);
        break;
      case "area":
        drawArea(ctx, scene);
        break;
      case "line":
        drawLine(ctx, scene);
        break;
      default:
        break;
    }

    drawLevels(ctx, scene);
    // The answer's own levels, above the derived ones and below the user's own
    // marks. They arrive from the engine already positioned -- the shell picks a
    // colour and strokes, which is the same contract `drawLevels` and the regions
    // follow. Whether a thesis belongs on *this* pane is decided in `render`,
    // where the request is built: the engine has no way to know which instrument
    // a bare price belongs to.
    drawOverlays(ctx, scene);
    drawIndicatorEvidence(ctx, scene);    // The user's own marks, above everything: a drawing that could cover the
    // answer's levels, or the price labels, would be an annotation they cannot
    // read.
    drawDrawings(ctx, scene);

    // The live price, topmost of all: it is the one mark on the chart that must
    // never be hidden, because every other layer describes the market and this
    // one *is* the market, now.
    drawLastPrice(ctx, scene);

    drawAxis(ctx, scene);
  }

  /// The last-price line: the horizontal rule other platforms draw at the live
  /// price, with the price in the axis.
  ///
  /// The engine positions the line -- the y comes from the same price scale as
  /// every candle, so the tag cannot drift from the axis it sits in -- and this
  /// function paints: the rule across the plot, the tag in the axis gutter, and
  /// the up/down colour the candles already use. Skipped when the scene carries
  /// no live price: history-only, no feed yet, or the price outside this view.
  function drawLastPrice(ctx, scene) {
    const lp = scene.last_price;
    if (!lp || !Number.isFinite(lp.y)) return;
    const colour = lp.up ? COLORS.up : COLORS.down;

    ctx.strokeStyle = colour;
    ctx.lineWidth = 1;
    ctx.setLineDash([2, 3]);
    ctx.beginPath();
    ctx.moveTo(scene.plot.x, Math.round(lp.y) + 0.5);
    ctx.lineTo(scene.plot.x + scene.plot.w, Math.round(lp.y) + 0.5);
    ctx.stroke();
    ctx.setLineDash([]);

    // The price marker and the tag it carries. Filled, so it reads over the
    // axis ticks; the gutter is the engine's own right pad, which is where the
    // ticks' numbers live too -- this one simply wins, because it is the price.
    const text = fmtPrice(lp.price);
    ctx.font = "10px ui-monospace, monospace";
    const w = ctx.measureText(text).width + 8;
    const x = scene.plot.x + scene.plot.w + 2;
    const y = Math.min(Math.max(lp.y, 8), scene.height - 20);
    ctx.fillStyle = colour;
    ctx.fillRect(x, y - 8, w, 16);
    ctx.fillStyle = "#131722";
    ctx.textAlign = "left";
    ctx.fillText(text, x + 4, y + 3.5);
  }

  /// Regions -- the chart's only *area* overlay.
  ///
  /// Every coordinate, every price and the label itself come from the engine.
  /// This function picks a colour and fills a rectangle, which is the same
  /// contract `drawProfile` and `drawLevels` follow. Note there is no subtraction
  /// here: the band's height arrives as `h`, so a fill is
  /// `fillRect(x, y_top, w, h)` and nothing is computed from prices.
  ///
  /// The colour key is the region's `name`, so a concept the client defined is
  /// coloured by its own name the moment there is an entry for it -- and before
  /// then it falls back to `side`, which every region carries. That fallback is
  /// the whole reason a band the shell has never heard of still reads as a
  /// direction rather than as a grey rectangle.
  ///
  /// A fresh region is drawn solid and a mitigated one faded. That distinction is
  /// the whole reason the concept is worth drawing: a band price has already
  /// traded back through is not a level any more, and rendering the two the same
  /// is how a chart teaches someone to buy something that no longer exists.
  function drawRegions(ctx, scene) {
    if (!scene.regions.length) return;
    ctx.font = "10px ui-monospace, monospace";

    for (const region of scene.regions) {
      const colour = COLORS[region.name] || COLORS[region.side] || COLORS.text;
      ctx.globalAlpha = region.fresh ? 0.16 : 0.07;
      ctx.fillStyle = colour;
      ctx.fillRect(region.x, region.y_top, region.w, region.h);
      ctx.globalAlpha = 1;

      // The outline, so a band in a quiet stretch of chart is still visible.
      ctx.strokeStyle = colour;
      ctx.globalAlpha = region.fresh ? 0.7 : 0.35;
      ctx.setLineDash([3, 3]);
      ctx.strokeRect(region.x + 0.5, region.y_top + 0.5, region.w - 1, region.h - 1);
      ctx.setLineDash([]);
      ctx.globalAlpha = 1;

      ctx.fillStyle = colour;
      ctx.fillText(region.label, region.x + 4, region.y_top + 11);
    }
  }

  /// Generated indicators use a richer but still disciplined visual language:
  /// zones sit below price, their lifecycle changes the opacity/dash treatment,
  /// and the named evidence chain is drawn later above price. The engine has
  /// already placed every coordinate; this code only paints it.
  function drawIndicatorZones(ctx, scene) {
    const indicator = scene.indicator;
    if (!indicator || !indicator.zones.length) return;
    const palette = {
      created: "#a78bfa",
      active: "#2dd4bf",
      tapped: "#fbbf24",
      mitigated: "#94a3b8",
      invalidated: "#fb7185",
    };
    ctx.font = "600 10px ui-sans-serif, system-ui";
    for (const zone of indicator.zones) {
      const colour = palette[zone.state] || palette.active;
      const alpha = zone.state === "mitigated" || zone.state === "invalidated" ? 0.06 : 0.16;
      ctx.globalAlpha = alpha;
      ctx.fillStyle = colour;
      ctx.fillRect(zone.x, zone.y_top, zone.w, zone.h);
      ctx.globalAlpha = 1;
      ctx.strokeStyle = colour;
      ctx.lineWidth = 1;
      ctx.setLineDash(zone.state === "active" ? [] : [4, 3]);
      ctx.strokeRect(zone.x + 0.5, zone.y_top + 0.5, zone.w - 1, zone.h - 1);
      ctx.setLineDash([]);
      ctx.fillStyle = colour;
      ctx.fillText(`${zone.label} · ${zone.state}`, zone.x + 6, zone.y_top + 13);
    }
  }

  /// Draw the generated module's evidence graph after candles so it explains a
  /// setup without hiding price. The rounded path is a visual connection, not
  /// a synthetic price line: it joins two already-positioned logical events.
  function drawIndicatorEvidence(ctx, scene) {
    const indicator = scene.indicator;
    if (!indicator || (!indicator.markers.length && !indicator.links.length)) return;
    const palette = {
      bullish: "#34d399",
      bearish: "#fb7185",
      context: "#a78bfa",
      signal: "#60a5fa",
    };
    ctx.lineWidth = 1.25;
    ctx.strokeStyle = "rgba(167, 139, 250, 0.65)";
    ctx.setLineDash([3, 4]);
    for (const link of indicator.links) {
      ctx.beginPath();
      ctx.moveTo(link.from_x, link.from_y);
      ctx.quadraticCurveTo(link.control_x, link.control_y, link.to_x, link.to_y);
      ctx.stroke();
    }
    ctx.setLineDash([]);
    ctx.font = "600 10px ui-sans-serif, system-ui";
    for (const marker of indicator.markers) {
      const colour = palette[marker.kind] || palette.context;
      ctx.fillStyle = colour;
      ctx.beginPath();
      ctx.arc(marker.x, marker.y, marker.kind === "signal" ? 5 : 3.5, 0, Math.PI * 2);
      ctx.fill();
      ctx.fillStyle = "#d1d4dc";
      ctx.fillText(marker.label, marker.x + 7, marker.y - 7);
    }
  }

  /// The footprint ladder: bid x ask per level, per candle.
  ///
  /// ## What this is trying to be
  ///
  /// A footprint is read as a *grid*, so the drawing has to make the grid
  /// visible. The first version filled a cell only where there was a diagonal
  /// imbalance, so a ladder with few imbalances came out as a scatter of small
  /// boxes on a dark field: which prices traded, which side won each of them and
  /// where the value area sat were all in the data and none of them were on the
  /// screen. The reference chart in `docs/14` fills every cell that has volume,
  /// tints it by which side won, and outlines the ones that are imbalanced.
  ///
  /// Three rules follow, and each of them was a defect that could be seen:
  ///
  /// 1. **Every cell with volume is filled**, and the tint's strength is how
  ///    one-sided the level was, so the ladder reads as a heat map rather than
  ///    as a list of exceptions.
  /// 2. **The value area is banded per column, not per row.** The first version
  ///    asked whether *any* column had a value-area cell at that row and then
  ///    banded the full width -- so one candle's value area drew a stripe across
  ///    every other candle's ladder, at prices those candles never traded.
  /// 3. **The pair is drawn as `bid x ask`, centred.** Two numbers pushed into
  ///    the two halves of a wide cell stop reading as a pair at all, which is
  ///    what a 3-column window looked like: a number, a gap, another number.
  ///
  /// Every coordinate and every string still comes from the engine, and the
  /// ratio and run length arrive as numbers used only to pick an alpha and a
  /// line width -- presentation, not arithmetic.
  function drawFootprintGrid(ctx, scene) {
    const grid = scene.footprint;
    const font = Math.max(6, Math.min(11, grid.font_px));
    // The engine decides whether a number fits, not the shell. It used to be a
    // threshold here -- `font_px >= 7` -- while the engine clamped its font at 6,
    // and the two drifted: a window of the row count the route targets came out at
    // 6.9px, so every cell was drawn as a colour and not one number was. A ladder
    // with no numbers is not a footprint.
    const showText = grid.show_text;
    // Looser than the padding the engine sized the font for (3px a side), on
    // purpose: this check can only drop a pair the engine already decided fits,
    // and a margin that drifted *above* the engine's would silently undo its
    // arithmetic -- which is the shape of the bug this whole arrangement fixes.
    const margin = Math.max(2, font * 0.3);

    ctx.font = `${font}px ui-monospace, monospace`;
    ctx.textBaseline = "middle";
    ctx.textAlign = "center";

    // Behind everything, so the eye finds the value area first -- one column at
    // a time.
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
      // A faint column background, then the frame. Without it a row where a
      // candle traded nothing is the same colour as the space outside the grid,
      // and a sparse ladder stops reading as columns at all -- which is what the
      // reference chart's tinted columns are for.
      ctx.fillStyle = "#1a1e29";
      ctx.fillRect(column.x, scene.plot.y, column.w, scene.plot.h);
      ctx.strokeStyle = COLORS.grid;
      ctx.lineWidth = 1;
      ctx.strokeRect(column.x + 0.5, scene.plot.y + 0.5, column.w - 1, scene.plot.h - 1);

      for (const cell of column.cells) {
        const total = cell.bid + cell.ask;
        // A price level with no volume is background. It has a row on the shared
        // axis -- that is what the axis is -- but nothing traded there.
        if (total <= 0) continue;

        // `side` is the *diagonal* imbalance: this level measured against the one
        // below it, not simply which of this cell's two numbers is larger. That
        // is why a bid-heavy cell can still be flagged as a buy imbalance, and
        // why the tint here answers a different question from `side`.
        const buyShare = cell.ask / total;
        const leansBuy = buyShare >= 0.5;
        const oneSided = Math.abs(buyShare - 0.5) * 2;
        const colour = leansBuy ? COLORS.footBuy : COLORS.footSell;

        ctx.globalAlpha = 0.3 + oneSided * 0.42;
        ctx.fillStyle = colour;
        ctx.fillRect(cell.x + 1, cell.y, cell.w - 2, cell.h - 1);
        ctx.globalAlpha = 1;

        if (cell.is_poc) {
          // The column's own point of control: the price it did most of its
          // business at. Outlined rather than filled, so the numbers survive it.
          ctx.strokeStyle = COLORS.poc;
          ctx.lineWidth = 1;
          ctx.strokeRect(cell.x + 1.5, cell.y + 0.5, cell.w - 3, Math.max(1, cell.h - 1));
        }

        if (cell.side) {
          // How one-sided (`ratio`) and how many levels in a row (`stacked`) --
          // the two numbers a footprint is actually traded on. They set the
          // outline rather than being printed over the pair, which at 7px would
          // leave neither legible.
          const strength = Math.max(0, Math.min(1, ((cell.ratio || 1) - 1) / 3));
          ctx.strokeStyle = colour;
          ctx.globalAlpha = 0.7 + strength * 0.3;
          ctx.lineWidth = 1 + Math.min(2, (cell.stacked || 1) - 1);
          ctx.strokeRect(cell.x + 1.5, cell.y + 0.5, cell.w - 3, Math.max(1, cell.h - 1));
          ctx.globalAlpha = 1;
        }

        if (!showText) continue;
        const pair = `${cell.bid_text} x ${cell.ask_text}`;
        // A pair that does not fit is not drawn at all: a truncated number is a
        // wrong number, and a ladder of wrong numbers is worse than a ladder of
        // colours. Below ~54px a column is a heat map, which is the honest thing
        // for it to be.
        if (ctx.measureText(pair).width > cell.w - margin * 2) continue;
        // One colour for the pair. The tint already says which side won, and
        // splitting the pair into a bright half and a dim half cost three text
        // measurements per cell on every frame of a pan.
        ctx.fillStyle = "#d1d4dc";
        ctx.fillText(pair, cell.x + cell.w / 2, cell.y + cell.h / 2);
      }

      // The candle's own totals, in its own column, under its own ladder.
      const summary = column.summary;
      ctx.fillStyle = "#1e222d";
      ctx.fillRect(summary.x + 1, summary.y, summary.w - 2, summary.h);
      ctx.strokeStyle = COLORS.grid;
      ctx.lineWidth = 1;
      ctx.strokeRect(summary.x + 0.5, summary.y + 0.5, summary.w - 1, summary.h - 1);
      if (showText) {
        ctx.textAlign = "center";
        ctx.fillStyle = "#d1d4dc";
        ctx.fillText(summary.volume_text, summary.x + summary.w / 2, summary.y + font * 0.95);
        ctx.fillStyle = summary.delta_positive ? COLORS.up : COLORS.down;
        ctx.fillText(summary.delta_text, summary.x + summary.w / 2, summary.y + summary.h - font * 0.75);
      }
    }
    ctx.textAlign = "left";
  }

  /// The window totals, as a strip under the chart.
  function renderFootprintStats(grid) {
    const node = el("footprintStats");
    if (!grid) {
      node.hidden = true;
      node.innerHTML = "";
      return;
    }
    const s = grid.stats;
    const field = (label, value, className) =>
      `<span><b>${label}</b><span class="${className || ""}">${escapeHtml(value)}</span></span>`;
    node.innerHTML = [
      field("trades", s.trades.toLocaleString()),
      field("columns", s.columns),
      field("rows", s.rows),
      field("bid", s.bid_text),
      field("ask", s.ask_text),
      field("total", s.total_text),
      field("delta", s.delta_text, s.delta_positive ? "pass" : "fail"),
      field("max Δ", s.max_delta_text, "pass"),
      field("min Δ", s.min_delta_text, "fail"),
    ].join("");
    node.hidden = false;
  }

  /// OHLC bars: a vertical range with an open tick and a close tick.
  function drawOhlcBars(ctx, scene) {
    const half = scene.candles.length
      ? Math.max(2, (scene.candles[0].w / 2) * 1.3)
      : 3;
    ctx.strokeStyle = COLORS.wick;
    ctx.lineWidth = 1;
    for (const bar of scene.candles) {
      const centre = bar.x + bar.w / 2;
      ctx.beginPath();
      ctx.moveTo(centre, bar.wick_top);
      ctx.lineTo(centre, bar.wick_bottom);
      // Open to the left, close to the right: the convention that makes a bar
      // readable without colour.
      ctx.moveTo(centre - half, bar.open_y);
      ctx.lineTo(centre, bar.open_y);
      ctx.moveTo(centre, bar.close_y);
      ctx.lineTo(centre + half, bar.close_y);
      ctx.stroke();
    }
  }

  function drawLine(ctx, scene) {
    strokePath(ctx, scene.line, false);
  }

  function drawArea(ctx, scene) {
    strokePath(ctx, scene.line, true);
  }

  function strokePath(ctx, points, fill) {
    if (points.length < 2) return;
    ctx.beginPath();
    ctx.moveTo(points[0].x, points[0].y);
    for (const point of points.slice(1)) ctx.lineTo(point.x, point.y);
    ctx.strokeStyle = COLORS.accent;
    ctx.lineWidth = 1.5;
    ctx.stroke();

    if (fill) {
      ctx.lineTo(points[points.length - 1].x, scene.plot.y + scene.plot.h);
      ctx.lineTo(points[0].x, scene.plot.y + scene.plot.h);
      ctx.closePath();
      ctx.globalAlpha = 0.15;
      ctx.fillStyle = COLORS.accent;
      ctx.fill();
      ctx.globalAlpha = 1;
    }
    ctx.lineWidth = 1;
  }

  function drawGrid(ctx, scene) {
    ctx.strokeStyle = COLORS.grid;
    ctx.lineWidth = 1;
    ctx.font = "10px ui-monospace, monospace";
    ctx.fillStyle = COLORS.text;
    for (const tick of scene.ticks) {
      ctx.beginPath();
      ctx.moveTo(scene.plot.x, tick.y);
      ctx.lineTo(scene.plot.x + scene.plot.w, tick.y);
      ctx.stroke();
      ctx.fillText(tick.price.toFixed(2), scene.plot.x + scene.plot.w + 6, tick.y + 3);
    }
  }

  function drawCandles(ctx, scene) {
    for (const bar of scene.candles) {
      const colour = bar.up ? COLORS.up : COLORS.down;
      ctx.strokeStyle = colour;
      ctx.fillStyle = colour;
      // The wick.
      ctx.beginPath();
      ctx.moveTo(bar.x + bar.w / 2, bar.wick_top);
      ctx.lineTo(bar.x + bar.w / 2, bar.wick_bottom);
      ctx.stroke();
      // The body. A doji has zero height and would vanish, so it gets one pixel.
      const height = Math.max(1, bar.body_bottom - bar.body_top);
      ctx.fillRect(bar.x, bar.body_top, bar.w, height);
    }
  }

  function drawProfile(ctx, scene) {
    for (const bar of scene.profile) {
      // Buy share on the left, sell on the right, so the split is visible without
      // a second chart.
      ctx.fillStyle = COLORS.profile;
      ctx.fillRect(bar.x, bar.y, bar.w, bar.h);
      ctx.fillStyle = bar.in_value_area ? COLORS.value : COLORS.up;
      ctx.globalAlpha = 0.55;
      ctx.fillRect(bar.x, bar.y, bar.w * bar.buy_ratio, bar.h);
      ctx.globalAlpha = 1;
    }
  }

  function drawLevels(ctx, scene) {
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
  }

  /// How close a pointer has to be to a handle to grab it, in canvas pixels.
  ///
  /// Two numbers, and they are different on purpose: a handle is a small target
  /// the user is aiming at, so it gets a forgiving radius, while a line is a large
  /// one the user is aiming *near*, so a generous radius would steal drags that
  /// were meant to pan.
  const HANDLE_RADIUS = 7;
  /// How close it has to be to a line, or inside a rectangle, to grab the drawing.
  const GRAB_RADIUS = 5;

  /// The user's own shapes.
  ///
  /// Every coordinate arrives positioned and every choice is already made, so this
  /// function strokes, fills and writes text -- the same contract `drawRegions` and
  /// `drawFootprintGrid` follow, and the reason the price scale stays in Rust.
  ///
  /// The parts are a flat list rather than a shape per kind, because a Fibonacci is
  /// seven lines and a label each, and the engine has already decided that. Adding
  /// a kind is a variant in Rust and no change here.
  function drawDrawings(ctx, scene) {
    if (!scene.drawings.length) return;

    for (const drawing of scene.drawings) {
      const colour = COLORS[drawing.kind] || COLORS.text;
      ctx.strokeStyle = colour;
      ctx.fillStyle = colour;
      ctx.font = "10px ui-monospace, monospace";

      for (const part of drawing.parts) {
        switch (part.shape) {
          case "segment":
            ctx.setLineDash(part.dashed ? [4, 4] : []);
            ctx.beginPath();
            ctx.moveTo(part.x1, part.y1);
            ctx.lineTo(part.x2, part.y2);
            ctx.stroke();
            ctx.setLineDash([]);
            break;

          case "rect":
            if (part.filled) {
              ctx.globalAlpha = drawing.selected ? 0.18 : 0.1;
              ctx.fillRect(part.x, part.y, part.w, part.h);
              ctx.globalAlpha = 1;
            }
            ctx.strokeRect(part.x + 0.5, part.y + 0.5, part.w - 1, part.h - 1);
            break;

          case "text":
            ctx.fillText(part.text, part.x, part.y);
            break;

          case "handle":
            // A filled dot. It is the one thing on this canvas a user is meant to
            // aim at, so it is drawn as a target rather than as a point.
            ctx.beginPath();
            ctx.arc(part.x, part.y, HANDLE_RADIUS - 3, 0, Math.PI * 2);
            ctx.fill();
            break;

          default:
            break;
        }
      }
    }
  }

  function drawAxis(ctx, scene) {
    ctx.fillStyle = COLORS.text;
    ctx.font = "10px ui-monospace, monospace";
    const from = new Date(scene.from / 1e6);
    const to = new Date(scene.to / 1e6);
    ctx.fillText(from.toISOString().slice(0, 16).replace("T", " "), scene.plot.x, scene.height - 8);
    const label = to.toISOString().slice(0, 16).replace("T", " ");
    ctx.fillText(label, scene.plot.x + scene.plot.w - ctx.measureText(label).width, scene.height - 8);
  }

  /// Stroke the answer's own levels.
  ///
  /// Every coordinate arrives already resolved -- `y` and `band_y` are canvas
  /// pixels from the engine's own price scale. This function picks a colour from
  /// the role, fills the band when there is one, and writes the label. It does
  /// no arithmetic on a price at all, which is the change from the version that
  /// mapped the three thesis prices itself: that copy of the scale drifted from
  /// the engine's on every resize and zoom, so the stop-to-target band sat a few
  /// pixels off the candles it was describing.
  ///
  /// The role is a closed vocabulary from the engine, not the label text. A
  /// shell that matched on the label would colour "Stop Loss" as `other` the
  /// first time an answer phrased it differently.
  function drawOverlays(ctx, scene) {
    if (!scene.overlays || !scene.overlays.length) return;

    // Bands first, under the lines that bound them: a fill drawn over its own
    // edges dulls them, and the edges are the part that marks a price.
    for (const overlay of scene.overlays) {
      if (overlay.band_y === null || overlay.band_y === undefined) continue;
      const top = Math.min(overlay.y, overlay.band_y);
      const height = Math.abs(overlay.band_y - overlay.y);
      ctx.globalAlpha = overlay.filled ? 0.12 : 0;
      if (overlay.filled) {
        ctx.fillStyle = COLORS[overlay.role] || COLORS.text;
        ctx.fillRect(scene.plot.x, top, scene.plot.w, height);
      }
      ctx.globalAlpha = 1;
    }

    for (const overlay of scene.overlays) {
      const colour = COLORS[overlay.role] || COLORS.text;
      ctx.strokeStyle = colour;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.moveTo(scene.plot.x, overlay.y);
      ctx.lineTo(scene.plot.x + scene.plot.w, overlay.y);
      ctx.stroke();

      // A band's far edge is drawn too, dashed: it is the same claim as the
      // near one but the consumer of the two is the shaded area, and a solid
      // line there would read as a third level.
      if (overlay.band_y !== null && overlay.band_y !== undefined) {
        ctx.setLineDash([4, 3]);
        ctx.beginPath();
        ctx.moveTo(scene.plot.x, overlay.band_y);
        ctx.lineTo(scene.plot.x + scene.plot.w, overlay.band_y);
        ctx.stroke();
        ctx.setLineDash([]);
      }

      ctx.fillStyle = colour;
      ctx.font = "bold 10px ui-monospace, monospace";
      // The price comes back from the engine rather than being read off the
      // thesis here, so a level the answer cited as a bare `level` is labelled
      // with the number that was actually drawn.
      const text = `${overlay.label} ${overlay.price.toFixed(2)}`;
      ctx.fillText(text, scene.plot.x + scene.plot.w - ctx.measureText(text).width - 4, overlay.y - 3);
    }
    ctx.lineWidth = 1;
  }

  // ---------------------------------------------------------------------------
  // Candles, and the live channel
  // ---------------------------------------------------------------------------

  async function loadCandles() {
    const symbol = el("symbol").value;
    const timeframe = el("timeframe").value;
    const limit = el("limit").value;
    const response = await api(
      `/candles?symbol=${symbol}&timeframe=${timeframe}&limit=${limit}`
    );
    // Say where the bars came from, so "is this live or a fallback?" is
    // answered on the page instead of guessed at. memory = the live in-memory
    // buffer the feed fills; venue = REST-fetched history for this window.
    const src = response.source;
    const noteEl = document.getElementById("chartNote");
    if (noteEl && src) {
      noteEl.textContent = `candles: ${src.memory} from live buffer, ${src.venue} fetched from venue (${response.candles?.length ?? 0} total)`;
    }
    return response.candles || [];
  }

  let candles = [];
  // The trade-level ladders, when the mode asks for them and the window has
  // trades. Kept beside `candles` rather than inside them: a footprint needs both
  // the OHLC for the axis and the ladders for the grid, and they come from two
  // routes.
  let footprint = null;
  // Why the live channel cannot carry anything, when the server says so. Held
  // rather than written straight into the strip, because `render` owns that strip
  // and a message written once is wiped by the next pan -- which is the "the
  // chart is frozen and nothing anywhere says why" that this exists to end.
  let feedNotice = "";

  /// How wide a ladder cell has to be for the numbers that are in it, in pixels.
  ///
  /// Learned from the engine, not chosen here. This was `MIN_COLUMN_PX` -- 54,
  /// then 64 -- matched by hand against the engine's font floor and glyph ratio,
  /// in another language, with nothing failing when the two drifted apart. That
  /// is the same defect as the font itself, one level up: a constant in the shell
  /// and a constant in the engine that have to agree.
  ///
  /// `Grid::min_cell_px` is that arithmetic read backwards, and it is the widest
  /// pair **in the window**, so it follows the instrument -- a symbol whose
  /// volumes print as `1.21 K` gets wider cells than one printing `0.44`. The
  /// column count is what decides how many candles are on screen, so this is the
  /// number that decides that, and it was being decided by a guess.
  ///
  /// Zero until the engine has answered once.
  let footCellPx = 0;

  /// The cell width the first load assumes, before the engine has answered.
  ///
  /// Deliberately generous, because the failure it risks is the expensive one:
  /// too few columns costs some width, and too many is what makes the engine
  /// stop drawing numbers at all. One load is enough to correct it.
  const SEED_CELL_PX = 64;

  /// What the live channel is doing, so the page can *say* it rather than imply it.
  ///
  /// `at` is when the last frame arrived, not when the socket opened. A socket
  /// that is open and silent is the exact failure this exists to expose, and a
  /// badge driven by the connection state would stay green straight through it.
  const live = { state: "idle", at: 0, bar: 0 };

  /// Whether the chart's window stays pinned to the newest bar.
  ///
  /// On by default -- a live chart that needs a reload to show the next candle
  /// is the failure, not the feature -- and switched off by any pan or drag
  /// along the time axis, because a user who scrolled back to a swing low does
  /// not want the chart yanked to the right edge under them. The Fit button
  /// turns it back on: "show everything again" includes what arrives next.
  let followLive = true;

  /// The newest price this pane knows, from the feed's forming bar.
  ///
  /// Kept on the pane rather than read back out of `candles`: the last element
  /// of a cleared or refetching array is not a live price, and a line drawn
  /// from it would flicker to nothing between frames for no reason a user
  /// could see.
  let lastPrice = null;

  /// The live price as the engine request wants it, or null when there is not
  /// one. A function rather than a bare read so `render` cannot snapshot a
  /// stale value into a request built a frame later.
  function livePrice() {
    return Number.isFinite(lastPrice) ? lastPrice : null;
  }

  /// The clock the badge counts with. Its own timer rather than a side effect of
  /// `render`, because the reading that matters is the one where nothing redraws.
  let liveTimer = 0;

  async function refresh() {
    const message = el("chartMsg");
    footprint = null;
    // A reload is followed by a reconnect, so whatever the last channel said
    // about itself is about to be said again -- or not.
    feedNotice = "";
    // Before the first render, so the drawings are in the frame the candles land
    // in rather than appearing a beat later. They belong to the symbol, so this is
    // also where a symbol change picks up the new one's.
    await loadDrawings();

    if (el("mode").value === "footprint") {
      // A footprint is built from trades, not candles, so it comes from its own
      // route -- and when that window has no trades the route says so, which is a
      // different answer from "nothing happened".
      try {
        const data = await loadFootprint();
        footprint = data;
        // Build the candle series from the same response, so the axis and the
        // ladders cannot disagree about which window is on screen.
        candles = data.candles.map((c) => ({
          symbol: data.symbol,
          timeframe: data.timeframe,
          open_time: c.open_time,
          open: c.open,
          high: c.high,
          low: c.low,
          close: c.close,
          volume: c.volume,
          buy_volume: c.ask_volume,
          sell_volume: c.bid_volume,
        }));
        render();
        message.textContent = "";
        return;
      } catch (e) {
        // Fall through to candles: the engine then draws the candle-derived
        // profile and says why, which is better than an empty canvas.
        message.textContent = e.message;
      }
    }

    try {
      candles = await loadCandles();
      render();
      if (!message.textContent) message.textContent = candles.length ? "" : "no candles in this window";
    } catch (e) {
      message.textContent = e.message;
    }
  }

  /// Whether the zones overlay is switched on.
  ///
  /// The button's `aria-pressed` is the state, rather than a second variable that
  /// can drift out of step with what the button says.
  function zonesOn() {
    return el("zones").getAttribute("aria-pressed") === "true";
  }

  /// Rebuild the scene and repaint.
  ///
  /// `gesture` is what the user just did, if anything. The engine applies it to
  /// the window below and answers with the result, which becomes the window for
  /// the next frame -- so the shell only ever holds a *resolved* window and never
  /// computes one. That is what keeps the clamping, the minimum bar count and the
  /// price floor in a single implementation, in Rust, where the tests reach them.
  function render(gesture = null) {
    if (!wasm) return;
    const wrap = el("chart").parentElement;
    const request = {
      candles,
      width: wrap.clientWidth,
      height: wrap.clientHeight,
      mode: el("mode").value,
      lines: ["vwap", "poc", "vah", "val"],
      // Whether to detect and draw the supply/demand zones. The engine does the
      // detecting, on the candles this request already carries, so switching this
      // on costs no extra round trip.
      zones: zonesOn(),
      // Whether the window stays pinned to the newest bar. On while the user has
      // not scrolled or panned back; any pan or drag along the time axis turns it
      // off, and the Fit button turns it back on. Without it the engine re-resolves
      // the same bars every frame while new ones append off-screen -- which is why
      // live candles used to appear only after a reload.
      follow: followLive,
      // The live price, for the last-price line the engine positions and this
      // shell colours. From the feed's own forming bar -- the live price *is*
      // that bar's last trade -- and null whenever there is not one yet.
      last_price: livePrice(),
      footprint: footprint ? footprint.candles : [],
      footprint_trades: footprint ? footprint.trades : 0,
      // The user's drawings, with the one being placed appended. An anchor may be
      // a `fraction` here -- that is how placing and dragging work -- and the scene
      // answers with every anchor absolute, which is the only form the API accepts.
      //
      // `selected` is *derived* from `selectedDrawing` rather than stored on each
      // drawing, the same arrangement the toolbar's `aria-pressed` uses: one
      // variable, so the two cannot disagree. Storing it meant every write to the
      // list had to remember to re-apply it, and one of them did not -- a drawing
      // that had just been saved came back from `fromServer` unselected and offered
      // no handles, so the shape the user had this second finished drawing could
      // not be grabbed.
      drawings: (placing ? [...drawings, placing] : drawings).map((drawing) => ({
        ...drawing,
        selected: drawing.id === selectedDrawing,
      })),
      // The magnet. Engine-side, like every mapping: the shell says whether
      // snapping is wanted and the engine decides what the pointer hit.
      snap: magnet,
      // The answer's levels, as **prices**. Mapped to pixels by the engine, not
      // here: this pane has no price scale, and the copy of one it used to carry
      // (`drawThesis`'s own `y =`) drifted from the engine's on every resize and
      // zoom -- so a band sat a few pixels off the candles it described, which
      // reads as "the level moved" rather than "the overlay is stale".
      //
      // Only for the instrument the thesis is about. The engine cannot know
      // which symbol an overlay belongs to -- a price is just a number -- so a
      // BTCUSDT entry drawn over an ETHUSDT chart would be a level that means
      // nothing. That check is here because this is where the symbol is known.
      //
      // `el("symbol").value`, not a bare `symbol`: this function has no local of
      // that name (the one in `loadCandles` is a different function's), so a
      // bare identifier would be a `ReferenceError` the moment a thesis existed
      // -- and short-circuiting on `thesis &&` is exactly what would hide it,
      // because the read that throws is the one after the guard.
      overlays: thesis && thesis.symbol === el("symbol").value ? thesisOverlays(thesis) : [],
      indicator,
    };
    // Assigned rather than sent as `null`: a null is not a missing field, and the
    // engine's `Viewport` is a struct rather than an option, so `viewport: null`
    // would be a deserialization error rather than a default. Absent means
    // "everything, fitted", which is where a chart starts.
    if (viewport) request.viewport = viewport;
    if (gesture) request.gesture = gesture;

    scene = buildScene(request);
    viewport = scene.viewport;
    // The engine's own answer to "how wide must a cell be for the numbers that
    // are in this window", kept for the next window the user asks for. The count
    // decides the window, the window decides the numbers, and the numbers decide
    // the count -- so sizing the next load from what this one actually needed
    // reaches the fixed point in one step instead of being guessed at every time.
    if (scene.footprint && scene.footprint.min_cell_px > 0) {
      footCellPx = Math.ceil(scene.footprint.min_cell_px);
    }
    el("chartNote").textContent = feedNotice || scene.note || "";
    renderFootprintStats(scene.footprint);
    // The toolbar is about the *selection*, and the selection changes from the
    // chart as well as from the toolbar -- a click on a shape, an `Esc`, a drop.
    // Updating it here means there is one place that decides and no path that
    // forgets, which is the same arrangement `aria-pressed` already uses.
    refreshDrawingButtons();
    // The badge's *state* changes here; its age ticks on its own timer, because
    // an age that only updated when something else redrew would be frozen at
    // whatever it read the last time the chart moved -- which is the one reading
    // it must never give.
    refreshLiveBadge();
    draw();
  }

  // ---------------------------------------------------------------------------
  // Chart interaction: wheel, drag, fit
  //
  // The shell's whole contribution to zooming is to turn a pointer position into a
  // *fraction of the plot rectangle* and say what the user did. It never decides
  // which bar is under the cursor or which price sits at the top of the plot --
  // those are the two numbers a chart gets wrong when two implementations
  // disagree, and `docs/14` keeps them in Rust.
  // ---------------------------------------------------------------------------

  /// Where a pointer is, as a fraction of the plot rectangle.
  ///
  /// The only arithmetic here is a pixel offset over a rectangle's width, which is
  /// a display concern rather than a market one. Clamped to the plot, so a pointer
  /// that has wandered onto the price axis zooms about the nearest edge rather
  /// than about a fraction outside the chart.
  function plotFraction(event) {
    if (!scene) return null;
    const rect = el("chart").getBoundingClientRect();
    const x = (event.clientX - rect.left - scene.plot.x) / scene.plot.w;
    const y = (event.clientY - rect.top - scene.plot.y) / scene.plot.h;
    return { x: Math.min(1, Math.max(0, x)), y: Math.min(1, Math.max(0, y)) };
  }

  /// How much one pixel of wheel travel zooms.
  ///
  /// Exponential, so one notch is the same *proportion* at every zoom level. A
  /// linear step crawls when zoomed out and lurches when zoomed in, because the
  /// same number of bars is a different fraction of the window at each end.
  const ZOOM_PER_PIXEL = 0.0015;

  function onWheel(event) {
    if (!scene) return;
    const at = plotFraction(event);
    if (!at) return;
    // Ours now: the page must not scroll behind the chart.
    event.preventDefault();

    // `deltaMode` is pixels in Chrome and lines in Firefox, so normalise here and
    // keep the engine's `factor` a plain multiplier. `deltaY` is positive when the
    // wheel rolls towards the user, which is zoom *out*.
    const pixels = event.deltaY * (event.deltaMode === 1 ? 16 : 1);
    const factor = Math.exp(-pixels * ZOOM_PER_PIXEL);

    // Shift is the price axis, which is the convention every charting package
    // uses and therefore the one a trader will try first.
    applyGesture(
      event.shiftKey
        ? { kind: "zoom_price", factor, anchor: at.y }
        : { kind: "zoom_time", factor, anchor: at.x }
    );
  }

  // The pointer gesture in progress, or null.
  //
  // One object with a `mode` rather than three nullable globals, because the three
  // are mutually exclusive -- the pointer is panning, moving an anchor, or drawing
  // something new -- and three variables would be three chances for two of them to
  // be set at once.
  //
  // `appliedX`/`appliedY` are the last position actually *sent*, not the last one
  // seen; see `onPointerMove`.
  let drag = null;

  function onPointerDown(event) {
    if (!scene || event.button !== 0) return;

    if (tool !== "cursor") {
      startPlacing(event);
      return;
    }

    const hit = hitTest(event);
    if (hit) {
      // A grab on a handle or a body selects that drawing and moves it. Selection
      // follows the grab rather than a separate click, because on a chart the two
      // are one intent and asking for both is one gesture too many.
      select(hit.drawing);
      // The engine's absolute anchors at grab time. The drag itself writes
      // fractions into the list, so by the time the pointer comes up the "before"
      // state exists only here -- capturing it in `finishMoving` would capture
      // fractions and an undo would PUT them, which the storage rightly refuses.
      const grabbed = resolvedDrawing(hit.drawing);
      drag = {
        mode: "move",
        target: hit,
        startX: event.clientX,
        startY: event.clientY,
        // Only a body grab needs these. A handle drag writes the pointer's own
        // position into one anchor, so where the anchors started is not a question
        // it asks; a body drag moves both, which means it needs the starting point
        // in a unit it can add to.
        base: hit.anchor === null ? fractionsOf(hit.drawing) : null,
        movedFrom: grabbed
          ? {
              a1: { ...grabbed.a1 },
              a2: grabbed.a2 ? { ...grabbed.a2 } : null,
            }
          : null,
      };
      // `moving`, not `dragging`: the cursor should say which of the two drags
      // this is, and only one of them moves the view.
      el("chart").classList.add("moving");
    } else {
      // Empty canvas: nothing is selected, and the drag pans.
      select(null);
      drag = { mode: "pan", appliedX: event.clientX, appliedY: event.clientY };
      el("chart").classList.add("dragging");
    }
    el("chart").setPointerCapture(event.pointerId);
  }

  function onPointerMove(event) {
    if (!scene || !drag) return;

    if (drag.mode === "pan") {
      // Measured from the last *applied* position rather than the last event, so a
      // move that gets coalesced into the next frame does not lose its pixels --
      // otherwise a fast drag visibly lags behind the pointer.
      const dx = (event.clientX - drag.appliedX) / scene.plot.w;
      const dy = (event.clientY - drag.appliedY) / scene.plot.h;
      if (dx === 0 && dy === 0) return;
      drag.appliedX = event.clientX;
      drag.appliedY = event.clientY;
      // Dragging the chart to the right reveals *older* bars, so the view moves the
      // other way -- the direction a finger moves a sheet of paper. Vertically it
      // is the same way round as the pointer, because price runs up the screen.
      // Any horizontal drag is the user taking the window back from the live
      // edge: following stops, and only Fit (or a series change) resumes it.
      if (dx !== 0) followLive = false;
      applyGesture({ kind: "pan", time: -dx, price: dy });
      return;
    }

    // A placement and a handle drag both put a *point* where the pointer is, so
    // neither needs a delta: the anchor's new position is the pointer's, and the
    // engine is what turns it into a time and a price. Measured from the pointer
    // rather than accumulated, which is why there are no skipped pixels to lose.
    //
    // The body drag is the third case and does not want this at all -- it works
    // from the pointer's *offset*, and `plotFraction` clamps to the plot, so
    // asking for it here would freeze a drawing at the plot's edge the moment the
    // user dragged it past one.
    if (drag.mode === "place") {
      const at = plotFraction(event);
      if (!at) return;
      // The second anchor follows the pointer; the first stays where the drag
      // began. A horizontal line has no second anchor to move.
      if (placing && placing.kind !== "hline") {
        placing.a2 = { unit: "fraction", x: at.x, y: at.y };
      }
      scheduleRender();
      return;
    }

    // A move is the only mode left, and it is one of two: a handle, or the body.
    // `target` is read here rather than above because a placement has none.
    if (drag.target.anchor !== null) {
      const at = plotFraction(event);
      if (!at) return;
      // A handle: that one anchor follows the pointer and the other stays put.
      // `0` is the first anchor and `1` the second, which is the engine's own
      // numbering -- it emits the handles, so it decides what they are called.
      const drawing = drawings.find((d) => d.id === drag.target.drawing);
      if (!drawing) return;
      const anchor = { unit: "fraction", x: at.x, y: at.y };
      if (drag.target.anchor === 1) drawing.a2 = anchor;
      else drawing.a1 = anchor;
      scheduleRender();
      return;
    }

    // The body: the whole drawing moves, so both anchors take the *same* delta.
    // Dragging one of them to the pointer instead -- which is what this branch did
    // before it had `drag.base` -- moves an endpoint rather than the shape, and a
    // rectangle dragged by its middle collapses to a corner.
    //
    // The delta is the pointer's, over the plot's size: the same division the pan
    // above does, and the only arithmetic the shell is allowed to do with a
    // position. Measured from the drag's start rather than accumulated, so a
    // coalesced frame cannot lose pixels and make the shape lag the pointer.
    const moving = drawings.find((d) => d.id === drag.target.drawing);
    if (!moving || !drag.base) return;
    const dx = (event.clientX - drag.startX) / scene.plot.w;
    const dy = (event.clientY - drag.startY) / scene.plot.h;
    moving.a1 = shifted(drag.base.a1, dx, dy);
    if (drag.base.a2) moving.a2 = shifted(drag.base.a2, dx, dy);
    scheduleRender();
  }

  function onPointerUp(event) {
    if (!drag) return;
    const finished = drag;
    drag = null;
    // Both drag classes, not just the one this gesture used. A class left on the
    // canvas outlives the gesture that set it, and from then on the cursor
    // describes a drag that is not happening.
    el("chart").classList.remove("dragging", "moving");
    if (el("chart").hasPointerCapture(event.pointerId)) {
      el("chart").releasePointerCapture(event.pointerId);
    }

    if (finished.mode === "place") finishPlacing();
    else if (finished.mode === "move") finishMoving(finished.target.drawing, finished.movedFrom);
  }

  /// A pointer the browser took away -- a touch that became a scroll, a window
  /// that lost focus. The gesture is abandoned rather than committed: whatever the
  /// pointer was doing, the user did not finish it.
  function onPointerCancel(event) {
    if (!drag) return;
    drag = null;
    el("chart").classList.remove("dragging", "moving");
    if (el("chart").hasPointerCapture(event.pointerId)) {
      el("chart").releasePointerCapture(event.pointerId);
    }
    if (placing) {
      placing = null;
      renderNow();
    }
  }

  // The gesture waiting for the next frame, and the frame it is waiting for.
  let waitingGesture = null;
  let gestureFrame = 0;

  /// Fold a new gesture into the one already waiting for the next frame.
  ///
  /// Folding rather than replacing, because both kinds compose: two pans of half a
  /// span are a pan of one span, and two zooms of 1.1 are a zoom of 1.21. Replacing
  /// would drop the earlier movement, and a drag that outran the frame rate would
  /// lag the pointer by however much it dropped.
  ///
  /// Not exactly equal to applying them in sequence, and worth being honest about:
  /// the engine clamps each gesture it sees, so a burst folded into one frame can
  /// travel slightly further than the same burst spread across frames. The
  /// alternative is one wasm rebuild and one full repaint per wheel event, at wheel
  /// rate.
  ///
  /// A gesture of a *different* kind replaces the waiting one. That only happens
  /// when two input devices are used inside a single frame, and the most recent
  /// event is the better guess at what the user meant.
  function foldGesture(waiting, next) {
    if (!waiting || waiting.kind !== next.kind) return next;
    switch (next.kind) {
      case "pan":
        return { kind: "pan", time: waiting.time + next.time, price: waiting.price + next.price };
      case "zoom_time":
      case "zoom_price":
        return { kind: next.kind, factor: waiting.factor * next.factor, anchor: next.anchor };
      default:
        return next;
    }
  }

  /// Rebuild the scene and repaint, at most once per frame.
  ///
  /// The shared tail of `applyGesture` and of the drawing gestures: a wheel, a pan
  /// and a drag of an anchor all end in one engine rebuild and one repaint, and
  /// none of them needs more than one per frame.
  function scheduleRender() {
    if (gestureFrame) return;
    gestureFrame = requestAnimationFrame(() => {
      gestureFrame = 0;
      const next = waitingGesture;
      waitingGesture = null;
      render(next);
    });
  }

  /// Send a gesture, at most one engine rebuild per frame.
  function applyGesture(gesture) {
    waitingGesture = foldGesture(waitingGesture, gesture);
    scheduleRender();
  }

  /// Rebuild and repaint *now*, cancelling anything waiting for a frame.
  ///
  /// For the two moments where a frame of delay is wrong: a drop, which has to read
  /// the engine's answer before anything else can move, and a click, which is a
  /// down and an up with no frame between them at all.
  function renderNow() {
    if (gestureFrame) {
      cancelAnimationFrame(gestureFrame);
      gestureFrame = 0;
    }
    waitingGesture = null;
    render();
  }

  /// Throw the window away, so the next frame fits everything again.
  ///
  /// Called when the *series* changes -- a different symbol, timeframe or bar
  /// count. Not on a chart-type change, where the same candles are still on
  /// screen and the window is still the one the user chose. Following resets
  /// with it: a new series has no scroll position to preserve, and a live
  /// chart starts watching its newest bar, not its oldest.
  function resetViewport() {
    viewport = null;
    followLive = true;
  }

  // ---------------------------------------------------------------------------
  // Chart drawings: placing, moving, storing
  //
  // Undo/redo operates on **commands**, not on snapshots (`docs/21`): one edit
  // is one object with the two directions of the edit in it, and the stacks
  // hold the history. The capture closures are written at the edit sites, where
  // the before-state is in hand -- which is the only place it exists, and the
  // reason this is a function rather than a diff recorder. The network save
  // that follows a drawing command is deliberately *not* the command: undo and
  // redo are a local navigation of what is on screen, and each direction
  // re-issues the API write for its own end state, so a reload agrees with the
  // screen either way.
  function runCommand(label, doFn, undoFn) {
    doFn();
    undoStack.push({ label, do: doFn, undo: undoFn });
    if (undoStack.length > 100) undoStack.shift();
    redoStack = [];
    refreshHistoryButtons();
  }

  /// Undo the last command, onto the redo stack.
  function undo() {
    const command = undoStack.pop();
    if (!command) return;
    command.undo();
    redoStack.push(command);
    refreshHistoryButtons();
  }

  /// Redo the last undone command, back onto the undo stack.
  function redo() {
    const command = redoStack.pop();
    if (!command) return;
    command.do();
    undoStack.push(command);
    refreshHistoryButtons();
  }

  /// Say which of the two history buttons can do anything. Disabled rather
  /// than inert, for the same reason Delete is: a button that looks pressable
  /// and does nothing is a lie in a smaller size.
  function refreshHistoryButtons() {
    const up = el("undo");
    const down = el("redo");
    if (up) up.disabled = undoStack.length === 0;
    if (down) down.disabled = redoStack.length === 0;
  }

  /// Which tool, of a group's flyout buttons, is the pressed one -- the trigger
  /// reads it, so "Lines · Trend" says what is active without opening it.
  function groupPressedLabel(groupName) {
    if (tool === "cursor") return null;
    const entry = toolRegistry && toolRegistry.find(
      (t) => t.kind === tool && t.group === groupName,
    );
    return entry ? entry.label : null;
  }

  // The shell's part is to say *where on the screen*; the engine's is to say what
  // that is. Nothing here converts a pixel into a price -- a placement or a drag
  // writes a `fraction` anchor into the request, and the scene answers with the
  // same drawing in absolute terms, which is what gets stored. So there is one
  // implementation of "which price is under the pointer", in Rust, and a drawing
  // cannot land somewhere the engine did not put it.
  // ---------------------------------------------------------------------------

  /// Where a pointer is, in canvas coordinates.
  ///
  /// Not the same as `plotFraction`, which is clamped to the plot and is what the
  /// engine is given. This one is deliberately unclamped, because a hit test has
  /// to be able to answer "nothing" -- a clamped point is always inside the plot,
  /// so it would always grab whatever is nearest the edge.
  function canvasPoint(event) {
    const rect = el("chart").getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  }

  /// What is under the pointer, if anything.
  ///
  /// The only geometry here is a distance between two screen points, which is a
  /// display concern -- the same class as the division in `plotFraction`. *Which*
  /// price or bar a point means is never derived here: the engine emits a `handle`
  /// part at every anchor it would let the user move, so this looks for a target
  /// the engine put there rather than deciding where one should be.
  ///
  /// Handles are tested first. They sit on top of the drawing they belong to, so a
  /// body test that ran first would make an anchor impossible to grab.
  function hitTest(event) {
    if (!scene) return null;
    const at = canvasPoint(event);

    for (const drawing of scene.drawings) {
      for (const part of drawing.parts) {
        if (part.shape !== "handle") continue;
        if (Math.hypot(part.x - at.x, part.y - at.y) <= HANDLE_RADIUS) {
          return { drawing: drawing.id, anchor: part.anchor };
        }
      }
    }

    for (const drawing of scene.drawings) {
      for (const part of drawing.parts) {
        if (part.shape === "segment" && distanceToSegment(at, part) <= GRAB_RADIUS) {
          return { drawing: drawing.id, anchor: null };
        }
        if (part.shape === "rect" && insideRect(at, part)) {
          return { drawing: drawing.id, anchor: null };
        }
      }
    }
    return null;
  }

  /// How far a point is from a segment, in canvas pixels.
  ///
  /// Clamped to the segment's ends, so a point past the end measures to the end
  /// rather than to the infinite line -- otherwise a trendline would be grabbable
  /// along a ray it is not drawn on.
  function distanceToSegment(point, segment) {
    const dx = segment.x2 - segment.x1;
    const dy = segment.y2 - segment.y1;
    const lengthSquared = dx * dx + dy * dy;
    if (lengthSquared === 0) return Math.hypot(point.x - segment.x1, point.y - segment.y1);
    const along = Math.min(
      1,
      Math.max(0, ((point.x - segment.x1) * dx + (point.y - segment.y1) * dy) / lengthSquared)
    );
    return Math.hypot(point.x - (segment.x1 + along * dx), point.y - (segment.y1 + along * dy));
  }

  function insideRect(point, rect) {
    return (
      point.x >= rect.x &&
      point.x <= rect.x + rect.w &&
      point.y >= rect.y &&
      point.y <= rect.y + rect.h
    );
  }

  // How many drawings this tab has started, for naming one before the server has.
  let drawingCounter = 0;

  /// Begin a new drawing at the pointer.
  ///
  /// A placement is a drag like any other, which is why it sets `drag` rather than
  /// keeping a state of its own. `onPointerMove` needs it to carry the pointer
  /// moves, and -- the part that is easy to leave out -- `onPointerUp` needs it to
  /// reach `finishPlacing` at all: without it every handler returns early and the
  /// shape appears under the press and then simply sits there, unstored.
  function startPlacing(event) {
    const at = plotFraction(event);
    if (!at) return;
    placing = {
      id: `new-${(drawingCounter += 1)}`,
      kind: tool,
      a1: { unit: "fraction", x: at.x, y: at.y },
      // Two separate objects even when they start in the same place: `a2` follows
      // the pointer and `a1` does not, and one shared object would move both.
      a2: tool === "hline" ? null : { unit: "fraction", x: at.x, y: at.y },
      label: null,
    };
    drag = { mode: "place", appliedX: event.clientX, appliedY: event.clientY };
    el("chart").setPointerCapture(event.pointerId);
    // No drag class. `crosshair` is already the canvas's cursor and it is the right
    // one for drawing; adding `dragging` here would claim the chart is about to be
    // panned, which is the one thing a placement does not do.
    scheduleRender();
  }

  /// The price a drawing marks, as the engine resolved it.
  ///
  /// Read from the scene's own object rather than computed, for the same reason
  /// `fractionsOf` reads the resolved anchors: the shell cannot turn a pointer
  /// position into a price, and an anchor the engine has already resolved
  /// carries the answer.
  ///
  /// A drawing can legitimately have no price -- as an unresolved anchor, or
  /// once the whole shape has been dragged off the plot. `null` rather than `0`,
  /// because a level at zero on a BTC chart is a fact and "we do not know" is
  /// not.
  function anchorPrice(drawing) {
    const resolved = resolvedDrawing(drawing.id);
    const anchor = resolved ? resolved.a2 || resolved.a1 : null;
    if (anchor && anchor.unit === "absolute" && Number.isFinite(anchor.price)) {
      return anchor.price;
    }
    // Not yet resolved by the engine: fall back to whatever the stored anchor
    // says, which is absolute for anything that has been saved.
    const stored = drawing.a2 || drawing.a1;
    if (stored && stored.unit === "absolute" && Number.isFinite(stored.price)) {
      return stored.price;
    }
    return null;
  }

  /// The engine's answer for one drawing: the same shape, anchors absolute.
  ///
  /// This is where a placement or a drag stops being a pointer position. The scene
  /// is the only thing that knows what a fraction resolved to, and it is what gets
  /// stored -- so a reload puts the drawing back exactly where it was drawn.
  function resolvedDrawing(id) {
    return scene ? scene.drawings.find((d) => d.id === id) : undefined;
  }

  /// Where a drawing's anchors sit in plot fractions, as the engine reported them.
  ///
  /// Read from the scene rather than computed, and that is the whole point: the
  /// shell has no way to turn a price into a fraction, so it asks for the answer
  /// instead of deriving one.
  function fractionsOf(id) {
    const drawing = resolvedDrawing(id);
    if (!drawing) return null;
    return { a1: drawing.a1_fraction, a2: drawing.a2_fraction ?? null };
  }

  /// A plot fraction moved by a fraction of the plot.
  ///
  /// Adding two fractions is the same class of arithmetic as the division in
  /// `plotFraction`: both are about where something is on the canvas. Neither is
  /// about what a price is, which is why this is allowed here and `price * 1.01`
  /// would not be.
  function shifted(fraction, dx, dy) {
    return { unit: "fraction", x: fraction.x + dx, y: fraction.y + dy };
  }

  /// Store the drawing that was just placed.
  async function finishPlacing() {
    const pending = placing;
    if (!pending) return;

    // Render synchronously first: the coalesced frame may not have run -- a click
    // is a down and an up with no frame between them -- and the engine's answer is
    // the only thing that knows what the fractions resolved to.
    renderNow();
    const resolved = resolvedDrawing(pending.id);
    const reason = scene ? scene.note : null;
    placing = null;

    if (!resolved) {
      // The engine refused it, and its note says why. The shape leaves with
      // `placing` -- a drawing that cannot be drawn must not sit in the list
      // looking like one that can -- and the reason is put back on screen, because
      // a refusal that flashes past along with the shape is one nobody reads.
      renderNow();
      if (reason) note(reason);
      return;
    }
    selectedDrawing = resolved.id;
    // One command for the whole placement, and the save is **awaited** before
    // the command is recorded: `createDrawing` replaces the shell's `new-N` id
    // with the server's row id, and a command recorded before that swap would
    // undo by an id that no longer exists. `byShape` is the reconciliation
    // both directions use -- after a save, after a delete, after a redo, the
    // drawing is found by what it *is*, not by which id it carries today.
    // Errors surface here rather than vanishing into a floating promise, the
    // way `void createDrawing(...)` would hide them.
    const shape = shapeKey(resolved);
    const byShape = () => drawings.find((d) => shapeKey(d) === shape);
    try {
      await createDrawing(resolved);
    } catch (e) {
      note(`the drawing was not saved: ${e.message}`);
      return;
    }
    runCommand(
      "draw",
      // The do-half is what redo runs. The fresh draw's save has already
      // happened above, so this acts only when the shape is actually absent --
      // which is exactly what redo-after-undo is. Unguarded, a fresh draw would
      // save twice: once awaited for its error handling, once from here.
      () => {
        if (byShape()) return;
        void createDrawing(resolved);
      },
      () => {
        const target = byShape();
        if (!target) return;
        drawings = drawings.filter((d) => d !== target && d.id !== target.id);
        if (selectedDrawing === target.id) selectedDrawing = null;
        renderNow();
        void api(`/drawings/${target.id}`, { method: "DELETE" }).catch(() => {});
      },
    );
  }

  /// Store a drawing the user has just moved.
  ///
  /// `movedFrom` is the engine's absolute anchors as the scene reported them when
  /// the grab began -- captured there because the drag overwrites the list with
  /// fractions, and an undo that reinstated fractions would be refused by the
  /// storage. Async because the command is recorded **after** the PUT settles:
  /// the move's undo must restore the anchors the server actually accepted, and
  /// a record-before-settle would let a failed PUT undo into an error.
  async function finishMoving(id, movedFrom) {
    renderNow();
    const resolved = resolvedDrawing(id);
    const stored = drawings.find((d) => d.id === id);
    if (!resolved || !stored) return;

    // The list takes the engine's numbers, so what is on screen and what is about
    // to be stored are the same two points rather than two roundings of one.
    const before = movedFrom ?? { a1: stored.a1, a2: stored.a2 };
    const after = { a1: resolved.a1, a2: resolved.a2 };
    // Applied optimistically -- the shape follows the pointer's release, which
    // is what "released" means -- and reconciled after the PUT like any save.
    stored.a1 = after.a1;
    stored.a2 = after.a2;
    renderNow();
    try {
      await api(`/drawings/${id}`, {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(drawingBody(stored)),
      });
    } catch (e) {
      note(`the drawing was moved but not saved: ${e.message}`);
      return;
    }
    // "Already at the after-anchors" is the guard the do-half needs: the fresh
    // move applied them above, so only a redo-after-undo finds them behind.
    const sameAnchor = (a, b) =>
      (!a && !b) || Boolean(a && b && a.time === b.time && a.price === b.price);
    const isAfter = (d) =>
      sameAnchor(d.a1, after.a1) && sameAnchor(d.a2, after.a2);
    runCommand(
      "move",
      () => {
        const target = byShapeId(stored, id);
        if (!target || isAfter(target)) return;
        target.a1 = after.a1;
        target.a2 = after.a2;
        if (selectedDrawing && selectedDrawing !== target.id) selectedDrawing = target.id;
        renderNow();
        void putDrawing(target);
      },
      () => {
        // By the time this runs the drawing's id may have changed (a later
        // undo/redo cycle through a delete), so it is found by shape.
        const target = byShapeId(stored, id);
        if (!target) return;
        target.a1 = before.a1;
        target.a2 = before.a2;
        if (selectedDrawing && selectedDrawing !== target.id) selectedDrawing = target.id;
        renderNow();
        void putDrawing(target);
      },
    );
  }

  /// The stored drawing that *is* `hint`, matching first by id and then by
  /// shape -- see `shapeKey` for why the second half exists.
  function byShapeId(hint, fallbackId) {
    return drawings.find((d) => d.id === (hint && hint.id)) || drawings.find((d) => d.id === fallbackId) || drawings.find((d) => shapeKey(d) === shapeKey(hint));
  }

  /// The API's body for one drawing.
  ///
  /// Built from the engine's object rather than from a shape of our own: `kind` and
  /// the two anchors are exactly what `scene.drawings` reports, which is why they
  /// are read from there and never assembled from a pointer position.
  function drawingBody(drawing) {
    return {
      kind: drawing.kind,
      a1: drawing.a1,
      a2: drawing.a2 ?? null,
      label: drawing.label ?? null,
    };
  }

  /// Whether two drawing objects are the *same shape*, ignoring their ids.
  ///
  /// Undo has to find a drawing again after its id has changed underneath it:
  /// a save replaces the shell's `new-N` with the server's UUID, and a redo
  /// creates the shape a second time under a third id. The shape -- kind,
  /// anchors, label -- is what survives all of that, and the engine has
  /// already resolved both anchors to absolute numbers, so the comparison is
  /// exact equality rather than a tolerance.
  const shapeKey = (d) =>
    [
      d.kind,
      d.a1 && d.a1.time, d.a1 && d.a1.price,
      d.a2 && d.a2.time, d.a2 && d.a2.price,
      d.label ?? "",
    ].join("|");

  async function createDrawing(drawing) {
    // On the chart first and in the database second. The managed instance costs
    // about a second a statement, and a shape that disappears for a second after
    // the user finished drawing it reads as a refusal -- the same reasoning as
    // `deleteSelected`, in the other direction.
    drawings.push(drawing);
    selectedDrawing = drawing.id;
    renderNow();

    let reason = null;
    try {
      const saved = await api("/drawings", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ symbol: el("symbol").value, ...drawingBody(drawing) }),
      });
      // The row's id replaces the local one, so the next drag moves the stored
      // drawing rather than creating a second copy of it.
      drawings = drawings.map((d) => (d.id === drawing.id ? fromServer(saved) : d));
      selectedDrawing = saved.id;
    } catch (e) {
      // It was never stored, so it must not stay on the chart looking like one
      // that was. The same rule `finishPlacing` applies to a refusal, applied to
      // the one refusal only the network can produce.
      drawings = drawings.filter((d) => d.id !== drawing.id);
      if (selectedDrawing === drawing.id) selectedDrawing = null;
      reason = `the drawing was not saved: ${e.message}`;
    }
    // The render owns the note strip -- it writes the engine's own note into it --
    // so a failure has to be stated *after* the frame that was meant to show it,
    // or it is wiped by the render it was asking for.
    renderNow();
    if (reason) note(reason);
  }

  async function putDrawing(drawing) {
    try {
      await api(`/drawings/${drawing.id}`, {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(drawingBody(drawing)),
      });
    } catch (e) {
      // The move is on screen but not stored. Saying so is the only honest option:
      // silently reverting would look like the drag had been ignored, and silently
      // keeping it would mean a reload loses the change.
      note(`the drawing was moved but not saved: ${e.message}`);
    }
  }

  /// How long an armed "Clear all" stays armed.
  ///
  /// Long enough to be a deliberate second click, short enough that a button
  /// nobody touches again does not stay dangerous. The timeout is the point: a
  /// control that stays armed until something else happens fires on a click the
  /// user made about something else.
  const CLEAR_ARMED_MS = 4000;

  /// When the armed "Clear all" gives up, as a `Date.now()` reading. 0 is disarmed.
  let clearArmedUntil = 0;
  let clearArmedTimer = 0;

  /// Say which of the drawing controls can do anything.
  ///
  /// Delete is *disabled* rather than drawn and then silently returning: a
  /// button that is drawn and refuses is worse than one that is not there, and
  /// "nothing is selected" is a state the user can be in at any moment rather
  /// than an error.
  function refreshDrawingButtons() {
    const remove = el("deleteDrawing");
    if (remove) remove.disabled = !selectedDrawing;
  }

  /// Disarm "Clear all", if it is armed.
  function disarmClear() {
    if (!clearArmedUntil) return;
    clearArmedUntil = 0;
    if (clearArmedTimer) {
      clearTimeout(clearArmedTimer);
      clearArmedTimer = 0;
    }
    const button = el("clearDrawings");
    if (button) {
      button.textContent = "Clear all";
      button.setAttribute("aria-pressed", "false");
    }
  }

  /// Remove the selected drawing.
  async function deleteSelected() {
    const id = selectedDrawing;
    if (!id) return;
    // Off the chart first and out of the database second. The managed instance
    // costs about a second a statement, and a delete button that does nothing for
    // a second reads as broken.
    const removed = drawings.find((d) => d.id === id);
    selectedDrawing = null;
    drawings = drawings.filter((d) => d.id !== id);
    renderNow();
    try {
      await api(`/drawings/${id}`, { method: "DELETE" });
    } catch (e) {
      note(`the drawing left the chart but not the database: ${e.message}`);
    }
    if (removed) {
      // The removal itself is the command's do-half; the undo-half puts it back
      // and saves it. New id from the server on the way back, like any create.
      // The do-half is guarded the same way the draw and move commands are: the
      // fresh delete has already run above, so it only removes again when the
      // drawing is actually present -- which is what redo-after-undo is.
      runCommand(
        "delete",
        () => {
          const target =
            drawings.find((d) => d.id === removed.id) ||
            drawings.find((d) => shapeKey(d) === shapeKey(removed));
          if (!target) return;
          drawings = drawings.filter((d) => d !== target && d.id !== target.id);
          if (selectedDrawing === target.id) selectedDrawing = null;
          renderNow();
          void api(`/drawings/${target.id}`, { method: "DELETE" }).catch(() => {});
        },
        () => void createDrawing(removed),
      );
    }
  }

  /// Remove every drawing on this symbol, after asking.
  ///
  /// ## Why this asks
  ///
  /// This button was labelled `Clear` and deleted everything the instant it was
  /// clicked, sitting in a row of *drawing tools*. So the reading every other
  /// charting package trains -- "clear the thing I have selected" -- was wrong,
  /// and nothing said so. A control whose label does not match what it does is
  /// worse than a missing one: the user finds out afterwards, and the drawings
  /// are already gone.
  ///
  /// So it is named for what it does and it takes two clicks. The second click is
  /// the confirmation, and it is deliberately not a modal: an interrupt dialog
  /// for a chart annotation is a heavier cost than the mistake it prevents.
  async function clearDrawings() {
    const button = el("clearDrawings");
    const now = Date.now();
    if (now > clearArmedUntil) {
      clearArmedUntil = now + CLEAR_ARMED_MS;
      if (button) {
        button.textContent = "Sure?";
        button.setAttribute("aria-pressed", "true");
      }
      clearArmedTimer = setTimeout(disarmClear, CLEAR_ARMED_MS);
      return;
    }
    disarmClear();

    const ids = drawings.map((d) => d.id);
    if (!ids.length) return;
    const removedAll = drawings;
    selectedDrawing = null;
    placing = null;
    drawings = [];
    renderNow();
    // One at a time. A burst of concurrent deletes over a ten-connection pool is
    // how a request path starts failing for somebody else, and nothing here is in
    // a hurry. The command is recorded **after** the deletes settle, like every
    // other command in this file: an undo that ran while the deletes were still
    // in flight would re-create rows the deletes were about to (or had just)
    // removed, and the "clear" would end as a duplicate.
    for (const id of ids) {
      try {
        await api(`/drawings/${id}`, { method: "DELETE" });
      } catch (e) {
        note(`some drawings were not removed from the database: ${e.message}`);
        return;
      }
    }
    runCommand(
      "clear",
      // Guarded like every do-half here: the fresh clear removed everything
      // above, so this only acts when shapes are actually present -- which is
      // what redo-after-undo is.
      () => {
        const remaining = drawings.filter((d) =>
          removedAll.some((r) => shapeKey(d) === shapeKey(r))
        );
        if (!remaining.length) return;
        for (const d of remaining) {
          void api(`/drawings/${d.id}`, { method: "DELETE" }).catch(() => {});
        }
        drawings = drawings.filter((d) => !remaining.includes(d));
        if (selectedDrawing && remaining.some((d) => d.id === selectedDrawing)) {
          selectedDrawing = null;
        }
        renderNow();
      },
      () => {
        for (const drawing of removedAll) void createDrawing(drawing);
      },
    );
  }

  /// Select one drawing, or none.
  ///
  /// Only the variable. Which drawing is selected is sent to the engine from
  /// `selectedDrawing` at render time, so there is no per-drawing flag to keep in
  /// step -- and the early return is safe for the same reason, where it would have
  /// been a bug against stored flags.
  function select(id) {
    if (selectedDrawing === id) return;
    selectedDrawing = id;
    scheduleRender();
  }

  /// Switch the active tool.
  ///
  /// `aria-pressed` is the state, like the Zones toggle: one variable, and the
  /// button cannot disagree with it. The tool stays active until it is changed, so
  /// several shapes can be drawn in a row.
  function selectTool(name) {
    tool = name;
    for (const button of root.querySelectorAll(".tools button[data-tool]")) {
      button.setAttribute("aria-pressed", String(button.dataset.tool === name));
    }
    // A group's trigger labels itself with the tool the user picked, so the
    // closed toolbar still says what is armed -- "Lines" reads "Lines · Ray".
    refreshGroupTriggers();
  }

  /// Update every group trigger's word from the pressed tool.
  ///
  /// The trigger's resting label is the group's own; while one of its tools is
  /// active the label gains that tool, and the `data-` attributes are where the
  /// resting label and group name survive the update. No-op on a fallback
  /// toolbar that has no groups yet.
  function refreshGroupTriggers() {
    for (const trigger of root.querySelectorAll(".toolGroupTrigger")) {
      const base = trigger.dataset.groupLabel;
      if (!base) continue;
      const pressed = groupPressedLabel(trigger.dataset.group);
      trigger.textContent = pressed ? `${base} · ${pressed}` : base;
      trigger.title = pressed ? `${base}: ${pressed} is armed` : trigger.title;
    }
  }

  /// Toggle the magnet. `aria-pressed` is the state, exactly as a tool's is.
  function toggleMagnet() {
    magnet = !magnet;
    const button = el("magnet");
    if (button) button.setAttribute("aria-pressed", String(magnet));
    scheduleRender();
  }

  /// Rebuild this pane's tool buttons from the engine's registry, grouped into
  /// labelled flyouts (`docs/21`).
  ///
  /// The markup ships a flat fallback of five buttons so the toolbar exists
  /// before the engine loads; this replaces them with the registry's own set.
  /// A button is *generated*, not stored: the click handler is rebound below,
  /// and the pressed state is re-derived by `selectTool` from `tool`, so there
  /// is no per-button state to carry across. Kept as the pane's last child so
  /// the right-click menu hosting and the harness selectors both keep working.
  function buildToolbarFromRegistry() {
    if (!toolRegistry || !toolRegistry.length) return;
    const toolbar = root.querySelector(".tools") || document.querySelector(".tools");
    if (!toolbar) return;

    // The controls that are not tools: they sit after the groups, in this order.
    const controls = [
      ...toolbar.querySelectorAll("button[data-magnet], button[data-undo], button[data-redo], .deleteDrawing, .clearDrawings"),
    ];

    // One flyout per group, in the engine's order. The trigger carries the
    // group's label; the flyout is a plain list of buttons, so the harness's
    // `button[data-tool="…"]` selectors keep working against the generated set.
    const groups = [];
    for (const entry of toolRegistry) {
      let group = groups.find((g) => g.name === entry.group);
      if (!group) {
        // The trigger's word is the engine's own `group_label`; a registry row
        // from an older engine without one falls back to the wire name.
        const label = entry.group_label || entry.group;
        group = { name: entry.group, label, tools: [] };
        groups.push(group);
      }
      group.tools.push(entry);
    }

    const frag = document.createDocumentFragment();
    // The cursor is not a registry entry -- it is the shell's selection state,
    // not an object the engine draws -- so it stays the first, hand-written one.
    const cursor = document.createElement("button");
    cursor.dataset.tool = "cursor";
    cursor.textContent = "Cursor";
    cursor.title = "Select a drawing, move its anchors, or pan the chart";
    cursor.setAttribute("aria-pressed", String(tool === "cursor"));
    frag.appendChild(cursor);

    for (const group of groups) {
      const wrap = document.createElement("div");
      wrap.className = "toolGroup";
      const trigger = document.createElement("button");
      trigger.className = "toolGroupTrigger";
      trigger.textContent = group.label;
      trigger.title = group.tools.map((t) => t.label).join(", ");
      // What `refreshGroupTriggers` needs to relabel the trigger later: which
      // group it opens, and what its resting word is.
      trigger.dataset.group = group.name;
      trigger.dataset.groupLabel = group.label;
      trigger.setAttribute("aria-haspopup", "true");
      trigger.setAttribute("aria-expanded", "false");
      const flyout = document.createElement("div");
      flyout.className = "toolFlyout";
      flyout.hidden = true;
      for (const entry of group.tools) {
        const button = document.createElement("button");
        button.dataset.tool = entry.kind;
        button.textContent = entry.label;
        button.title = entry.title || entry.label;
        button.setAttribute("aria-pressed", String(tool === entry.kind));
        flyout.appendChild(button);
      }
      // Open on the trigger, close on any pick inside the same click.
      trigger.addEventListener("click", (event) => {
        event.stopPropagation();
        const open = !flyout.hidden;
        for (const other of toolbar.querySelectorAll(".toolFlyout")) other.hidden = true;
        flyout.hidden = open;
        trigger.setAttribute("aria-expanded", String(!flyout.hidden));
      });
      flyout.addEventListener("click", () => {
        flyout.hidden = true;
        trigger.setAttribute("aria-expanded", "false");
      });
      wrap.appendChild(trigger);
      wrap.appendChild(flyout);
      frag.appendChild(wrap);
    }
    for (const control of controls) frag.appendChild(control);

    toolbar.replaceChildren(frag);
    // The generated buttons get the same listener the fallback had.
    for (const button of toolbar.querySelectorAll("button[data-tool]")) {
      button.addEventListener("click", () => selectTool(button.dataset.tool));
    }
    // And the pane's own wiring for the controls it kept. `data-wired` makes
    // the whole rebuild idempotent: it runs once from `wire` and again when the
    // page's post-load loop rebuilds every pane, and a control wired twice
    // would toggle twice per click -- which is a magnet that does nothing.
    const wire = (selector, handler) => {
      const button = toolbar.querySelector(selector);
      if (button && !button.dataset.wired) {
        button.dataset.wired = "1";
        button.addEventListener("click", handler);
      }
    };
    wire("button[data-magnet]", toggleMagnet);
    wire("button[data-undo]", undo);
    wire("button[data-redo]", redo);
  }

  /// This symbol's drawings.
  async function loadDrawings() {
    const symbol = el("symbol").value.toUpperCase();
    selectedDrawing = null;
    placing = null;

    if (!token()) {
      // Drawings belong to a user, so without a token there is nothing to ask for
      // and the answer would be a 401 rather than an empty list. Cleared rather
      // than kept: the previous user's drawings are not this one's.
      drawings = [];
      return;
    }

    try {
      const data = await api(`/drawings?symbol=${encodeURIComponent(symbol)}`);
      // The response echoes the symbol it is for, so a reply that arrived after the
      // user moved on is discarded rather than drawn on the wrong chart, at prices
      // that look plausible.
      drawings = data.symbol === symbol ? data.drawings.map(fromServer) : [];
    } catch (e) {
      drawings = [];
      note(`drawings could not be loaded: ${e.message}`);
    }
  }

  /// One drawing from the API, in the engine's own shape.
  ///
  /// Four fields, and `selected` is deliberately not one of them: the API has no
  /// opinion about which drawing this tab is looking at, and `render` derives it
  /// from `selectedDrawing`. Carrying it here would be a field nothing reads.
  function fromServer(drawing) {
    return {
      id: drawing.id,
      kind: drawing.kind,
      a1: drawing.a1,
      a2: drawing.a2 ?? null,
      label: drawing.label ?? null,
    };
  }

  /// Say something about the chart, in the strip the engine's own notes use.
  function note(message) {
    el("chartNote").textContent = message;
  }

  /// Milliseconds per bar, for sizing a footprint window.
  const BAR_MS = { "1m": 60_000, "5m": 300_000, "1h": 3_600_000, "4h": 14_400_000 };

  /// Fetch a footprint for a window that actually has trades.
  ///
  /// ## Why this asks for coverage first
  ///
  /// Trades are backfilled in capped chunks, so the *newest* candles almost never
  /// have any -- and asking for them returns a 404. That is correct and useless:
  /// the user selects "Footprint" and gets an error naming a window they have to
  /// work out for themselves.
  ///
  /// So the chart asks where the trades are and uses that window. Selecting the
  /// chart type is then enough.
  ///
  /// ## Why the column count comes from the engine
  ///
  /// A ladder cell has to fit the widest `bid x ask` in the window -- eleven
  /// characters for `1.40 x 2.85`, more for a symbol whose volumes carry a `K`.
  /// The column count is a layout decision, so the shell makes it -- but *how
  /// wide a cell has to be* is not a layout decision the shell can make, because
  /// it is the engine's own font floor read backwards.
  ///
  /// This used to be a constant here, `MIN_COLUMN_PX`, and it went 54 then 64 by
  /// hand. Both were guesses at a number the engine already knows: 54 came out at
  /// a 6.98px font in a real window -- one hundredth of a pixel under the point
  /// where the engine stops drawing numbers -- and 64 was the fudge for it. The
  /// engine now reports `Grid::min_cell_px`, which is the same arithmetic the
  /// layout uses to size its font, so `plot / min_cell_px` is the *most* columns
  /// that stay legible rather than a count kept safely under the real figure.
  ///
  /// It also fixes the width the count is divided by. `width` was the element,
  /// and the plot is narrower than the element by the price axis -- which is what
  /// made the 54px column a 53px cell. The last scene knows the plot exactly.
  async function loadFootprint() {
    const symbol = el("symbol").value;
    const timeframe = el("timeframe").value;

    const elementW = el("chart").parentElement.clientWidth || 900;
    // Before the first scene there is nothing to ask, so subtract the axis the
    // way `drawAxis` lays it out -- an estimate, but an estimate of the *right*
    // rectangle rather than of the element that contains it.
    const plotW = scene?.plot?.w || Math.max(200, elementW - 62);

    // How many candles fit is `plot / cell`, and the engine says how wide a cell
    // its own numbers need. The 60 cap is a sanity bound rather than a layout
    // rule: past it the ladders are a texture, whatever the arithmetic says.
    const cellPx = footCellPx || SEED_CELL_PX;
    const columns = Math.max(8, Math.min(60, Math.floor(plotW / cellPx)));

    const coverage = await api(`/footprint/coverage?symbol=${symbol}`);
    const bar = BAR_MS[timeframe] || 300_000;
    // The newest `columns` bars of the covered span.
    const to = coverage.to;
    const from = Math.max(coverage.from, to - columns * bar);

    // No bucket_size: the route picks one from the window's own price range, so
    // the row count is readable whatever the instrument costs.
    return await api(
      `/footprint?symbol=${symbol}&timeframe=${timeframe}&from=${from}&to=${to}`
    );
  }

  /// A bar's opening time as `HH:MM`, in the zone the chart is already labelled in.
  ///
  /// A unit conversion at a boundary rather than market arithmetic -- the same
  /// trade `docs/14` allows for a timestamp. Everything else about this bar came
  /// from the engine or the server.
  function formatBar(ns) {
    if (!ns) return "--:--";
    const at = new Date(Math.floor(Number(ns) / 1e6));
    const pad = (n) => String(n).padStart(2, "0");
    return `${pad(at.getUTCHours())}:${pad(at.getUTCMinutes())}`;
  }

  /// How far the drawn ladder is behind the live feed, in milliseconds.
  ///
  /// Zero when there is nothing to compare, or when the chart is not drawn from
  /// stored trades at all. This exists because the badge's main reading is about
  /// the *channel*, and in footprint mode the chart is not drawn from the channel:
  /// the ladder comes from `/footprint`, which is built from stored trades. So a
  /// page can honestly report a live feed while the thing on screen has not moved
  /// since the last backfill -- which is precisely the report that produced this
  /// badge, and the reason a badge that only knew about the socket would have been
  /// green through all of it.
  function ladderLagMs() {
    if (!footprint || !live.bar || !footprint.candles || !footprint.candles.length) return 0;
    const newest = footprint.candles.reduce(
      (best, candle) => Math.max(best, Number(candle.open_time)),
      0
    );
    if (!newest) return 0;
    // Both are nanoseconds, so the difference is taken before the conversion --
    // a nanosecond timestamp is past the point where a double counts integers,
    // and subtracting the two first keeps the interval exact.
    return Math.max(0, (Number(live.bar) - newest) / 1e6);
  }

  /// A lag in the unit that makes it readable: minutes while it is minutes, hours
  /// once it is not.
  function lagText(ms) {
    if (ms >= 3_600_000) return `${Math.round(ms / 3_600_000)}h`;
    return `${Math.max(1, Math.round(ms / 60_000))}m`;
  }

  /// Say what the live channel is doing, and make the claim checkable.
  ///
  /// The badge carries the **age of the last frame that arrived**, not the state
  /// of the socket, because those are different facts and only one of them is
  /// what the user is asking about. A socket can be open and silent -- which is
  /// precisely the report this exists to answer, seven hours of the same candles
  /// with nothing on the page saying whether the channel was working -- so a
  /// badge driven by `readyState` would have sat there green through all of it.
  ///
  /// The age is also why it is a measurement rather than a label: it counts up
  /// while nothing arrives, and the count resets when a bar closes. If the feed
  /// stops, the number climbing past the threshold *is* the evidence.
  ///
  /// Silence is normal for most of a bar, so the threshold is a bar and a half
  /// plus a margin rather than a few seconds -- a closed-candle feed has nothing
  /// to say between closes, and a badge that flickered amber every bar would
  /// teach the user to ignore it.
  function refreshLiveBadge() {
    const badge = el("feedStatus");
    if (!badge) return;

    const bar = BAR_MS[el("timeframe").value] || 300_000;
    const quietFor = bar * 1.5 + 30_000;
    const age = live.at ? Date.now() - live.at : Infinity;
    const seconds = Math.round(age / 1000);

    const set = (state, text, title) => {
      badge.dataset.state = state;
      badge.textContent = text;
      if (title) badge.title = title;
    };

    if (live.state === "idle") {
      set("idle", "no channel yet", "This chart has not opened a market channel.");
      return;
    }
    if (live.state === "connecting") {
      set("idle", "connecting…", "Opening the market channel.");
      return;
    }
    if (live.state === "nofeed") {
      set(
        "nofeed",
        "no feed",
        "The server has no market feed configured, so nothing will arrive unless something else publishes. See the note under the chart."
      );
      return;
    }
    if (live.state === "offline") {
      set(
        "offline",
        "offline",
        "The market channel closed. Reload to reopen it."
      );
      return;
    }
    // Open, and no bar yet. A closed-candle feed says nothing until a bar closes,
    // so this is the honest reading for up to a whole bar after connecting --
    // "live" here would be a claim about data that has not arrived.
    if (!live.at) {
      set(
        "idle",
        "waiting for a bar",
        "The channel is open. This feed publishes closed bars, so the first one arrives at the end of the current bar."
      );
      return;
    }
    if (age > quietFor) {
      set(
        "stale",
        `quiet ${Math.round(seconds / 60)}m`,
        `No candle has arrived for ${seconds}s. The channel is open, so this is either a quiet market or a feed that has stopped.`
      );
      return;
    }
    // Live on the wire, and the chart still not moving. This is the reading that
    // makes the badge trustworthy rather than reassuring: in footprint mode the
    // ladder is built from stored trades, and nothing persists the live feed yet,
    // so the channel can be perfectly healthy while the ladder is hours old.
    // Saying "live" there would be the one thing this badge must never do.
    const barMs = BAR_MS[el("timeframe").value] || 300_000;
    const lag = ladderLagMs();
    if (lag > barMs * 2) {
      set(
        "behind",
        `feed live · ladder ${lagText(lag)} behind`,
        `Candles are arriving on the market channel, but this chart is drawn from stored trades and the newest stored one is ${lagText(lag)} old. The feed is live; the stored series is not.`
      );
      return;
    }
    // The age is the point: it is what makes "live" a reading rather than a
    // promise, and it is what the user can watch reset when a bar closes.
    set(
      "live",
      `live · ${el("symbol").value} ${el("timeframe").value} · ${formatBar(live.bar)} · ${seconds}s ago`,
      "Candles are arriving on the market channel. The age resets each time a bar closes."
    );
  }

  /// Follow the live candle channel.
  ///
  /// The token goes in the query string because a browser cannot set a header on
  /// a WebSocket handshake. The channel is public anyway, but passing the token
  /// when we have one keeps the code honest about which channels need it.
  function connectLive() {
    // Closing a socket that has not opened yet is what makes a browser say
    // "WebSocket is closed before the connection is established" -- and it
    // throws away a handshake already in flight, so a pane that is re-pointed
    // quickly pays for several connections to use one. A socket still
    // connecting is told to close itself when it gets there instead.
    //
    // Captured, because `socket` is about to point at the new one and a closure
    // over the variable would close the socket we are trying to open.
    if (socket) {
      const stale = socket;
      if (stale.readyState === WebSocket.OPEN) stale.close();
      else stale.onopen = () => stale.close();
    }
    const symbol = el("symbol").value;
    const timeframe = el("timeframe").value;
    const scheme = location.protocol === "https:" ? "wss" : "ws";
    const query = token() ? `?token=${encodeURIComponent(token())}` : "";

    // A reconnect is a new channel, so the old reading is not evidence about the
    // new one. Reset before opening rather than after: the gap between the two
    // would otherwise show the previous socket's age against the new socket.
    live.state = "connecting";
    live.at = 0;
    live.bar = 0;
    // Any reconnect this socket had scheduled is its own funeral: the new
    // connection is the plan now, and a timer left running would close it a
    // few seconds later for a death that already stopped mattering.
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = 0;
    }
    refreshLiveBadge();

    const ws = new WebSocket(
      `${scheme}://${location.host}/ws/market/${symbol}/${timeframe}${query}`
    );
    socket = ws;
    ws.onopen = () => {
      if (socket !== ws) return;
      live.state = "open";
      refreshLiveBadge();
    };
    ws.onmessage = (event) => {
      // Same guard as `onopen`: a frame from a channel this pane has already
      // left is not evidence about the one it is on now.
      if (socket !== ws) return;
      let frame;
      try {
        frame = JSON.parse(typeof event.data === "string" ? event.data : new TextDecoder().decode(event.data));
      } catch { return; }

      if (frame.type === "data") {
        // Replace the last candle if it is the same bucket, append a newer one,
        // and drop an older one. The server publishes forming frames and closes
        // from one task in order -- a bucket's close, then the next bucket's
        // forming bar -- but this pane also survives reconnects, lag and a
        // window that was refetched mid-stream -- anywhere the frame sequence
        // can hiccup. In-order frames are handled in order, out-of-order ones
        // refused: the chart never moves backwards. One path for every frame,
        // because a forming bar and the close that supersedes it are the same
        // event at two moments, not two kinds of data. This is the only
        // market-data decision this file makes, and it is about identity, not
        // value.
        const incoming = frame.payload;
        const last = candles[candles.length - 1];
        if (last && incoming.open_time < last.open_time) {
          // stale frame for a bucket already superseded
        } else if (last && last.open_time === incoming.open_time) {
          candles[candles.length - 1] = incoming;
        } else {
          candles.push(incoming);
        }
        if (candles.length > Number(el("limit").value) + 50) candles.shift();
        // The live price is the newest bar's own close -- on a forming frame,
        // its last trade. Written before the repaint so the price line and the
        // newest bar move in the same frame rather than one after the other.
        lastPrice = incoming.close;
        // Stamped on arrival rather than on the bar's own time: a replayed bar
        // arrives now, and "is the feed alive" is a question about arrivals.
        live.at = Date.now();
        live.bar = incoming.open_time;
        // A frame outranks a notice, and this is the only place that can say so.
        // The server explains a silence; it does not promise the silence lasts --
        // `MARKET_FEED` being off means *this gateway* opens no feed, not that
        // nothing will ever publish into the bus, and a channel that was told
        // there is no feed still forwards a candle that arrives. Without this,
        // that candle would make the age tick while the badge went on saying
        // "no feed", which is the badge contradicting its own evidence.
        live.state = "open";
        render();
      } else if (frame.type === "notice") {
        // The server's explanation outranks the engine's note. "The feed is not
        // configured" is why the candles are not moving; the engine's own note
        // would be a true answer to a different question. A close does not erase
        // it -- `onclose` deliberately leaves the message alone, because a socket
        // that closed is not a reason to forget why.
        feedNotice = frame.message;
        live.state = "nofeed";
        el("chartNote").textContent = feedNotice;
        refreshLiveBadge();
      } else if (frame.type === "lagged") {
        // The gap is real: refetch the window and open a fresh channel, rather
        // than redrawing a chart with a hole in it and a note nobody reads.
        // The old socket is replaced on purpose; `connectLive` closes it, and
        // its guards keep a superseded socket from speaking for the pane.
        refresh().then(connectLive);
      }
    };
    ws.onclose = () => {
      // A superseded socket must not speak for this pane: it would null the
      // *new* one and set the badge from the death of the old, which is a chart
      // reporting on a channel nobody is using.
      if (socket !== ws) return;
      socket = null;
      // `nofeed` outranks `offline`: the channel closing is not news when the
      // server has already said why, and "offline" beside "no feed is
      // configured" would be two answers to one question.
      if (live.state !== "nofeed") live.state = "offline";
      refreshLiveBadge();
      // A closed channel used to stay closed until a reload, which is how a
      // chart ends up "live" with candles that stopped arriving an hour ago.
      // The feed dies for ordinary reasons -- a network blip, a deploy -- and
      // a chart that cannot recover from one is a chart that lies. Reconnect
      // with backoff; the reset inside `connectLive` clears the stale reading.
      // `nofeed` is *not* retried: the server has said why nothing will arrive,
      // and hammering it will not change `MARKET_FEED`.
      if (live.state !== "nofeed") scheduleReconnect();
    };
  }
  // ---------------------------------------------------------------------------
  // This pane's own listeners
  // ---------------------------------------------------------------------------

  /// The pending reconnect, if there is one. Pane state rather than page state:
  /// two panes watch two channels, and one dying must not reschedule the other.
  let reconnectTimer = 0;

  /// Reopen the channel after a close, with backoff.
  ///
  /// The feed dies for ordinary reasons -- a network blip, a gateway deploy --
  /// and a chart that needs a reload to recover is a chart that quietly lies
  /// for as long as the outage lasts. Backoff rather than an immediate retry,
  /// because a gateway that is restarting will refuse a burst of reconnects
  /// just as it refused the first, and capped at five seconds so recovery is
  /// quick once the endpoint is back.
  function scheduleReconnect() {
    if (reconnectTimer) return;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = 0;
      // The pane may have been re-pointed (or closed) while waiting; closing
      // and re-opening here is what `connectLive` already handles.
      connectLive();
    }, 2000 + Math.floor(Math.random() * 3000));
  }

  // ---------------------------------------------------------------------------
  // The pane's own chrome: title, zoom pair, collapse/expand pair, and the
  // right-click menu that hosts this pane's real controls.
  // ---------------------------------------------------------------------------

  /// Paint the title line from the controls the chart actually runs on.
  ///
  /// Read from the selects rather than stored, for the same reason `symbol()`
  /// is: whatever the next request will use is the truth, and a cached string
  /// would go stale the moment the context menu changed a select behind the
  /// pane's back. Called after anything that can change those values.
  function paintTitle() {
    const symbol = el("symbol").value || "…";
    const timeframe = el("timeframe").value;
    const mode = el("mode");
    const modeLabel = mode.selectedOptions[0] ? mode.selectedOptions[0].textContent : "";
    el("paneTitle").textContent = `${symbol} · ${timeframe}${modeLabel ? " · " + modeLabel.trim() : ""}`;
  }

  /// One button press worth of zoom.
  ///
  /// The engine owns what zoom means (`zoom_time` about the middle of the plot);
  /// this only says "the user asked". The factor follows the wheel's convention
  /// (in `onWheel`: negative deltaY -> factor > 1 -> fewer bars on screen), so
  /// `zoomIn` sends a factor above one. Anchored to the plot centre rather than
  /// a pointer position, because a button has no pointer.
  function zoomStep(zoomIn) {
    if (!scene) return;
    const factor = Math.exp((zoomIn ? 1 : -1) * 0.22);
    applyGesture({ kind: "zoom_time", factor, anchor: 0.5 });
  }

  /// Collapse to the title bar / back.
  ///
  /// A class rather than inline styles, so the CSS owns the layout rule and a
  /// collapsed pane gives its row share back to its sibling automatically
  /// (`.chartPane.min` hides the canvas box; flexbox does the rest).
  function setMin(min) {
    root.classList.toggle("min", min);
    el("minBtn").title = min ? "Restore this chart" : "Collapse this chart to its title bar";
  }

  /// Expand this chart over the whole `#charts` area, and back.
  ///
  /// `position: absolute; inset: 0` over a relative container covers every row
  /// and splitter without touching their layout numbers, so closing the expand
  /// restores the exact grid the user was looking at. `#charts` gets a class
  /// while a child is expanded so the rows provide the positioning context.
  function setExpanded(expanded) {
    const charts = el("charts");
    root.classList.toggle("exp", expanded);
    charts.classList.toggle("exp-ing", expanded);
    el("expBtn").title = expanded ? "Back to the grid" : "Expand this chart over the others";
    if (expanded) draw();
    else for (const pane of panes) pane.redraw();
  }

  /// Close the context menu, returning the hosted controls to the pane.
  ///
  /// The one place that knows how to put them back in order: selects first,
  /// then the status, then the tool row at the end. A pane whose menu forgot
  /// where things lived would lose its controls on the first open/close.
  function closeMenu() {
    const menu = document.getElementById("chartMenu");
    // The guard is the whole safety story of a *shared* menu: if another pane's
    // controls are in there, this pane must not "return" them (it would file
    // pane B's symbol select into pane A's bar). Hosting clears this flag;
    // nothing else sets it.
    if (!menu || menu.dataset.owner !== String(paneId) || !menu.classList.contains("open")) return;
    // The nodes are captured *before* anything moves. The tool row in
    // particular is found in the menu it is hosted in, not the pane: while the
    // menu is open it is outside this pane's subtree, and a lookup from `root`
    // is null. (Each of these lookups in the wrong place stranded the controls
    // once already.)
    const tools = menu.querySelector(".tools") || root.querySelector(".tools");
    const bar = root.querySelector(".chartBar");
    menuHosted = false;
    for (const node of [...menu.querySelectorAll("select, .zones, .fit, .load, .feedStatus")]) {
      bar.appendChild(node);
    }
    // The tool row's home is the chart wrap (it is an overlay on the canvas),
    // not the bar the selects live in.
    el("chartWrap").appendChild(tools);
    bar.hidden = true;
    tools.hidden = true;
    menu.classList.remove("open");
    menu.replaceChildren();
    delete menu.dataset.owner;
  }

  /// Open the right-click menu for this pane, at the pointer.
  ///
  /// The menu does not copy controls; it *hosts* them. The pane's real selects
  /// and tool buttons are moved into the menu, their `change`/`click` wiring
  /// untouched, and moved home on close -- so the menu can never disagree with
  /// the pane about the series, and there is exactly one wiring of every
  /// control in the file. Any other pane's open menu closes first.
  function openMenu(x, y) {
    for (const other of panes) {
      if (other !== paneApi) other.closeMenu();
    }
    const menu = document.getElementById("chartMenu");
    menu.dataset.owner = String(paneId);
    menuHosted = true;
    // Captured first, for the same reason `closeMenu` does: after these nodes
    // move into the menu they are outside the pane subtree and `el()` cannot
    // find them again.
    const tools = root.querySelector(".tools");
    menu.replaceChildren(
      Object.assign(document.createElement("p"), { className: "menuLabel", textContent: "Series" }),
      el("symbol"), el("timeframe"), el("limit"),
      Object.assign(document.createElement("p"), { className: "menuLabel", textContent: "Chart" }),
      el("mode"), el("zones"), el("fit"), el("load"),
      el("feedStatus"),
      document.createElement("hr"),
      Object.assign(document.createElement("p"), { className: "menuLabel", textContent: "Drawing tools" }),
      tools
    );
    // Hosted, the tool row is a plain menu section; it keeps its own tool
    // wiring either way.
    tools.hidden = false;
    menu.classList.add("open");
    const box = menu.getBoundingClientRect();
    menu.style.left = `${Math.min(x, window.innerWidth - box.width - 8)}px`;
    menu.style.top = `${Math.min(y, window.innerHeight - box.height - 8)}px`;
  }

  /// The listeners for the pane's own chrome. Kept beside `wire()` but separate
  /// from it: `wire()` is about the chart and its series, this is about the
  /// pane as a panel.
  function wireChrome() {
    el("zoomIn").addEventListener("click", () => zoomStep(true));
    el("zoomOut").addEventListener("click", () => zoomStep(false));
    el("minBtn").addEventListener("click", () => setMin(!root.classList.contains("min")));
    el("expBtn").addEventListener("click", () => setExpanded(!root.classList.contains("exp")));
    // Right-click on the chart opens the menu; anything else closes it. On the
    // canvas itself the browser's own menu is ours to take -- the chart is the
    // one thing on the page with no use for "Save image as".
    el("chartWrap").addEventListener("contextmenu", (event) => {
      event.preventDefault();
      setActive(paneApi);
      openMenu(event.clientX, event.clientY);
    });
    // A click on this pane's canvas closes it. Everywhere else is covered once,
    // at document level in `wireContextMenuDocument` below -- a window-level
    // listener per pane would close a menu the moment its own controls were
    // clicked, because the menu is not inside any pane.
    el("chart").addEventListener("pointerdown", closeMenu);
  }

  /// Attach the listeners that are about *this* pane: its canvas, its tool
  /// buttons, its series selects, its own reload. The two that are not -- the
  /// keyboard and the resize -- stay on the window and are routed to the active
  /// pane, because a keyboard has no way to say which canvas it means.
  function wire() {
    wireChrome();
    el("load").addEventListener("click", () => {
      resetViewport();
      refresh().then(connectLive);
    });
    // A redraw, not a refetch: the zones are detected from the candles the engine
    // already has, so there is nothing new to ask the backend for.
    el("zones").addEventListener("click", (event) => {
      const button = event.currentTarget;
      const on = button.getAttribute("aria-pressed") !== "true";
      button.setAttribute("aria-pressed", on ? "true" : "false");
      render();
    });
    // Changing the chart type can change the *window* (a footprint uses the span
    // that has trades), so it refetches rather than just redrawing. The viewport
    // is deliberately kept: the same candles are still on screen.
    el("mode").addEventListener("change", () => { refresh(); paintTitle(); });
    // These change the series itself, so the window means nothing afterwards -- a
    // bar index into the old series is not a bar in the new one, and the limit
    // select changes how many exist at all.
    el("timeframe").addEventListener("change", () => {
      resetViewport();
      refresh().then(connectLive);
      paintTitle();
    });
    el("symbol").addEventListener("change", () => {
      // A different instrument has different timeframes, so the list is rebuilt
      // before the fetch that reads the chosen one.
      fillTimeframes(el("symbol").value);
      resetViewport();
      refresh().then(connectLive);
      paintTitle();
      if (hooks.onSymbolChange) hooks.onSymbolChange(paneApi);
    });
    // Fit is also where following resumes: "show the whole series again"
    // includes the bars that arrive next, and a button that refits while the
    // chart stays frozen behind the live edge would have to be pressed every
    // few seconds to do its one job.
    el("fit").addEventListener("click", () => {
      followLive = true;
      applyGesture({ kind: "fit" });
    });
    for (const button of root.querySelectorAll(".tools button[data-tool]")) {
      button.addEventListener("click", () => selectTool(button.dataset.tool));
    }
    // The registry may already be here -- a pane cloned after the engine loaded
    // gets the full set immediately. Before it loads this is a no-op, and the
    // page's post-load loop rebuilds every pane once the engine arrives.
    buildToolbarFromRegistry();
    // Two destructive controls rather than one ambiguous one: this deletes the
    // selection, the other deletes everything and asks first. The `title` on each
    // is the only place either behaviour is stated, so they have to be exact.
    el("deleteDrawing").addEventListener("click", deleteSelected);
    el("clearDrawings").addEventListener("click", clearDrawings);
    // `passive: false` because the handler calls `preventDefault`. Without it the
    // browser assumes the listener cannot cancel, and scrolls the page behind the
    // chart anyway -- which is the "I can't zoom" report, from the other end.
    el("chart").addEventListener("wheel", onWheel, { passive: false });
    el("chart").addEventListener("pointerdown", onPointerDown);
    el("chart").addEventListener("pointermove", onPointerMove);
    el("chart").addEventListener("pointerup", onPointerUp);
    // Not `onPointerUp`. A cancelled pointer is a gesture the user did not finish,
    // and committing a drawing from it would store a shape nobody drew.
    el("chart").addEventListener("pointercancel", onPointerCancel);
  }

  // ---------------------------------------------------------------------------
  // This pane's series options
  // ---------------------------------------------------------------------------

  /// Replace a select's options and pick one.
  ///
  /// `innerHTML` rather than `new Option`, because the labels are text that came
  /// from the server and this is the one place that has to escape them.
  function fillSelect(select, options, chosen) {
    select.innerHTML = options
      .map((o) => `<option value="${escapeHtml(o.value)}">${escapeHtml(o.label)}</option>`)
      .join("");
    select.value = chosen;
  }

  /// The timeframes this pane's symbol has, each labelled with its bar count.
  ///
  /// The count is in the label because it is the difference between a chart that
  /// is broken and a chart that is telling the truth: `15m` holds three bars on
  /// this deployment, and a selector that said only "15m" would read as a bug.
  ///
  /// Ordered by length, because the order is this shell's to decide and not
  /// the server's to grant -- see `frameMinutes`. The server sends a ladder
  /// today; it sent an alphabetical one before that, and neither is a reason
  /// for the page to stop knowing what "the next timeframe up" means.
  function fillTimeframes(symbol) {
    const entry = instruments.find((c) => c.symbol === symbol);
    const frames = [...(entry ? entry.timeframes : [])].sort(
      (a, b) => frameMinutes(a.timeframe) - frameMinutes(b.timeframe)
    );
    const values = frames.map((f) => f.timeframe);
    const keep = values.includes(el("timeframe").value) ? el("timeframe").value : values[0] || "";
    fillSelect(
      el("timeframe"),
      frames.map((f) => ({
        value: f.timeframe,
        label: `${f.timeframe} · ${f.candles >= 1000 ? `${Math.round(f.candles / 1000)}k` : f.candles}`,
      })),
      keep
    );
  }

  /// The series this symbol has the most of.
  ///
  /// What a chart opens on when nothing has an opinion. Derived rather than
  /// named, so it stays right when a different timeframe becomes the deepest.
  function deepestFrame(symbol) {
    const entry = instruments.find((c) => c.symbol === symbol);
    let best = null;
    for (const frame of entry ? entry.timeframes : []) {
      if (!best || frame.candles > best.candles) best = frame;
    }
    return best ? best.timeframe : "";
  }

  // ---------------------------------------------------------------------------
  // What the page may ask a pane to do
  //
  // Everything else in here is the pane's own business. The page cannot reach
  // `scene`, `viewport` or the drawing list, which is what stops it growing a
  // second opinion about any of them.
  // ---------------------------------------------------------------------------

  const paneApi = {
    root,
    canvas: el("chart"),

    /// This pane's series. Read from the selects rather than stored, so there is
    /// one answer: whatever the next request will use is what this returns.
    symbol: () => el("symbol").value,
    timeframe: () => el("timeframe").value,

    /// Attach a validated generated indicator to this chart immediately. The
    /// caller may only pass a preview returned by the workspace revision API;
    /// source itself never runs in the browser.
    attachIndicator(output) {
      indicator = output || null;
      renderNow();
    },

    /// What the user is looking at, as `POST /agent/ask` accepts it.
    ///
    /// Built *here* rather than at page scope because every field comes from
    /// state the page deliberately cannot reach -- `scene` and `drawings`. The
    /// page could scrape the selects for the symbol and timeframe, and it would
    /// then have a second opinion about which bars are on screen the moment the
    /// two drifted.
    ///
    /// ## What is deliberately not sent
    ///
    /// No candle data. The agent reads the window itself through the same
    /// `WindowService` the chart is drawn from, so sending prices here would
    /// create a second source for one fact -- and the two would differ exactly
    /// where it matters most, on the newest bar. This packet says *where to
    /// look*, never what is there.
    ///
    /// Every optional field is omitted when unknown rather than sent as `null`:
    /// the server's `ChartContext` defaults each one, and a `null` price axis
    /// would have to be told apart from an absent one for no gain.
    chartPacket() {
      if (!scene) return null;
      const packet = { timeframe: el("timeframe").value };

      // `from`/`to` are open times in unix nanoseconds, straight from the
      // engine -- the one place that decides which bars are visible. `to` is
      // one past the last bar's close, so the range is [first_open, one_past_last).
      //
      // Compared against `null` rather than tested for truthiness: a chart
      // scrolled fully to the left resolves `from` to timestamp `0`, and `0` is
      // a real bar that a truthiness check would silently drop -- sending a
      // window with an end and no start, which the server renders as a range it
      // cannot describe.
      if (scene.from !== null && scene.from !== undefined) packet.visible_from_ns = scene.from;
      if (scene.to !== null && scene.to !== undefined) packet.visible_to_ns = scene.to;
      if (Number.isFinite(scene.price_min) && Number.isFinite(scene.price_max)) {
        packet.price_low = scene.price_min;
        packet.price_high = scene.price_max;
      }

      // The shapes the user actually drew, not the one being placed: a
      // half-finished trendline is not analysis yet, and sending it would let
      // the agent cite a level the user was still deciding about.
      //
      // An anchor carries either a `price` or a `fraction`, and only the price
      // is meaningful to an analysis. A shape with no priced anchor at all is
      // still sent, because "the user drew a zone here" is context worth having
      // -- it is the price that is omitted, not the shape.
      const drawn = drawings
        .map((drawing) => ({
          kind: drawing.kind,
          price: anchorPrice(drawing),
          label: drawing.label || null,
        }))
        .filter(Boolean);
      if (drawn.length) packet.drawings = drawn;

      return packet;
    },

    /// Rebuild this pane's series options from the coverage the page read.
    ///
    /// The pane owns its selects, so the pane fills them; the page owns the list,
    /// because reading it once for four panes is one request instead of four.
    /// `preferred` keeps the current instrument when it is still there, so
    /// re-applying the list cannot move a chart off the symbol it is showing.
    fillSeries(entries, preferred, preferredTimeframe) {
      instruments = entries;
      const symbols = entries.map((entry) => entry.symbol);
      const symbol = symbols.includes(preferred) ? preferred : symbols[0] || "";
      fillSelect(el("symbol"), symbols.map((value) => ({ value, label: value })), symbol);
      fillTimeframes(symbol);
      if (preferredTimeframe) el("timeframe").value = preferredTimeframe;
      // Otherwise the richest series, not the first option. The list is ordered
      // by length, so the first option is `1m` -- and the alphabetically first
      // was `15m`, which holds three bars. A chart that opens looking broken
      // teaches the user the page is broken, and the rule that avoids it is
      // derived from the data rather than naming `5m` here.
      else el("timeframe").value = deepestFrame(symbol);
      paintTitle();
    },

    /// Put a freshly cloned pane back to its starting state.
    ///
    /// A clone carries whatever the pane it came from was showing -- the active
    /// outline, the note the user was reading, the footprint stats, the tool. A
    /// new chart that starts by claiming something about itself is worse than one
    /// that starts blank, so each of them is cleared rather than inherited.
    reset() {
      root.classList.remove("active");
      // A clone must not inherit the panel states of the pane it came from: a
      // copy that opens collapsed or blown up over the grid is a copy that
      // looks broken.
      root.classList.remove("min", "exp");
      el("charts").classList.remove("exp-ing");
      el("chartNote").textContent = "";
      el("footprintStats").hidden = true;
      el("chartMsg").textContent = "";
      selectTool("cursor");
    },

    /// The strip the user reads when the chart cannot be drawn at all: a failed
    /// engine load, an instrument list that could not be read.
    setMessage(text) { el("chartMsg").textContent = text; },

    /// Rebuild and repaint at most once per frame, if there is anything to
    /// rebuild. What the page's resize listener calls.
    redraw() { if (scene) scheduleRender(); },

    /// Handle a key that is about this pane, and say whether it was used.
    key(event) {
      if (event.key === "Escape") {
        // The menu first: Escape means "back out of what I just opened", and a
        // menu it opened is ahead of any tool state in that queue.
        closeMenu();
        if (placing) {
          // The shape, not the tool. Escape mid-drag means "not this one", and
          // having to pick the tool again afterwards would be a second punishment
          // for one mistake.
          placing = null;
          renderNow();
        } else {
          selectTool("cursor");
        }
        return true;
      }
      if ((event.key === "Delete" || event.key === "Backspace") && selectedDrawing) {
        event.preventDefault();
        deleteSelected();
        return true;
      }
      return false;
    },

    refresh,
    connectLive,
    draw,
    render,
    renderNow,
    scheduleRender,
    resetViewport,
    applyGesture,
    selectTool,
    deleteSelected,
    /// Undo/redo for the page's Ctrl+Z / Ctrl+Y routing. Takes the direction
    /// as a string because the router has no reason to hold two references.
    history(direction) {
      if (direction === "redo") redo();
      else undo();
    },
    toggleMagnet,
    /// Swap this pane's fallback buttons for the registry's set. Called by the
    /// page once the engine has loaded; a no-op before that.
    rebuildToolbar: buildToolbarFromRegistry,
    /// The right-click menu is shared, so the page and the other panes can both
    /// ask this pane to give the controls back (another pane opening the menu,
    /// a click elsewhere, this pane going away).
    closeMenu,
    /// Repaint the title line from the live controls. Cheap; called after any
    /// change that can move a select.
    paintTitle,

    /// Take this pane off the page. Its own listeners go with its elements; the
    /// four things that outlive them are the channel, a frame waiting for one,
    /// the clock the live badge counts with, and any reconnect the channel had
    /// scheduled.
    destroy() {
      // A pane that is gone must not be left hosting the shared menu: the next
      // `closeMenu` from any pane would file this one's controls into a pane
      // that no longer exists.
      closeMenu();
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = 0;
      }
      if (socket) socket.close();
      if (gestureFrame) {
        cancelAnimationFrame(gestureFrame);
        gestureFrame = 0;
      }
      // A pane that is gone must not keep a timer running: the badge would be
      // written into a detached element forever, and a closed pane's channel
      // would go on being described by a chart that no longer exists.
      if (liveTimer) {
        clearInterval(liveTimer);
        liveTimer = 0;
      }
      socket = null;
    },
  };

  wire();
  // The age has to tick on its own, and this is why it is a timer rather than a
  // side effect of `render`: the badge's whole job is to report how long it has
  // been since the last frame, and the case that matters is the one where nothing
  // is redrawing because nothing is arriving.
  liveTimer = setInterval(refreshLiveBadge, 1000);
  refreshLiveBadge();
  return paneApi;
}

// ---------------------------------------------------------------------------
// The panes
//
// A list of charts, and which one the aside is about. Everything here is about
// the *page*: how many charts there are, which is active, and what follows from
// that. What a chart does with itself is `createChartPane`'s business.
// ---------------------------------------------------------------------------

/// How many charts a page may hold.
///
/// Four, because past that no pane is readable -- and because an uncapped "add"
/// button is a way to make the page unusable by accident. The layout is a row,
/// so this is also what keeps each pane wider than its own toolbar.
const MAX_PANES = 4;

/// The panes, oldest first.
const panes = [];

/// Ids handed to panes in creation order. The right-click chart menu is one
/// element shared by every pane, so a pane stamps it with its id while hosting
/// and refuses to touch it otherwise -- see `openMenu`/`closeMenu`.
let nextPaneId = 1;

/// The instrument the aside is about, or "" before a pane exists.
///
/// The aside's panels describe one chart -- the agent is asked about one market,
/// a backtest runs on one instrument, the book is one symbol's depth -- and the
/// active pane is which one. Reading it through here rather than from a select
/// means there is no way to ask about a chart that is not on the page.
const activeSymbol = () => (activePane ? activePane.symbol() : "");
const activeTimeframe = () => (activePane ? activePane.timeframe() : "");

/// The instruments the platform can chart, as last read.
///
/// Page-level rather than per pane, because the answer is the same for every
/// pane and four panes asking four times is four round trips for one list. Each
/// pane is *given* it and rebuilds its own selects from it.
let coverage = [];

/// The pane the aside is about.
///
/// One at a time, because the aside's panels describe one instrument -- the book
/// is a book, the thesis is a thesis -- and there is one aside. A pane is marked
/// when it is touched, which is the only gesture a keyboard-less page has for
/// saying "this one".
let activePane = null;

/// Make `pane` the one the aside follows.
///
/// Called from a capture-phase listener on the pane, so it is set *before* the
/// pane's own handlers run: a press that selects a drawing and activates a chart
/// is one gesture, and anything reacting to it should already agree about which
/// chart the user meant.
function setActive(pane) {
  if (!pane || activePane === pane) return;
  // The book is per symbol and there is one of it, so it follows the chart the
  // user is looking at -- and only when the instrument actually changed, because
  // reconnecting a socket to the same symbol would drop the ladder for nothing.
  const moved = !activePane || activePane.symbol() !== pane.symbol();
  activePane = pane;
  for (const other of panes) other.root.classList.toggle("active", other === pane);
  if (moved) connectBook();
}

/// What a new pane should open on: the next timeframe up that can fill a chart.
///
/// "The next one in the list" is not enough. This deployment holds **three** `15m`
/// bars against 53,182 `5m` ones, so a second chart opening one step up from `5m`
/// would open on three candles -- and the first thing anyone does with a new
/// feature is judge it. The number of bars a chart is asking for is already on
/// screen in the bar-limit select, so that is the threshold rather than a figure
/// invented here: a timeframe that cannot fill the window this pane would ask for
/// is not a chart yet. If none can, the next one up is still the answer -- the
/// feature always does something, and the label says how thin it is.
function nextFrame(symbol, from, wanted) {
  const entry = coverage.find((c) => c.symbol === symbol);
  const ordered = [...(entry ? entry.timeframes : [])].sort(
    (a, b) => frameMinutes(a.timeframe) - frameMinutes(b.timeframe)
  );
  const at = ordered.findIndex((f) => f.timeframe === from);
  const up = at >= 0 ? ordered.slice(at + 1) : ordered;
  const fills = up.find((f) => f.candles >= wanted);
  return (fills || up[0] || {}).timeframe;
}

/// Add a pane, cloning the one the markup ships.
///
/// Cloned rather than built from a string: the markup is where a pane is
/// described, and a second description in JavaScript is a second thing to keep in
/// step with it. The clone is reset, because a copy of a chart is not the same
/// thing as a new chart -- see `reset`.
function addPane() {
  if (panes.length >= MAX_PANES || !activePane) return null;

  // Cloned from the *active* pane rather than the first one: "another chart of
  // what I am looking at" is the intent, and the chart the user is looking at is
  // the one they just touched.
  const node = activePane.root.cloneNode(true);

  // The pane lands in a **row wrapper** -- two panes per row, a new row (and
  // its splitter) opened only when the current one is full. The wrapper, not
  // the page, owns the 2-column layout, so "add chart" never produces a pane
  // too thin to read and the row splitter always has exactly two things to
  // split.
  let row = activePane.root.closest(".chartRow");
  if (!row || row.querySelectorAll(".chartPane").length >= 2) {
    const splitter = document.createElement("button");
    splitter.className = "splitter row-splitter";
    splitter.type = "button";
    splitter.setAttribute("aria-label", "Drag to resize the chart rows");
    el("charts").appendChild(splitter);
    row = document.createElement("div");
    row.className = "chartRow";
    el("charts").appendChild(row);
  }
  row.appendChild(node);
  // Two panes in a row is the grid's whole point, and the bootstrap row ships
  // with `.single` (one column) -- only this append can end that. Kept as a
  // class rather than left to `:has()` alone so the column count never depends
  // on one selector being supported.
  row.classList.toggle("single", row.querySelectorAll(".chartPane").length < 2);

  const pane = createChartPane(node, {
    onSymbolChange: (which) => { if (which === activePane) connectBook(); },
  });
  panes.push(pane);
  pane.reset();

  // The same instrument on the *next* timeframe up that can fill a chart. An
  // "Add chart" that produced an identical chart would look like nothing had
  // happened, and comparing two timeframes is what a second chart is for; either
  // dropdown changes it.
  const wanted = Number(node.querySelector(".limit").value) || 0;
  pane.fillSeries(
    coverage,
    activePane.symbol(),
    nextFrame(activePane.symbol(), activePane.timeframe(), wanted)
  );

  // Marked active before anything is fetched, so the aside is already about the
  // chart the user just asked for rather than the one it was cloned from.
  setActive(pane);
  refreshCloseButtons();
  return pane;
}

/// Take a pane off the page.
function closePane(pane) {
  // The last chart is not closable. An empty page has no way back, and the button
  // that would do it is hidden rather than refusing -- see `refreshCloseButtons`.
  if (panes.length < 2) return;

  const at = panes.indexOf(pane);
  if (at < 0) return;
  const row = pane.root.closest(".chartRow");
  pane.destroy();
  pane.root.remove();
  panes.splice(at, 1);
  // An emptied row takes its splitter with it, and hands its flex share to the
  // survivors: a stub row holding only a splitter would be dead space the user
  // has to drag around.
  if (row && row.querySelectorAll(".chartPane").length === 0) {
    // A splitter sits between two rows, so an emptied row strands the one on
    // either side of it -- the *previous* one when a later row died, the *next*
    // one when the first row did (there is nothing before it). Leaving either
    // behind is a leading orphan: a dead handle dragging against nothing.
    const prev = row.previousElementSibling;
    if (prev && prev.classList.contains("row-splitter")) prev.remove();
    else {
      const next = row.nextElementSibling;
      if (next && next.classList.contains("row-splitter")) next.remove();
    }
    row.remove();
  }
  refreshCloseButtons();
  // The survivor of a two-pane row drops back to a single column -- the same
  // class that made the bootstrap row one pane wide, removed by `addPane`.
  // Checked on `isConnected`: an emptied row was just removed, and toggling a
  // detached node is noise.
  if (row && row.isConnected) {
    row.classList.toggle("single", row.querySelectorAll(".chartPane").length < 2);
  }

  if (activePane === pane) {
    // The one to its left, or the first if it was leftmost. Whichever it is, the
    // aside has to be about something, and something on the page.
    activePane = null;
    setActive(panes[Math.max(0, at - 1)]);
  }
}

/// Show the close buttons only when closing is possible, and the add button only
/// when adding is.
///
/// A control that is drawn and then refuses is worse than one that is not there:
/// the user learns the page is broken rather than that the limit is reached.
function refreshCloseButtons() {
  for (const pane of panes) {
    pane.root.querySelector(".close").hidden = panes.length < 2;
  }
  el("split").disabled = panes.length >= MAX_PANES;
  // The status bar counts the panes on screen, so "how much am I looking at"
  // is answered without counting title bars.
  const count = document.getElementById("sbCharts");
  if (count) count.textContent = `${panes.length} chart${panes.length === 1 ? "" : "s"}`;
}

/// Build the first pane from the markup, and make it the active one.
function openFirstPane() {
  // The markup ships one pane; wrap it in the row structure the rest of the
  // panes are added to, so every pane's parent is a `.chartRow` from here on.
  const node = el("charts").querySelector(".chartPane");
  const row = document.createElement("div");
  row.className = "chartRow single";
  node.replaceWith(row);
  row.appendChild(node);
  const pane = createChartPane(node, {
    onSymbolChange: (which) => { if (which === activePane) connectBook(); },
  });
  panes.push(pane);
  setActive(pane);
  return pane;
}

/*
  Drag-resizable panels. Three drags, each one writing a single layout number:

  - `#splitWatch`  -> the watchlist rail's width (`--watch-w`)
  - `#splitAside`  -> the aside's width (`--aside-w`)
  - `.row-splitter` -> the flex share of the chart row above vs. the row below

  The widths are custom properties on `:root`, so the CSS owns every rule that
  consumes them (including a future media query) and this code only owns the
  number. Both width splitters sit to the LEFT of the panel they resize, which
  fixes the drag direction: moving the handle toward a panel gives it space.
  Rows split by flex-grow, not pixels, so the split survives a window resize
  proportionally instead of clipping the bottom row. Every change funnels
  through `pane.redraw()`, which re-measures the canvas inside the same
  once-per-frame coalescing a window resize uses.
*/
function wireSplitters() {
  const clamp = (value, lo, hi) => Math.min(hi, Math.max(lo, value));

  // The saved widths restore the layout the user dragged to last time. A
  // missing or corrupt entry just means the CSS defaults stand.
  try {
    const saved = JSON.parse(localStorage.getItem("splitterWidths") || "{}");
    if (Number.isFinite(saved.watch)) document.documentElement.style.setProperty("--watch-w", `${saved.watch}px`);
    if (Number.isFinite(saved.aside)) document.documentElement.style.setProperty("--aside-w", `${saved.aside}px`);
  } catch { /* a broken blob is not worth a broken page */ }

  const persist = () => {
    try {
      const styles = getComputedStyle(document.documentElement);
      localStorage.setItem("splitterWidths", JSON.stringify({
        watch: parseFloat(styles.getPropertyValue("--watch-w")) || undefined,
        aside: parseFloat(styles.getPropertyValue("--aside-w")) || undefined,
      }));
    } catch { /* private mode, quota, whatever -- the drag still worked */ }
  };

  const repaint = () => { for (const pane of panes) pane.redraw(); };

  // One pointer session per handle: capture the pointer so a drag that strays
  // off the 6px strip keeps coming here, and end it on pointerup or pointercancel
  // so a released button never leaves the page half-grabbed.
  function wireWidth(handle, property, min, max, sign, baseWidth) {
    handle.addEventListener("pointerdown", (event) => {
      event.preventDefault();
      const startWidth = clamp(baseWidth(), min, max);
      const startX = event.clientX;
      handle.setPointerCapture(event.pointerId);
      handle.classList.add("dragging");
      const move = (moveEvent) => {
        const width = clamp(startWidth + sign * (moveEvent.clientX - startX), min, max);
        document.documentElement.style.setProperty(property, `${Math.round(width)}px`);
        repaint();
      };
      const stop = () => {
        handle.classList.remove("dragging");
        handle.removeEventListener("pointermove", move);
        handle.removeEventListener("pointerup", stop);
        handle.removeEventListener("pointercancel", stop);
        persist();
        repaint();
      };
      handle.addEventListener("pointermove", move);
      handle.addEventListener("pointerup", stop);
      handle.addEventListener("pointercancel", stop);
    });
  }

  // The rail grows rightward; the aside grows leftward (its handle is on its
  // left edge), hence the opposite signs. The baseline is the panel element's
  // real width, not the CSS variable: the variable is unset until a first drag
  // writes it, and reading it there would start every first drag from the
  // minimum instead of from where the layout actually is.
  const currentWidth = (panel, property, fallback) => {
    const fromVar = parseFloat(getComputedStyle(document.documentElement).getPropertyValue(property));
    return Number.isFinite(fromVar) && fromVar > 0
      ? fromVar
      : panel.getBoundingClientRect().width || fallback;
  };
  wireWidth(el("splitWatch"), "--watch-w", 180, 460, +1, () => currentWidth(el("sideWatch"), "--watch-w", 250));
  wireWidth(el("splitAside"), "--aside-w", 280, 640, -1, () => currentWidth(document.querySelector("main > aside"), "--aside-w", 380));

  // Row splitters are created with their row (`addPane`), so they are delegated:
  // one listener on the container covers every row that ever exists.
  el("charts").addEventListener("pointerdown", (event) => {
    const splitter = event.target.closest(".row-splitter");
    if (!splitter) return;
    event.preventDefault();
    const above = splitter.previousElementSibling;
    const below = splitter.nextElementSibling;
    if (!above || !below || !above.classList.contains("chartRow") || !below.classList.contains("chartRow")) return;

    // px-per-grow is the exchange rate between pixels dragged and flex-grow:
    // the free height each `1 1 0` row currently divides. Computed once, at
    // grab time, from the live layout.
    const rows = [...el("charts").querySelectorAll(".chartRow")];
    const grows = rows.map((row) => {
      const grow = parseFloat(row.style.flexGrow);
      return Number.isFinite(grow) ? grow : 1;
    });
    const totalGrow = grows.reduce((sum, grow) => sum + grow, 0);
    const splitterPx = [...el("charts").children]
      .filter((child) => child.classList.contains("row-splitter"))
      .reduce((sum, child) => sum + child.offsetHeight, 0);
    const pxPerGrow = (el("charts").clientHeight - splitterPx) / totalGrow;
    const startAbove = parseFloat(above.style.flexGrow);
    const startBelow = parseFloat(below.style.flexGrow);
    const startY = event.clientY;

    splitter.setPointerCapture(event.pointerId);
    splitter.classList.add("dragging");
    const move = (moveEvent) => {
      const delta = (moveEvent.clientY - startY) / pxPerGrow;
      // 0.2 keeps a sliver of each row on screen; a fully collapsed row is a
      // splitter pair with nothing between them, which no drag recovers from.
      const aboveGrow = clamp((Number.isFinite(startAbove) ? startAbove : 1) + delta, 0.2, 8);
      const belowGrow = clamp((Number.isFinite(startBelow) ? startBelow : 1) - delta, 0.2, 8);
      above.style.flexGrow = aboveGrow.toFixed(3);
      below.style.flexGrow = belowGrow.toFixed(3);
      repaint();
    };
    const stop = () => {
      splitter.classList.remove("dragging");
      splitter.removeEventListener("pointermove", move);
      splitter.removeEventListener("pointerup", stop);
      splitter.removeEventListener("pointercancel", stop);
      repaint();
    };
    splitter.addEventListener("pointermove", move);
    splitter.addEventListener("pointerup", stop);
    splitter.addEventListener("pointercancel", stop);
  });
}

/// The instruments the platform can chart, read once for every pane.
///
/// `GET /symbols` reports each symbol that has candles and, per timeframe, how
/// many there are and how many are missing. The page used to ship a hardcoded
/// `<option>BTCUSDT</option>` and a timeframe list that omitted `15m`, which the
/// database has -- so it could not offer an instrument it was able to draw, and
/// could not say why a timeframe was thin.
async function loadCoverage() {
  try {
    const entries = await api("/symbols");
    coverage = Array.isArray(entries) ? entries : [];
  } catch (e) {
    // Not fatal. A pane with no instruments says so, which is a better answer
    // than an empty chart or a page that never finishes loading.
    coverage = [];
    for (const pane of panes) pane.setMessage(`the instrument list could not be read: ${e.message}`);
  }
  return coverage;
}

/// Follow the order-book channel.
///
/// This panel derives nothing. The ladder arrives with each level's cumulative
/// size and a bar width already worked out, both in Rust, because summing
/// depth here would be a second implementation of a number -- and then two
/// parts of this shell could disagree about the same book. Formatting is all
/// that is left to do, and that is all this does.
function connectBook() {
  if (bookSocket) {
    // Marked before closing, because `close()` fires `onclose` synchronously in
    // this shell's own harness and asynchronously in a browser -- and the mark
    // has to be set on the *old* socket either way, which is why it is not
    // cleared from the new one below.
    bookSocket.bookSuperseded = true;
    bookSocket.close();
  }
  clearTimeout(bookRetry);
  // The book follows the *active* pane, because there is one book and one aside.
  // A pane that is not active has no claim on it, however recently it changed.
  const symbol = activePane ? activePane.symbol() : "";
  if (!symbol) return;
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${scheme}://${location.host}/ws/orderbook/${symbol}`);
  bookSocket = ws;
  // A socket we closed on purpose must not report its own closure.
  ws.bookSuperseded = false;
  // Whether the server explained itself before closing. Set by a `notice`
  // frame, read by `onclose`, which is the difference between "the book is
  // still syncing" (a message worth keeping) and a bare disconnect.
  ws.bookNotice = null;

  ws.onmessage = (event) => {
    let frame;
    try {
      frame = JSON.parse(
        typeof event.data === "string" ? event.data : new TextDecoder().decode(event.data)
      );
    } catch { return; }

    if (frame.type === "data") renderBook(frame.payload);
    // The channel says why when there is no book, and a DOM that showed an
    // empty ladder instead would look like a market with no liquidity.
    else if (frame.type === "notice") {
      ws.bookNotice = frame.message;
      el("bookMsg").textContent = frame.message;
    } else if (frame.type === "lagged") {
      el("bookMsg").textContent = `the book dropped ${frame.dropped} update(s)`;
    }
  };
  ws.onclose = () => {
    // Only the socket we are still meant to be using may speak for the panel.
    if (bookSocket !== ws) return;
    bookSocket = null;

    // Two closes look identical from here and mean opposite things.
    //
    // A close we asked for is not news: switching panes aborts a socket whose
    // handshake may not have finished, which the browser reports as "closed
    // before the connection is established". Saying anything would tell the
    // user their book broke every time they changed chart.
    if (ws.bookSuperseded) return;

    // The server explains itself when it can. `/ws/orderbook` closes after
    // `DEPTH_GRACE` with a notice naming MARKET_FEED and the sync state, and
    // overwriting that with "disconnected" would throw away the only useful
    // sentence in the exchange.
    if (ws.bookNotice) {
      el("bookMsg").textContent = ws.bookNotice;
    } else {
      el("bookMsg").textContent = "The book disconnected.";
    }

    // A book that was a few seconds late should appear when it arrives, not
    // need a page reload. This is the case the live log showed: the feed is
    // healthy and the book does sync, so a close here is almost always "not
    // yet" rather than "never" -- and only the retry can tell them apart.
    scheduleBookRetry(symbol);
  };
}

/// How long to wait before re-asking for a book that closed.
///
/// The server's own grace is 5s, and the observed sync times are single-digit
/// diffs, so one second is comfortably inside a healthy startup and far below
/// anything a user would call a hang.
const BOOK_RETRY_MS = 1000;

/// Re-open the book channel for `symbol`, if it is still the right one.
///
/// The retry re-checks that the pane has not moved: a user who switched to
/// another instrument while this one was backing off must not have the old
/// symbol's book yanked back onto the panel.
function scheduleBookRetry(symbol) {
  clearTimeout(bookRetry);
  bookRetry = setTimeout(() => {
    if (bookSocket) return;
    const wanted = activePane ? activePane.symbol() : "";
    if (wanted !== symbol) return;
    connectBook();
  }, BOOK_RETRY_MS);
}

function bookRows(levels, side) {
  return levels
    .map(
      (row) => `<div class="ladder-row ${side}">
        <span class="ladder-bar" style="width:${row.bar_pct}%"></span>
        <span>${row.price.toFixed(2)}</span>
        <span>${row.quantity.toFixed(4)}</span>
        <span>${row.cumulative.toFixed(4)}</span>
      </div>`
    )
    .join("");
}

function renderBook(ladder) {
  if (!ladder.bids.length && !ladder.asks.length) {
    el("bookLadder").innerHTML = "";
    el("bookMsg").textContent = "The book is empty.";
    return;
  }

  el("bookMsg").textContent =
    ladder.spread === null || ladder.spread === undefined
      ? "no spread: one side of the book is empty"
      : `spread ${ladder.spread.toFixed(2)}`;

  // Asks run worst-to-best downwards so the best ask sits against the spread,
  // the way the two sides meet in the middle of a ladder.
  const asks = [...ladder.asks].reverse();
  el("bookLadder").innerHTML = `<div class="ladder">
    <div class="ladder-head">Ask · size · cumulative</div>
    ${bookRows(asks, "ask")}
    <div class="ladder-head">Bid · size · cumulative</div>
    ${bookRows(ladder.bids, "bid")}
  </div>`;
}

// ---------------------------------------------------------------------------
// Thesis
// ---------------------------------------------------------------------------

const STATUS_CLASS = { pass: "pass", fail: "fail", unknown: "unknown" };

/// A step the agent reported, as a line of English.
///
/// The stages come from `ai_agent::Progress`. A stage this file does not know
/// falls back to its own name rather than vanishing -- a new step should show
/// up as something odd, not as nothing.
const STAGE_TEXT = {
  reading_market: (p) => `reading ${p.timeframes} timeframe(s) for ${p.symbol}`,
  thinking: (p) =>
    p.answering
      ? `writing the thesis (turn ${p.turn} of ${p.total})`
      : `analysing (turn ${p.turn} of ${p.total})`,
  tool: (p) => `calling ${p.name}`,
  tool_done: (p) => `${p.name} ${p.ok ? "returned" : "failed"}`,
  correcting: (p) => `rejected, asking for a correction: ${p.reason}`,
};

function progressText(step) {
  const render = STAGE_TEXT[step.stage];
  return render ? render(step) : step.stage;
}

function escapeHtml(value) {
  return String(value == null ? "" : value).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
  );
}

/// The thesis card.
///
/// A string rather than a write to the DOM, because the transcript holds one
/// per turn and needs to compose them.
function thesisHtml(t) {
  const checks = [...(t.higher_timeframe_checks || []), ...(t.order_flow_checks || [])]
    .map(
      (check) => `<li>
        <span class="status ${STATUS_CLASS[check.status] || "unknown"}">${check.status}</span>
        <span><strong>${escapeHtml(check.label)}</strong><br />
        <span class="muted">${escapeHtml(check.detail)}</span></span>
      </li>`
    )
    .join("");

  return `
    <dl class="kv">
      <dt>direction</dt><dd>${t.direction}</dd>
      <dt>confidence</dt><dd>${t.confidence_pct.toFixed(0)}%</dd>
      <dt>entry</dt><dd>${t.entry_price.toFixed(2)}</dd>
      <dt>stop</dt><dd>${t.stop_price.toFixed(2)}</dd>
      <dt>target</dt><dd>${t.target_price.toFixed(2)}</dd>
      <dt>R:R</dt><dd>${t.risk_reward.toFixed(2)}</dd>
      <dt>skill</dt><dd>${escapeHtml(t.skill_used || "—")}</dd>
    </dl>
    <h2>Checks</h2>
    <ul class="checks">${checks}</ul>
    <h2>Invalidation</h2>
    <p class="muted">${escapeHtml(t.invalidation || "—")}</p>
    <h2>Narrative</h2>
    <pre>${escapeHtml(t.narrative || "")}</pre>`;
}

/// The conversation, drawn from `turns`.
///
/// The panel used to replace its whole contents on every question, so the
/// answer you were reading disappeared the moment you asked the next one. A
/// transcript keeps them, which is what a chat panel is for -- and it is the
/// only place the agent's steps are ever recorded, since nothing stores them.
function renderTranscript() {
  if (!turns.length) {
    el("thesis").innerHTML = `<p class="empty">Ask a question. The thesis's own levels are drawn on the chart.</p>`;
    return;
  }

  const running = turns.length - 1;
  el("thesis").innerHTML = turns
    .map((turn, index) => {
      const steps = turn.steps.map((step) => `<li>${escapeHtml(progressText(step))}</li>`).join("");
      // The run in flight shows its steps as they arrive; a finished one folds
      // them away, because the answer is what a reader came back for.
      const stepsHtml = !steps
        ? ""
        : index === running && !turn.answer && !turn.error
          ? `<ul class="steps">${steps}</ul>`
          : `<details class="steps-wrap"><summary>${turn.steps.length} step(s)</summary>
               <ul class="steps">${steps}</ul></details>`;

      const body = turn.answer
        ? thesisHtml(turn.answer.thesis)
        : turn.error
          ? `<p class="fail">${escapeHtml(turn.error)}</p>`
          : `<p class="empty">Working…</p>`;

      return `<div class="turn">
        <p class="q">${escapeHtml(turn.question)}</p>
        ${stepsHtml}
        ${body}
      </div>`;
    })
    .join("");
}

/// Why the agent channel refused to open.
///
/// A refused WebSocket handshake reaches script as an opaque `error` event:
/// per spec the response status is **not** exposed, so `onerror` cannot tell an
/// expired token from an unconfigured agent from a host that does not exist —
/// all three arrive identically, and "could not reach the agent channel" was
/// the most this shell could honestly say until it went and looked.
///
/// ## What it asks, and why those two things
///
/// The refusal does have a reason, and both halves are already served with
/// meaning elsewhere:
///
/// * `/capabilities` reports the `agent` capability. When Bedrock is not
///   configured it is `not_configured` with a `warning` naming the consequence,
///   so this can say "the AI analyst is not configured on this deployment"
///   rather than sending the user to check a token that is fine.
/// * `/auth/me` (through `api`) distinguishes a token the server accepts from
///   one it does not, because that is the *only* remaining reason a
///   handshake would be refused with the agent configured.
///
/// The two are genuinely different problems with opposite fixes — sign in
/// again, versus tell the operator to set `AWS_BEDROCK_*` — and reporting one
/// as the other is worse than reporting neither. A user with a seven-day-old
/// token who reads "the agent is not configured" goes looking for a server
/// variable they cannot see while the real fix is one click.
///
/// This runs **after** the socket has already failed, so it costs nothing on
/// the happy path, and it never turns a working deployment into a broken one:
/// if neither probe produces an answer, the caller keeps its generic message.
async function agentChannelReason() {
  // Is the agent there at all? This is the deployment-level answer and it does
  // not need the token to be valid, which is why it is asked first.
  try {
    const report = await api("/capabilities");
    const agent = (report.capabilities || []).find((c) => c.name === "agent");
    if (agent && agent.readiness === "not_configured") {
      return "the AI analyst is not configured on this deployment";
    }
    if (agent && agent.readiness === "degraded") {
      return "the AI analyst is configured but not usable";
    }
  } catch {
    // The capabilities route itself is down; fall through to the token check,
    // which will fail too and produce the honest generic message.
  }

  // Agent is configured, so the handshake was refused over the credential.
  try {
    await api("/auth/me");
  } catch (e) {
    // `api` already turns a non-2xx into a message carrying the server's own
    // words, and the 401 body here says the session token is missing,
    // malformed or expired.
    return `the agent channel refused the connection: ${e.message}`;
  }
  return null;
}

/// The agent socket, opened on first use.
///
/// Lazily rather than at load: an anonymous visitor cannot open it (the
/// channel is authenticated), and a failed socket at page load would be a red
/// message about a feature they have not tried yet.
function ensureAgentSocket() {
  if (agentReady) return agentReady;
  if (!token()) return Promise.reject(new Error("Sign in to ask the agent."));
  // The channel only echoes the session id back, so any unique string will do.
  // It exists so a server log can tell two tabs apart.
  if (!agentSession) agentSession = Math.random().toString(36).slice(2, 10) + Date.now().toString(36);

  const scheme = location.protocol === "https:" ? "wss" : "ws";
  agentReady = new Promise((resolve, reject) => {
    const ws = new WebSocket(
      `${scheme}://${location.host}/ws/agent/${agentSession}?token=${encodeURIComponent(token())}`
    );
    agentSocket = ws;
    ws.onopen = () => resolve(ws);
    ws.onerror = () => {
      // Ask the server why rather than reporting the browser's silence. The
      // socket is already dead either way; this decides what to *say*, and a
      // wrong reason sends the user at the wrong fix.
      agentChannelReason().then((reason) => {
        reject(new Error(reason || "could not reach the agent channel"));
      });
    };
    ws.onmessage = onAgentFrame;
    ws.onclose = () => {
      // Only the socket we are still meant to be using may speak for the panel.
      if (agentSocket !== ws) return;
      agentSocket = null;
      agentReady = null;
      // A socket that dies mid-run leaves the question unanswered, and the Ask
      // button must not stay disabled waiting for a reply that cannot arrive.
      if (!asking) return;
      const turn = turns[turns.length - 1];
      if (turn && !turn.answer && !turn.error) {
        turn.error = "the agent channel closed before it answered";
      }
      asking = false;
      paintAsk();
      renderTranscript();
    };
  });
  return agentReady;
}

function onAgentFrame(event) {
  let frame;
  try {
    frame = JSON.parse(
      typeof event.data === "string" ? event.data : new TextDecoder().decode(event.data)
    );
  } catch {
    return;
  }

  const turn = turns[turns.length - 1];
  if (!turn) return;

  // Everything below is wrapped, because a throw in here is otherwise visible
  // only in the browser console: the panel sits on "Working…" or shows an
  // answer with no chart, and nothing on the page says why. That is how
  // `draw()` being out of scope shipped -- the one place it could be seen was
  // a console the user happened to have open.
  //
  // Reported rather than swallowed. A swallowed failure is worse than a crash:
  // the question looks like it is still running.
  try {
    applyAgentFrame(frame, turn);
  } catch (e) {
    turn.answer = null;
    turn.error = `the answer arrived but could not be shown: ${e && e.message ? e.message : e}`;
    thesis = null;
    asking = false;
    paintAsk();
    renderTranscript();
  }
}

function applyAgentFrame(frame, turn) {
  if (frame.type === "progress") {
    turn.steps.push(frame.payload);
    renderTranscript();
  } else if (frame.type === "data") {
    turn.answer = frame.payload;
    // The levels go on the chart, which is the point of asking.
    thesis = frame.payload.thesis;
    asking = false;
    paintAsk();
    renderTranscript();
    redrawThesis();
  } else if (frame.type === "notice") {
    // A notice is a refusal or a failure -- a rate limit, a bad request, a
    // model error -- and it ends this question.
    turn.error = frame.message;
    thesis = null;
    asking = false;
    paintAsk();
    renderTranscript();
    redrawThesis();
  }
}

/// Put the current thesis on every chart, and take the old one off.
///
/// `redraw()`, not `draw()`. The answer's levels are positioned by the *engine*
/// -- the shell has no price scale and must not grow one -- so a new thesis means
/// a new request. `draw()` repaints the scene already in hand, which was right
/// when the shell mapped the three prices itself and is now a thesis that is
/// stored and never drawn.
///
/// Not `activePane` alone, either: each chart draws the thesis when the thesis is
/// about *its* symbol, so a second chart on the same instrument has to be
/// repainted too. And not `draw()`, which is one pane's own function -- calling
/// it from page scope was a `ReferenceError` once, and the answer arrived with
/// nothing drawn.
function redrawThesis() {
  for (const pane of panes) pane.redraw();
}

/// The thesis's own prices, as the overlays a chart request carries.
///
/// Three levels and a band, built from the thesis's numeric fields. The engine
/// resolves the coordinates -- this function does no arithmetic on a price, and
/// that is the whole design: the stop-to-target band used to be mapped here with
/// the shell's own copy of the price scale, which drifted from the engine's on
/// every resize and zoom.
///
/// A `direction` of `none` carries zeroed levels (the thesis is exempt from the
/// level checks precisely so an honest "no trade" is not forced to invent one),
/// and a level at zero is not a price -- so a stand-aside thesis contributes
/// nothing rather than three lines along the bottom of the chart.
function thesisOverlays(thesis) {
  if (!thesis || thesis.direction === "none") return [];
  const levels = [
    { price: thesis.stop_price, label: "stop", role: "stop" },
    { price: thesis.entry_price, label: "entry", role: "entry" },
    { price: thesis.target_price, label: "target", role: "target" },
  ].filter((level) => Number.isFinite(level.price) && level.price > 0);

  // The band is one overlay spanning stop to target, not two more overlays:
  // two lines that disagreed would shade the wrong region, and the engine can
  // order the edges itself because it knows which way the trade points.
  if (levels.length === 3) {
    const entry = levels.find((level) => level.role === "entry");
    const stop = levels.find((level) => level.role === "stop");
    const target = levels.find((level) => level.role === "target");
    entry.band_to = target.price;
    entry.filled = true;
    // The stop gets no band: the shaded region is the *reward* the thesis is
    // claiming, and shading the risk as well would make the two look alike.
    stop.band_to = null;
  }
  return levels;
}

function paintAsk() {
  const button = el("ask");
  button.disabled = asking;
  button.textContent = asking ? "Working…" : "Ask";
}

async function ask() {
  const question = el("question").value.trim();
  if (!question || asking) return;

  const turn = { question, steps: [], answer: null, error: null };
  turns.push(turn);
  asking = true;
  el("question").value = "";
  paintAsk();
  renderTranscript();

  try {
    const ws = await ensureAgentSocket();
    const message = {
      symbol: activeSymbol(),
      question,
      timeframes: [activeTimeframe()],
    };

    // The viewport goes on every question while it is switched on. The images
    // are captured at send time rather than at attach time: the charts move
    // between the two, and a picture of where the user *was* would have the
    // agent reasoning about a view nobody is looking at any more.
    if (attachChart && activePane) {
      const packet = activePane.chartPacket();
      if (packet) {
        // The primary image is the chart the user is looking at. The rest of
        // the ladder -- 1d, 4h, 1h, 5m of the same symbol -- is then captured
        // automatically: an open pane of the right timeframe is photographed,
        // and any rung nothing open shows is rendered offscreen from that
        // timeframe's candle history, so the agent sees the setup across
        // timeframes whether or not the user opened four charts.
        const { primary: primaryShot, shots } = await captureLadder(activePane.symbol());
        if (primaryShot) packet.screenshot = primaryShot;
        // Timeframes the symbol has no series for are simply not attached:
        // capturing a chart that does not exist is not an option, the agent
        // still reads every timeframe through its ladder, and the prompt names
        // what it got.
        if (shots.length) packet.screenshots = shots;
        if (!primaryShot && !shots.length) {
          // No picture made it at all. The viewport still goes, so the agent
          // still knows where the user is -- but the user is told, because
          // "it can see your chart" is exactly the sort of belief that goes
          // wrong quietly.
          activePane.setMessage(
            "the chart picture could not be captured, so this question went with the " +
              "viewport only -- the agent still knows which symbol, resolution and window " +
              "you are looking at."
          );
        }
        message.chart = packet;
      }
    }

    ws.send(JSON.stringify(message));
  } catch (e) {
    turn.error = e.message;
    asking = false;
    paintAsk();
    renderTranscript();
  }
}

/// Whether to attach the chart's viewport and a screenshot to each question.
let attachChart = false;

/// Most a screenshot may be, in bytes, before the shell downscales.
///
/// Matches the server's `MAX_SCREENSHOT_BYTES`. Checking here as well is not
/// redundant: the server refuses an oversize payload with a 422 and the turn is
/// lost, whereas the shell can downscale and send something usable. A refusal the
/// client could have avoided is a bug in the client.
const MAX_SCREENSHOT_BYTES = 4 * 1024 * 1024;

/// Longest edge of a downscaled capture, in pixels.
///
/// A chart is legible to a vision model well below its native resolution, and a
/// 4K pane at full size is several megabytes for detail nothing reads. This is
/// the same "build a superset in Rust, let the shell only set width" split
/// `docs/14` uses for presentation, applied to what leaves the page.
const MAX_SCREENSHOT_EDGE = 1600;

/// The most images one question attaches.
///
/// Mirrors the server's `MAX_SCREENSHOTS`: a primary plus a 1d/4h/1h/5m ladder
/// fills it exactly, and sending more would only have them dropped.
const MAX_ATTACHED_IMAGES = 3;

/// Capture the chart canvas as a PNG the agent can see.
///
/// ## Why this is `toDataURL` and not `getImageData`
///
/// `getImageData` would mean reading every pixel and re-encoding by hand or
/// through an offscreen canvas, for no benefit: the browser's own PNG encoder is
/// faster and produces a smaller file. `toDataURL` also handles the
/// device-pixel-ratio backing store for us, so what is captured is what is drawn
/// rather than a crop of its top-left corner.
///
/// Returns `null` when a capture is not possible, which the caller reports rather
/// than sending a question the user believes has a picture attached.
function captureChart(canvas, timeframeLabel = "") {
  if (!canvas || !canvas.toDataURL) return null;
  try {
    // A downscale pass. `drawImage` on an offscreen canvas is the only way to
    // resize without re-encoding twice, and it keeps the aspect ratio so the
    // chart the model sees is the shape of the chart the user sees.
    const scale = Math.min(1, MAX_SCREENSHOT_EDGE / Math.max(canvas.width, canvas.height));
    if (scale >= 1) {
      // Background first, even at native size. This used to be the fast path
      // that skipped it: the chart is drawn on a transparent canvas, so the
      // PNG carried transparent pixels where the model expects a plot, and a
      // vision model reads that as "there is no chart here" -- which is the
      // exact report of the screenshot feature not working. One fillRect is
      // the whole fix; the encode dominates either way.
      const opaque = document.createElement("canvas");
      opaque.width = canvas.width;
      opaque.height = canvas.height;
      const octx = opaque.getContext("2d");
      octx.fillStyle = "#131722";
      octx.fillRect(0, 0, opaque.width, opaque.height);
      octx.drawImage(canvas, 0, 0);
      return screenshotFromUrl(opaque.toDataURL("image/png"), timeframeLabel);
    }

    const scaled = document.createElement("canvas");
    scaled.width = Math.max(1, Math.round(canvas.width * scale));
    scaled.height = Math.max(1, Math.round(canvas.height * scale));
    const ctx = scaled.getContext("2d");
    // A chart is drawn on a transparent background, so without this the PNG has
    // transparent pixels where the model expects a plot. A screenshot that looks
    // like a dark void reads to a vision model as "there is no chart here".
    ctx.fillStyle = "#131722";
    ctx.fillRect(0, 0, scaled.width, scaled.height);
    ctx.drawImage(canvas, 0, 0, scaled.width, scaled.height);
    return screenshotFromUrl(scaled.toDataURL("image/png"), timeframeLabel);
  } catch (e) {
    // A tainted canvas, an out-of-memory resize, a browser that refuses. All of
    // them mean no picture, and none of them should stop the question.
    return null;
  }
}

/*
  The auto screenshot ladder.

  One question about one market carries four pictures of it -- 1d, 4h, 1h, 5m,
  in that order, because that is the order an analysis reads a market in. The
  active pane's own image goes first as the primary; every other rung is filled
  from an open pane of the right timeframe, or rendered offscreen from that
  timeframe's candle history when no pane holds it -- so the ladder does not
  depend on which charts the user happened to open.
*/
const LADDER_TIMEFRAMES = ["1d", "4h", "1h", "5m"];

async function captureLadder(symbol) {
  const activeTf = activePane ? activePane.timeframe() : "";
  const primary = activePane ? captureChart(activePane.canvas, activeTf) : null;
  const shots = [];
  const taken = new Set(activeTf ? [activeTf] : []);

  for (const tf of LADDER_TIMEFRAMES) {
    if (taken.has(tf)) continue;
    // An open pane of this symbol at this timeframe is already showing exactly
    // this picture -- reuse it rather than re-render a copy.
    const openPane = panes.find(
      (pane) => pane !== activePane && pane.symbol() === symbol && pane.timeframe() === tf
    );
    if (openPane) {
      const shot = captureChart(openPane.canvas, tf);
      if (shot) { shots.push(shot); taken.add(tf); }
      continue;
    }
    // Nothing open shows it. Only render one if the symbol actually has the
    // series: a blank picture of a timeframe that does not exist would be a lie
    // dressed as analysis.
    const entry = coverage.find((instrument) => instrument.symbol === symbol);
    const frame = entry && entry.timeframes.find((f) => f.timeframe === tf);
    if (!frame || frame.candles === 0) continue;
    try {
      const shot = await renderOffscreen(symbol, tf);
      if (shot) { shots.push(shot); taken.add(tf); }
    } catch { /* one missing rung is not a failed ask */ }
  }
  // The wire budget: the primary rides `screenshot`, these ride `screenshots`,
  // and both sides cap the total at MAX_SCREENSHOTS.
  return { primary, shots: shots.slice(0, MAX_ATTACHED_IMAGES) };
}

/// Render one chart of a symbol+timeframe no open pane is showing, and capture
/// it.
///
/// The scratch pane is a real pane: same markup, same engine, same drawing code
/// -- only offscreen and without a live channel, because a capture is a moment,
/// not a stream. It is removed as soon as the capture is taken.
async function renderOffscreen(symbol, timeframe) {
  const template = el("charts").querySelector(".chartPane");
  if (!template) return null;
  const holder = document.createElement("div");
  // Offscreen but laid out: `display:none` would give the canvas a zero box and
  // nothing to draw into. 1024x700 is a shape a vision model reads comfortably.
  holder.style.cssText = "position:fixed;left:-99999px;top:0;width:1024px;height:700px;";
  holder.innerHTML = template.outerHTML;
  document.body.appendChild(holder);
  try {
    const node = holder.querySelector(".chartPane");
    const pane = createChartPane(node, {});
    // Fill both selects from the page's coverage and land on the wanted series,
    // exactly as a visible pane is set up -- one code path, not a second.
    pane.fillSeries(coverage, symbol, timeframe);
    if (pane.symbol() !== symbol || pane.timeframe() !== timeframe) return null;
    // Bars, then one synchronous render+draw. A fresh pane has no viewport, so
    // the engine fits -- which is the view an analysis wants.
    await pane.refresh();
    pane.draw();
    return captureChart(pane.canvas, timeframe);
  } finally {
    holder.remove();
  }
}

/// Split a `data:` URL into the media type and payload the API wants.
///
/// Returns `null` for anything the server would refuse, so the shell does not
/// send a payload it already knows will come back as a 422.
function screenshotFromUrl(url, timeframeLabel = "") {
  const match = /^data:([^;,]+);base64,(.+)$/.exec(url || "");
  if (!match) return null;
  const [, media_type, data] = match;
  if (!["image/png", "image/jpeg", "image/webp"].includes(media_type)) return null;
  // Base64 is 4 characters per 3 bytes, so the byte count is a `* 3 / 4`. The
  // budget is in *image* bytes on both sides, which is the mistake that rejects a
  // legal image when it is measured in the wrong unit.
  if ((data.length * 3) / 4 > MAX_SCREENSHOT_BYTES) return null;
  // Labelled with the chart's timeframe when the caller knows it: a question
  // carrying four images is unreadable to the model without captions, and the
  // prompt lists them by this label in order.
  const label = timeframeLabel ? `${timeframeLabel} chart` : null;
  return label ? { media_type, data, label } : { media_type, data };
}

/// Turn chart attachment on and off.
function paintAttach() {
  const button = el("attachChart");
  button.setAttribute("aria-pressed", attachChart ? "true" : "false");
  el("attachNote").textContent = attachChart
    ? "the chart's viewport, drawings and a 1d/4h/1h/5m screenshot ladder of this symbol go with each question"
    : "";
}


// ---------------------------------------------------------------------------
// Strategy, backtest, bots
// ---------------------------------------------------------------------------

let savedStrategyId = null;

// Who wrote the text currently in the box, and whether it has changed since it
// was last stored.
//
// `created_by` is one of `ai_agent` | `visual_builder` | `developer_sdk` and a
// stored strategy keeps it forever, so the label has to be earned: a document
// the agent drafted is `ai_agent`, and the moment it is edited by hand here it
// is `developer_sdk`. It used to be hardcoded to `visual_builder`, which is the
// one mode this shell does not have -- every strategy was filed as written by a
// builder that does not exist.
let sourceOrigin = "developer_sdk";
let sourceDirty = false;

/// The text changed, so it is the user's own work now -- unless the change
/// came from one of the other two modes, which name themselves.
function markSourceDirty(origin) {
  sourceDirty = true;
  sourceOrigin = origin || "developer_sdk";
}

function setStrategyMode(mode) {
  for (const button of document.querySelectorAll(".modes button")) {
    button.setAttribute("aria-selected", String(button.dataset.mode === mode));
  }
  el("nlPane").hidden = mode !== "nl";
  el("builderPane").hidden = mode !== "builder";
  el("dslPane").hidden = mode !== "dsl";
}

/// Ask the agent for a document, then show it in the editor.
async function generateStrategy() {
  const message = el("nlMsg");
  const description = el("nlDescription").value.trim();
  if (!description) {
    message.innerHTML = `<span class="fail">Describe the setup first.</span>`;
    return;
  }

  message.textContent = "Generating…";
  try {
    const result = await api("/agent/generate-strategy", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        description,
        market: activeSymbol(),
        timeframe: activeTimeframe(),
      }),
    });

    el("strategySource").value = result.yaml;
    sourceOrigin = "ai_agent";
    sourceDirty = true;
    savedStrategyId = null;
    paintStrategyActions();
    // Switch to the document: the point of generating is to read what came
    // back before anything is stored or run.
    setStrategyMode("dsl");

    const repairs = result.repaired_errors || [];
    el("strategyMsg").innerHTML =
      `<span class="pass">generated</span> — ${escapeHtml(result.document.name)} v${escapeHtml(
        result.document.version
      )}<br /><span class="muted">${result.attempts} attempt(s)` +
      (repairs.length
        ? `; the validator rejected ${repairs.length} thing(s) and the agent fixed them`
        : "") +
      `. Save to make it yours.</span>`;
    message.textContent = "";
  } catch (e) {
    // 401 and 429 are the two ordinary refusals: one is "sign in", the other is
    // this endpoint's per-user limit, because it calls a paid model.
    message.innerHTML =
      e.status === 401
        ? `<span class="fail">Sign in to generate.</span> <span class="muted">It calls a paid model, so it is per-user and rate limited.</span>`
        : `<span class="fail">${escapeHtml(e.message)}</span>`;
  }
}

/// Enable the buttons that only make sense with a stored strategy.
function paintStrategyActions() {
  el("deleteStrategy").disabled = !savedStrategyId;
}

// ---------------------------------------------------------------------------
// The visual builder -- docs/14's third editor mode
// ---------------------------------------------------------------------------
//
// The form is a view over the same textarea the other two modes use. Nothing
// here parses YAML or decides what a document means:
//
//   * the vocabulary comes from `GET /strategies/schema`, generated from
//     `strategy-dsl`, so the builder cannot offer a condition the validator
//     would reject;
//   * loading a document into the form goes through `POST /strategies/validate`,
//     which echoes the document as the real parser understood it.
//
// What is left in JavaScript is assembly: dropdowns into a document, which is
// no more arithmetic than a form filling in a template.

let builderSchema = null; // the vocabulary, fetched once
let builderForm = null; // the form as last rendered
let builderText = null; // the document text the form was built from

async function loadBuilderSchema() {
  try {
    builderSchema = await api("/strategies/schema");
    return true;
  } catch (e) {
    el("strategyMsg").innerHTML = `<span class="fail">${escapeHtml(e.message)}</span>`;
    return false;
  }
}

/// Get the form ready to be shown, importing the current text if it changed.
///
/// Returns false and explains why when there is nothing to import: the
/// document in the box does not parse, so there is no form to show.
async function prepareBuilder() {
  if (!builderSchema && !(await loadBuilderSchema())) return false;

  const box = el("strategySource");
  if (builderForm && box.value === builderText) return true;

  try {
    const result = await api("/strategies/validate", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ source: box.value }),
    });
    builderForm = StrategyBuilder.formFromDocument(result.document, builderSchema);
    builderText = box.value;
    return true;
  } catch (e) {
    if (builderForm) {
      // A form the user can see beats a mode they cannot leave: keep the one
      // from the last document that did parse and say so. Otherwise a
      // half-built document would trap them in the raw editor.
      el("strategyMsg").innerHTML =
        `<span class="unknown">the document no longer validates, so the builder is showing the version it last read</span>`;
      builderText = box.value;
      return true;
    }
    el("strategyMsg").innerHTML =
      `<span class="fail">${escapeHtml(e.message)}</span>` +
      issueList((e.details && e.details.issues) || []);
    return false;
  }
}

async function selectStrategyMode(mode) {
  if (mode === "builder" && !(await prepareBuilder())) return;
  setStrategyMode(mode);
  if (mode === "builder") renderBuilderForm();
}

// -- rendering --------------------------------------------------------------

function options(values, selected) {
  return values
    .map(
      (v) =>
        `<option value="${escapeHtml(v)}"${
          String(v) === String(selected) ? " selected" : ""
        }>${escapeHtml(v)}</option>`
    )
    .join("");
}

const brow = (body) => `<div class="brow">${body}</div>`;
const card = (title, body) => `<div class="bcard"><h3>${escapeHtml(title)}</h3>${body}</div>`;

function fieldOptions(selected) {
  return options(builderSchema.fields.map((f) => f.name), selected);
}

function documentCard() {
  const f = builderForm;
  return card("Document", [
    brow(`<input data-f="name" value="${escapeHtml(f.name)}" placeholder="name" />`),
    brow(
      `<input data-f="version" value="${escapeHtml(f.version)}" placeholder="version" style="flex:0 1 64px" />` +
        `<select data-f="kind" title="what this document is for">${options(
          builderSchema.document_kinds,
          f.kind
        )}</select>`
    ),
    brow(`<input data-f="market" value="${escapeHtml(f.market)}" placeholder="market" />`),
    brow(
      `<input data-f="description" value="${escapeHtml(f.description)}" placeholder="description" />`
    ),
    brow(`<input data-f="skillRef" value="${escapeHtml(f.skillRef)}" placeholder="skill ref" />`),
  ].join(""));
}

function timeframesCard() {
  const rows = builderForm.timeframes
    .map(
      (t, i) =>
        brow(
          `<input data-t="${i}" data-p="name" value="${escapeHtml(t.name)}" placeholder="name" />` +
            `<select data-t="${i}" data-p="tf">${options(builderSchema.timeframes, t.tf)}</select>` +
            `<button class="btiny" data-act="del-timeframe" data-t="${i}" title="remove">−</button>`
        )
    )
    .join("");
  return card(
    "Timeframes",
    rows + brow(`<button class="btiny" data-act="add-timeframe">+ timeframe</button>`)
  );
}

function riskCard() {
  const risk = builderForm.risk;
  const params = StrategyBuilder.stopParams(builderSchema, risk.stop.kind)
    .map(
      (name) =>
        `<input data-risk="param" data-sp="${escapeHtml(name)}" value="${escapeHtml(
          risk.stop.params[name] || ""
        )}" placeholder="${escapeHtml(name)}" />`
    )
    .join("");
  return card("Risk", [
    brow(
      `<label>risk %</label><input data-risk="maxRiskPct" value="${escapeHtml(
        risk.maxRiskPct
      )}" />`
    ),
    brow(
      `<label>stop</label><select data-risk="stopKind">${options(
        builderSchema.stops.map((s) => s.kind),
        risk.stop.kind
      )}</select>${params}`
    ),
    brow(
      `<label><input type="checkbox" data-risk="hasTakeProfit"${
        risk.hasTakeProfit ? " checked" : ""
      } /> target</label>` +
        (risk.hasTakeProfit
          ? `<select data-risk="tpType">${options(
              builderSchema.take_profit_types,
              risk.takeProfit.type
            )}</select><input data-risk="tpValue" value="${escapeHtml(
              risk.takeProfit.value
            )}" />`
          : "")
    ),
    brow(
      `<label>direction</label><select data-f="direction" title="blank means: whatever the stop rule implies">` +
        `<option value="">from the stop</option>${options(
          builderSchema.directions,
          builderForm.direction
        )}</select>`
    ),
  ].join(""));
}

/// The controls for one operand: what kind it is, then its value.
function operandControls(at, part, operand) {
  const kindSelect = `<select ${at} data-p="${part}-kind">${options(
    ["number", "field", "string", "bool"],
    operand.kind
  )}</select>`;
  let value;
  switch (operand.kind) {
    case "field":
      value = `<select ${at} data-p="${part}-value">${fieldOptions(operand.value)}</select>`;
      break;
    case "bool":
      value = `<select ${at} data-p="${part}-value">${options(
        ["true", "false"],
        operand.value ? "true" : "false"
      )}</select>`;
      break;
    default:
      value = `<input ${at} data-p="${part}-value" value="${escapeHtml(operand.value)}" />`;
  }
  return kindSelect + value;
}

function clauseHtml(group, row, clause, index) {
  const at = `data-g="${group}" data-r="${row}" data-c="${index}"`;
  const head =
    `<label><input type="checkbox" ${at} data-p="not"${
      clause.not ? " checked" : ""
    } /> not</label>` +
    `<select ${at} data-p="form" title="what kind of condition this is">${options(
      ["compare", "call", "raw"],
      clause.form
    )}</select>`;

  let body;
  if (clause.form === "raw") {
    body = `<input ${at} data-p="text" value="${escapeHtml(clause.text)}" />`;
  } else if (clause.form === "call") {
    body =
      `<select ${at} data-p="func">${options(
        builderSchema.funcs.map((f) => f.name),
        clause.func
      )}</select>` +
      (clause.args || [])
        .map((arg, i) => operandControls(at, `arg${i}`, arg))
        .join("");
  } else {
    body =
      `<select ${at} data-p="left-value">${fieldOptions(clause.left.value)}</select>` +
      `<select ${at} data-p="op">` +
      `<option value="">is true</option>${options(builderSchema.operators, clause.op)}</select>` +
      (clause.op ? operandControls(at, "right", clause.right) : "") +
      (clause.op && clause.right.kind === "number"
        ? `<label title="mark the number as tunable"><input type="checkbox" ${at} data-p="tunable"${
            clause.tunable ? " checked" : ""
          } /> tunable</label>`
        : "");
  }

  return brow(
    head +
      body +
      `<button class="btiny" data-act="del-clause" ${at} title="remove">−</button>`
  );
}

function rowHtml(group, row, index) {
  const frames = builderForm.timeframes.filter((t) => t.name.trim()).map((t) => t.name);
  const at = `data-g="${group}" data-r="${index}"`;
  return (
    `<div class="bcond${row.clauses.some((c) => c.form === "raw") ? " raw" : ""}">` +
    brow(
      `<select ${at} data-p="timeframe" title="which declared timeframe this reads">${options(
        frames,
        row.timeframe
      )}</select>` +
        (row.clauses.length > 1
          ? `<select ${at} data-p="joiner">${options(["and", "or"], row.joiner)}</select>`
          : "") +
        `<input ${at} data-p="label" value="${escapeHtml(row.label)}" placeholder="label" />` +
        `<button class="btiny" data-act="del-row" ${at} title="remove">−</button>`
    ) +
    row.clauses.map((c, i) => clauseHtml(group, index, c, i)).join("") +
    brow(`<button class="btiny" data-act="add-clause" ${at}>+ clause</button>`) +
    `</div>`
  );
}

function groupCard(key) {
  const meta = StrategyBuilder.GROUPS[key];
  const rows = builderForm.groups[key];
  const body = rows.length
    ? rows.map((r, i) => rowHtml(key, r, i)).join("")
    : `<p class="bempty">no conditions</p>`;
  return card(
    meta.label,
    body + brow(`<button class="btiny" data-act="add-row" data-g="${key}">+ condition</button>`)
  );
}

// -- concepts ---------------------------------------------------------------
// A concept is a measurement a client defines: a window of candles plus a band.
// The vocabulary (selector names, comparisons, window bounds, per-document cap)
// comes from `GET /strategies/schema`, same as every other dropdown here. The
// form model lives in `builder.js`; this only renders it and routes edits back.

function conceptCard(index, c) {
  const at = `data-con="${index}"`;

  // A concept the builder cannot model stays raw text -- shown, editable as
  // text, and sent to the validator exactly as written. Saying what it is
  // beats silently reshaping it into a guess.
  if (c.raw !== undefined) {
    return card(
      `concept ${index + 1} · kept as text`,
      brow(
        `<input ${at} data-p="raw" value="${escapeHtml(c.raw)}" title="a shape the builder cannot model -- left exactly as written" />`
      ) +
        brow(`<button class="btiny" data-act="del-concept" ${at}>− remove</button>`)
    );
  }

  const selectors = StrategyBuilder.conceptSelectorsFromSchema(builderSchema);
  const ops = StrategyBuilder.conceptOpsFromSchema(builderSchema);
  const sides = StrategyBuilder.conceptSidesFromSchema(builderSchema);
  const bounds = StrategyBuilder.conceptWindowFromSchema(builderSchema);
  const idxValue = (sel) =>
    escapeHtml(sel && sel.index !== undefined && sel.index !== null ? String(sel.index) : "");

  const reqRows = (c.require || [])
    .map((r, i) => {
      const ra = `${at} data-req="${i}"`;
      return brow(
        `<select ${ra} data-p="req-left-selector" title="left operand">${options(
          selectors,
          r.left && r.left.selector
        )}</select>` +
          `<input ${ra} data-p="req-left-index" value="${idxValue(r.left)}" class="bnum" title="candle index, oldest is 0" />` +
          `<select ${ra} data-p="req-op">${options(ops, r.op)}</select>` +
          `<select ${ra} data-p="req-right-selector" title="right operand">${options(
            selectors,
            r.right && r.right.selector
          )}</select>` +
          `<input ${ra} data-p="req-right-index" value="${idxValue(r.right)}" class="bnum" title="candle index, oldest is 0" />` +
          `<button class="btiny" data-act="del-req" ${ra} title="remove">−</button>`
      );
    })
    .join("");

  return card(
    `concept ${index + 1}${c.name ? ` · ${escapeHtml(c.name)}` : ""}`,
    brow(
      `<input ${at} data-p="name" value="${escapeHtml(
        c.name
      )}" placeholder="name (fvg)" title="how a condition references it: concepts.fvg.exists" />` +
        `<input ${at} data-p="label" value="${escapeHtml(
          c.label
        )}" placeholder="chart label" title="how the band reads on a chart; defaults to the name" />`
    ) +
      brow(
        `<select ${at} data-p="side" title="which side is expected to react from a band this finds">${options(
          sides,
          c.side
        )}</select>` +
          `<label class="bfield">window <input ${at} data-p="window" value="${escapeHtml(
            c.window
          )}" class="bnum" /> candles (${bounds.min}..${bounds.max})</label>` +
          `<input ${at} data-p="min_band_ratio" value="${escapeHtml(
            c.min_band_ratio
          )}" placeholder="min band ratio" title="the band must be at least this share of the window's own range; empty draws every match" />`
      ) +
      brow(
        `<label class="bfield">cheaper edge</label>` +
          `<select ${at} data-p="lower-selector">${options(
            selectors,
            c.lower && c.lower.selector
          )}</select>` +
          `<input ${at} data-p="lower-index" value="${idxValue(c.lower)}" class="bnum" title="candle index, oldest is 0" />`
      ) +
      brow(
        `<label class="bfield">dearer edge</label>` +
          `<select ${at} data-p="upper-selector">${options(
            selectors,
            c.upper && c.upper.selector
          )}</select>` +
          `<input ${at} data-p="upper-index" value="${idxValue(c.upper)}" class="bnum" title="candle index, oldest is 0" />`
      ) +
      (reqRows || `<p class="bempty">no requirements -- the band fires whenever it exists</p>`) +
      brow(`<button class="btiny" data-act="add-req" ${at}>+ requirement</button>`) +
      brow(`<button class="btiny" data-act="del-concept" ${at}>− remove concept</button>`)
  );
}

function conceptsCard() {
  const model = builderForm.concepts;
  if (!model) {
    return card(
      "Concepts",
      `<p class="bempty">none declared</p>` +
        brow(`<button class="btiny" data-act="add-concepts" title="define a measurement the conditions can reference">+ concepts</button>`)
    );
  }
  const max = StrategyBuilder.conceptMaxFromSchema(builderSchema);
  const body =
    model.items.map((c, i) => conceptCard(i, c)).join("") +
    (model.items.length < max
      ? brow(`<button class="btiny" data-act="add-concept">+ concept</button>`)
      : `<p class="bempty">at the vocabulary's cap of ${max} concepts per document</p>`);
  return card("Concepts", body);
}

function renderBuilderForm() {
  const keys = builderForm.kind === "indicator" ? [] : StrategyBuilder.GROUP_KEYS;
  el("builderForm").innerHTML =
    documentCard() +
    timeframesCard() +
    (builderForm.kind === "indicator" ? "" : riskCard()) +
    keys.map(groupCard).join("") +
    conceptsCard();
  renderBuilderNote();
}

/// What the form is missing, and what it could not model.
///
/// Separate from [`renderBuilderForm`] because this runs on every keystroke and
/// rebuilding the controls would throw away whatever the user was typing.
function renderBuilderNote() {
  // Say what is wrong before the round trip, and be honest about the
  // conditions held as text because the builder cannot model them.
  const issues = StrategyBuilder.allFormIssues(builderForm, builderSchema);
  const raw = StrategyBuilder.rawRowCount(builderForm);
  const rawConcepts = (builderForm.concepts && builderForm.concepts.items || [])
    .filter((c) => c.raw !== undefined).length;
  let note = "";
  if (issues.length) {
    note += `<span class="fail">${issues
      .slice(0, 3)
      .map((i) => escapeHtml(i))
      .join("<br />")}</span>`;
  }
  if (raw) {
    note +=
      (note ? "<br />" : "") +
      `<span class="unknown">${raw} condition(s) are kept as text -- the builder cannot model them, so they are left exactly as written</span>`;
  }
  if (rawConcepts) {
    note +=
      (note ? "<br />" : "") +
      `<span class="unknown">${rawConcepts} concept(s) are kept as text -- the validator decides whether the selector shape is accepted</span>`;
  }
  el("builderMsg").innerHTML = note;
}

/// Write the form back into the textarea, which is the one source of truth.
function applyBuilderToSource() {
  try {
    const yaml = StrategyBuilder.toYaml(
      StrategyBuilder.documentFromConceptForm(builderForm, builderSchema)
    );
    el("strategySource").value = yaml;
    builderText = yaml;
    markSourceDirty("visual_builder");
    renderBuilderNote();
  } catch (e) {
    el("builderMsg").innerHTML = `<span class="fail">${escapeHtml(e.message)}</span>`;
  }
}

// -- editing ----------------------------------------------------------------

// A change to one of these changes which controls exist, so the form has to be
// rebuilt; a change to anything else only rewrites the document.
const STRUCTURAL = new Set([
  "form",
  "right-kind",
  "arg0-kind",
  "arg1-kind",
  "func",
  "stopKind",
  "hasTakeProfit",
  "kind",
]);

function onBuilderInput(event) {
  const target = event.target;
  const part = target.dataset.p;
  if (!part) return;
  const value = target.type === "checkbox" ? target.checked : target.value;
  const kind = target.type === "checkbox" ? value : String(value);

  const d = target.dataset;
  if (d.f !== undefined) {
    builderForm[d.f] = kind;
  } else if (d.t !== undefined) {
    builderForm.timeframes[Number(d.t)][part] = kind;
  } else if (d.c !== undefined) {
    const clause =
      builderForm.groups[d.g][Number(d.r)].clauses[Number(d.c)];
    if (part === "text") clause.text = kind;
    else if (part === "not") clause.not = value;
    else if (part === "tunable") clause.tunable = value;
    else if (part === "form") {
      const next = StrategyBuilder.newClause();
      next.form = kind;
      if (kind === "compare") Object.assign(next, { left: clause.left || next.left });
      Object.assign(clause, next);
    } else if (part === "left-value") {
      clause.left = { kind: "field", value: kind };
    } else if (part === "op") {
      clause.op = kind;
    } else if (part === "right-kind") {
      clause.right = { kind, value: kind === "field" ? "close" : kind === "bool" ? false : "" };
    } else if (part === "right-value") {
      clause.right.value = kind === "bool" ? kind === "true" : kind;
    } else if (part.startsWith("arg")) {
      const arg = clause.args[Number(/^arg(\d+)/.exec(part)[1])];
      if (part.endsWith("-kind")) arg.kind = kind;
      else arg.value = kind === "bool" ? kind === "true" : kind;
    } else if (part === "func") {
      clause.func = kind;
      const arity = (builderSchema.funcs.filter((f) => f.name === kind)[0] || {}).arity || [0, 0];
      clause.args = [];
      for (let i = 0; i < arity[0]; i += 1) {
        clause.args.push({ kind: "number", value: "0" });
      }
    }
  } else if (d.r !== undefined) {
    builderForm.groups[d.g][Number(d.r)][part] = kind;
  } else if (d.risk !== undefined) {
    const risk = builderForm.risk;
    if (d.risk === "param") risk.stop.params[d.sp] = kind;
    else if (d.risk === "hasTakeProfit") risk.hasTakeProfit = value;
    else if (d.risk === "tpType") risk.takeProfit.type = kind;
    else if (d.risk === "tpValue") risk.takeProfit.value = kind;
    else if (d.risk === "stopKind") risk.stop = { kind, params: {} };
    else risk[d.risk] = kind;
  } else if (d.con !== undefined) {
    const model = builderForm.concepts;
    if (!model) return;
    const c = model.items[Number(d.con)];
    if (!c) return;
    const r = d.req !== undefined ? (c.require || [])[Number(d.req)] : null;
    if (r) {
      // A requirement operand is stored as a selector; editing one piece must
      // not discard the other, the way changing a clause's kind does not.
      const sides = {
        "req-left": r.left,
        "req-right": r.right,
      };
      if (part === "req-left-selector" || part === "req-right-selector") {
        const target = sides[part.replace("-selector", "")];
        if (target) target.selector = kind;
      } else if (part === "req-left-index" || part === "req-right-index") {
        const target = sides[part.replace("-index", "")];
        if (target) target.index = kind;
      } else if (part === "req-op") {
        r.op = kind;
      }
    } else if (part === "raw") {
      c.raw = kind;
    } else if (part === "name") {
      c.name = kind;
    } else if (part === "label") {
      c.label = kind;
    } else if (part === "side") {
      c.side = kind;
    } else if (part === "window") {
      c.window = kind;
    } else if (part === "min_band_ratio") {
      c.min_band_ratio = kind;
    } else if (part === "lower-selector" && c.lower) {
      c.lower = { selector: kind, index: c.lower.index };
    } else if (part === "lower-index" && c.lower) {
      c.lower = { selector: c.lower.selector, index: kind };
    } else if (part === "upper-selector" && c.upper) {
      c.upper = { selector: kind, index: c.upper.index };
    } else if (part === "upper-index" && c.upper) {
      c.upper = { selector: c.upper.selector, index: kind };
    }
  }

  if (STRUCTURAL.has(part) || STRUCTURAL.has(d.risk)) renderBuilderForm();
  applyBuilderToSource();
}

function onBuilderClick(event) {
  const button = event.target.closest("button[data-act]");
  if (!button) return;
  const d = button.dataset;

  switch (d.act) {
    case "add-timeframe":
      builderForm.timeframes.push({ name: "", tf: builderSchema.timeframes[0] });
      break;
    case "del-timeframe":
      builderForm.timeframes.splice(Number(d.t), 1);
      break;
    case "add-row":
      builderForm.groups[d.g].push(
        StrategyBuilder.newRow(builderForm.timeframes[0] && builderForm.timeframes[0].name)
      );
      break;
    case "del-row":
      builderForm.groups[d.g].splice(Number(d.r), 1);
      break;
    case "add-clause":
      builderForm.groups[d.g][Number(d.r)].clauses.push(StrategyBuilder.newClause());
      break;
    case "del-clause": {
      const clauses = builderForm.groups[d.g][Number(d.r)].clauses;
      clauses.splice(Number(d.c), 1);
      if (!clauses.length) builderForm.groups[d.g].splice(Number(d.r), 1);
      break;
    }
    case "add-concepts":
      // The model is created on demand, not by `emptyForm`: an empty default
      // concept would be a permanent issue on forms that never wanted one.
      builderForm.concepts = StrategyBuilder.conceptModelFromSchema(builderSchema);
      break;
    case "add-concept": {
      const model = builderForm.concepts;
      if (!model) break;
      const max = StrategyBuilder.conceptMaxFromSchema(builderSchema);
      if (model.items.length >= max) break;
      model.items.push(StrategyBuilder.newConcept(builderSchema));
      break;
    }
    case "del-concept": {
      const model = builderForm.concepts;
      if (!model) break;
      model.items.splice(Number(d.con), 1);
      // A model with no concepts left is dropped entirely, so the block comes
      // off the document instead of hanging around as an empty list.
      if (!model.items.length) delete builderForm.concepts;
      break;
    }
    case "add-req": {
      const model = builderForm.concepts;
      if (!model) break;
      const c = model.items[Number(d.con)];
      if (!c || c.raw !== undefined) break;
      if (!c.require) c.require = [];
      c.require.push(StrategyBuilder.newRequirement(builderSchema));
      break;
    }
    case "del-req": {
      const model = builderForm.concepts;
      if (!model) break;
      const c = model.items[Number(d.con)];
      if (!c || !c.require) break;
      c.require.splice(Number(d.req), 1);
      break;
    }
    default:
      return;
  }

  applyBuilderToSource();
}

// The editor's starting document comes from `GET /strategies/reference`, which
// serves `strategies/liquidity-sweep.yaml` -- the same file `strategy-dsl`'s
// test suite asserts validates. Deliberately NOT embedded here: a second copy
// is a second thing to drift, and the editor starting empty is exactly the bug
// this replaces.
//
// The editor used to be seeded only from a *saved* strategy, and a new account
// has none -- so `GET /strategies` returned `[]`, the textarea stayed empty, and
// Validate and Save both sent `{source: ""}` and got back
// `STRATEGY_PARSE_FAILED: missing field 'name'`. The buttons looked broken; the
// API was behaving correctly and the shell was sending nothing.

/// Load one shipped strategy into the editor.
async function loadExample(file) {
  const message = el("strategyMsg");
  message.textContent = "Loading…";
  try {
    const response = await fetch(
      `/strategies/reference${file ? `?file=${encodeURIComponent(file)}` : ""}`
    );
    if (!response.ok) {
      const body = await response.json().catch(() => null);
      throw new Error(body?.error?.message || `the example is ${response.status}`);
    }
    el("strategySource").value = await response.text();
    // A loaded example is not saved yet, so the backtest and the bot buttons
    // would be pointing at the previous document.
    savedStrategyId = null;
    sourceDirty = true;
    paintStrategyActions();
    message.innerHTML = `<span class="muted">loaded ${escapeHtml(
      file || "the default"
    )}. Validate, then Save to make it yours.</span>`;
  } catch (e) {
    message.innerHTML = `<span class="fail">${escapeHtml(e.message)}</span>`;
  }
}

/// Fill the picker with what the deployment ships.
async function loadExampleList() {
  try {
    const examples = await api("/strategies/examples");
    el("example").innerHTML = examples
      .map((e) => `<option value="${escapeHtml(e.file)}">${escapeHtml(e.name)}</option>`)
      .join("");
    return examples.length ? examples[0].file : null;
  } catch {
    // Older deployments have no listing; the default still resolves.
    el("example").innerHTML = `<option value="">default example</option>`;
    return null;
  }
}

/// Put something valid in the editor.
async function seedEditor() {
  const message = el("strategyMsg");

  // A saved strategy wins: the user's own document is more useful than an
  // example. Signing out is fine here -- Validate needs no token.
  try {
    const strategies = await api("/strategies");
    if (strategies.length) {
      savedStrategyId = strategies[0].id;
      el("strategySource").value = JSON.stringify(strategies[0].document, null, 2);
      // Straight from storage: saving it again would only produce a duplicate.
      sourceOrigin = strategies[0].created_by || "developer_sdk";
      sourceDirty = false;
      paintStrategyActions();
      message.innerHTML = `<span class="muted">editing your ${escapeHtml(
        strategies[0].name
      )} v${escapeHtml(strategies[0].version)}</span>`;
      await loadExampleList();
      return;
    }
  } catch { /* not signed in, or none saved */ }

  const first = await loadExampleList();
  await loadExample(first);
}

/// The validator's field-level issues, as list items.
///
/// docs/12 carries these in `details.issues` with a path per issue; showing
/// them is the whole reason the envelope has a details field.
function issueList(issues) {
  return issues.length
    ? `<ul class="checks">${issues
        .map(
          (i) =>
            `<li><span class="status fail">${escapeHtml(i.path)}</span><span>${escapeHtml(
              i.message
            )}</span></li>`
        )
        .join("")}</ul>`
    : "";
}

async function validateStrategy() {
  const message = el("strategyMsg");
  message.textContent = "Validating…";
  try {
    const result = await api("/strategies/validate", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ source: el("strategySource").value }),
    });
    message.innerHTML = `<span class="pass">valid</span> — ${escapeHtml(result.name)} v${escapeHtml(result.version)}`;
  } catch (e) {
    message.innerHTML =
      `<span class="fail">${escapeHtml(e.message)}</span>` +
      issueList((e.details && e.details.issues) || []);
  }
}

async function saveStrategy() {
  const message = el("strategyMsg");

  // Editing a stored strategy stores a *new version* rather than overwriting
  // it, because its backtests point at the document that produced them. So an
  // unchanged document has nothing to save, and the way to publish a change is
  // to bump `version:` in the document.
  if (savedStrategyId && !sourceDirty) {
    message.innerHTML = `<span class="muted">already saved. Bump the document's <code>version</code> to store a new one.</span>`;
    return;
  }
  if (!sourceDirty && !savedStrategyId) {
    sourceDirty = true;
  }

  const editing = Boolean(savedStrategyId);
  try {
    const saved = await api(editing ? `/strategies/${savedStrategyId}` : "/strategies", {
      method: editing ? "PUT" : "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ source: el("strategySource").value, created_by: sourceOrigin }),
    });
    savedStrategyId = saved.id;
    sourceDirty = false;
    paintStrategyActions();
    message.innerHTML =
      `<span class="pass">${editing ? "saved as a new version" : "saved"}</span> — ${escapeHtml(
        saved.name
      )} v${escapeHtml(saved.version)}` +
      (saved.supersedes
        ? `<br /><span class="muted">replaces ${escapeHtml(saved.supersedes)}; its backtests still point at the old document</span>`
        : `<br /><span class="muted">now you can run a backtest or launch a paper bot</span>`);
    // A new version is a new id, so the runs under the old one are no longer
    // this strategy's. The list has to follow.
    await refreshBacktestRuns();
  } catch (e) {
    // A 401 here is the ordinary "not signed in yet" case rather than an error
    // worth showing verbatim, and the fix is one click away.
    message.innerHTML =
      e.status === 401
        ? `<span class="fail">Sign in to save.</span> <span class="muted">Validate works without an account; saving is per-user.</span>`
        : `<span class="fail">${escapeHtml(e.message)}</span>`;
  }
}

async function deleteStrategy() {
  if (!savedStrategyId) return;
  const message = el("strategyMsg");
  if (!window.confirm("Delete this strategy and its backtests?")) return;

  try {
    await api(`/strategies/${savedStrategyId}`, { method: "DELETE" });
    savedStrategyId = null;
    sourceDirty = true;
    paintStrategyActions();
    message.innerHTML = `<span class="muted">deleted. Load an example or describe a new setup.</span>`;
    // The runs went with it, so the list and the curve go too.
    await refreshBacktestRuns();
  } catch (e) {
    // 409 is the interesting one: a bot is still running this document, and the
    // server refuses rather than pulling it out from under the bot.
    message.innerHTML = `<span class="fail">${escapeHtml(e.message)}</span>`;
  }
}

/// Draw an equity curve.
///
/// Every number below is already a percentage of the box: `plot.rs` did the
/// scaling, because `docs/14` keeps arithmetic over a run's numbers in Rust.
/// What is left here is turning points into a string, which is formatting and
/// is exactly what a shell is for. `toFixed` is the only transformation.
function curveSvg(plot) {
  const points = plot.points
    .map((point) => `${point.x.toFixed(2)},${point.y.toFixed(2)}`)
    .join(" ");

  // The water line: above it the run is up, below it the run is down. Rust
  // places it, and leaves it out when zero falls outside the box.
  const zero =
    plot.zero_y === null || plot.zero_y === undefined
      ? ""
      : `<line x1="0" y1="${plot.zero_y.toFixed(2)}" x2="100" y2="${plot.zero_y.toFixed(
          2
        )}" />`;

  return `
    <svg class="curve" viewBox="0 0 100 100" preserveAspectRatio="none" role="img"
         aria-label="equity curve, ${plot.min.toFixed(2)} to ${plot.max.toFixed(2)} R">
      ${zero}
      <polyline points="${points}" />
    </svg>
    <div class="curve-axis">
      <span>${plot.max.toFixed(2)}R</span><span>${plot.min.toFixed(2)}R</span>
    </div>`;
}

/// Draw one stored run.
///
/// A run is read back rather than re-run: it is an observation made at a
/// moment (`docs/13`), and the candles underneath it change.
function renderBacktest(run) {
  const r = run.report || {};
  const window = `${new Date(run.from / 1e6).toLocaleDateString()} → ${new Date(
    run.to / 1e6
  ).toLocaleDateString()}`;

  // Two different reasons for a missing curve, and the trade count is what
  // tells them apart -- the report says how many trades it took either way.
  const note = run.equity_plot
    ? ""
    : `<p class="muted">${
        r.total_trades
          ? "This run was stored before the equity curve was kept, so there is nothing to draw."
          : "No trades in this window, so there is no curve."
      }</p>`;

  el("backtestOut").innerHTML = `
    <div class="run-head">${escapeHtml(run.symbol)} · ${escapeHtml(window)}</div>
    <dl class="kv">
      <dt>trades</dt><dd>${r.total_trades ?? 0}</dd>
      <dt>win rate</dt><dd>${((r.win_rate ?? 0) * 100).toFixed(1)}%</dd>
      <dt>average R</dt><dd>${(r.average_r ?? 0).toFixed(3)}</dd>
      <dt>net</dt><dd>${(r.net_return_pct ?? 0).toFixed(2)}R</dd>
      <dt>max DD</dt><dd>${(r.max_drawdown_pct ?? 0).toFixed(2)}R</dd>
    </dl>
    ${run.equity_plot ? curveSvg(run.equity_plot) : ""}
    ${note}
    <p class="muted">${escapeHtml(r.assumptions?.return_units || "")}</p>`;
}

/// List this strategy's runs, newest first, and draw one of them.
///
/// `selectId` is the run to land on; without it the current selection is kept,
/// falling back to the newest.
async function refreshBacktestRuns(selectId) {
  const select = el("backtestRuns");
  if (!savedStrategyId) {
    select.innerHTML = `<option value="">No runs yet</option>`;
    el("backtestOut").innerHTML = "";
    return;
  }

  let runs;
  try {
    runs = await api(`/strategies/${savedStrategyId}/backtests`);
  } catch (e) {
    select.innerHTML = `<option value="">Runs unavailable</option>`;
    el("backtestOut").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
    return;
  }

  if (!runs.length) {
    select.innerHTML = `<option value="">No runs yet</option>`;
    el("backtestOut").innerHTML = "";
    return;
  }

  select.innerHTML = runs
    .map((run) => {
      const when = new Date(run.created_at / 1e6).toLocaleString();
      const r = run.report || {};
      return `<option value="${escapeHtml(run.id)}">${escapeHtml(when)} · ${
        r.total_trades ?? 0
      } trades · ${(r.net_return_pct ?? 0).toFixed(2)}R</option>`;
    })
    .join("");

  // A selection that is no longer in the list -- a deleted run, a different
  // strategy -- falls back to the newest rather than leaving nothing drawn.
  const wanted = selectId || select.value;
  select.value = runs.some((run) => run.id === wanted) ? wanted : runs[0].id;
  await showBacktest(select.value);
}

async function showBacktest(id) {
  if (!id) return;
  try {
    renderBacktest(await api(`/backtests/${id}`));
  } catch (e) {
    el("backtestOut").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
  }
}

async function runBacktest() {
  if (!savedStrategyId) {
    el("backtestOut").innerHTML = `<p class="fail">Save the strategy first.</p>`;
    return;
  }
  el("backtestOut").innerHTML = `<p class="empty">Running…</p>`;
  try {
    const created = await api(`/strategies/${savedStrategyId}/backtest`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        symbol: activeSymbol(),
        from: el("btFrom").value,
        to: el("btTo").value,
      }),
    });
    // The list is what runs exist, so the new one is drawn by re-reading it
    // rather than from this response. One render path, so a run drawn here and
    // the same run drawn later cannot differ.
    await refreshBacktestRuns(created.id);
  } catch (e) {
    el("backtestOut").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
  }
}

async function launchBot() {
  if (!savedStrategyId) {
    el("botsOut").innerHTML = `<p class="fail">Save the strategy first.</p>`;
    return;
  }
  try {
    const bot = await api("/bots", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ strategy_id: savedStrategyId }),
    });
    // Launching is the one moment the user certainly wants to watch, so the
    // log opens on the new bot rather than making them find it and click.
    await refreshBots();
    watchBot(bot.id);
  } catch (e) {
    el("botsOut").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
  }
}

/// One decision's outcome, as a line of English.
///
/// `DecisionOutcome` has no serde tag, so a unit variant arrives as a bare
/// string and a struct variant as a single-key object. Both shapes are handled
/// here rather than by changing the engine's wire format for the panel's sake.
const UNIT_OUTCOMES = {
  NoContext: "not enough context yet",
  NoSignal: "no signal",
};

function outcomeText(outcome) {
  if (typeof outcome === "string") return UNIT_OUTCOMES[outcome] || outcome;
  const [kind, body] = Object.entries(outcome || {})[0] || ["unknown", {}];
  switch (kind) {
    case "EntryQueued":
      return `entry queued (${(body.reasons || []).length} condition(s))`;
    case "EntryFilled":
      return "entry filled";
    case "EntryDenied":
      return `entry denied by ${body.limit} (${body.value})`;
    case "EntryRefused":
      return `entry refused: ${body.reason}`;
    case "ExitFilled":
      return `exit filled (${body.trigger})`;
    case "Closed":
      return `closed ${body.r_multiple >= 0 ? "+" : ""}${body.r_multiple.toFixed(2)}R (${body.trigger})`;
    case "Halted":
      return `halted: ${body.reason}`;
    default:
      return kind;
  }
}

/// One socket frame, as a line of English.
function botEventText(frame) {
  const payload = frame.payload || {};
  if (frame.kind === "started") return `watching ${payload.symbol}`;
  if (frame.kind === "stopped") {
    const trades = `${payload.trades ?? 0} trade(s)`;
    return payload.halt_reason
      ? `stopped after ${trades} — ${payload.halt_reason}`
      : `stopped after ${trades}`;
  }
  if (frame.kind === "decision") {
    const record = payload.record || {};
    const at = record.at ? new Date(record.at / 1e6).toLocaleTimeString() : "";
    return `${at} · ${record.price ?? "?"} · ${outcomeText(record.outcome)}`;
  }
  return frame.kind || "event";
}

/// Follow one bot's activity.
///
/// The channel is one socket per bot and filters server-side, so a watcher
/// receives only its own bot's events. Nothing here polls: a decision appears
/// when the bot makes it, which is the whole reason the supervisor broadcasts
/// instead of letting clients ask.
function watchBot(id) {
  if (botSocket) botSocket.close();
  botLog = [];
  watchedBot = id;

  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const query = token() ? `?token=${encodeURIComponent(token())}` : "";
  const ws = new WebSocket(`${scheme}://${location.host}/ws/bots/${id}${query}`);
  botSocket = ws;

  ws.onmessage = (event) => {
    let frame;
    try {
      frame = JSON.parse(
        typeof event.data === "string" ? event.data : new TextDecoder().decode(event.data)
      );
    } catch {
      return;
    }

    if (frame.type === "data") {
      // The log is newest-first on screen, so it is capped from the far end:
      // an afternoon of 1m decisions would otherwise grow without bound.
      botLog.push(frame.payload);
      if (botLog.length > BOT_LOG_LIMIT) botLog.shift();
      renderBots();
    } else if (frame.type === "notice") {
      botLog.push({ kind: "notice", message: frame.message });
      renderBots();
    } else if (frame.type === "lagged") {
      botLog.push({ kind: "lagged", dropped: frame.dropped });
      renderBots();
    }
  };
  ws.onclose = () => {
    // Only the socket we are still meant to be using may speak for the panel.
    if (botSocket !== ws) return;
    botSocket = null;
    renderBots();
  };

  renderBots();
}

function unwatchBot() {
  if (botSocket) botSocket.close();
  botSocket = null;
  watchedBot = null;
  botLog = [];
  renderBots();
}

/// The bot panel, drawn from state.
///
/// Everything it shows -- the list and the live log -- comes from `bots`,
/// `botLog` and `watchedBot`, so a socket frame and a refresh land in the same
/// renderer and cannot disagree about what a bot is doing.
function renderBots() {
  if (!bots.length) {
    el("botsOut").innerHTML = `<p class="empty">No bots yet.</p>`;
    return;
  }

  el("botsOut").innerHTML = bots
    .map((bot) => {
      const watching = bot.id === watchedBot;
      const log = watching ? botLogHtml() : "";
      const open = bot.id === notificationsOpen;
      const feed = open ? notificationsHtml(bot) : "";
      // "Killed" is a `status`, not a flag of its own -- so the button reads the
      // same field the table above it shows and the two cannot disagree about
      // whether the switch is thrown.
      const killed = bot.status === "killed";
      // The count comes from the activity summary and the list from
      // `/bots/{id}/notifications`; they read the same `bot.notification` rows,
      // so a count above zero with an empty list would be a bug worth seeing.
      const raised = bot.activity?.notifications ?? 0;
      return `<dl class="kv">
          <dt>id</dt><dd>${escapeHtml(bot.id.slice(0, 8))}</dd>
          <dt>status</dt><dd>${escapeHtml(bot.status)}</dd>
          <dt>mode</dt><dd>${escapeHtml(bot.mode)}${
            bot.venue ? ` on ${escapeHtml(bot.venue)}` : ""
          }</dd>
          <dt>supervised</dt><dd>${bot.supervised_here}</dd>
          <dt>trades</dt><dd>${bot.activity?.trades ?? 0}</dd>
          <dt>decisions</dt><dd>${bot.activity?.decisions ?? 0}</dd>
          <dt>cumulative R</dt><dd>${(bot.activity?.cumulative_r ?? 0).toFixed(3)}</dd>
          <dt>notifications</dt><dd>${raised}</dd>
        </dl>
        <div class="row">
          <button data-bot="${bot.id}" data-act="${watching ? "unwatch" : "watch"}">${
            watching ? "Stop watching" : "Watch"
          }</button>
          <button data-bot="${bot.id}" data-act="${open ? "hide-notifications" : "notifications"}">${
            open ? "Hide notifications" : "Notifications"
          }</button>
          <button data-bot="${bot.id}" data-act="pause">Pause</button>
          <button data-bot="${bot.id}" data-act="resume">Resume</button>
          <button data-bot="${bot.id}" data-act="delete">Delete</button>
          <button data-bot="${bot.id}" data-act="kill" class="danger"
                  ${killed ? "disabled" : ""}
                  title="No new entries, and an open position is closed at market">${
                    killed ? "Killed" : "Kill switch"
                  }</button>
        </div>
        ${feed}
        ${log}`;
    })
    .join("<hr />");
}

/// A bot's notifications, newest first.
///
/// This is the reader the audit rows never had. `docs/11` asks for the user to
/// be *notified* on a breach and the risk engine has always written a
/// `bot.notification` row for one; until this existed the only trace was the
/// count in the row above, which says something happened and not what.
function notificationsHtml(bot) {
  const entries = botNotifications[bot.id];
  if (entries === undefined) {
    return `<p class="muted">Loading notifications…</p>`;
  }
  if (!entries.length) {
    // Distinguished from "loading" on purpose: an empty list is the answer to
    // the question, and a spinner that never resolves is how a broken read
    // looks like a slow one.
    return `<p class="muted">No notifications. A breach or a clamped risk limit appears here.</p>`;
  }
  const rows = entries
    .map((n) => {
      const at = n.at ? new Date(n.at / 1e6).toLocaleString() : "";
      const cls = n.severity === "critical" ? "fail" : "muted";
      return `<li class="${cls}"><strong>${escapeHtml(n.title)}</strong> · ${escapeHtml(
        n.severity
      )}<br /><span class="muted">${at} · ${escapeHtml(n.kind)}</span><br />${escapeHtml(n.body)}</li>`;
    })
    .join("");
  return `<ul class="botlog">${rows}</ul>`;
}

/// Fetch a bot's notifications and show them.
async function toggleNotifications(id) {
  if (notificationsOpen === id) {
    notificationsOpen = null;
    renderBots();
    return;
  }
  notificationsOpen = id;
  delete botNotifications[id];
  renderBots();
  try {
    botNotifications[id] = await api(`/bots/${id}/notifications`);
  } catch (e) {
    // Kept as an entry rather than dropped, so the failure renders in the list
    // instead of leaving the panel on "Loading…" forever.
    botNotifications[id] = [
      { kind: "error", severity: "critical", title: "Could not read notifications", body: e.message, at: 0 },
    ];
  }
  renderBots();
}

/// The live log for the watched bot, newest first.
function botLogHtml() {
  if (!botLog.length) {
    return `<p class="muted">Watching. A decision appears here as the bot makes it — on the
      decision timeframe, so the first one can be minutes away.</p>`;
  }
  const rows = botLog
    .slice()
    .reverse()
    .map((entry) => {
      if (entry.kind === "notice") return `<li class="fail">${escapeHtml(entry.message)}</li>`;
      if (entry.kind === "lagged") {
        return `<li class="fail">dropped ${entry.dropped} event(s)</li>`;
      }
      return `<li>${escapeHtml(botEventText(entry))}</li>`;
    })
    .join("");
  return `<ul class="botlog">${rows}</ul>`;
}

async function refreshBots() {
  try {
    bots = await api("/bots");
    // A bot that has gone is not one to keep a socket open for.
    if (watchedBot && !bots.some((bot) => bot.id === watchedBot)) unwatchBot();
    else renderBots();
  } catch (e) {
    el("botsOut").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
  }
}

/// The scan panel, drawn from the last `GET /scan`.
///
/// ## Why the summary is shown verbatim and the failures are their own list
///
/// The route already writes the sentence a user should read, with the
/// consequences named -- how many instruments were measured, whether the ranking
/// covers everything asked for, and whether the universe was the venue's or the
/// list the user typed. Rephrasing it here would be a second opinion about what
/// the scan means, and the two would drift.
///
/// `failures` is a separate list from `rows` and is rendered as one, because "we
/// could not measure this" and "this ranked last" are different claims. A row
/// with a null value drawn in rank order would state the first while looking like
/// the second.
///
/// `as_of_ms` is on every row and is shown, not dropped: a ranking reads as "now",
/// and on a thin listing whose newest bar is an hour old that is false in a way
/// the reader cannot see.
function renderScan() {
  if (!scan) {
    el("scanOut").innerHTML = `<p class="empty">Pick a measurement and scan.</p>`;
    return;
  }

  const age = (ms) => {
    if (ms === null || ms === undefined) return "unknown";
    const mins = Math.round(Math.max(0, Date.now() - ms) / 60000);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.round(mins / 60);
    return hours < 48 ? `${hours}h ago` : `${Math.round(hours / 24)}d ago`;
  };

  const dash = `<span class="muted">—</span>`;
  const row = (r, rank) => `<tr>
      <td>${rank || dash}</td>
      <td>${escapeHtml(r.symbol)}</td>
      <td>${r.value === null || r.value === undefined ? dash : r.value.toFixed(2)}</td>
      <td class="muted">${escapeHtml(age(r.as_of_ms))}</td>
      <td class="muted">${r.bars}</td>
      <td class="muted">${r.error ? escapeHtml(r.error) : ""}</td>
    </tr>`;

  const ranked = (scan.rows || []).map((r, i) => row(r, i + 1)).join("");
  const unmeasured = (scan.failures || []).map((r) => row(r, 0)).join("");

  el("scanOut").innerHTML = `
    <p class="muted">${escapeHtml(scan.summary || "")}</p>
    ${
      ranked
        ? `<table class="scan">
             <thead><tr><th>#</th><th>symbol</th><th>${escapeHtml(scan.metric || "")}</th>
               <th>measured</th><th>bars</th><th></th></tr></thead>
             <tbody>${ranked}</tbody>
           </table>`
        : `<p class="empty">Nothing could be ranked.</p>`
    }
    ${
      unmeasured
        ? `<details class="steps-wrap"><summary>${
            (scan.failures || []).length
          } could not be measured</summary>
             <table class="scan">
               <thead><tr><th></th><th>symbol</th><th>value</th>
                 <th>measured</th><th>bars</th><th>reason</th></tr></thead>
               <tbody>${unmeasured}</tbody>
             </table>
           </details>`
        : ""
    }
    ${scan.note ? `<p class="muted">${escapeHtml(scan.note)}</p>` : ""}`;
}

/// Run a scan and show it.
///
/// The `symbols` box is sent only when it has something in it, and that is the
/// difference between two universes the route reports back: an empty box asks
/// about the venue's indexed instruments, a filled one asks about exactly what was
/// typed. Sending an empty `symbols=` would be a third thing -- an explicit
/// request for nothing -- so the parameter is omitted rather than left blank, and
/// the panel's `universe` line is what tells the user which of the two they got.
async function runScan() {
  const button = el("scanGo");
  button.disabled = true;
  el("scanOut").innerHTML = `<p class="empty">Scanning…</p>`;
  try {
    const params = new URLSearchParams();
    params.set("metric", el("scanMetric").value);
    params.set("timeframe", el("scanTimeframe").value);
    const typed = el("scanSymbols").value.trim();
    if (typed) params.set("symbols", typed);

    scan = await api(`/scan?${params.toString()}`);
    renderScan();
  } catch (e) {
    el("scanOut").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
  } finally {
    // In `finally` rather than after the render: a scan that fails still has to
    // give the button back, or the panel is a dead end the user cannot retry.
    button.disabled = false;
  }
}

/// The live-trading panel, drawn from state.
///
/// Two facts per venue, and they are shown separately on purpose.
/// `credentials_configured` and `opted_in` fail identically from a user's point
/// of view -- "I turned it on and it still refuses" -- and the fix is different:
/// one is this checkbox, the other is an environment variable on the API
/// process that no button here can change. Collapsing them into one indicator
/// would leave the second case looking like a broken switch.
function renderVenues() {
  if (!venues.length) {
    el("venuesOut").innerHTML = `<p class="empty">No venues configured.</p>`;
    return;
  }

  el("venuesOut").innerHTML = venues
    .map((v) => {
      const req = v.requirements || {};
      // The gate's own thresholds, read from the API rather than hardcoded:
      // a UI that states "20 trades" while the gate says 30 is worse than one
      // that says nothing.
      const rules = [];
      if (req.min_paper_trades) rules.push(`${req.min_paper_trades} closed paper trades`);
      if (req.min_paper_hours) rules.push(`${req.min_paper_hours}h of paper trading`);
      if (req.max_paper_loss_r !== undefined) {
        rules.push(`no worse than ${req.max_paper_loss_r}R`);
      }

      const credentials = v.credentials_configured
        ? `<span class="ok">credentials present</span>`
        : `<span class="fail">no credentials on this deployment — set them on the API process; this button cannot</span>`;

      return `<dl class="kv">
          <dt>venue</dt><dd>${escapeHtml(v.venue)}</dd>
          <dt>live trading</dt><dd>${
            v.opted_in ? `<span class="ok">opted in</span>` : "off"
          }</dd>
          <dt>credentials</dt><dd>${credentials}</dd>
        </dl>
        <p class="muted">A strategy may go live here once it has ${escapeHtml(
          rules.join(", ") || "a paper track record"
        )}.</p>
        <div class="row">
          <button data-venue="${escapeHtml(v.venue)}" data-act="${
            v.opted_in ? "revoke" : "opt-in"
          }" class="${v.opted_in ? "danger" : "primary"}"
                  title="${
                    v.opted_in
                      ? "Liquidates and stops every bot trading here"
                      : "Allows strategies that pass the gate to trade real money here"
                  }">${v.opted_in ? "Revoke" : "Opt in"}</button>
        </div>`;
    })
    .join("<hr />");
}

async function refreshVenues() {
  try {
    venues = await api("/venues");
    renderVenues();
  } catch (e) {
    el("venuesOut").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
  }
}

/// Opt a venue in, or revoke it.
///
/// Revoking reports what it stopped. That number is the whole reason the
/// response carries it: "revoked" on its own does not tell an operator whether
/// a bot was mid-position when they pressed it, and that is the thing they need
/// to know next.
async function venueAction(venue, act) {
  try {
    const result = await api(`/venues/${encodeURIComponent(venue)}/${act}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      // The reason is optional and recorded verbatim in the audit trail. Sent
      // empty from here: `docs/15` wants the opt-in attributable, and a
      // checkbox that demanded a paragraph would be worked around.
      body: JSON.stringify({}),
    });
    const stopped = result?.bots_killed?.length ?? 0;
    if (stopped) {
      el("venuesOut").insertAdjacentHTML(
        "afterbegin",
        `<p class="fail">${escapeHtml(venue)}: threw the kill switch on ${stopped} bot(s).</p>`
      );
    }
    await refreshVenues();
    // A revoke stops bots, so the list above this panel is now stale.
    if (stopped) await refreshBots();
  } catch (e) {
    el("venuesOut").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
  }
}

async function botAction(id, act) {
  if (act === "watch") {
    watchBot(id);
    return;
  }
  if (act === "unwatch") {
    unwatchBot();
    return;
  }
  // Both of these are reads of `/bots/{id}/notifications`, which is a GET. They
  // are handled here rather than falling through to the POST below, which would
  // send `POST /bots/{id}/notifications` and get a 405.
  if (act === "notifications" || act === "hide-notifications") {
    await toggleNotifications(id);
    return;
  }
  try {
    if (act === "delete") await api(`/bots/${id}`, { method: "DELETE" });
    else await api(`/bots/${id}/${act}`, { method: "POST" });
    // The status a button changes is the list's, so the list is re-read.
    await refreshBots();
  } catch (e) {
    el("botsOut").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
  }
}

// ---------------------------------------------------------------------------
// The watchlist
//
// Which market, before anything about one market: every other panel answers
// about the symbol the active chart is showing, and this is the one that
// chooses it. Two sources fill the same list -- the page's instruments plus
// the user's favorites by default, the venue's whole listing under a search --
// and prices tick from `GET /tickers`, whose server-side cache makes polling
// it every few seconds cheap enough to leave running while the pane is open.
// ---------------------------------------------------------------------------

const WATCHLIST_FAVORITES_KEY = "watchlist.favorites";
/// How often the pane re-reads `GET /tickers`. The route caches server-side
/// for five, so asking faster buys nothing; asking slower makes the prices
/// read as stale while the rows are on screen.
const WATCHLIST_TICK_MS = 5000;

/// Favorites, persisted in this browser.
///
/// `localStorage` and not the database, deliberately, for the same reason the
/// rest of the shell keeps its own view state locally: a favorite is a
/// preference about this workstation, not data another user of the account
/// needs to agree on. The page already treats reload-persistence as local
/// (`agentSession` does the same).
function watchlistFavorites() {
  try {
    const raw = JSON.parse(localStorage.getItem(WATCHLIST_FAVORITES_KEY) || "[]");
    return Array.isArray(raw) ? raw.filter((s) => typeof s === "string") : [];
  } catch {
    return [];
  }
}

function toggleFavorite(symbol) {
  const favorites = new Set(watchlistFavorites());
  if (favorites.has(symbol)) favorites.delete(symbol);
  else favorites.add(symbol);
  try {
    localStorage.setItem(WATCHLIST_FAVORITES_KEY, JSON.stringify([...favorites]));
  } catch {
    // A full or blocked store drops the preference rather than the click.
  }
  return favorites.has(symbol);
}

/// The last ticker row per symbol, and when the batch that filled it landed.
const watchlistTickers = new Map();
let watchlistTickersAt = 0;
let watchlistTickTimer = 0;

/// The search result, held so typing does not fight the render loop: rows come
/// from here while a query stands, and from the page's instruments when it
/// does not. `null` means no search is on screen.
let watchlistSearch = null;

/// Prices for everything the list is currently showing. One request covers
/// favorites, search results and the active symbol, because the route takes a
/// symbol list; the server caches the venue answer, so this is cheap to run
/// on its own clock.
async function fetchWatchlistTickers() {
  const rows = watchlistRows().map((row) => row.symbol);
  if (rows.length > 500) {
    // More rows than one venue call can filter for: ask for the venue's most
    // active markets instead and keep whatever overlaps this list. The named
    // form caps at 100 symbols server-side, and a multi-thousand-symbol URL
    // would be a worse request than the summary one.
    try {
      const response = await api("/tickers?limit=1000");
      for (const ticker of response.tickers || []) watchlistTickers.set(ticker.symbol, ticker);
      watchlistTickersAt = Date.now();
      renderWatchlist();
    } catch {
      // The list keeps its last prices and its age note, as below.
    }
    return;
  }
  const symbols = [...new Set([...rows, activeSymbol()].filter(Boolean))];
  if (!symbols.length) return;
  try {
    const response = await api(`/tickers?symbols=${encodeURIComponent(symbols.join(","))}`);
    for (const ticker of response.tickers || []) watchlistTickers.set(ticker.symbol, ticker);
    watchlistTickersAt = Date.now();
    renderWatchlist();
  } catch {
    // The list keeps its last prices and its age note. A watchlist that
    // quietly stops ticking is exactly what `wlSource` exists to name, so
    // the failure says nothing here and lets the age grow.
  }
}

/// What the list shows right now.
///
/// A standing search answers with the venue's matches; with no query, the
/// venue's **whole** listing is the default. The previous default -- favorites
/// plus the handful of instruments the page had already charted -- read as a
/// broken watchlist: five rows where the venue trades thousands. `MARKET_SYMBOLS`
/// warms feeds and seeds this list until the index lands; it is not a permission.
function watchlistRows() {
  if (watchlistSearch) {
    return watchlistSearch.results.map((r) => ({ symbol: r.symbol, trading: r.trading }));
  }
  const seen = new Set();
  const rows = [];
  for (const symbol of [
    ...watchlistFavorites(),
    ...coverage.map((entry) => entry.symbol),
    ...watchlistUniverse,
  ]) {
    if (!seen.has(symbol)) {
      seen.add(symbol);
      rows.push({ symbol });
    }
  }
  return rows;
}

/// Every symbol the venue lists, read once when the index is available.
/// Fetched independently of a search so the default view is the whole venue
/// rather than the few instruments this browser has charted so far.
let watchlistUniverse = [];

async function loadWatchlistUniverse() {
  if (watchlistUniverse.length) return;
  try {
    const response = await api("/symbols/search?q=&limit=2000");
    watchlistUniverse = (response.results || []).map((r) => r.symbol);
    renderWatchlist();
    fetchWatchlistTickers();
  } catch {
    // The default rows stay the charted instruments; the pane already says how
    // many symbols it is showing, and a failed index fetch retries on the next
    // open. A failure here has a fallback, unlike a failed search, which is why
    // it stays silent while `runWatchlistSearch` does not.
  }
}

async function runWatchlistSearch(query) {
  const trimmed = query.trim();
  if (!trimmed) {
    watchlistSearch = null;
    renderWatchlist();
    return;
  }
  try {
    const response = await api(`/symbols/search?q=${encodeURIComponent(trimmed)}&limit=25`);
    // The user may have kept typing while this was in flight; an answer to a
    // question they already revised would flash the wrong rows.
    if (el("wlSearch").value.trim() !== trimmed) return;
    watchlistSearch = response;
    renderWatchlist();
    fetchWatchlistTickers();
  } catch (e) {
    el("wlSource").textContent = `search failed: ${e.message}`;
  }
}

/// Price text, at whatever precision the magnitude actually has.
function fmtPrice(value) {
  if (!Number.isFinite(value)) return "—";
  if (value >= 1000) return value.toLocaleString("en-US", { maximumFractionDigits: 2 });
  if (value >= 1) return value.toFixed(value >= 100 ? 2 : 4);
  return value.toPrecision(4);
}

function renderWatchlist() {
  const rowsEl = el("wlRows");
  const empty = el("wlEmpty");
  const tags = el("wlTags");
  const favorites = watchlistFavorites();
  const favoriteSet = new Set(favorites);
  const rows = watchlistRows();
  // The clear affordance exists only while a query stands: a button for a
  // state that cannot happen is noise above the list.
  el("wlClear").hidden = !watchlistSearch;

  // While searching, favorites sit above the results as one-press exits: the
  // search replaced the default list, and this is how the user gets a pinned
  // symbol back without clearing the query.
  if (watchlistSearch) {
    tags.hidden = false;
    tags.innerHTML = "";
    for (const symbol of favorites) {
      const chip = document.createElement("button");
      chip.className = "wlTag";
      chip.textContent = `★ ${symbol}`;
      chip.onclick = () => chartFromWatchlist(symbol);
      tags.appendChild(chip);
    }
  } else {
    tags.hidden = true;
    tags.innerHTML = "";
  }

  rowsEl.innerHTML = "";
  empty.hidden = rows.length > 0;
  if (!rows.length) {
    empty.textContent = watchlistSearch
      ? `nothing on the venue matches "${watchlistSearch.query}".`
      : "star a row to pin it here, or search every symbol the venue lists.";
  }

  el("wlCount").textContent = watchlistSearch
    ? `${rows.length} of ${watchlistSearch.indexed} listed symbols`
    : `${rows.length} instrument${rows.length === 1 ? "" : "s"}` +
      (watchlistUniverse.length ? " · whole venue" : "");
  // "Prices 2s ago" -- the age is the honest part. A watchlist whose clock
  // stopped showing an age has silently stopped ticking, and the number going
  // stale is how that is discovered.
  el("wlSource").textContent = watchlistTickersAt
    ? `prices ${Math.max(0, Math.round((Date.now() - watchlistTickersAt) / 1000))}s ago`
    : "";

  for (const row of rows) {
    const ticker = watchlistTickers.get(row.symbol);
    const line = document.createElement("div");
    line.className = "watchRow";

    const star = document.createElement("button");
    star.className = "star";
    star.setAttribute("aria-pressed", String(favoriteSet.has(row.symbol)));
    star.textContent = favoriteSet.has(row.symbol) ? "★" : "☆";
    star.title = favoriteSet.has(row.symbol) ? "remove from favorites" : "add to favorites";
    // Not a click on the row: charting and favoriting are different intents
    // about the same row, so one must not trigger the other.
    star.onclick = (event) => {
      event.stopPropagation();
      toggleFavorite(row.symbol);
      renderWatchlist();
    };

    const sym = document.createElement("span");
    sym.className = "wlSym";
    sym.textContent = row.symbol;
    if (row.trading === false) sym.title = "the venue is not currently accepting orders for this symbol";

    const price = document.createElement("span");
    price.className = "wlPrice";
    price.textContent = ticker ? fmtPrice(ticker.last_price) : "—";

    const change = document.createElement("span");
    if (ticker) {
      const up = ticker.price_change_percent >= 0;
      change.className = `wlChange ${up ? "up" : "down"}`;
      change.textContent = `${up ? "+" : ""}${ticker.price_change_percent.toFixed(2)}%`;
    } else {
      change.className = "wlChange";
    }

    line.onclick = () => chartFromWatchlist(row.symbol);
    line.append(star, sym, price, change);
    rowsEl.appendChild(line);
  }
}

/// Chart a watchlist row on the active pane.
///
/// The one piece that has to be careful: a searched symbol may not be in the
/// pane's series list yet (the list is built from what the platform has
/// charted). It is appended with a full, zero-coverage timeframe ladder --
/// `/candles` fetches history on demand, so coverage is not a permission --
/// and the selects are rebuilt before the fetch that reads them.
function chartFromWatchlist(symbol) {
  if (!activePane) return;
  const known = coverage.some((entry) => entry.symbol === symbol);
  if (!known) {
    coverage = [
      ...coverage,
      {
        symbol,
        timeframes: ["1m", "5m", "15m", "1h", "4h", "1d", "1w"].map((t) => ({
          timeframe: t,
          candles: 0,
          first: 0,
          last: 0,
        })),
      },
    ];
    // Every pane shares the page's instrument list; adding to it rebuilds
    // each pane's options so the new symbol is not a one-pane fiction.
    for (const pane of panes) pane.fillSeries(coverage, pane.symbol() || undefined);
    activePane.fillSeries(coverage, symbol, "15m");
  } else {
    activePane.fillSeries(coverage, symbol);
  }
  setActive(activePane);
  activePane.refresh().then(() => activePane.connectLive());
}

function startWatchlistTicker() {
  if (watchlistTickTimer) return;
  fetchWatchlistTickers();
  watchlistTickTimer = setInterval(fetchWatchlistTickers, WATCHLIST_TICK_MS);
}

function stopWatchlistTicker() {
  if (watchlistTickTimer) {
    clearInterval(watchlistTickTimer);
    watchlistTickTimer = 0;
  }
}

function wireWatchlist() {
  const input = el("wlSearch");
  let debounce = 0;
  input.addEventListener("input", () => {
    clearTimeout(debounce);
    debounce = setTimeout(() => runWatchlistSearch(input.value), 250);
  });
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      clearTimeout(debounce);
      runWatchlistSearch(input.value);
    }
    if (event.key === "Escape") {
      input.value = "";
      runWatchlistSearch("");
    }
  });
  // The visible way back to the full list. Escape already does it, but only
  // for a keyboard user who has guessed so; the default view is the venue's
  // whole listing now, and getting back to it must not be a trick.
  el("wlClear").addEventListener("click", () => {
    input.value = "";
    runWatchlistSearch("");
    input.focus();
  });
}

// ---------------------------------------------------------------------------
// Broker connections (`/brokers`) -- the user's own exchange accounts.
//
// The API is shaped so the client cannot leak a credential: `POST` sends it
// once and every response omits it, so the form is write-only by construction
// and re-displaying nothing is not a discipline the shell has to keep.
// ---------------------------------------------------------------------------

let brokerCatalog = [];

async function loadBrokerCatalog() {
  const select = el("brokerVenue");
  if (!brokerCatalog.length) {
    try {
      brokerCatalog = await api("/brokers/available");
    } catch (e) {
      el("brokerGuidance").textContent =
        `the venue list is unavailable on this deployment (${e.message})`;
      return;
    }
  }
  select.innerHTML = "";
  for (const venue of brokerCatalog) {
    select.append(
      Object.assign(document.createElement("option"), {
        value: venue.venue,
        textContent: venue.name,
      })
    );
  }
  paintBrokerGuidance();
}

function paintBrokerGuidance() {
  const venue = brokerCatalog.find((v) => v.venue === el("brokerVenue").value);
  const guidance = el("brokerGuidance");
  if (!venue) { guidance.textContent = ""; return; }
  guidance.innerHTML = "";
  const link = Object.assign(document.createElement("a"), {
    href: venue.keys_url,
    target: "_blank",
    rel: "noreferrer",
    textContent: `create a key on ${venue.name} ↗`,
  });
  guidance.append(link, document.createTextNode(` — ${venue.guidance}`));
}

async function refreshBrokers() {
  const out = el("brokerList");
  if (!token()) {
    out.innerHTML = '<p class="empty">Sign in to see your connected accounts.</p>';
    return;
  }
  out.innerHTML = '<p class="empty">Loading…</p>';
  try {
    const accounts = await api("/brokers");
    if (!accounts.length) {
      out.innerHTML = '<p class="empty">No exchange accounts connected yet.</p>';
      return;
    }
    out.innerHTML = "";
    for (const account of accounts) {
      const row = Object.assign(document.createElement("div"), { className: "brokerRow" });
      const status = Object.assign(document.createElement("span"), {
        className: `status-${account.status}`,
        textContent: account.status,
        title: account.last_error || "",
      });
      const label = Object.assign(document.createElement("span"), {
        className: "venue",
        textContent: `${account.venue} · ${account.label}`,
      });
      const spacer = Object.assign(document.createElement("span"), { className: "spacer" });
      const verify = Object.assign(document.createElement("button"), {
        textContent: "Verify",
        title: "Ask the venue to check this key now",
      });
      verify.onclick = () => verifyBroker(account.id, verify);
      const remove = Object.assign(document.createElement("button"), {
        className: "danger",
        textContent: "Disconnect",
      });
      remove.onclick = async () => {
        remove.disabled = true;
        try {
          await api(`/brokers/${account.id}`, { method: "DELETE" });
          refreshBrokers();
        } catch (e) {
          remove.disabled = false;
          alert(e.message);
        }
      };
      row.append(label, status, spacer, verify, remove);
      out.append(row);
    }
  } catch (e) {
    out.innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
  }
}

async function verifyBroker(id, button) {
  button.disabled = true;
  try {
    await api(`/brokers/${id}/verify`, { method: "POST" });
  } catch (e) {
    alert(e.message);
  }
  refreshBrokers();
}

async function connectBroker() {
  const msg = el("brokerMsg");
  const venue = el("brokerVenue").value;
  const label = el("brokerLabel").value.trim();
  const apiKey = el("brokerKey").value.trim();
  const apiSecret = el("brokerSecret").value.trim();
  if (!venue || !label || !apiKey || !apiSecret) {
    msg.textContent = "pick a venue, name the account, and paste both key and secret";
    return;
  }
  msg.textContent = "connecting…";
  try {
    await api("/brokers", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ venue, label, api_key: apiKey, api_secret: apiSecret }),
    });
    el("brokerKey").value = "";
    el("brokerSecret").value = "";
    msg.textContent = "connected — verify it to enable live trading";
    refreshBrokers();
  } catch (e) {
    msg.textContent = e.message;
  }
}

// ---------------------------------------------------------------------------
// AI model settings (`/agent/provider-config`) -- the user's own provider.
//
// The deployment's primary model is set once in its environment; a user who
// stores a config here overrides it for every /agent call they make. The key
// is write-only: GET answers *whether* one is stored, never what it is.
// ---------------------------------------------------------------------------

const AI_PROVIDERS = [
  { id: "", name: "— platform primary model —", needsKey: false },
  { id: "openai", name: "OpenAI", base: "https://api.openai.com/v1", models: ["gpt-4o", "gpt-4o-mini", "o3"] },
  { id: "anthropic", name: "Anthropic (Claude)", base: "https://api.anthropic.com/v1", models: ["claude-sonnet-4-5", "claude-opus-4-1", "claude-haiku-4-5"] },
  { id: "openrouter", name: "OpenRouter", base: "https://openrouter.ai/api/v1", models: ["openai/gpt-4o", "anthropic/claude-sonnet-4.5", "deepseek/deepseek-chat"] },
  { id: "deepseek", name: "DeepSeek", base: "https://api.deepseek.com/v1", models: ["deepseek-chat", "deepseek-reasoner"] },
  { id: "grok", name: "xAI (Grok)", base: "https://api.x.ai/v1", models: ["grok-3", "grok-3-mini"] },
  { id: "huggingface", name: "HuggingFace", base: "https://router.huggingface.co/v1", models: ["meta-llama/Llama-3.3-70B-Instruct"] },
  { id: "openai-compat", name: "Custom (OpenAI-compatible)", base: "", models: [] },
];

function paintAiProviderOptions(selected) {
  const select = el("aiProvider");
  select.innerHTML = "";
  for (const provider of AI_PROVIDERS) {
    select.append(
      Object.assign(document.createElement("option"), {
        value: provider.id,
        textContent: provider.name,
      })
    );
  }
  select.value = selected || "";
}

function paintAiModelHints() {
  const provider = AI_PROVIDERS.find((p) => p.id === el("aiProvider").value);
  const model = el("aiModelId");
  model.setAttribute("list", "aiModelList");
  let datalist = document.getElementById("aiModelList");
  if (!datalist) {
    datalist = Object.assign(document.createElement("datalist"), { id: "aiModelList" });
    document.body.append(datalist);
  }
  datalist.innerHTML = "";
  for (const id of provider && provider.models ? provider.models : []) {
    datalist.append(Object.assign(document.createElement("option"), { value: id }));
  }
  // A placeholder suggestion, never a silent override: the field keeps
  // whatever the user typed.
  if (!model.value && provider && provider.models && provider.models.length) {
    model.placeholder = `e.g. ${provider.models[0]}`;
  }
  // Bedrock is not reachable per-user (it authenticates with the deployment's
  // AWS credentials), and a custom endpoint is the one row that needs a URL.
  el("aiBaseUrlRow").hidden = !(provider && provider.id === "openai-compat");
}

async function refreshAiModel() {
  const out = el("aiCurrent");
  if (!token()) {
    out.innerHTML = '<p class="empty">Sign in to see your model settings.</p>';
    return;
  }
  try {
    const config = await api("/agent/provider-config");
    const provider = AI_PROVIDERS.find((p) => p.id === config.provider);
    // The composer's chip mirrors the stored config, so the input always
    // names what will answer it.
    try {
      localStorage.setItem("atp.modelChip", `${provider ? provider.name : config.provider} · ${config.model_id}`);
    } catch {}
    paintModelChip();
    out.innerHTML = "";
    const card = Object.assign(document.createElement("div"), { className: "aiCurrent" });
    card.innerHTML =
      `<strong>${escapeHtml(provider ? provider.name : config.provider)}</strong>` +
      ` · ${escapeHtml(config.model_id)}` +
      `<span class="muted">key stored: ${config.has_key ? "yes" : "no"}` +
      `${config.base_url ? ` · endpoint: ${escapeHtml(config.base_url)}` : ""}</span>`;
    out.append(card);
    paintAiProviderOptions(config.provider);
    el("aiModelId").value = config.model_id;
    el("aiBaseUrl").value = config.base_url || "";
  } catch (e) {
    if (e.code === "NO_PROVIDER_CONFIG") {
      out.innerHTML =
        '<p class="empty">You are using the platform\'s primary model. Store your own below to override it.</p>';
      paintAiProviderOptions("");
      el("aiModelId").value = "";
    } else {
      out.innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
    }
  }
}

/// The AI pane's prompt chips. They are preset prompts: clicking one opens the
/// composer of a chat (creating one when there is none -- a chip that answers
/// "no chats yet" with silence is a dead button) and sends its text.
async function runAiChip(promptText) {
  if (!wsActiveId) {
    const name = promptText.split(/[.\u2014]/)[0].slice(0, 40).trim() || "Chip chat";
    try {
      const symbol = activePane ? activePane.symbol() : "BTCUSDT";
      const created = await api("/indicator-workspaces", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name, symbol, timeframe: el("wsTimeframe").value.trim() || "5m" }),
      });
      await loadWorkspaces();
      // Through the same opener a manual create uses, so the name, the
      // message list and the revision state are all set, not just the id.
      if (created && created.id) await selectWorkspace(created.id);
    } catch (e) {
      el("wsChatMsg").textContent = e.message;
      return;
    }
  }
  if (!wsActiveId) return;
  const input = el("wsChatInput");
  input.value = promptText;
  sendWorkspaceMessage();
}

async function saveAiModel() {
  const msg = el("aiMsg");
  const provider = el("aiProvider").value;
  if (!provider) {
    msg.textContent = "pick a provider, or press “Use platform model” to remove yours";
    return;
  }
  const modelId = el("aiModelId").value.trim();
  const apiKey = el("aiKey").value.trim();
  if (!modelId) { msg.textContent = "which model id?"; return; }
  msg.textContent = "saving…";
  try {
    await api("/agent/provider-config", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        provider,
        model_id: modelId,
        api_key: apiKey || null,
        base_url: el("aiBaseUrl").value.trim() || null,
        extra_headers: {},
      }),
    });
    el("aiKey").value = "";
    msg.textContent = "saved — your model now answers your questions";
    refreshAiModel();
  } catch (e) {
    msg.textContent = e.message;
  }
}

async function clearAiModel() {
  const msg = el("aiMsg");
  msg.textContent = "removing…";
  try {
    await api("/agent/provider-config", { method: "DELETE" });
    msg.textContent = "the platform's primary model applies again";
    el("aiModelId").value = "";
    el("aiBaseUrl").value = "";
    try { localStorage.removeItem("atp.modelChip"); } catch {}
    paintModelChip();
    refreshAiModel();
  } catch (e) {
    if (e.code === "NO_PROVIDER_CONFIG") {
      msg.textContent = "you had no model stored";
    } else {
      msg.textContent = e.message;
    }
  }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

function selectPane(name) {
  for (const button of document.querySelectorAll(".tabs button")) {
    button.setAttribute("aria-selected", String(button.dataset.pane === name));
  }
  // Every pane lives in its own container; the watchlist pane lives in the
  // left rail and is the one pane that never hides, because the rail shows it
  // beside every tab. Hiding it would blank the rail; showing another pane
  // never touches it.
  for (const pane of document.querySelectorAll(".pane")) {
    pane.hidden = pane.id !== `pane-${name}` && pane.id !== "pane-watchlist";
  }
  // The live-trading panel is read when it is opened rather than at startup:
  // opt-in state is changed from elsewhere (the API, another tab) and a value
  // cached at page load would show a venue as revoked after it was re-enabled.
  if (name === "bots") refreshVenues();
  // Indicator workspaces are loaded when the tab is opened, because they
  // can be created or modified from other tabs.
  if (name === "indicator") loadWorkspaces();
  // Settings panels read on open for the same reason: a config changed in
  // another tab (or by the API) must not be shown stale by a cached copy.
  if (name === "brokers") { refreshBrokers(); loadBrokerCatalog(); }
  if (name === "aimodel") refreshAiModel();
  // The watchlist ticks only while it is on screen: a hidden pane's prices
  // nobody can see are five-second requests for nothing, and reopening the
  // pane re-prices it immediately anyway.
  if (name === "watchlist") {
    renderWatchlist();
    loadWatchlistUniverse();
    startWatchlistTicker();
  } else {
    stopWatchlistTicker();
  }
}

async function main() {
  paintSession();

  document.querySelectorAll(".tabs button").forEach((button) =>
    button.addEventListener("click", () => selectPane(button.dataset.pane))
  );

  wireWatchlist();
  // The watchlist has no aside tab any more (it lives in its own rail), so
  // nothing else would ever call its pane path -- the rail prices itself here,
  // once, and the rail's own interactions keep it current after that. The pane
  // also ships `hidden` in the markup (only a `selectPane` used to clear it),
  // so the rail's one pane is shown here directly.
  document.getElementById("pane-watchlist").hidden = false;
  renderWatchlist();
  loadWatchlistUniverse();
  // Workspace event listeners.
  el("wsCreate").onclick = createWorkspace;
  document.querySelectorAll("[data-ai-chip]").forEach((chip) =>
    chip.addEventListener("click", () => runAiChip(chip.dataset.aiChip))
  );
  // The model chip is a shortcut to the AI Model tab, where the choice lives.
  const modelChip = document.getElementById("aiModelChip");
  if (modelChip) modelChip.addEventListener("click", () => selectPane("aimodel"));
  el("wsDelete").onclick = deleteWorkspace;
  el("wsBack").onclick = showWorkspaceList;
  el("wsChatSend").onclick = sendWorkspaceMessage;
  // The composer is one line until it needs more: a growing box keeps the
  // conversation on screen instead of letting a long prompt push it away, and
  // the ceiling matches the CSS so the send button never leaves the frame.
  const growComposer = () => {
    const t = el("wsChatInput");
    t.style.height = "auto";
    t.style.height = Math.min(t.scrollHeight, 120) + "px";
  };
  el("wsChatInput").addEventListener("input", growComposer);
  el("wsChatInput").onkeydown = (e) => {
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); sendWorkspaceMessage(); }
  };

  el("signinToggle").addEventListener("click", () => {
    if (token()) {
      setToken("");
    } else {
      // Signed out: the home page is already in front (the gate shows it),
      // so hand the user to its card.
      el("homeEmail").focus();
    }
  });
  // The home page's door. Same authenticate; signing out from the header puts
  // the home page back in front.
  el("homeSignIn").addEventListener("click", () => homeAuth(false));
  el("homeRegister").addEventListener("click", () => homeAuth(true));
  el("homePassword").addEventListener("keydown", (e) => { if (e.key === "Enter") homeAuth(false); });

  // Broker + AI model settings.
  el("brokerVenue").addEventListener("change", paintBrokerGuidance);
  el("brokerConnect").addEventListener("click", connectBroker);
  el("aiProvider").addEventListener("change", paintAiModelHints);
  el("aiSave").addEventListener("click", saveAiModel);
  el("aiClear").addEventListener("click", clearAiModel);
  // Seed both selects once so the panes are ready before they are ever opened.
  paintAiProviderOptions("");
  paintAiModelHints();

  // The scanner. Run on the button, and on Enter in the symbols box, because a
  // field with one box beside it has to answer to Enter -- a user who types three
  // symbols and hits return should not be told to reach for the mouse.
  el("scanGo").addEventListener("click", runScan);
  el("scanSymbols").addEventListener("keydown", (e) => {
    if (e.key === "Enter") runScan();
  });
  // Changing either dropdown re-runs a scan that is already on screen rather than
  // leaving it. A stale ranking under a new heading is the failure this avoids,
  // and the alternative -- showing nothing until the button is pressed again --
  // leaves the user unsure whether the change took.
  for (const id of ["scanMetric", "scanTimeframe"]) {
    el(id).addEventListener("change", () => { if (scan) runScan(); });
  }

  // -------------------------------------------------------------------------
  // The page: a list of panes, and which one the aside is about
  // -------------------------------------------------------------------------

  // The first pane, built from the markup. Before the engine is loaded, because a
  // failed engine load is something a pane has to be able to say.
  openFirstPane();
  refreshCloseButtons();
  wireSplitters();

  // A pane is marked when it is touched, which is the page's only way of knowing
  // which chart the user means: the aside describes one chart, and neither a
  // keyboard nor a resize has a target of its own.
  //
  // Delegated on the container rather than bound per pane, so a pane added later
  // needs no wiring -- and capture phase, so the mark is set *before* the pane's
  // own handlers run. A press that selects a drawing and activates a chart is one
  // gesture, and anything reacting to it should already agree which chart it was.
  const paneOf = (node) => panes.find((pane) => pane.root === node);
  const ownerOf = (target) => {
    const node = target.closest ? target.closest(".chartPane") : null;
    return node ? paneOf(node) : undefined;
  };
  el("charts").addEventListener(
    "pointerdown",
    (event) => setActive(ownerOf(event.target)),
    true
  );
  // A change to a pane's controls is a claim on the aside too.
  el("charts").addEventListener("change", (event) => setActive(ownerOf(event.target)));
  el("charts").addEventListener("click", (event) => {
    const pane = ownerOf(event.target);
    if (pane && event.target.closest(".close")) closePane(pane);
  });

  el("split").addEventListener("click", () => {
    const pane = addPane();
    if (pane) pane.refresh().then(pane.connectLive);
  });

  // Delete removes the active pane's selected drawing; Escape cancels -- a
  // drawing in progress first, and the tool after that; Ctrl+Z / Ctrl+Y (and
  // Ctrl+Shift+Z) walk the command history. Bound to the window rather than to a
  // canvas, because a canvas is not focusable: a keyboard user would have to
  // click it first, and a click on the chart is already a selection. Routed to
  // the *active* pane for the same reason a keyboard has no way to say which
  // canvas it means -- which is why touching a pane marks it.
  window.addEventListener("keydown", (event) => {
    // Not while the user is typing. The strategy editor and the question box are
    // on the same page, and a Backspace in a textarea has to delete a character.
    const tag = document.activeElement && document.activeElement.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA") return;
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "z") {
      event.preventDefault();
      if (activePane) activePane.history(event.shiftKey ? "redo" : "undo");
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "y") {
      event.preventDefault();
      if (activePane) activePane.history("redo");
      return;
    }
    if (activePane) activePane.key(event);
  });

  // A resize is a gesture like any other: it ends in one engine rebuild and one
  // repaint *per pane*, and dragging a window edge fires dozens of them a second.
  // Rendered synchronously, that is a rebuild per event -- so it goes through the
  // same once-per-frame coalescing a wheel or a pan does. The canvas is
  // re-measured inside `draw()`, which is what makes this the resize path at all.
  window.addEventListener("resize", () => {
    for (const pane of panes) pane.redraw();
  });

  // The chart menu closes on any press it did not start. The menu itself stops
  // propagation (its controls must not close it), and a pane's canvas has its
  // own closer, so this only has to catch the rest of the page. A tool flyout
  // closes with it: an open flyout is a menu, and it stops its own trigger's
  // click but not this one.
  document.getElementById("chartMenu").addEventListener("pointerdown", (e) => e.stopPropagation());
  document.addEventListener("pointerdown", () => {
    for (const pane of panes) pane.closeMenu();
    // A tool flyout closes with any press it did not start. Its trigger stops
    // the click that opened it; it does not stop this one.
    for (const flyout of document.querySelectorAll(".toolFlyout")) {
      flyout.hidden = true;
      const trigger = flyout.parentElement && flyout.parentElement.querySelector(".toolGroupTrigger");
      if (trigger) trigger.setAttribute("aria-expanded", "false");
    }
  });

  // The watchlist rail: hide it, and bring it back. The sliver lives at the
  // screen's left edge only while the rail is gone, so a removed list is one
  // click from returning and nothing extra is on screen while it is not.
  const rail = document.getElementById("sideWatch");
  const railSplit = document.getElementById("splitWatch");
  const railShow = document.getElementById("railShow");
  try {
    if (localStorage.getItem("watchlist.hidden") === "1") {
      rail.classList.add("hideRail");
      railSplit.classList.add("hideRail");
      railShow.hidden = false;
    }
  } catch { /* private mode: the rail just starts visible */ }
  document.getElementById("railHide").addEventListener("click", () => {
    rail.classList.add("hideRail");
    railSplit.classList.add("hideRail");
    railShow.hidden = false;
    try { localStorage.setItem("watchlist.hidden", "1"); } catch { /* the drag still worked */ }
    for (const pane of panes) pane.redraw();
  });
  railShow.addEventListener("click", () => {
    rail.classList.remove("hideRail");
    railSplit.classList.remove("hideRail");
    railShow.hidden = true;
    try { localStorage.removeItem("watchlist.hidden"); } catch { /* as above */ }
    for (const pane of panes) pane.redraw();
  });
  el("ask").addEventListener("click", ask);
  el("question").addEventListener("keydown", (e) => { if (e.key === "Enter") ask(); });
  // A toggle rather than a one-shot: the follow-up question is the common case,
  // and re-pressing a button before every message is a habit users drop.
  el("attachChart").addEventListener("click", () => {
    attachChart = !attachChart;
    paintAttach();
  });
  paintAttach();

  el("example").addEventListener("change", (e) => loadExample(e.target.value));
  el("validate").addEventListener("click", validateStrategy);
  el("save").addEventListener("click", saveStrategy);
  el("deleteStrategy").addEventListener("click", deleteStrategy);
  el("generate").addEventListener("click", generateStrategy);
  // Typing in the document makes it the user's own work, whatever produced it.
  el("strategySource").addEventListener("input", () => markSourceDirty());
  document.querySelectorAll(".modes button").forEach((button) =>
    button.addEventListener("click", () => selectStrategyMode(button.dataset.mode))
  );
  el("builderForm").addEventListener("input", onBuilderInput);
  el("builderForm").addEventListener("click", onBuilderClick);
  el("backtest").addEventListener("click", runBacktest);
  el("backtestRuns").addEventListener("change", (e) => showBacktest(e.target.value));
  el("launch").addEventListener("click", launchBot);
  el("refreshBots").addEventListener("click", () => {
    refreshBots();
    refreshVenues();
  });
  el("botsOut").addEventListener("click", (e) => {
    const button = e.target.closest("button[data-bot]");
    if (button) botAction(button.dataset.bot, button.dataset.act);
  });
  // Delegated, like the bot row: the panel is re-rendered on every refresh, so
  // a listener bound to the buttons themselves would be thrown away with them.
  el("venuesOut").addEventListener("click", (e) => {
    const button = e.target.closest("button[data-venue]");
    if (button) venueAction(button.dataset.venue, button.dataset.act);
  });

  try {
    wasm = await loadEngine();
  } catch (e) {
    // The engine is the chart, so without it every pane says so and nothing is
    // fetched. The panels that do not need a chart are wired above and still work.
    for (const pane of panes) pane.setMessage(e.message);
    return;
  }
  for (const pane of panes) pane.setMessage("");
  // The toolbar is built from the engine's registry (`docs/21`), so it is
  // swapped in the moment the engine arrives -- a pane wired before this point
  // carries the markup's fallback set until now. Called after the message
  // clear, because `buildToolbarFromRegistry` is a no-op while null.
  for (const pane of panes) pane.rebuildToolbar();

  // The editor is never empty: a saved strategy if there is one, otherwise the
  // reference document. An empty box makes Validate and Save look broken when
  // they are only being sent nothing.
  await seedEditor();
  // The editor may have opened on a saved strategy, which has runs. They are
  // read on load so the panel is never blank when there is something to show.
  await refreshBacktestRuns();

  // The instrument list comes before the first fetch: a pane reads its symbol
  // from its own select, and an empty select would ask for `symbol=`.
  const entries = await loadCoverage();
  for (const pane of panes) pane.fillSeries(entries, pane.symbol() || undefined);
  // The watchlist's default rows are exactly this list; repainting it here
  // means the pane is never opened to an empty list after instruments load.
  // The venue's whole listing follows async, so the default view grows from
  // "what this page has charted" to "everything the venue trades" without
  // blocking the first paint on an index fetch.
  renderWatchlist();
  loadWatchlistUniverse();

  if (activePane.symbol()) {
    await activePane.refresh();
    activePane.connectLive();
  }
  // The DOM opens its own socket rather than riding the chart's: the two have
  // different reconnection stories, and the book has to be able to say "no
  // depth feed" without the chart looking broken.
  connectBook();
}

// ---------------------------------------------------------------------------
// Indicator Workspaces
// ---------------------------------------------------------------------------

let wsWorkspaces = []; // the user's indicator workspaces
let wsActiveId = null; // currently selected workspace id
let wsRevisions = []; // revisions for the active workspace
let wsMessages = []; // chat messages for the active workspace
let wsAlerts = []; // alert preferences for the active workspace

async function loadWorkspaces() {
  const el_ = el("wsList");
  try {
    wsWorkspaces = await api("/indicator-workspaces");
  } catch (e) {
    el_.innerHTML = `<p class="error">${e.message}</p>`;
    return;
  }
  if (!wsWorkspaces.length) {
    el_.innerHTML = `<p class="empty">No chats yet — create one above to start.</p>`;
    return;
  }
  el_.innerHTML = wsWorkspaces.map(ws => `
    <div class="ws-item" data-id="${ws.id}">
      <strong>${escapeHtml(ws.name)}</strong>
      <span class="muted">${escapeHtml(ws.symbol)} ${escapeHtml(ws.timeframe)}</span>
      ${ws.active_revision_id ? '<span class="up" title="Has an active revision">●</span>' : ''}
    </div>
  `).join("");
  el_.querySelectorAll(".ws-item").forEach(item => {
    item.onclick = () => selectWorkspace(item.dataset.id);
  });
}

// Leaving a conversation returns to the list and forgets the selection, so
// reopening the tab never lands on a chat the user did not pick.
function showWorkspaceList() {
  wsActiveId = null;
  el("wsActive").hidden = true;
  el("wsListView").hidden = false;
  loadWorkspaces();
}

async function selectWorkspace(id) {
  wsActiveId = id;
  // Re-fetch workspace list so active_revision_id is current (a new
  // revision may have been created since the last load).
  await loadWorkspaces();
  const ws = wsWorkspaces.find(w => w.id === id);
  if (!ws) return;
  el("wsListView").hidden = true;
  el("wsActive").hidden = false;
  el("wsActiveName").textContent = ws.name;
  el("wsChatMsg").textContent = "";
  await Promise.all([loadRevisions(id), loadMessages(id), loadAlerts(id)]);
  // Auto-attach the active revision to the chart if one exists.
  if (ws.active_revision_id && activePane) {
    try {
      const rev = await api(`/indicator-workspaces/${id}/revisions/${ws.active_revision_id}`);
      if (rev.preview) {
        activePane.attachIndicator(rev.preview);
      }
      // Show what was attached in the chart note strip.
      const noteEl = document.getElementById('chartNote');
      if (noteEl) noteEl.textContent = `Attached indicator revision #${rev.revision_number} (${rev.preview?.evidence?.length || 0} evidence, ${rev.preview?.zones?.length || 0} zones)`;
    } catch (e) {
      const noteEl = document.getElementById('chartNote');
      if (noteEl) noteEl.textContent = `Failed to attach indicator: ${e.message}`;
    }
  }
}

async function loadRevisions(wsId) {
  const out = el("wsRevisions");
  try {
    wsRevisions = await api(`/indicator-workspaces/${wsId}/revisions`);
  } catch (e) {
    out.innerHTML = `<p class="error">${e.message}</p>`;
    return;
  }
  if (!wsRevisions.length) {
    out.innerHTML = `<p class="empty">No revisions yet.</p>`;
    return;
  }
  const ws = wsWorkspaces.find(w => w.id === wsId);
  const activeId = ws ? ws.active_revision_id : null;
  out.innerHTML = wsRevisions.map(r => {
    const isActive = r.id === activeId;
    const evidence = r.preview && r.preview.evidence ? r.preview.evidence.length : 0;
    return `
      <div class="ws-revision" style="padding:6px 0;border-bottom:1px solid var(--line)">
        <div class="row">
          <strong>#${r.revision_number}</strong>
          <span class="muted">${escapeHtml(r.summary)}</span>
          ${isActive ? '<span class="up">active</span>' : ''}
          <span class="muted">${evidence} evidence</span>
        </div>
        <div class="muted" style="font-size:11px">${escapeHtml(r.change_summary)}</div>
        <div class="row" style="margin-top:4px">
          <button onclick="restoreRevision('${wsId}','${r.id}')" title="Set as active">Restore</button>
          <button onclick="viewRevision('${wsId}','${r.id}')" title="View source and preview">View</button>
        </div>
      </div>
    `;
  }).join("");
}

async function restoreRevision(wsId, revId) {
  try {
    await api(`/indicator-workspaces/${wsId}/revisions/${revId}/restore`, { method: "POST", headers: { "content-type": "application/json" }, body: "{}" });
    await selectWorkspace(wsId);
  } catch (e) {
    alert(e.message);
  }
}

async function viewRevision(wsId, revId) {
  try {
    const rev = await api(`/indicator-workspaces/${wsId}/revisions/${revId}`);
    // Always attach the indicator to the chart — even when no signals
    // fired the preview still carries zones, markers, and the strategy
    // document itself.
    if (rev.preview && activePane) {
      activePane.attachIndicator(rev.preview);
    }
    // Show the source in the strategy editor if available.
    const src = document.getElementById('strategySource');
    if (src && rev.source) {
      src.value = rev.source;
    }
    // Show a non-blocking status message in the chart note strip.
    const noteEl = document.getElementById('chartNote');
    if (noteEl) noteEl.textContent = `Revision #${rev.revision_number} attached (${rev.preview?.evidence?.length || 0} evidence, ${rev.preview?.zones?.length || 0} zones, ${rev.preview?.markers?.length || 0} markers)`;
  } catch (e) {
    const noteEl = document.getElementById('chartNote');
    if (noteEl) noteEl.textContent = `Failed to attach revision: ${e.message}`;
  }
}

async function loadMessages(wsId) {
  const out = el("wsChat");
  try {
    wsMessages = await api(`/indicator-workspaces/${wsId}/messages`);
  } catch (e) {
    out.innerHTML = `<p class="error">${e.message}</p>`;
    return;
  }
  if (!wsMessages.length) {
    // An empty stream, not an empty paragraph: the CSS shows the first-prompt
    // hint only while the element has no children.
    out.innerHTML = "";
    return;
  }
  out.innerHTML = wsMessages.map(m => {
    const isUser = m.role === "user";
    // Revision messages carry the generated YAML in their payload so the
    // user can read exactly what was produced instead of trusting a claim.
    const src = !isUser && m.payload && typeof m.payload.source === "string" ? m.payload.source : null;
    const srcBlock = src ? `
      <details>
        <summary>Generated source (read-only, ${src.split("\n").length} lines)</summary>
        <pre class="ws-source">${escapeHtml(src)}</pre>
      </details>
    ` : "";
    const when = m.created_at ? new Date(m.created_at / 1e6).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : "";
    return `
      <div class="msg ${isUser ? "user" : "ai"}">
        <div class="avatar" aria-hidden="true">${isUser ? "🧑" : "✦"}</div>
        <div>
          <div class="bubble">${escapeHtml(m.content)}${srcBlock}</div>
          <div class="meta">${isUser ? "You" : "AI"}${when ? ` · ${when}` : ""}</div>
        </div>
      </div>
    `;
  }).join("");
  out.scrollTop = out.scrollHeight;
}

async function sendWorkspaceMessage() {
  if (!wsActiveId) return;
  const input = el("wsChatInput");
  const content = input.value.trim();
  if (!content) return;
  input.value = "";
  input.style.height = "auto";
  const msg = el("wsChatMsg");
  msg.textContent = "Generating…";
  try {
    const resp = await api(`/indicator-workspaces/${wsActiveId}/messages`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ content }),
    });
    // Show the revision info from the response before refreshing.
    if (resp && resp.revision) {
      msg.textContent = `Revision #${resp.revision.revision_number} created — attaching to chart…`;
    }
    await selectWorkspace(wsActiveId);
    msg.textContent = "Done.";
  } catch (e) {
    msg.textContent = e.message;
  }
}

async function loadAlerts(wsId) {
  const out = el("wsAlerts");
  try {
    wsAlerts = await api(`/indicator-workspaces/${wsId}/alerts`);
  } catch (e) {
    out.innerHTML = `<p class="error">${e.message}</p>`;
    return;
  }
  if (!wsAlerts.length) {
    out.innerHTML = `<p class="muted">No alert preferences. They are created when the AI generates setups.</p>`;
    return;
  }
  out.innerHTML = wsAlerts.map(a => `
    <div class="row">
      <span>${escapeHtml(a.event_name)}</span>
      <span class="muted">${a.enabled ? 'enabled' : 'disabled'}</span>
    </div>
  `).join("");
}

async function createWorkspace() {
  const symbol = activePane ? activePane.symbol() : "BTCUSDT";
  const name = el("wsName").value.trim();
  const timeframe = el("wsTimeframe").value.trim() || "5m";
  if (!name) { alert("Name is required"); return; }
  try {
    const created = await api("/indicator-workspaces", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name, symbol, timeframe }),
    });
    el("wsName").value = "";
    await loadWorkspaces();
    // Open the chat that was just created: a beginner types a name and expects
    // to land in it, not hunt for it in the list.
    if (created && created.id) await selectWorkspace(created.id);
  } catch (e) {
    alert(e.message);
  }
}

async function deleteWorkspace() {
  if (!wsActiveId) return;
  if (!confirm("Delete this chat and all its revisions?")) return;
  try {
    await api(`/indicator-workspaces/${wsActiveId}`, { method: "DELETE" });
    showWorkspaceList();
  } catch (e) {
    alert(e.message);
  }
}

// Wire up workspace event listeners in main().

// Refresh workspace list when the indicator tab is opened.
const origSelectPane = typeof selectPane === 'function' ? selectPane : null;

main();
