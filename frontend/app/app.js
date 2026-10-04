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

const API_BASE = "";

/// Decode one WebSocket frame, from either of the two encodings the gateway
/// sends.
///
/// The gateway's market and order-book channels send their JSON payloads as
/// **binary** frames (`Message::Binary` in `ws.rs` -- throughput, not style),
/// and a browser hands a binary frame to `onmessage` as a `Blob`, not as an
/// `ArrayBuffer`. A `TextDecoder` cannot read a Blob -- `decode(blob)` throws --
/// so the decoder that ran here before dropped *every candle frame on the
/// floor*, silently, and a chart that only ever moved on a reload was the
/// result. The `catch { return }` meant nothing ever said so.
///
/// Text frames (the `subscribed` hello, the `notice` and `lagged` frames) stay
/// strings; `decodeFrame` reads both. Every `onmessage` in this file goes
/// through this one function, so there is one answer to "how does a frame
/// arrive" rather than four.
async function decodeFrame(event) {
  if (typeof event.data === "string") return JSON.parse(event.data);
  const buffer =
    event.data instanceof ArrayBuffer ? event.data : await event.data.arrayBuffer();
  return JSON.parse(new TextDecoder().decode(buffer));
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

// ---- second-instrument support (docs/23 `sec=` / request.*) ----
//
// A script that declares `sec="ETHUSDT"` reads the pair's series through
// request.open/high/low/close/volume. The fetch happens ONCE per symbol and
// is cached; alignment onto this chart's bars happens per render, walking the
// primary series so indexes never drift. The cache is best-effort: a failed
// fetch leaves the cache empty and the script's reads report missing data,
// which is the truth.
const securityCache = new Map(); // symbol -> { candles, fetchedAt }
const SECURITY_TTL_NS = 60_000_000_000; // one minute
// Diagnostics: the multi-symbol path spans fetch, cache and scene request;
// a console peek at this map answers "did the pair arrive" in one step.
window.__securityCache = securityCache;
// Set by the chart closure once `renderNow` exists there: a fetch that lands
// after the first render needs the scene rebuilt, but `renderNow` is a
// closure local -- calling it from here used to throw `ReferenceError`, and
// the throw landed in this same promise's `.catch`, which then wiped the
// cache entry it had just filled. Data arrived; the callback murdered it.
let securityDataListener = null;

/// The `sec="SYMBOL"` header value of a script, uppercased, or null.
function scriptSecSymbol(source) {
  const first = String(source || "").split("\n", 1)[0] || "";
  const m = first.match(/sec\s*=\s*"([A-Za-z0-9_\-:.]+)"/);
  return m ? m[1].toUpperCase() : null;
}

/// Every `request.security("SYM", "tf", ...)` pair a script names, as
/// `SYM@TF` keys (docs/23 Phase 11). A regex over the source is enough:
/// the vet layer has already refused non-literal symbols, so every pair is
/// a plain quoted string in the source.
function scriptPoolKeys(source) {
  const keys = [];
  const re = /request\.security\(\s*"([A-Za-z0-9_\-:.]+)"\s*,\s*"([A-Za-z0-9_\-:.]+)"/g;
  let m;
  while ((m = re.exec(String(source || ""))) !== null) {
    const key = m[1].toUpperCase() + "@" + m[2].toUpperCase();
    if (!keys.includes(key)) keys.push(key);
  }
  return keys;
}

/// Every `request.data("NAME")` name a script reads (docs/23 Phase 14):
/// platform feeds (`SYMBOL.field` ticker fields today). Plain regex — the
/// vet layer has already refused non-literal names.
function scriptDataNames(source) {
  const names = [];
  const re = /request\.data\(\s*"([A-Za-z0-9_.\-]+)"\s*\)/g;
  let m;
  while ((m = re.exec(String(source || ""))) !== null) {
    if (!names.includes(m[1])) names.push(m[1]);
  }
  return names;
}

// The /tickers snapshot cache for request.data: fetched at most once a
// minute, shared by every attached script (same TTL rule as the candles).
let tickerSnapshot = null;
let tickerSnapshotAt = 0;
async function tickerSnapshotFor() {
  const now = Date.now();
  if (tickerSnapshot && now - tickerSnapshotAt < 60_000) return tickerSnapshot;
  try {
    const resp = await api("/tickers");
    const map = {};
    for (const t of resp.tickers || []) map[t.symbol] = t;
    tickerSnapshot = map;
    tickerSnapshotAt = now;
  } catch (e) {
    console.warn("ticker snapshot for request.data failed:", e && e.message);
  }
  return tickerSnapshot;
}

/// Fill `data_series` for the names a script reads, from the ticker
/// snapshot: point-in-time values carried flat across the chart's bars
/// (the same contract the gateway preview uses). Reads the already-fetched
/// snapshot synchronously -- render cannot await; the snapshot listener
/// triggers the re-render once a cold-start fetch lands.
function dataSeriesFor(names, barCount) {
  if (!names.length || !tickerSnapshot) return null;
  const out = {};
  for (const name of names) {
    const [sym, field] = name.split(".");
    const t = tickerSnapshot[sym];
    if (!t) continue;
    const value = { change_pct: t.price_change_percent, quote_volume: t.quote_volume, high: t.high_price, low: t.low_price, last: t.last_price }[field];
    if (value == null || !Number.isFinite(value)) continue;
    out[name] = new Array(barCount).fill(value);
  }
  return out;
}

/// The second instrument's candles, aligned onto `primary`'s bars: candle i
/// covers the same window as primary[i]; bars the pair did not trade carry
/// its last close forward flat. Returns null until a fetch has succeeded.
function alignedSecurityCandles(sec, primarySymbol, timeframe, primary) {
  const cached = securityCache.get(sec);
  if (!cached || !cached.candles.length) return null;
  const out = [];
  let cursor = 0;
  const first = cached.candles[0];
  for (const bar of primary) {
    while (cursor + 1 < cached.candles.length && cached.candles[cursor + 1].open_time <= bar.open_time) cursor += 1;
    const c = cached.candles[cursor];
    // The whole cached candle, with only the timestamp re-stamped to the
    // primary bar's window: the engine deserializes these as full `Candle`
    // records (symbol, timeframe, volumes included) exactly like the chart's
    // own candles -- a reduced object used to fail deserialization, which
    // left the script's `request.*` reads with an empty series.
    if (c.open_time <= bar.open_time) {
      out.push({ ...c, open_time: bar.open_time });
    } else {
      out.push({ ...first, open_time: bar.open_time, open: first.close, high: first.close, low: first.close, close: first.close, volume: 0, buy_volume: 0, sell_volume: 0 });
    }
  }
  return out;
}

/// The pooled series for `key` (`SYM@TF`) aligned onto `primary`'s bars:
/// the pooled candle whose window CONTAINS the chart bar, completed only --
/// while that candle is still forming the script sees the previous one, so
/// a coarser-timeframe read never looks ahead. Mirrors the gateway's
/// `align_security_pooled`; the two must agree because the same script runs
/// in the preview and on the chart.
function alignedPoolSeries(key, primary) {
  const cached = securityCache.get(key);
  if (!cached || !cached.candles || !cached.candles.length) return null;
  const pooled = cached.candles;
  const [sym, tfStr] = key.split("@");
  const BAR_NS = { "1M": 60_000_000_000, "5M": 300_000_000_000, "15M": 900_000_000_000, "1H": 3_600_000_000_000, "4H": 14_400_000_000_000 };
  const tfNanos = BAR_NS[tfStr] || 60_000_000_000;
  const first = pooled[0];
  const out = [];
  let cursor = 0;
  for (const bar of primary) {
    while (cursor + 1 < pooled.length && pooled[cursor + 1].open_time <= bar.open_time) cursor += 1;
    const usable = pooled[cursor].open_time <= bar.open_time;
    const src = usable ? pooled[cursor] : first;
    const fill = usable ? src.close : first.close;
    out.push({ ...src, open_time: bar.open_time, open: usable ? src.open : fill, high: usable ? src.high : fill, low: usable ? src.low : fill, close: fill, volume: usable ? src.volume : 0, buy_volume: usable ? src.buy_volume : 0, sell_volume: usable ? src.sell_volume : 0, symbol: sym });
  }
  return out;
}

/// Refresh the security cache for `symbols` (fire and forget): one /candles
/// call per symbol not cached within the TTL. Pool keys (`SYM@TF`) ride the
/// same map, stored under the key string itself.
function refreshSecurityCandles(symbols, timeframe, barCount) {
  for (const sec of symbols) {
    if (!sec) continue;
    const cached = securityCache.get(sec);
    const now = Date.now();
    // fetchedAt=0 marks a failed attempt: retry right away instead of the
    // TTL silence a poisoned entry used to buy (60s of guaranteed-empty).
    if (cached && (cached.pending || (cached.fetchedAt && now - cached.fetchedAt < SECURITY_TTL_NS))) continue;
    const entry = cached || { candles: [], fetchedAt: 0, pending: false };
    entry.pending = true;
    entry.fetchedAt = 0;
    securityCache.set(sec, entry);
    // A `SYM@TF` pool key fetches at ITS OWN timeframe; a bare symbol uses
    // the chart's.
    const [keySym, keyTf] = sec.includes("@") ? sec.split("@") : [sec, null];
    const fetchTf = keyTf ? keyTf.toLowerCase() : timeframe;
    api(`/candles?symbol=${encodeURIComponent(keySym)}&timeframe=${encodeURIComponent(fetchTf)}&limit=${Math.min(barCount, 1500)}`)
      .then((resp) => {
        const list = resp && Array.isArray(resp.candles) ? resp.candles : [];
        if (!list.length) throw new Error(`empty candle list for ${sec}`);
        securityCache.set(sec, { candles: list, fetchedAt: Date.now(), pending: false });
        // The scene must rebuild with the data now that it exists. The
        // listener is the chart closure's `renderNow`, registered below;
        // before the chart exists there is nothing to rebuild.
        if (securityDataListener) securityDataListener();
      })
      .catch((err) => {
        // A failure leaves the slot EMPTY but unpoisoned: fetchedAt stays 0
        // so the next render retries, and the reason is on the console.
        securityCache.set(sec, { candles: [], fetchedAt: 0, pending: false });
        console.warn(`security fetch failed for ${sec}:`, err && err.message);
      });
  }
}

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
  "magnet", "aiLayer", "profileAnchor", "undo", "redo",
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
  // Whether the AI analysis layer is shown. OFF by default, and the default is
  // the honest one: an annotation the agent drew between the user's marks and
  // the candles is a claim that needs opting into, not wallpaper (`docs/21`).
  // The filter is *presentation only* — the rows stay in `drawings`, so undo,
  // deletion, and the object count are unaffected by what is being shown.
  let aiLayerOn = false;
  // Which edge the volume profile grows from. Presentation only, like the AI
  // layer: the engine positions every bar, this names the edge -- off (the
  // default) is the historical right-edge anchor, on mirrors it to the left.
  let profileLeft = false;
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
  // An older revision frozen under the active one, for the diff view. Drawn
  // faded by `drawIndicatorZones`; never re-detected (concepts stripped at
  // set time), because a diff between two *moving* layers is noise.
  let diffIndicator = null;
  // Pine-lite script layers attached to this chart (docs/23, docs/25):
  // [{ source, inputs, name, visible }]. Layers COMPOSE: each runs on every
  // frame and draws its own output over the same candles. `visible === false`
  // withholds the layer from the scene request rather than deleting it -- the
  // engine draws what it is sent, so the filter lives here, the same place
  // the AI drawing layer's does. Attached by the studio's "attach to chart"
  // action once the gateway has vetted the source.
  let attachedScripts = [];
  // Which layer's settings popover is open (its index in attachedScripts), or
  // null. Kept as the INDEX, not the layer object, so a re-attach replacing
  // the layer in place leaves the popover pointing at the same layer.
  let layerSettingsFor = null;

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
    fib_extension: "#26a69a",
    // The kinds the registry added. Distinct hues, one per kind, so a ray and
    // a trendline on the same chart are told apart by what they are; the
    // ruler is the value-area blue because it measures, like the fib.
    vline: "#d1d4dc",
    ray: "#e3b341",
    extended: "#787b86",
    measure: "#2962ff",
    // The 2026-09 parity kinds. Distinct hues again, one per kind; the
    // position boxes take the entry/stop/target vocabulary above because
    // that is what they *are* drawn from.
    channel: "#4caf8e",
    angle: "#d1d4dc",
    arc: "#e3b341",
    circle: "#9564e2",
    triangle: "#f59e0b",
    position_long: "#089981",
    position_short: "#f23645",
    dateprice_range: "#2962ff",
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

  // The static-layer cache: everything that only changes when the *scene*
  // changes, pre-rendered once and blitted each frame. A live chart re-draws
  // several times a second; the grid, zones, regions and profile are identical
  // between those frames, and redrawing 500 primitives at 60fps is the
  // difference between smooth and janky (docs/14's own advice, MDN's, and
  // every chart library's). The cache key is the scene identity the shell
  // assigns per rebuild -- anything that changes the static layers changes it.
  let staticLayer = null; // OffscreenCanvas
  let staticLayerKey = null;
  let staticSceneId = 0; // Bumped whenever `scene` is replaced.

  function draw() {
    // The freshest N bars whose script markers count as "just happened" and
    // so get the pulsing halo. 3 bars on any timeframe: a 1m signal pulses for
    // 3 minutes, a 1h one for 3 hours -- the event's own clock, not the UI's.
    const freshMarkerBars = 3;
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
    // Plain black behind the whole chart in footprint mode: the ladder's cell
    // colours are the story, and the blue-grey panel tint read as a draft
    // background over them. Other modes keep the theme colour.
    if (el("mode").value === "footprint") {
      ctx.fillStyle = "#000000";
      ctx.fillRect(0, 0, width, height);
    }
    if (!scene) return;

    // Resize or a new scene invalidates the cache. Keyed on size too, so a
    // window resize cannot blit a stale-size buffer.
    const cacheKey = `${staticSceneId}:${Math.floor(width)}x${Math.floor(height)}:${ratio}`;
    if (!staticLayer || staticLayerKey !== cacheKey) {
      staticLayer = document.createElement("canvas");
      staticLayer.width = canvas.width;
      staticLayer.height = canvas.height;
      const sctx = staticLayer.getContext("2d");
      sctx.setTransform(ratio, 0, 0, ratio, 0, 0);

      drawGrid(sctx, scene);
      // Zones go under everything, before the candles: a supply/demand band is a
      // backdrop the price is read against, not a mark on top of it.
      drawRegions(sctx, scene);
      drawIndicatorZones(sctx, scene);
      if (scene.footprint) drawFootprintGrid(sctx, scene);
      if (scene.profile.length) drawProfile(sctx, scene);
      staticLayerKey = cacheKey;
    }
    ctx.drawImage(staticLayer, 0, 0, width, height);

    // The engine says what to draw, so this is a dispatch rather than a decision.
    // Adding a chart type means adding a case here and a variant in Rust -- not
    // teaching JavaScript what a Heikin-Ashi candle is.
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
    // Overlay scripts' plots: a script with overlay=true draws into the price
    // pane, under the user's drawings and over the engine's levels -- the same
    // z-order an answer's overlays take, because a script plot is also an
    // opinion about a price the candles own.
    drawScriptOverlays(ctx, scene, freshMarkerBars);
    drawIndicatorEvidence(ctx, scene);
    // The user's own marks, above everything: a drawing that could cover the
    // answer's levels, or the price labels, would be an annotation they cannot
    // read.
    drawDrawings(ctx, scene);

    // The live price, topmost of all: it is the one mark on the chart that must
    // never be hidden, because every other layer describes the market and this
    // one *is* the market, now.
    drawLastPrice(ctx, scene);

    // Oscillator panes under the price plot -- RSI and its divergences. Every
    // coordinate comes from the engine; this fills and strokes, like everywhere
    // else in this file.
    drawSubPanes(ctx, scene);

    // Pine-lite script panes (docs/23) -- the same paint-only contract as
    // drawSubPanes: the engine ran the script, positioned every plot and
    // level, and this fills rectangles and strokes polylines. Nothing here
    // reads a price or computes a coordinate (the no-JS-math rule).
    drawScriptPanes(ctx, scene);

    // Strategy execution (docs/24 S2): the sim's fills as trade markers on
    // the price pane, the open position as a dashed box to the live bar, and
    // each strategy's equity curve in its own dedicated sub-pane. All
    // coordinates are the engine's; this only paints.
    drawStrategyLayers(ctx, scene);
    drawEquityPanes(ctx, scene);

    drawAxis(ctx, scene);
  }

  /// Sub-panes: oscillator plots below the price chart, each with its own
  /// y-scale. The engine computed the series, mapped it, placed the 30/70
  /// bands and the divergence lines; this only paints.
  function drawSubPanes(ctx, scene) {
    if (!scene.sub_panes || !scene.sub_panes.length) return;
    for (const pane of scene.sub_panes) {
      // Pane background and frame -- a slightly darker field than the price
      // plot, so the eye reads "separate measurement" at a glance.
      ctx.fillStyle = "rgba(13, 17, 26, 0.65)";
      ctx.fillRect(pane.plot.x, pane.plot.y, pane.plot.w, pane.plot.h);
      ctx.strokeStyle = COLORS.grid;
      ctx.lineWidth = 1;
      ctx.strokeRect(
        Math.round(pane.plot.x) + 0.5,
        Math.round(pane.plot.y) + 0.5,
        Math.max(1, Math.round(pane.plot.w) - 1),
        Math.max(1, Math.round(pane.plot.h) - 1)
      );

      // Reference bands (30/70): faint lines, labelled at the right edge.
      ctx.font = "9px ui-monospace, monospace";
      ctx.setLineDash([2, 3]);
      for (const level of pane.levels) {
        ctx.strokeStyle = "rgba(148, 163, 184, 0.35)";
        ctx.beginPath();
        ctx.moveTo(Math.round(pane.plot.x), Math.round(level.y) + 0.5);
        ctx.lineTo(pane.plot.x + pane.plot.w, Math.round(level.y) + 0.5);
        ctx.stroke();
        ctx.fillStyle = "rgba(148, 163, 184, 0.7)";
        ctx.fillText(String(level.value), pane.plot.x + pane.plot.w + 4, level.y + 3);
      }
      ctx.setLineDash([]);

      // The oscillator line itself.
      if (pane.line.length > 1) {
        ctx.strokeStyle = "#60a5fa";
        ctx.lineWidth = 1.25;
        ctx.beginPath();
        ctx.moveTo(pane.line[0].x, pane.line[0].y);
        for (const point of pane.line) ctx.lineTo(point.x, point.y);
        ctx.stroke();
      }

      // Divergence lines: the detector's own claim, drawn between the two RSI
      // extremes -- green for bullish, red for bearish, labelled once.
      ctx.font = "600 9px ui-sans-serif, system-ui";
      for (const div of pane.divergences) {
        const colour = div.kind === "bullish" ? "#34d399" : "#fb7185";
        ctx.strokeStyle = colour;
        ctx.lineWidth = 1.5;
        ctx.beginPath();
        ctx.moveTo(div.from_x, div.from_y);
        ctx.lineTo(div.to_x, div.to_y);
        ctx.stroke();
        // A small marker at the divergence's second extreme.
        ctx.fillStyle = colour;
        ctx.beginPath();
        ctx.arc(div.to_x, div.to_y, 2.5, 0, Math.PI * 2);
        ctx.fill();
        ctx.fillText(div.kind, div.to_x + 5, div.to_y + 3);
      }

      // Pane label, top-left: what this measurement is.
      ctx.fillStyle = "rgba(226, 232, 240, 0.85)";
      ctx.font = "600 10px ui-sans-serif, system-ui";
      ctx.fillText(pane.label, pane.plot.x + 6, pane.plot.y + 12);

      // Pane y-axis ticks, right-aligned to the shared price-axis column.
      ctx.font = "9px ui-monospace, monospace";
      ctx.fillStyle = "rgba(148, 163, 184, 0.9)";
      for (const tick of pane.ticks) {
        ctx.fillText(String(tick.value), pane.plot.x + pane.plot.w + 6, tick.y + 3);
      }
    }
  }

  /// Pine-lite script panes (docs/23): one pane per non-overlay script, with
  /// its plots, hlines and shapes already positioned by the engine. The pane's
  /// y-range came back with it, so the shell can even label the axis without
  /// arithmetic -- the engine precomputed value_min/value_max for that.
  function drawScriptPanes(ctx, scene) {
    if (!scene.script_panes || !scene.script_panes.length) return;
    for (const pane of scene.script_panes) {
      // Pane background and frame, matching drawSubPanes' look.
      ctx.fillStyle = "rgba(13, 17, 26, 0.65)";
      ctx.fillRect(pane.plot.x, pane.plot.y, pane.plot.w, pane.plot.h);
      ctx.strokeStyle = COLORS.grid;
      ctx.lineWidth = 1;
      ctx.strokeRect(
        Math.round(pane.plot.x) + 0.5,
        Math.round(pane.plot.y) + 0.5,
        Math.max(1, Math.round(pane.plot.w) - 1),
        Math.max(1, Math.round(pane.plot.h) - 1)
      );

      // hline() levels: dashed, labelled at the right edge.
      ctx.font = "9px ui-monospace, monospace";
      ctx.setLineDash([2, 3]);
      for (const level of pane.levels) {
        ctx.strokeStyle = rgbaFromPacked(level.color, 0.5);
        ctx.beginPath();
        ctx.moveTo(Math.round(pane.plot.x), Math.round(level.y) + 0.5);
        ctx.lineTo(pane.plot.x + pane.plot.w, Math.round(level.y) + 0.5);
        ctx.stroke();
      }
      ctx.setLineDash([]);

      // The plots, in the engine's own order and colours.
      for (const p of pane.plots) {
        if (p.points.length < 1) continue;
        ctx.strokeStyle = rgbaFromPacked(p.color, 1.0);
        ctx.lineWidth = p.linewidth || 1.25;
        ctx.beginPath();
        ctx.moveTo(p.points[0].x, p.points[0].y);
        for (const pt of p.points) ctx.lineTo(pt.x, pt.y);
        ctx.stroke();
      }

      // Shapes: the shared marker painter -- glow + glyph, so a pane signal
      // reads like its overlay sibling (no pulse in panes: a sub-pane signal
      // is a measurement, not a price-pane event).
      drawScriptMarkers(ctx, pane.shapes, 0);

      // Pane label, top-left, exactly like the built-in panes.
      ctx.fillStyle = "rgba(226, 232, 240, 0.85)";
      ctx.font = "600 10px ui-sans-serif, system-ui";
      ctx.fillText(pane.title, pane.plot.x + 6, pane.plot.y + 12);

      // The pane's own y-range at the right edge: top and bottom only. The
      // engine computed these from the script's own values.
      ctx.font = "9px ui-monospace, monospace";
      ctx.fillStyle = "rgba(148, 163, 184, 0.9)";
      ctx.fillText(fmtNum(pane.value_max), pane.plot.x + pane.plot.w + 6, pane.plot.y + 9);
      ctx.fillText(fmtNum(pane.value_min), pane.plot.x + pane.plot.w + 6, pane.plot.y + pane.plot.h);
    }
  }

  /// A packed-RGBA u32 (from a script's `color=`) to a CSS colour string.
  /// Pure formatting: the packing is the engine's, the alpha byte is its top
  /// one, and a missing alpha is opaque.
  function rgbaFromPacked(color, alpha) {
    const r = (color >>> 24) & 0xFF;
    const g = (color >>> 16) & 0xFF;
    const b = (color >>> 8) & 0xFF;
    const a = ((color & 0xFF) / 255) * (alpha == null ? 1.0 : alpha);
    return `rgba(${r}, ${g}, ${b}, ${a.toFixed(3)})`;
  }

  /// Strategy trades (docs/24 S2): every closed round trip paints a pair of
  /// triangle fills at the engine-positioned prices (up = long entry, down =
  /// short/exit side), an exit with a realized loss gets its pnl printed
  /// under it, and a still-open position stretches a dashed box from its
  /// entry fill to the right edge of the price plot -- "this is live" is the
  /// one thing a closed-trade glyph cannot say.
  function drawStrategyLayers(ctx, scene) {
    if (!scene.strategy_layers || !scene.strategy_layers.length) return;
    const plot = scene.plot;
    for (const layer of scene.strategy_layers) {
      for (const trade of layer.trades) {
        drawTradeFill(ctx, trade.entry, true);
        drawTradeFill(ctx, trade.exit, false);
        if (trade.pnl < 0) {
          ctx.fillStyle = "rgba(242, 54, 69, 0.9)";
          ctx.font = "9px ui-monospace, monospace";
          ctx.fillText(fmtNum(trade.pnl), trade.exit.x - 14, Math.min(plot.y + plot.h - 2, trade.exit.y + 14));
        }
      }
      if (layer.open_position) {
        const f = layer.open_position;
        const side = f.long ? COLORS.position_long : COLORS.position_short;
        const top = plot.y + 4;
        const bottom = plot.y + plot.h - 4;
        // A faint tint under the dashed border: against dense candles the
        // dash alone disappeared, the wash is what makes the live position
        // read as a region, not just an outline.
        ctx.fillStyle = rgbaFromString(side, 0.07);
        ctx.fillRect(f.x, top, Math.max(2, plot.x + plot.w - f.x), bottom - top);
        ctx.strokeStyle = side;
        ctx.lineWidth = 2;
        ctx.setLineDash([6, 4]);
        ctx.strokeRect(f.x, top, Math.max(2, plot.x + plot.w - f.x), bottom - top);
        ctx.setLineDash([]);
        // Entry marker rides the box's left edge.
        drawTradeFill(ctx, f, true);
      }
    }
  }

  /// One fill marker: a triangle pointing with the trade's side, glow under
  /// it like a script shape. `entry` only decides emphasis; direction is the
  /// fill's own `long` flag -- the engine already put the price on the pane.
  function drawTradeFill(ctx, f, entry) {
    const color = f.long ? COLORS.position_long : COLORS.position_short;
    const dir = (f.long && entry) || (!f.long && !entry) ? 1 : -1; // up for a long entry or a short exit
    const cy = f.y + (dir > 0 ? -7 : 7);
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.moveTo(f.x, cy - dir * 5);
    ctx.lineTo(f.x - 4.5, cy + dir * 3);
    ctx.lineTo(f.x + 4.5, cy + dir * 3);
    ctx.closePath();
    ctx.fill();
    if (entry) {
      ctx.strokeStyle = rgbaFromString(color, 0.55);
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.arc(f.x, cy, 8, 0, Math.PI * 2);
      ctx.stroke();
    }
  }

  function rgbaFromString(css, alpha) {
    const m = /#([0-9a-f]{6})/i.exec(css);
    if (!m) return css;
    const n = parseInt(m[1], 16);
    return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
  }

  /// Equity panes (docs/24 S2, §8.1): one dedicated sub-pane per strategy,
  /// the account curve already positioned, plus the report card -- the same
  /// headline numbers the gateway chat row carries, drawn top-left so the
  /// pane answers "did it make money" without a tooltip.
  function drawEquityPanes(ctx, scene) {
    if (!scene.equity_panes || !scene.equity_panes.length) return;
    for (const pane of scene.equity_panes) {
      ctx.fillStyle = "rgba(13, 17, 26, 0.65)";
      ctx.fillRect(pane.plot.x, pane.plot.y, pane.plot.w, pane.plot.h);
      ctx.strokeStyle = COLORS.grid;
      ctx.lineWidth = 1;
      ctx.strokeRect(
        Math.round(pane.plot.x) + 0.5,
        Math.round(pane.plot.y) + 0.5,
        Math.max(1, Math.round(pane.plot.w) - 1),
        Math.max(1, Math.round(pane.plot.h) - 1)
      );
      // The curve: one stroke, engine-positioned like every other polyline.
      if (pane.points.length > 1) {
        ctx.strokeStyle = "rgba(94, 168, 255, 0.95)";
        ctx.lineWidth = 1.25;
        ctx.beginPath();
        ctx.moveTo(pane.points[0].x, pane.points[0].y);
        for (const pt of pane.points) ctx.lineTo(pt.x, pt.y);
        ctx.stroke();
      }
      // Axis range, right edge -- same convention as drawScriptPanes.
      ctx.font = "9px ui-monospace, monospace";
      ctx.fillStyle = "rgba(148, 163, 184, 0.9)";
      ctx.fillText(fmtNum(pane.value_max), pane.plot.x + pane.plot.w + 6, pane.plot.y + 9);
      ctx.fillText(fmtNum(pane.value_min), pane.plot.x + pane.plot.w + 6, pane.plot.y + pane.plot.h);
      // The report card, top-left, painted LAST so it sits over the curve:
      // an equity curve brackets initial_capital, so it hugs the pane top
      // exactly where the title used to cross it. A backing plate dims the
      // curve behind the text instead of letting it strike through.
      const layer = (scene.strategy_layers || []).find((l) => l.id === `script:${pane.title}`);
      const titleText = `${pane.title} — equity`;
      ctx.font = "600 10px ui-sans-serif, system-ui";
      let plateW = ctx.measureText(titleText).width;
      let reportText = "";
      if (layer) {
        const r = layer.report;
        reportText = `net ${fmtNum(r.net_profit)} · ${r.total_trades} trades · win ${(r.win_rate * 100).toFixed(0)}% · dd ${(r.max_drawdown * 100).toFixed(1)}%`;
        ctx.font = "9px ui-monospace, monospace";
        plateW = Math.max(plateW, ctx.measureText(reportText).width);
      }
      const pad = 5;
      ctx.fillStyle = "rgba(10, 14, 22, 0.82)";
      ctx.fillRect(pane.plot.x + 3, pane.plot.y + 2, plateW + pad * 2, layer ? 29 : 17);
      ctx.fillStyle = "rgba(226, 232, 240, 0.85)";
      ctx.font = "600 10px ui-sans-serif, system-ui";
      ctx.fillText(titleText, pane.plot.x + 3 + pad, pane.plot.y + 13);
      if (layer) {
        const r = layer.report;
        ctx.font = "9px ui-monospace, monospace";
        ctx.fillStyle = r.net_profit >= 0 ? COLORS.position_long : COLORS.position_short;
        ctx.fillText(reportText, pane.plot.x + 3 + pad, pane.plot.y + 25);
      }
    }
  }

  // Animation clock for fresh script markers (see `drawScriptMarkers`).
  let markerPulseTimer = 0;

  /// One script marker: a soft radial glow under a clean glyph, so a signal
  /// reads at a glance against candles instead of getting lost among them.
  /// A marker on one of the freshest bars also PULSES -- an expanding fading
  /// halo -- because "this just fired" is part of what the marker means. The
  /// pulse animates via `markerPulseTimer`; everything else is static.
  function drawScriptMarkers(ctx, shapes, pulseWindow = 0) {
    const now = performance.now();
    let pulsing = false;
    for (const shape of shapes) {
      const down = /down/i.test(shape.glyph || "");
      const cx = shape.x;
      const cy = shape.y;
      // The glow: one radial gradient per marker, alpha from the marker's
      // own colour so a red signal glows red. Cheap enough per frame.
      const glowR = 11;
      const grad = ctx.createRadialGradient(cx, cy, 1.5, cx, cy, glowR);
      grad.addColorStop(0, rgbaFromPacked(shape.color, 0.5));
      grad.addColorStop(1, rgbaFromPacked(shape.color, 0.0));
      ctx.fillStyle = grad;
      ctx.beginPath();
      ctx.arc(cx, cy, glowR, 0, Math.PI * 2);
      ctx.fill();
      // Fresh marker: the expanding halo. bar is the visible-slice index the
      // engine now reports; markers with no bar (older engines) never pulse.
      if (pulseWindow > 0 && typeof shape.bar === "number" && shape.bar >= 0) {
        const fresh = shape.bar >= pulseWindow;
        if (fresh) {
          const phase = ((now / 900) + shape.bar * 0.35) % 1;
          const haloR = 6 + phase * 14;
          ctx.strokeStyle = rgbaFromPacked(shape.color, 0.75 * (1 - phase));
          ctx.lineWidth = 2;
          ctx.beginPath();
          ctx.arc(cx, cy, haloR, 0, Math.PI * 2);
          ctx.stroke();
          pulsing = true;
        }
      }
      // The glyph itself, larger than the old 7px triangle so the glow has
      // something to belong to.
      ctx.fillStyle = rgbaFromPacked(shape.color, 1.0);
      ctx.beginPath();
      if (down) {
        ctx.moveTo(cx, cy + 6);
        ctx.lineTo(cx - 5.5, cy - 3);
        ctx.lineTo(cx + 5.5, cy - 3);
      } else {
        ctx.moveTo(cx, cy - 6);
        ctx.lineTo(cx - 5.5, cy + 3);
        ctx.lineTo(cx + 5.5, cy + 3);
      }
      ctx.closePath();
      ctx.fill();
    }
    // Someone is pulsing: schedule the next frame so the halo breathes even
    // when the feed is quiet. One timer for the whole chart, self-cancelling
    // the moment no fresh markers remain.
    if (pulsing && !markerPulseTimer) {
      markerPulseTimer = requestAnimationFrame(() => {
        markerPulseTimer = 0;
        draw();
      });
    } else if (!pulsing && markerPulseTimer) {
      cancelAnimationFrame(markerPulseTimer);
      markerPulseTimer = 0;
    }
  }

  /// Overlay scripts (docs/23): polylines already mapped through the price
  /// pane's scale, painted under the user's drawings. Nothing computed here.
  function drawScriptOverlays(ctx, scene, freshMarkerBars = 0) {
    if (!scene.script_overlays || !scene.script_overlays.length) return;
    for (const overlay of scene.script_overlays) {
      // Drawing objects (line.new/label.new/box.new): under the user's own
      // drawings, over the candles -- the same z-order a script plot takes.
      for (const o of overlay.objects || []) {
        if (o.Line) {
          const l = o.Line;
          ctx.strokeStyle = rgbaFromPacked(l.color, 1.0);
          ctx.lineWidth = l.width || 1.25;
          ctx.setLineDash(l.style === "dashed" ? [6, 4] : l.style === "dotted" ? [2, 3] : []);
          ctx.beginPath();
          ctx.moveTo(l.x1, l.y1);
          ctx.lineTo(l.x2, l.y2);
          ctx.stroke();
          ctx.setLineDash([]);
        } else if (o.Box) {
          const b = o.Box;
          // Clip to the plot the way drawZoneSet does: a zone extended to
          // "now" (bar_index + a large right coordinate, or a far-future
          // timestamp from box.new_time -- docs/28) must stop at the price
          // axis, not bleed into it. Coordinates are normalised so a script
          // that passed bottom before top still draws the band it meant
          // instead of a negative-height rect.
          const plot = scene.plot;
          const x = Math.max(Math.min(b.x1, b.x2), plot.x);
          const right = Math.min(Math.max(b.x1, b.x2), plot.x + plot.w);
          const w = right - x;
          const y = Math.min(b.y1, b.y2);
          const h = Math.abs(b.y2 - b.y1);
          if (w <= 0 || h <= 0) continue;
          ctx.fillStyle = rgbaFromPacked(b.color, 0.22);
          ctx.fillRect(x, y, w, h);
          ctx.strokeStyle = rgbaFromPacked(b.color, 0.8);
          ctx.lineWidth = 1;
          ctx.strokeRect(
            Math.round(x) + 0.5,
            Math.round(y) + 0.5,
            Math.max(1, Math.round(w) - 1),
            Math.max(1, Math.round(h) - 1)
          );
        } else if (o.Label) {
          const lb = o.Label;
          ctx.font = "600 10px ui-sans-serif, system-ui";
          const w = ctx.measureText(lb.text).width + 10;
          const h = 16;
          ctx.fillStyle = rgbaFromPacked(lb.color, 0.9);
          ctx.beginPath();
          ctx.roundRect(lb.x - w / 2, lb.y - h - 4, w, h, 3);
          ctx.fill();
          ctx.fillStyle = "#0b1120";
          ctx.fillText(lb.text, lb.x - w / 2 + 5, lb.y - 8);
        }
      }
      for (const p of overlay.plots) {
        if (p.points.length < 1) continue;
        ctx.strokeStyle = rgbaFromPacked(p.color, 1.0);
        ctx.lineWidth = p.linewidth || 1.25;
        ctx.beginPath();
        ctx.moveTo(p.points[0].x, p.points[0].y);
        for (const pt of p.points) ctx.lineTo(pt.x, pt.y);
        ctx.stroke();
        // Label the plot at its right end, so two overlay scripts do not
        // blur into one line set.
        const lastPt = p.points[p.points.length - 1];
        ctx.fillStyle = rgbaFromPacked(p.color, 1.0);
        ctx.font = "600 9px ui-sans-serif, system-ui";
        ctx.fillText(p.title, lastPt.x + 5, lastPt.y + 3);
      }
      // plotshape() markers: the shared painter gives every marker a soft
      // glow and the newest bars' markers a pulsing halo -- a signal is an
      // event, and an event that just happened should read as one.
      drawScriptMarkers(ctx, overlay.shapes || []);
    }
  }

  /// Compact number for a pane's axis labels: the value the engine computed,
  /// trimmed for display only.
  function fmtNum(v) {
    if (!Number.isFinite(v)) return "";
    if (Math.abs(v) >= 1000) return String(Math.round(v));
    return String(Math.round(v * 100) / 100);
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

  /// Presentation colour helpers. Both take a colour string the shell chose
  /// and return another colour string -- there is no market data here, only
  /// paint, which keeps docs/14's no-arithmetic rule intact.
  function hexToRgba(hex, alpha) {
    const n = parseInt(hex.slice(1), 16);
    return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
  }

  function roundRectPath(ctx, x, y, w, h, r) {
    const rr = Math.min(r, w / 2, h / 2);
    ctx.beginPath();
    ctx.moveTo(x + rr, y);
    ctx.arcTo(x + w, y, x + w, y + h, rr);
    ctx.arcTo(x + w, y + h, x, y + h, rr);
    ctx.arcTo(x, y + h, x, y, rr);
    ctx.arcTo(x, y, x + w, y, rr);
    ctx.closePath();
  }

  /// A zone label as a pill: dark plate, coloured text, thin coloured rim.
  /// Bare coloured text disappears over candles; the plate is what makes the
  /// reference charts' labels readable at every zoom.
  function drawZoneLabel(ctx, label, x, yTop, h, colour) {
    const pad = 4;
    const tw = ctx.measureText(label).width;
    const boxW = tw + pad * 2;
    const boxH = 14;
    // Above the band when the band is too short to hold the pill.
    const boxY = h >= boxH + 4 ? yTop + 3 : yTop - boxH - 3;
    ctx.fillStyle = "rgba(11, 14, 20, 0.82)";
    roundRectPath(ctx, x + 2, boxY, boxW, boxH, 4);
    ctx.fill();
    ctx.strokeStyle = hexToRgba(colour, 0.55);
    ctx.lineWidth = 1;
    roundRectPath(ctx, x + 2.5, boxY + 0.5, boxW - 1, boxH - 1, 4);
    ctx.stroke();
    ctx.fillStyle = colour;
    ctx.fillText(label, x + 2 + pad, boxY + 10);
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
      // Soft vertical fade -- the edge price reacts from reads slightly
      // stronger, which is where the reference charts' depth comes from.
      const grad = ctx.createLinearGradient(0, region.y_top, 0, region.y_top + region.h);
      grad.addColorStop(0, hexToRgba(colour, region.fresh ? 0.26 : 0.10));
      grad.addColorStop(1, hexToRgba(colour, region.fresh ? 0.08 : 0.03));
      ctx.fillStyle = grad;
      ctx.fillRect(region.x, region.y_top, region.w, region.h);

      // The outline, pixel-aligned so two adjacent bands never blur. Fresh
      // bands are solid, spent ones dashed -- the same distinction as before,
      // drawn cleaner.
      ctx.strokeStyle = hexToRgba(colour, region.fresh ? 0.8 : 0.3);
      ctx.lineWidth = 1;
      ctx.setLineDash(region.fresh ? [] : [3, 3]);
      ctx.strokeRect(
        Math.round(region.x) + 0.5,
        Math.round(region.y_top) + 0.5,
        Math.max(1, Math.round(region.w) - 1),
        Math.max(1, Math.round(region.h) - 1)
      );
      ctx.setLineDash([]);

      if (region.w > 28) {
        drawZoneLabel(ctx, region.label, region.x, region.y_top, region.h, colour);
      }
    }
  }

  /// Generated indicators use a richer but still disciplined visual language:
  /// zones sit below price, their lifecycle changes the opacity/dash treatment,
  /// and the named evidence chain is drawn later above price. The engine has
  /// already placed every coordinate; this code only paints it.
  // One hue per concept label, so a four-concept detector reads as four
  // layers instead of one blur: the label is the concept's own name, which
  // is stable across every chart the indicator runs on. Deterministic
  // hashing into a curated palette -- TV-colour-grade, dark-chart friendly.
  // Shared by zones and trendlines so one concept is one colour however it
  // is drawn.
  const LABEL_HUES = ["#2dd4bf", "#f472b6", "#60a5fa", "#fbbf24", "#a78bfa", "#fb7185", "#4ade80", "#fb923c"];
  const labelHue = (label) => {
    let h = 0;
    for (let i = 0; i < label.length; i++) h = (h * 31 + label.charCodeAt(i)) | 0;
    return LABEL_HUES[Math.abs(h) % LABEL_HUES.length];
  };

  function drawIndicatorZones(ctx, scene) {
    // The diff layer first, so the active layer paints over it: the old
    // revision is a ghost the new one answers, not a peer.
    if (scene.diff_indicator && scene.diff_indicator.zones.length) {
      drawZoneSet(ctx, scene.diff_indicator.zones, 0.35, null, scene.plot);
    }
    if (scene.diff_indicator && scene.diff_indicator.trendlines && scene.diff_indicator.trendlines.length) {
      drawTrendlineSet(ctx, scene.diff_indicator.trendlines, 0.35, scene.plot);
    }
    const indicator = scene.indicator;
    if (indicator && indicator.trendlines && indicator.trendlines.length) {
      drawTrendlineSet(ctx, indicator.trendlines, 1.0, scene.plot);
    }
    if (!indicator || !indicator.zones.length) return;
    ctx.font = "600 10px ui-sans-serif, system-ui";
    drawZoneSet(ctx, indicator.zones, 1.0, scene.sub_panes, scene.plot);
  }

  /// The generated module's fitted trendlines.
  ///
  /// Points are already positioned by the engine -- the shell connects them
  /// in order, clipped to the plot. A line through a concept's confirmed
  /// swing pivots is the drawing the user asked for when they said
  /// "trendline"; a band named "trendline" would not have been.
  function drawTrendlineSet(ctx, lines, strength, plot) {
    ctx.save();
    ctx.beginPath();
    ctx.rect(plot.x, plot.y, plot.w, plot.h);
    ctx.clip();
    ctx.lineWidth = 1.5;
    for (const line of lines) {
      if (!line.points || line.points.length < 2) continue;
      const colour = labelHue(line.label || "line");
      ctx.strokeStyle = hexToRgba(colour, 0.9 * strength);
      ctx.beginPath();
      ctx.moveTo(line.points[0].x, line.points[0].y);
      for (let i = 1; i < line.points.length; i++) ctx.lineTo(line.points[i].x, line.points[i].y);
      ctx.stroke();
      // The name rides the line's last point, full strength only: a ghost
      // labelled like a live layer would read as a second active indicator.
      if (strength >= 1.0) {
        const last = line.points[line.points.length - 1];
        ctx.fillStyle = hexToRgba(colour, 0.95);
        ctx.font = "600 10px ui-sans-serif, system-ui";
        ctx.fillText(line.label || "", last.x + 4, last.y - 4);
      }
    }
    ctx.restore();
  }

  /// One indicator zone set, faded by `strength` (1.0 = full treatment).
  ///
  /// Shared by the live layer and the diff ghost so a revision comparison is
  /// the same drawing at two opacities -- a different shape for the ghost
  /// would say "different indicator" when it means "different revision".
  function drawZoneSet(ctx, zones, strength, subPanes, plotArg) {
    // Zones clip to the plot the way the reference charts do: a band that
    // ran into the price axis is the single loudest "homemade" tell.
    const plot = plotArg;
    // Label collision pass: two concepts sharing a price zone would paint
    // their pills on top of each other, which is how a layered chart turned
    // into one blur. First pill wins; the crowded zone is still fully drawn,
    // it just does not shout twice.
    const placedLabels = [];
    for (const zone of zones) {
      // The concept owns the colour; the lifecycle owns the treatment
      // (docs/25): three tiers, so a chart reading left-to-right says "fresh,
      // tested, finished" without a legend. A FRESH zone (created/active)
      // draws at full strength; a TAPPED one -- price has entered but not
      // consumed it -- halves the treatment while staying solid, the SMC
      // "first touch" look; a SPENT one (mitigated/invalidated) fades to a
      // dashed ghost, history rather than a live level.
      const colour = labelHue(zone.label);
      const spent = zone.state === "mitigated" || zone.state === "invalidated";
      const tapped = !spent && zone.state === "tapped";
      const x = Math.max(zone.x, plot.x);
      const right = Math.min(zone.x + zone.w, plot.x + plot.w);
      const w = right - x;
      if (w <= 0 || zone.h <= 0) continue;

      // Soft vertical fade instead of a flat fill -- the top edge (the one
      // price reacts from) carries a little more colour than the far edge.
      // The strength multiplier is what makes the diff ghost a ghost.
      const grad = ctx.createLinearGradient(0, zone.y_top, 0, zone.y_top + zone.h);
      if (spent) {
        grad.addColorStop(0, hexToRgba(colour, 0.06 * strength));
        grad.addColorStop(1, hexToRgba(colour, 0.02 * strength));
      } else if (tapped) {
        grad.addColorStop(0, hexToRgba(colour, 0.16 * strength));
        grad.addColorStop(1, hexToRgba(colour, 0.05 * strength));
      } else {
        grad.addColorStop(0, hexToRgba(colour, 0.28 * strength));
        grad.addColorStop(1, hexToRgba(colour, 0.10 * strength));
      }
      ctx.fillStyle = grad;
      ctx.fillRect(x, zone.y_top, w, zone.h);

      // Crisp 1px border on the pixel grid; dashed only for spent history.
      // Pixel alignment is why two adjacent bands never blur into a stripe.
      ctx.strokeStyle = hexToRgba(colour, (spent ? 0.35 : tapped ? 0.5 : 0.85) * strength);
      ctx.lineWidth = 1;
      ctx.setLineDash(spent ? [4, 3] : []);
      ctx.strokeRect(
        Math.round(x) + 0.5,
        Math.round(zone.y_top) + 0.5,
        Math.max(1, Math.round(w) - 1),
        Math.max(1, Math.round(zone.h) - 1)
      );
      ctx.setLineDash([]);

      // Only zones still in play carry their name, and only at full strength
      // -- a ghost labelled like a live layer would read as a second active
      // indicator rather than as history. The collision box is the pill's
      // would-be rectangle, in plot space; an earlier overlapping pill wins.
      if (!spent && w > 28 && strength >= 1.0) {
        const pad = 4;
        const boxW = ctx.measureText(zone.label).width + pad * 2 + 4;
        const boxY = zone.h >= 18 ? zone.y_top + 3 : zone.y_top - 17;
        const box = { x: x + 2, y: boxY, w: boxW, h: 14 };
        const overlaps = placedLabels.some((b) =>
          box.x < b.x + b.w && box.x + box.w > b.x && box.y < b.y + b.h && box.y + box.h > b.y
        );
        if (!overlaps) {
          placedLabels.push(box);
          drawZoneLabel(ctx, zone.label, x, zone.y_top, zone.h, colour);
        }
      }
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
    // Markers take their zone's concept hue (same hash), falling back to the
    // side palette when a marker has no zone -- a bare sweep marker from a
    // replay-style output.
    const hues = ["#2dd4bf", "#f472b6", "#60a5fa", "#fbbf24", "#a78bfa", "#fb7185", "#4ade80", "#fb923c"];
    const labelHue = (label) => {
      let h = 0;
      for (let i = 0; i < label.length; i++) h = (h * 31 + label.charCodeAt(i)) | 0;
      return hues[Math.abs(h) % hues.length];
    };
    const zoneHue = new Map(
      (scene.indicator ? scene.indicator.zones : []).map((zone) => [zone.id, labelHue(zone.label)])
    );
    for (const marker of indicator.markers) {
      const colour = zoneHue.get(marker.evidence_id) || palette[marker.kind] || palette.context;
      ctx.fillStyle = colour;
      // TV-style glyphs: a direction reads from the shape before its colour.
      if (marker.kind === "bullish" || marker.kind === "bearish") {
        const up = marker.kind === "bullish";
        const r = 4;
        ctx.beginPath();
        if (up) {
          ctx.moveTo(marker.x, marker.y - r);
          ctx.lineTo(marker.x + r, marker.y + r);
          ctx.lineTo(marker.x - r, marker.y + r);
        } else {
          ctx.moveTo(marker.x, marker.y + r);
          ctx.lineTo(marker.x + r, marker.y - r);
          ctx.lineTo(marker.x - r, marker.y - r);
        }
        ctx.closePath();
        ctx.fill();
      } else if (marker.kind === "signal") {
        // A filled dot with a dark ring -- the "confirmation" glyph.
        ctx.beginPath();
        ctx.arc(marker.x, marker.y, 4.5, 0, Math.PI * 2);
        ctx.fill();
        ctx.strokeStyle = "rgba(11, 14, 20, 0.9)";
        ctx.lineWidth = 1.5;
        ctx.stroke();
      } else {
        // Context: a small diamond, quieter than a dot.
        const r = 3;
        ctx.beginPath();
        ctx.moveTo(marker.x, marker.y - r);
        ctx.lineTo(marker.x + r, marker.y);
        ctx.lineTo(marker.x, marker.y + r);
        ctx.lineTo(marker.x - r, marker.y);
        ctx.closePath();
        ctx.fill();
      }
      // The dot marks the spot; the name is drawn by the zone it belongs to.
      // Labelling every dot too double-painted a dense detector layer into
      // unreadability.
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
      // Plain black field, then the frame. The blue-grey "draft" tint read as
      // noise behind the cell colours -- the user asked for a plain black
      // background, and the cells are the colour story; the column edge is
      // still drawn so the grid stays legible over black.
      ctx.fillStyle = "#000000";
      ctx.fillRect(column.x, scene.plot.y, column.w, scene.plot.h);
      ctx.strokeStyle = "#232733";
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
          // The imbalance glow: the halo is strongest for a strong, stacked
          // imbalance, and sits *under* the outline so the outline stays crisp.
          ctx.save();
          ctx.shadowColor = colour;
          ctx.shadowBlur = 6 + strength * 10;
          ctx.strokeStyle = colour;
          ctx.globalAlpha = 0.55 + strength * 0.45;
          ctx.lineWidth = 1 + Math.min(2, (cell.stacked || 1) - 1);
          ctx.strokeRect(cell.x + 1.5, cell.y + 0.5, cell.w - 3, Math.max(1, cell.h - 1));
          ctx.restore();
        }

        if (!showText) continue;
        const pair = `${cell.bid_text} x ${cell.ask_text}`;
        // A pair that does not fit is not drawn at all: a truncated number is a
        // wrong number, and a ladder of wrong numbers is worse than a ladder of
        // colours. Below ~54px a column is a heat map, which is the honest thing
        // for it to be.
        if (ctx.measureText(pair).width > cell.w - margin * 2) continue;
        // The dominant side gets the bright half of the pair and the other side
        // a dim one: the eye finds the aggressor without reading either number.
        // One measurement still -- the pair is measured as a whole.
        ctx.fillStyle = "#d1d4dc";
        ctx.fillText(pair, cell.x + cell.w / 2, cell.y + cell.h / 2);
      }

      // The candle's own totals, in its own column, under its own ladder: the
      // per-column stats table the reference footprint layouts carry. Black to
      // match the plot, with the row rules as its only interior lines.
      const summary = column.summary;
      ctx.fillStyle = "#000000";
      ctx.fillRect(summary.x + 1, summary.y, summary.w - 2, summary.h);
      ctx.strokeStyle = COLORS.grid;
      ctx.lineWidth = 1;
      ctx.strokeRect(summary.x + 0.5, summary.y + 0.5, summary.w - 1, summary.h - 1);
      if (showText) {
        ctx.textAlign = "center";
        const lineH = summary.h / 4;
        const rows = [
          [summary.volume_text, null],
          [summary.delta_text, summary.delta_positive ? COLORS.up : COLORS.down],
          [summary.cvd_text, summary.cvd_positive ? COLORS.up : COLORS.down],
          [`${summary.ask_text}/${summary.bid_text}`, null],
        ];
        // Row rules first: the band is a table, and a table without rules is
        // four numbers floating in a box -- the "information over information"
        // report. One line per boundary, inside the cell's own frame.
        ctx.strokeStyle = "#262b38";
        ctx.lineWidth = 1;
        for (let row = 1; row < 4; row++) {
          const y = Math.round(summary.y + lineH * row) + 0.5;
          ctx.beginPath();
          ctx.moveTo(summary.x + 1, y);
          ctx.lineTo(summary.x + summary.w - 1, y);
          ctx.stroke();
        }
        rows.forEach(([text, colour], row) => {
          ctx.fillStyle = colour || "#d1d4dc";
          ctx.fillText(text, summary.x + summary.w / 2, summary.y + lineH * (row + 0.5));
        });
      }
    }

    // The band's row labels, once, in the gutter the engine reserves left of
    // the plot (`SUMMARY_LABEL_GUTTER`): the reference layouts put a table's
    // labels in their own column, right-aligned against the table's edge. Only
    // when the gutter actually has room -- a pane too narrow for it keeps its
    // numbers unlabelled rather than overlapping the first ladder.
    if (grid.columns.length) {
      const first = grid.columns[0].summary;
      const lineH = first.h / 4;
      const gutterRight = first.x - 6;
      const labels = ["Volume", "Delta", "CVD", "Ask/Bid"];
      if (scene.plot.x >= 52) {
        ctx.fillStyle = "#8a90a0";
        ctx.textAlign = "right";
        for (const [row, label] of labels.entries()) {
          const width = ctx.measureText(label).width;
          if (width <= gutterRight - 2) {
            ctx.fillText(label, gutterRight, first.y + lineH * (row + 0.5));
          }
        }
        ctx.textAlign = "center";
      }
    }

    // The time axis is its own strip, not spill from the band: one rule across
    // the full width under the band closes the table, and the labels live below
    // it -- the reference layouts' "this container ends here" line.
    if (grid.columns.length) {
      const first = grid.columns[0].summary;
      const ruleY = Math.round(first.y + first.h + 0.5);
      ctx.strokeStyle = COLORS.grid;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(scene.plot.x, ruleY);
      ctx.lineTo(scene.plot.x + scene.plot.w, ruleY);
      ctx.stroke();
    }

    // The bar's own opening time under each column, the way a footprint's time
    // axis reads: one label per column, not a tick grid.
    if (showText && grid.columns.length) {
      ctx.fillStyle = "#8a90a0";
      const labelY = grid.columns[0].summary.y + grid.columns[0].summary.h + font * 0.9;
      const slot = grid.columns[0].w;
      const every = Math.max(1, Math.ceil(46 / slot));
      for (const [index, column] of grid.columns.entries()) {
        if (index % every !== 0) continue;
        ctx.fillText(formatBar(column.open_time), column.x + column.w / 2, labelY);
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
      // POC and VPIN arrive from the order-flow routes, which ride behind the
      // chart: absent until they answer, never a placeholder zero.
      ...(orderflow.poc_text ? [field("POC", orderflow.poc_text)] : []),
      ...(orderflow.vpin_text
        ? [field("VPIN", orderflow.vpin_text, orderflow.vpin_class || "")]
        : []),
    ].join("");
    node.hidden = false;
  }

  /// Order-flow intelligence panel: the numbers the footprint grid cannot hold.
  ///
  /// Populated from the new order-flow routes (`docs/22`): per-class CVD, bar
  /// delta extremes, and VPIN. Every fetch degrades to a hidden section rather
  /// than an empty box -- "unavailable" is a state, not a zero.
  function renderOrderflowPanel() {
    const node = el("orderflowPanel");
    if (!node) return;
    if (el("mode").value !== "footprint" || !footprint) {
      node.hidden = true;
      return;
    }
    const field = (label, value, className) =>
      `<span><b>${label}</b><span class="${className || ""}">${escapeHtml(value)}</span></span>`;
    // Quantities are base-asset units (BTC-sized), so integers are wrong two
    // orders of magnitude away from 1: keep two decimals in the human range.
    const qty = (v) =>
      Math.abs(v) >= 1000 ? Math.round(v).toLocaleString() : v.toFixed(2);
    const parts = [];
    if (orderflow.classes) {
      for (const cls of orderflow.classes) {
        parts.push(field(
          `${cls.class} CVD`,
          `${cls.cvd_latest >= 0 ? "+" : "−"}${qty(Math.abs(cls.cvd_latest))}`,
          cls.cvd_latest >= 0 ? "pass" : "fail"
        ));
      }
    }
    if (orderflow.bar_stats) {
      const extremes = orderflow.bar_stats
        .filter((s) => s.trades > 0)
        .slice(-1)[0];
      if (extremes) {
        parts.push(field("bar max Δ", qty(extremes.max_delta), "pass"));
        parts.push(field("bar min Δ", qty(extremes.min_delta), "fail"));
      }
    }
    if (orderflow.icebergs) {
      parts.push(field("icebergs", orderflow.icebergs, ""));
    }
    if (orderflow.flipped_levels) {
      parts.push(field(
        "level flips",
        `${orderflow.flipped_levels} this session`,
        ""
      ));
    }
    if (parts.length) {
      node.innerHTML = parts.join("");
      node.hidden = false;
    } else {
      node.hidden = true;
    }
  }

  /// Pull the order-flow intelligence for the current footprint window.
  ///
  /// All three calls share the footprint's exact `from`/`to`, so a panel and a
  /// ladder can never describe different windows. Each call fails soft: one
  /// route being unavailable must not blank the others.
  async function loadOrderflow() {
    if (!footprint || !footprint.candles.length) return;
    const symbol = el("symbol").value;
    const timeframe = el("timeframe").value;
    const first = footprint.candles[0];
    const last = footprint.candles[footprint.candles.length - 1];
    const from = Math.floor(Number(first.open_time) / 1e6);
    const to = Math.floor((Number(last.open_time) + BAR_MS[timeframe]) / 1e6);
    const qs = `symbol=${symbol}&timeframe=${timeframe}&from=${from}&to=${to}`;

    const [size, stats, memory] = await Promise.allSettled([
      api(`/delta-by-size?${qs}`),
      api(`/bar-delta-stats?${qs}`),
      api(`/profile-memory?${qs}`),
    ]);

    orderflow = {};
    // Per-class CVD accumulates, so the newest candle's classes carry the
    // window's per-class CVD -- there is no separate top-level list.
    if (size.status === "fulfilled" && size.value.candles?.length) {
      const newest = size.value.candles[size.value.candles.length - 1];
      orderflow.classes = newest.classes.map((c) => ({
        class: c.class,
        cvd_latest: Number(c.cvd || 0),
      }));
    }
    if (stats.status === "fulfilled" && stats.value.candles) {
      orderflow.bar_stats = stats.value.candles;
    }
    if (memory.status === "fulfilled" && memory.value.sessions?.length) {
      const session = memory.value.sessions[memory.value.sessions.length - 1];
      orderflow.flipped_levels = session.levels.filter((l) => l.flipped_control).length;
      orderflow.poc_text = (() => {
        let best = null;
        for (const level of session.levels) {
          if (!best || level.volume > best.volume) best = level;
        }
        return best ? best.price_level.toPrecision(6) : null;
      })();
    }

    // Iceberg candidates and VPIN ride their own windows; both fail soft.
    try {
      const icebergs = await api(`/icebergs?symbol=${symbol}`);
      if (icebergs.icebergs?.length) {
        const best = icebergs.icebergs[0];
        orderflow.icebergs = `${icebergs.icebergs.length} (best ${best.ratio.toFixed(1)}x @ ${best.price.toPrecision(6)})`;
      } else if (icebergs.snapshots > 0) {
        orderflow.icebergs = "none in window";
      }
    } catch {
      // no depth history yet: the field stays absent
    }
    try {
      const vpin = await api(`/vpin?${qs}`);
      if (vpin.latest) {
        orderflow.vpin_text = vpin.latest.vpin.toFixed(2);
        orderflow.vpin_class = vpin.latest.vpin >= 0.6 ? "fail" : vpin.latest.vpin >= 0.3 ? "" : "pass";
      }
    } catch {
      // VPIN stays absent from the strip rather than showing a stale one.
    }

    renderOrderflowPanel();
    // Re-render the strip only from a *scene* grid: the raw footprint response
    // lacks the rendered text fields the strip formats.
    if (el("mode").value === "footprint" && lastGrid) renderFootprintStats(lastGrid);
  }

  /// The footprint stats strip wants the *grid's* rendered stats, which live on
  /// the wasm scene rather than the raw response; this returns the last grid.
  function gridFromScene() {
    return lastGrid || footprint;
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

          case "ellipse":
            // The engine's circle and arc arrive here: a full ellipse is a
            // circle, `half` clips to the upper arc. Centred with
            // `ctx.arc`-style parameters so the maths stays in Rust.
            ctx.beginPath();
            ctx.ellipse(part.cx, part.cy, part.rx, part.ry, 0, 0, Math.PI * 2);
            if (part.filled) {
              ctx.save();
              ctx.clip();
              if (part.half) {
                // Only the upper arc is stroked: clip to the centre line.
                ctx.clearRect(part.cx - part.rx - 1, part.cy, part.rx * 2 + 2, part.ry + 1);
              }
              ctx.restore();
            }
            ctx.stroke();
            break;

          case "polygon":
            ctx.beginPath();
            ctx.moveTo(part.points[0][0], part.points[0][1]);
            for (let i = 1; i < part.points.length; i += 1) {
              ctx.lineTo(part.points[i][0], part.points[i][1]);
            }
            ctx.closePath();
            if (part.filled) {
              ctx.globalAlpha = drawing.selected ? 0.18 : 0.1;
              ctx.fill();
              ctx.globalAlpha = 1;
            }
            ctx.stroke();
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
  // Order-flow intelligence for the current window (`docs/22`): size classes,
  // bar delta extremes, profile-memory flips, VPIN. Filled by `loadOrderflow`.
  let orderflow = {};
  // The last scene-built grid, so the stats strip prefers rendered values.
  let lastGrid = null;
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
  // How often the footprint poll refetches, in milliseconds. The feed's own
  // redraw cadence is ~1s (the collector's forming-bar snapshots); the ladder
  // changes meaningfully on trades, not on every frame, so 2s keeps the forming
  // candle visibly developing without hammering a route that aggregates 10k
  // trades per answer.
  const FOOTPRINT_POLL_MS = 2000;

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
    // The AI layer's badge count is per-symbol, so the toggle's title is
    // re-derived after every load (`docs/21` phase 3).
    refreshAiLayerButton();

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
        // The intelligence panel rides behind the ladder: same window, best
        // effort, never blocking the chart itself. The poll keeps the ladder
        // live while the user follows the edge -- it is the same fetch on a
        // timer, and it starts here so a pane opened straight into footprint
        // mode is live too.
        loadOrderflow();
        startFootprintPoll();
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
      // The window the fetch asked for is gone, so what is on screen must go
      // with it: repainting empty says "this series is not here" rather than
      // leaving the previous symbol's bars under the new one's title -- which
      // is the failed symbol switch the user reads as "the chart broke".
      candles = [];
      lastPrice = null;
      footprint = null;
      render();
      message.textContent = e.message;
    }
  }

  /// Live footprint polling: refetch the ladder while the user follows the edge.
  ///
  /// Candle mode is pushed by the websocket, but the ladder comes from the
  /// `/footprint` route, which nothing pushes -- so the forming bar never
  /// appeared and the chart read as frozen while the price badge moved. A poll
  /// on the feed's own cadence is the honest fix: the route answers from the
  /// same tape, so each poll re-renders the *developing* last candle plus any
  /// that closed since the last one.
  ///
  /// Only while the user is following the live edge (`followLive`): a panned-back
  /// window is history, and polling would fight the pan by sliding the window
  /// under the reader. A pan stops the poll; Fit resumes it -- the same rule the
  /// candles' own follow logic already uses.
  let footprintPoll = 0;
  let footprintBusy = false;
  // When the ladder last refetched, for the frame-driven rate cap. The fetch is
  // triggered *from the websocket handler* -- network events are never timer-
  // throttled, while setInterval in a long-lived tab is (a preview panel saw a
  // 2s interval fire about once a minute). The cap keeps ~1s frames from
  // hammering a route that aggregates ~10k trades per answer.
  let footprintLastFetch = 0;

  /// Called from the websocket handler on every market frame while footprint
  /// mode is live: the frame summarizes the same trades the ladder aggregates,
  /// so the frame is the ladder's "data changed" signal. Rate-capped; the busy
  /// flag prevents overlap; a panned-back window is left alone.
  function markFootprintDirty() {
    if (el("mode").value !== "footprint" || !followLive) return;
    const now = Date.now();
    if (now - footprintLastFetch < FOOTPRINT_POLL_MS || footprintBusy) return;
    footprintLastFetch = now;
    refreshFootprintOnce();
  }

  async function refreshFootprintOnce() {
    if (footprintBusy) return;
    footprintBusy = true;
    try {
      const data = await loadFootprint();
      footprint = data;
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
      loadOrderflow();
    } catch {
      // A failed refresh keeps the last good ladder on screen; the next tick
      // retries. The chart degrades to "a moment ago", never to blank.
    } finally {
      footprintBusy = false;
    }
  }

  function startFootprintPoll() {
    stopFootprintPoll();
    footprintPoll = setInterval(async () => {
      if (footprintBusy || !followLive || document.hidden) return;
      if (el("mode").value !== "footprint") return;
      // Backup path for a feed that has stopped pushing frames: refetch on the
      // timer as well, rate-capped the same way. A live feed reaches this line
      // having just fetched, so this is normally a no-op.
      const now = Date.now();
      if (now - footprintLastFetch < FOOTPRINT_POLL_MS) return;
      footprintLastFetch = now;
      await refreshFootprintOnce();
    }, FOOTPRINT_POLL_MS);
  }

  function stopFootprintPoll() {
    if (footprintPoll) {
      clearInterval(footprintPoll);
      footprintPoll = 0;
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
      // The layer filter (`docs/21` phase 3): with the AI layer off, rows the
      // agent created are withheld from the *request*, which is the only place
      // a filter can live — the engine draws what it is sent, and the shell is
      // what decides what was sent. `placing` is never filtered: it is the
      // shape under the user's own pointer right now.
      drawings: (placing ? [...drawings, placing] : drawings)
        .filter((drawing) => aiLayerOn || drawing.created_by !== "ai")
        .map((drawing) => ({
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
      // The profile's edge, in the engine's own spelling. Assigned only when
      // set: absent means Right, which keeps every existing request byte-identical.
      ...(profileLeft ? { profile_anchor: "left" } : {}),
      indicator,
      // The frozen older revision, drawn faded behind the live layer. Sent as
      // its own request field so the engine positions it with the same frame
      // everything else uses -- a second indicator field would double-paint.
      diff_indicator: diffIndicator,
    };
    // The live layer. An attached indicator that carries its document's
    // concepts is sent as a **definition**, not a snapshot: the engine
    // re-detects it on this request's candles every frame -- any symbol, any
    // timeframe, the forming bar included -- so the drawing is always current
    // and survives series changes. The static `indicator` snapshot is then
    // withheld: drawing both would double-paint the same detections, which is
    // half of how a dense layer became unreadable.
    if (indicator && Array.isArray(indicator.concepts) && indicator.concepts.length) {
      request.live_indicator = {
        name: indicator.name || "generated indicator",
        concepts: indicator.concepts,
      };
      request.indicator = null;
    }
    // Assigned rather than sent as `null`: a null is not a missing field, and the
    // engine's `Viewport` is a struct rather than an option, so `viewport: null`
    // would be a deserialization error rather than a default. Absent means
    // "everything, fitted", which is where a chart starts.
    if (viewport) request.viewport = viewport;
    if (gesture) request.gesture = gesture;

    // A generated indicator whose document mentions divergence gets an RSI
    // sub-pane: the detector reports RSI/price divergences, and those belong
    // under the chart with their own scale -- the same reason TradingView puts
    // oscillators in lower panes. The engine computes and positions everything;
    // this only names the measurement it wants.
    const liveNames = (indicator && Array.isArray(indicator.concepts) ? indicator.concepts : [])
      .map((c) => String(c.name || ""))
      .join(" ");
    if (/divergence|rsi/i.test(liveNames)) {
      request.sub_panes = [{ kind: "rsi", period: 14, overbought: 70, oversold: 30 }];
    }

    // Pine-lite scripts (docs/23): sent as source + inputs, run by the engine
    // over the visible candles every frame. A script whose header declares a
    // second instrument (`sec="...") also carries that pair's candles,
    // time-aligned onto this chart's own bars from the cached fetch -- the
    // alignment walks the primary series so indexes never drift. An empty
    // list keeps the field out of the JSON -- an older engine ignores it.
    // Layers the eye turned off are withheld here (docs/25): the engine draws
    // what it is sent, so a hidden layer is simply not sent -- the layer list
    // keeps the source, and showing it again is a request, not a re-attach.
    const visibleScripts = attachedScripts.filter((s) => s.visible !== false);
    if (visibleScripts.length) {
      // Kick the cache for every declared second instrument, then read it --
      // the first render after a fresh attach draws without the pair and the
      // cache's own renderNow() rebuilds the scene once the data lands.
      // `el("symbol").value` / `el("timeframe").value`, not bare identifiers:
      // this function has no locals of those names (the ones in `loadCandles`
      // are a different function's) -- a bare `timeframe` here was a
      // `ReferenceError` that killed every render once a script attached.
      // Pool keys (`SYM@TF`) ride the same cache: Phase 11 scripts name
      // their pairs per call, and the fetch fires at the key's own tf.
      const poolKeys = visibleScripts.flatMap((s) => scriptPoolKeys(s.source));
      refreshSecurityCandles(
        visibleScripts
          .map((s) => scriptSecSymbol(s.source))
          .concat(poolKeys),
        el("timeframe").value,
        candles.length
      );
      // Phase 14: request.data series — the snapshot fetch is async (render
      // is sync), so the first render draws without the feed values and the
      // data listener rebuilds the scene when the snapshot lands.
      const dataNames = visibleScripts.flatMap((s) => scriptDataNames(s.source));
      request.scripts = visibleScripts.map((s) => {
        const spec = { source: s.source, inputs: s.inputs || {} };
        const sec = scriptSecSymbol(s.source);
        if (sec) {
          const aligned = alignedSecurityCandles(
            sec,
            el("symbol").value,
            el("timeframe").value,
            candles
          );
          if (aligned) spec.security = aligned;
        }
        // Phase 11: every named pair, aligned onto this chart's bars. A key
        // whose fetch has not landed yet is simply absent -- the engine's
        // read reports the missing key, and the cache's listener rebuilds
        // the scene when the data arrives.
        const keys = scriptPoolKeys(s.source);
        if (keys.length) {
          const pool = {};
          for (const key of keys) {
            const aligned = alignedPoolSeries(key, candles);
            if (aligned) pool[key] = aligned;
          }
          if (Object.keys(pool).length) spec.series_pool = pool;
        }
        // Phase 14: filled synchronously from the LAST ticker snapshot (a
        // cold start renders without feed values until the snapshot lands,
        // then the listener below re-renders once it has).
        const names = scriptDataNames(s.source);
        if (names.length && tickerSnapshot) {
          const ds = dataSeriesFor(names, candles.length);
          if (ds && Object.keys(ds).length) spec.data_series = ds;
        }
        return spec;
      });
      if (dataNames.length) {
        tickerSnapshotFor().then((snap) => {
          if (snap && typeof securityDataListener === "function") securityDataListener();
        });
      }
    }

    scene = buildScene(request);
    // New scene ⇒ new static-layer identity: without this the next draw would
    // find a matching cacheKey and blit the previous scene's grid, zones and
    // profile over the new candles.
    staticSceneId += 1;
    viewport = scene.viewport;
    // The engine's own answer to "how wide must a cell be for the numbers that
    // are in this window", kept for the next window the user asks for. The count
    // decides the window, the window decides the numbers, and the numbers decide
    // the count -- so sizing the next load from what this one actually needed
    // reaches the fixed point in one step instead of being guessed at every time.
    if (scene.footprint && scene.footprint.min_cell_px > 0) {
      footCellPx = Math.ceil(scene.footprint.min_cell_px);
    }
    // The note strip has three authors, in priority order: the feed's own
    // notice, the engine's note about the last build, and -- provenance
    // (`docs/21` phase 3) -- why the *selected* AI drawing is there. Derived
    // here rather than written by `select`, because render owns this strip and
    // a note written before the frame it belongs to is wiped by it: that was
    // the save-failure bug one layer down, back again.
    const selectedAi = drawings.find(
      (d) => d.id === selectedDrawing && d.created_by === "ai" && d.reason
    );
    const aiReason = selectedAi
      ? `AI: ${selectedAi.reason}${
          selectedAi.confidence != null
            ? ` (confidence ${Math.round(selectedAi.confidence * 100)}%)`
            : ""
        }`
      : "";
    el("chartNote").textContent = feedNotice || scene.note || aiReason;
    // The order-flow panel re-renders the strip with the *scene* grid, so the
    // strip keeps the engine's formatted numbers instead of falling back to the
    // raw response, which has no rendered text fields.
    lastGrid = scene.footprint;
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
  /// Hit-test the generated markers under the pointer and fill the evidence
  /// tooltip. Nearest marker inside the radius wins -- markers can overlap at
  /// dense zoom-outs, and the closest one is the one the pointer is on.
  function updateEvidenceTip(event) {
    const tip = document.getElementById("evidenceTip");
    if (!tip) return;
    const indicator = scene && scene.indicator;
    const markers = indicator && Array.isArray(indicator.markers) ? indicator.markers : [];
    const zones = indicator && Array.isArray(indicator.zones) ? indicator.zones : [];
    const rect = el("chart").getBoundingClientRect();
    const px = event.clientX - rect.left;
    const py = event.clientY - rect.top;
    const radius = 9;
    let best = null;
    let bestDist = Infinity;
    for (const marker of markers) {
      if (typeof marker.x !== "number" || typeof marker.y !== "number") continue;
      const d = Math.hypot(marker.x - px, marker.y - py);
      if (d <= radius && d < bestDist) {
        best = marker;
        bestDist = d;
      }
    }
    // Zone inspector (docs/26): when no marker claims the pointer, the zone
    // under it answers "what is this band and why is it here". Zones paint in
    // array order, so the LAST containing zone is the topmost one. Only zones
    // inside the plot count -- a zone clipped to the axis edge must not light
    // up from the price axis.
    let zone = null;
    if (!best && scene && scene.plot) {
      const inPlot =
        px >= scene.plot.x && px <= scene.plot.x + scene.plot.w &&
        py >= scene.plot.y && py <= scene.plot.y + scene.plot.h;
      if (inPlot) {
        for (let i = zones.length - 1; i >= 0; i--) {
          const z = zones[i];
          const x1 = Math.max(z.x, scene.plot.x);
          const x2 = Math.min(z.x + z.w, scene.plot.x + scene.plot.w);
          if (px >= x1 && px <= x2 && py >= z.y_top && py <= z.y_top + z.h) {
            zone = z;
            break;
          }
        }
      }
    }
    if (!best && !zone) {
      tip.hidden = true;
      return;
    }
    tip.hidden = false;
    if (best && (best.explanation || best.label)) {
      tip.innerHTML =
        `<div class="tipKind">${escapeHtml(best.label || best.kind || "evidence")}</div>` +
        (best.explanation ? `<div>${escapeHtml(best.explanation)}</div>` : "");
    } else if (zone) {
      // The lifecycle word is the same tier the paint uses, so the tooltip
      // never describes a zone differently from how it is drawn.
      const stateWord =
        zone.state === "mitigated" ? "mitigated — fully traded through" :
        zone.state === "invalidated" ? "invalidated" :
        zone.state === "tapped" ? "tapped — price entered, not consumed" :
        "active — untouched";
      tip.innerHTML =
        `<div class="tipKind">${escapeHtml(zone.label || "zone")} <span class="tipState">${escapeHtml(stateWord)}</span></div>` +
        (typeof zone.price_low === "number" && typeof zone.price_high === "number"
          ? `<div class="tipBand">${fmtPrice(zone.price_low)} – ${fmtPrice(zone.price_high)}</div>`
          : "") +
        (zone.explanation ? `<div>${escapeHtml(zone.explanation)}</div>` : "");
    } else {
      tip.hidden = true;
      return;
    }
    // Follow the pointer, clamped to the chart area so it never covers the
    // axis or runs off the window edge.
    const wrap = el("chartWrap").getBoundingClientRect();
    const left = Math.min(px + 14, wrap.width - 330);
    const top = Math.min(py + 14, wrap.height - 70);
    tip.style.left = `${Math.max(0, left)}px`;
    tip.style.top = `${Math.max(0, top)}px`;
  }

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
    if (event.shiftKey) {
      applyGesture({ kind: "zoom_price", factor, anchor: at.y });
      return;
    }
    // A sideways wheel -- a trackpad's two-finger slide, or a tilt wheel --
    // pans instead of zooming, the way every charting package reads it. The
    // delta is a fraction of the plot, the same unit the drag-pan below uses,
    // so a two-finger slide and a drag feel like the same gesture; the sign is
    // the drag's (fingers right reveal older bars), and any horizontal pan
    // takes the window back from the live edge exactly as a drag does.
    const sideways = event.deltaX * (event.deltaMode === 1 ? 16 : 1);
    if (Math.abs(sideways) > Math.abs(pixels)) {
      const time = -(sideways / scene.plot.w) * 4;
      if (sideways !== 0) {
        followLive = false;
        stopFootprintPoll();
      }
      applyGesture({ kind: "pan", time, price: 0 });
      return;
    }
    applyGesture({ kind: "zoom_time", factor, anchor: at.x });
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
        // it asks; a body drag moves the whole shape, which means it needs the
        // starting point in a unit it can add to.
        base: hit.anchor === null ? fractionsOf(hit.drawing) : null,
        movedFrom: grabbed
          ? {
              a1: { ...grabbed.a1 },
              a2: grabbed.a2 ? { ...grabbed.a2 } : null,
              a3: grabbed.a3 ? { ...grabbed.a3 } : null,
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
    if (!scene) return;
    // Evidence tooltips: while no gesture is in flight, the pointer is tested
    // against the generated markers' positions -- every marker already knows
    // *why* it fired (`explanation`), and surfacing that on hover is the
    // evidence chain made legible without touching the data model. Hit radius
    // is generous because the glyphs are 3-5px.
    if (!drag) updateEvidenceTip(event);
    // A click-click-click placement is alive *between* clicks too: with no
    // button held there is no `drag`, but the pending anchor still has to
    // follow the pointer or the user draws blind -- the shape would sit frozen
    // at the last click until the next one. Hover tracking is the difference
    // between a placement the user can aim and one they cannot.
    if (!drag && placing && placing.pending) {
      const at = plotFraction(event);
      if (!at) return;
      const anchor = { unit: "fraction", x: at.x, y: at.y };
      if (placing.pending === 2) placing.a2 = anchor;
      else if (placing.pending === 3) placing.a3 = anchor;
      scheduleRender();
      return;
    }
    if (!drag) return;

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
      if (dx !== 0) {
        followLive = false;
        // A panned-back window is history: the poll would slide the window
        // under the reader, so it stops until Fit resumes following.
        stopFootprintPoll();
      }
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
      // Mid-placement the *pending* anchor follows the pointer and the ones
      // already fixed stay where they were put. Which anchor is pending is a
      // field on the placement (see `startPlacing`), not something inferred.
      if (placing && placing.pending === 2) {
        placing.a2 = { unit: "fraction", x: at.x, y: at.y };
      } else if (placing && placing.pending === 3) {
        placing.a3 = { unit: "fraction", x: at.x, y: at.y };
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

    // The body: the whole drawing moves, so every anchor takes the *same* delta.
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
    // The third anchor moves with the shape or a channel keeps its line while
    // its width runs away -- which reads as the channel having moved.
    if (drag.base.a3) moving.a3 = shifted(drag.base.a3, dx, dy);
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

    if (finished.mode === "place") {
      // A click-click-click tool is only *finished* by its last click. Every
      // earlier release just fixes the current anchor and leaves `placing`
      // armed -- the pointer keeps drawing the next anchor, and the placement
      // survives the capture being released because the next click re-captures.
      const needed = anchorsNeeded();
      if (placing && needed > 2 && placing.clicks < needed) return;
      finishPlacing();
    } else if (finished.mode === "move") {
      finishMoving(finished.target.drawing, finished.movedFrom);
    }
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
  // A second-instrument fetch that lands after this closure exists needs the
  // scene rebuilt -- see the listener variable up at the security cache.
  if (typeof securityDataListener !== "undefined") {
    securityDataListener = renderNow;
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

  /// Drop everything this pane holds *about* a series, because the series is
  /// changing.
  ///
  /// The window (`resetViewport`) is one part. The other two are the candles
  /// themselves and the attached indicator: a refetch that fails -- an unknown
  /// symbol, a gateway that cannot reach the venue -- used to leave the old
  /// series on screen under a new symbol's title, and the old chart's
  /// indicator drawn over it. What the user saw after a failed switch was the
  /// previous chart lying about what it was showing. Both go before the fetch:
  /// a chart that briefly shows nothing is honest, and the fetch repaints it
  /// the moment it has something true to show.
  function resetSeries() {
    resetViewport();
    candles = [];
    // A live indicator (one carrying its concepts) survives the reset: its
    // definition is not tied to a series, and the next render re-detects it on
    // the new one. A snapshot-only preview is dropped -- its coordinates
    // describe the old series only.
    if (!(indicator && Array.isArray(indicator.concepts) && indicator.concepts.length)) {
      indicator = null;
    }
    lastPrice = null;
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
    // The global row's undo/redo mirror this pane's stacks. Guarded: it is
    // built only once the engine has loaded, and only when this pane is the
    // one the global buttons act on does its state belong on them.
    if (typeof refreshGlobalTools === "function" && globalTools.built && activePane === paneApi) {
      refreshGlobalTools();
    }
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
        // A polygon's body is its inside, not a line: a triangle grabbed by
        // its middle must move, the same rule a rectangle follows.
        if (part.shape === "polygon" && insidePolygon(at, part.points)) {
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

  /// Whether a point is inside a polygon, by the ray-crossing test.
  ///
  /// The same rule `insideRect` applies to a rectangle, generalised: cast a
  /// ray from the point and count the edges it crosses -- odd is inside. Only
  /// used for grabbing a drawn triangle by its body, so it runs on a
  /// three-vertex list once per pointerdown, never in the render loop.
  function insidePolygon(point, points) {
    if (!points || points.length < 3) return false;
    let inside = false;
    for (let i = 0, j = points.length - 1; i < points.length; j = i += 1) {
      const [xi, yi] = points[i];
      const [xj, yj] = points[j];
      const crosses = yi > point.y !== yj > point.y;
      if (crosses) {
        const xAtY = ((xj - xi) * (point.y - yi)) / (yj - yi) + xi;
        if (point.x < xAtY) inside = !inside;
      }
    }
    return inside;
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
  /// How many anchors the armed tool places: 1 a click, 2 a drag, 3 a
  /// click-click-click (channel, arc, triangle). Read from the engine's own
  /// registry, with the same unknown-tool fallback `needs_second_anchor` has:
  /// demanding more rather than less of an unrecognised kind is the safe way
  /// to be wrong, and an unknown kind fails the click through to a drag.
  function anchorsNeeded() {
    if (tool === "hline" || tool === "vline") return 1;
    if (tool === "channel" || tool === "arc" || tool === "triangle") return 3;
    if (tool === "cursor") return 0;
    return 2;
  }

  function startPlacing(event) {
    const at = plotFraction(event);
    if (!at) return;
    const needed = anchorsNeeded();
    // A click-click-click tool in flight: this pointerdown is its next click,
    // which *fixes* the pending anchor and opens the following one. The first
    // click must have armed `placing` already, so falling through here without
    // it would be a second gesture on a tool that has none.
    if (placing && placing.clicks < needed) {
      placing.clicks += 1;
      // The click that fixes a2 is the one that opens a3. The last click
      // opens nothing -- its release finishes the shape.
      placing.pending = placing.clicks < needed ? placing.clicks + 1 : 0;
      drag = { mode: "place", appliedX: event.clientX, appliedY: event.clientY };
      el("chart").setPointerCapture(event.pointerId);
      scheduleRender();
      return;
    }
    placing = {
      id: `new-${(drawingCounter += 1)}`,
      kind: tool,
      a1: { unit: "fraction", x: at.x, y: at.y },
      // Two separate objects even when they start in the same place: the next
      // anchor follows the pointer and the placed ones do not, and one shared
      // object would move both.
      //
      // `clicks` counts the anchors the user has *fixed*, and `pending` is the
      // anchor the pointer is drawing: 2 after the first click of any
      // multi-anchor tool, 3 once a three-anchor tool's second click has fixed
      // a2, and 0 when nothing is pending. Explicit rather than inferred from
      // null-vs-undefined because the engine refuses an `a3` on a kind that
      // does not take one -- a mis-encoded 2-anchor drag would store its
      // second anchor into `a3` and the shape would vanish with a note.
      clicks: 1,
      pending: needed >= 2 ? 2 : 0,
      a2: needed >= 2 ? { unit: "fraction", x: at.x, y: at.y } : null,
      a3: null,
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
    return {
      a1: drawing.a1_fraction,
      a2: drawing.a2_fraction ?? null,
      a3: drawing.a3_fraction ?? null,
    };
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
    // Callers gate this on the placement actually being complete --
    // `onPointerUp` fires it only on the finishing release, and a cancelled
    // pointer abandons the placement rather than completing it. So there is
    // deliberately no re-check here: a second one would have to restate the
    // anchor-count rule, and a restated rule is a rule that drifts.

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
    const before = movedFrom ?? { a1: stored.a1, a2: stored.a2, a3: stored.a3 ?? null };
    const after = { a1: resolved.a1, a2: resolved.a2, a3: resolved.a3 ?? null };
    // Applied optimistically -- the shape follows the pointer's release, which
    // is what "released" means -- and reconciled after the PUT like any save.
    stored.a1 = after.a1;
    stored.a2 = after.a2;
    stored.a3 = after.a3;
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
      sameAnchor(d.a1, after.a1) &&
      sameAnchor(d.a2, after.a2) &&
      sameAnchor(d.a3 ?? null, after.a3);
    runCommand(
      "move",
      () => {
        const target = byShapeId(stored, id);
        if (!target || isAfter(target)) return;
        target.a1 = after.a1;
        target.a2 = after.a2;
        target.a3 = after.a3;
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
        target.a3 = before.a3;
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
      a3: drawing.a3 ?? null,
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
      d.a3 && d.a3.time, d.a3 && d.a3.price,
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
    // The global toolbar mirrors the armed tool, whichever surface picked it.
    // Guarded because it may not exist yet (no engine, no build).
    if (typeof refreshGlobalTools === "function" && globalTools.built) refreshGlobalTools();
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

  /// Say on the AI layer's button what the layer holds and whether it is on.
  ///
  /// The closed toolbar has to answer "is there anything to see?" without a
  /// click: a toggle that reveals nothing when switched on reads as broken
  /// rather than empty. Derived, never stored -- `drawings` is the one list.
  function refreshAiLayerButton() {
    const button = el("aiLayer");
    if (!button) return;
    const count = drawings.filter((d) => d.created_by === "ai").length;
    button.title = count
      ? `${count} AI-drawn object${count === 1 ? "" : "s"} ${aiLayerOn ? "shown" : "hidden"}`
      : "Show objects the AI agent drew on this chart -- none exist yet";
  }

  /// Toggle the AI analysis layer (`docs/21` phase 3).
  ///
  /// Presentation only, which is why it schedules a render and nothing else:
  /// the rows stay in `drawings`, so undo, delete and the counts do not care
  /// what is being shown.
  function toggleAiLayer() {
    aiLayerOn = !aiLayerOn;
    const button = el("aiLayer");
    if (button) button.setAttribute("aria-pressed", String(aiLayerOn));
    refreshAiLayerButton();
    scheduleRender();
  }

  /// Toggle which edge the volume profile is anchored to.
  ///
  /// Presentation only, like every other overlay switch: the engine places the
  /// bars and this pane merely asks for the left edge. The static-layer cache
  /// needs no help -- a new scene bumps its identity already.
  function toggleProfileAnchor() {
    profileLeft = !profileLeft;
    const button = el("profileAnchor");
    if (button) button.setAttribute("aria-pressed", String(profileLeft));
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
      ...toolbar.querySelectorAll("button[data-magnet], button[data-ai-layer], button[data-undo], button[data-redo], .deleteDrawing, .clearDrawings"),
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
    wire("button[data-ai-layer]", toggleAiLayer);
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
  /// The provenance trio (`docs/21`) is carried the same way `label` is — a
  /// fact about the row that travels with it — and `created_by: "ai"` is what
  /// the AI layer filter keys on. Absent for a human-drawn row, which is most
  /// rows, so the fields are dropped rather than carried as nulls.
  function fromServer(drawing) {
    const out = {
      id: drawing.id,
      kind: drawing.kind,
      a1: drawing.a1,
      a2: drawing.a2 ?? null,
      // The third anchor rides the same rule as the second: absent when the
      // row has none, absolute when it does. A channel that lost its width on
      // reload would be a stored shape that restores as a different shape.
      a3: drawing.a3 ?? null,
      label: drawing.label ?? null,
    };
    if (drawing.created_by) {
      out.created_by = drawing.created_by;
      if (drawing.confidence != null) out.confidence = drawing.confidence;
      if (drawing.reason) out.reason = drawing.reason;
    }
    return out;
  }

  /// Say something about the chart, in the strip the engine's own notes use.
  function note(message) {
    el("chartNote").textContent = message;
    // The strip sits under the chart, where a user mid-drag is looking at the
    // crosshair -- and a refusal they never see is, to them, "nothing
    // happened". The toast floats the same words over the plot for a few
    // seconds; the strip keeps the permanent record.
    toast(message);
  }

  /// One quiet message over the plot, gone again before it becomes wallpaper.
  ///
  /// A single element per pane, shown and cleared by timer -- so a burst of
  /// refusals reads as one persistent message rather than three overlapping
  /// ones, and nothing accumulates in the DOM.
  let toastTimer = 0;
  function toast(message) {
    pageToast(message);
    if (!message) return;
    let elToast = el("chartWrap").querySelector(".chartToast");
    if (!elToast) {
      elToast = document.createElement("div");
      elToast.className = "chartToast";
      elToast.setAttribute("role", "status");
      el("chartWrap").appendChild(elToast);
    }
    elToast.textContent = message;
    elToast.classList.add("show");
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => {
      elToast.classList.remove("show");
      toastTimer = 0;
    }, 4000);
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
    // A select with no options has nothing to aim at. Wiring fires `connectLive`
    // on every series change, including the one a symbol change makes while the
    // timeframe list is being rebuilt -- and a channel to `/ws/market//` can only
    // 404, over and over, with the backoff hiding it from everyone but the log.
    // Refused here rather than retried there.
    if (!el("symbol").value || !el("timeframe").value) {
      live.state = "idle";
      refreshLiveBadge();
      return;
    }
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
    ws.onmessage = async (event) => {
      // Same guard as `onopen`: a frame from a channel this pane has already
      // left is not evidence about the one it is on now.
      if (socket !== ws) return;
      let frame;
      try {
        // Binary frames arrive as Blobs (see `decodeFrame`); decoding them
        // wrong is what kept this chart frozen until a reload.
        frame = await decodeFrame(event);
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
        // The ladder's forming candle is built from the same trades this frame
        // summarizes, so the frame marks the ladder stale and the poll (or the
        // next timer wake, however late a throttled tab's timers fire) refetches
        // it. Without this the footprint fetched once and froze while the price
        // line kept moving -- the "price moves, footprint does not" report.
        markFootprintDirty();
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

  /// Rebuild the layer-settings popover from the pane's layer state
  /// (docs/25). Derived, never stored: the form's controls are regenerated
  /// from `attachedScripts[layerSettingsFor]` on every open and every layer
  /// change, so the form and the request cannot disagree about what a layer
  /// runs with.
  function renderLayerSettingsPopover() {
    const pop = root.querySelector(".layerSettings");
    if (!pop) return;
    const layer = layerSettingsFor !== null ? attachedScripts[layerSettingsFor] : null;
    if (!layer || !layer.inputsSpec || !layer.inputsSpec.length) {
      pop.hidden = true;
      pop.innerHTML = "";
      return;
    }
    pop.hidden = false;
    const rows = layer.inputsSpec
      .map((decl, i) => {
        const label = escapeHtml(decl.title || decl.name);
        const current = Object.prototype.hasOwnProperty.call(layer.inputs, decl.name)
          ? layer.inputs[decl.name]
          : decl.default;
        if (decl.kind === "bool") {
          const checked = current === true || current === 1;
          return (
            `<label class="lsRow"><span>${label}</span>` +
            `<input type="checkbox" data-input="${i}"${checked ? " checked" : ""} /></label>`
          );
        }
        if (decl.kind === "int" || decl.kind === "float") {
          const step = decl.kind === "int" ? "1" : "any";
          const value = typeof current === "number" ? current : "";
          return (
            `<label class="lsRow"><span>${label}</span>` +
            `<input type="number" step="${step}" data-input="${i}" value="${value}" /></label>`
          );
        }
        // string/color inputs typecheck but the VM has no string evaluator
        // yet: the row says so instead of offering a control that does
        // nothing.
        return `<label class="lsRow"><span>${label}</span><span class="muted">edited in code</span></label>`;
      })
      .join("");
    pop.innerHTML =
      `<div class="lsHead"><span>${escapeHtml(layer.name || "script")} settings</span>` +
      `<button type="button" class="lsClose" title="Close">×</button></div>` +
      rows +
      `<div class="lsFoot"><button type="button" class="lsReset" title="Every input back to its declared default">Reset</button>` +
      `<button type="button" class="lsDone">Done</button></div>`;
    // Field edits apply on `change` (checkbox at once, a number when it
    // commits) -- per-keystroke re-renders would fight the user's typing.
    pop.querySelectorAll("[data-input]").forEach((control) => {
      control.addEventListener("change", () => {
        const decl = layer.inputsSpec[parseInt(control.dataset.input, 10)];
        if (!decl) return;
        if (decl.kind === "bool") {
          layer.inputs[decl.name] = control.checked;
        } else {
          const value = parseFloat(control.value);
          // A half-typed number ("1e", "-") is not a value: keep the old one
          // until the field commits something finite.
          if (!Number.isFinite(value)) return;
          layer.inputs[decl.name] = decl.kind === "int" ? Math.round(value) : value;
        }
        renderNow();
      });
    });
    pop.querySelector(".lsClose").addEventListener("click", () => {
      layerSettingsFor = null;
      renderLayerSettingsPopover();
    });
    pop.querySelector(".lsDone").addEventListener("click", () => {
      layerSettingsFor = null;
      renderLayerSettingsPopover();
    });
    pop.querySelector(".lsReset").addEventListener("click", () => {
      layer.inputs = {};
      renderNow();
      renderLayerSettingsPopover();
    });
  }

  /// The listeners for the pane's own chrome. Kept beside `wire()` but separate
  /// from it: `wire()` is about the chart and its series, this is about the
  /// pane as a panel.
  function wireChrome() {
    // The generated-indicator chip: its × removes the layer from this pane.
    // The chip itself is synced by `syncIndicatorChip`, called wherever the
    // attachment or the pane title changes.
    const chipRemove = root.querySelector(".indicatorChipRemove");
    if (chipRemove) {
      chipRemove.addEventListener("click", (event) => {
        event.stopPropagation();
        const chip = root.querySelector(".indicatorChip");
        const name = chip ? chip.querySelector(".indicatorChipName").textContent : "indicator";
        detachIndicator();
        toast(`Removed ${name} from this chart`);
      });
    }
    // Script-layer chips (docs/25): one delegated listener on the container,
    // because `syncIndicatorChip` rebuilds the chips on every sync -- wiring
    // each button would die with the node it was wired to. The eye toggles
    // the layer's visibility, the × removes it, and both derive their target
    // from the chip's data-layer index.
    const layerChips = root.querySelector(".layerChips");
    if (layerChips) {
      layerChips.addEventListener("click", (event) => {
        event.stopPropagation();
        const button = event.target.closest("button[data-layer]");
        if (!button) return;
        const index = parseInt(button.dataset.layer, 10);
        if (!(index >= 0)) return;
        if (button.classList.contains("layerChipEye")) {
          const scripts = paneApi.attachedScripts();
          const layer = scripts[index];
          if (!layer) return;
          paneApi.setScriptVisible(index, layer.visible === false);
        } else if (button.classList.contains("layerChipGear")) {
          paneApi.toggleLayerSettings(index);
        } else if (button.classList.contains("layerChipRemove")) {
          const scripts = paneApi.attachedScripts();
          const name = scripts[index] ? scripts[index].name || "script" : "script";
          paneApi.removeScriptAt(index);
          toast(`Removed ${name} from this chart`);
        }
      });
    }
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
    // The bar count is part of the series: a smaller limit drops bars the old
    // array still holds, so the candles go and are refetched at the new count.
    el("limit").addEventListener("change", () => {
      resetSeries();
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
    // that has trades), so it refetches rather than just redrawing.
    el("mode").addEventListener("change", () => {
      // A mode change throws the zoom away. The footprint fetches its own window
      // and the engine re-fits, which it must: a viewport resolved against candle
      // prices makes the ladder's levels vanish behind the old axis (footprint
      // mode fetches its own data, so the price range of the previous mode has
      // nothing to say about the ladder's). The shell_check probe caught exactly
      // this as `scene.footprint: null` after switching modes.
      resetViewport();
      refresh();
      paintTitle();
    });
    // These change the series itself, so the window means nothing afterwards -- a
    // bar index into the old series is not a bar in the new one, and the limit
    // select changes how many exist at all. A limit change can also *drop* bars
    // the old array still holds, so the candles go too.
    el("timeframe").addEventListener("change", () => {
      resetSeries();
      refresh().then(connectLive);
      paintTitle();
    });
    el("symbol").addEventListener("change", () => {
    // A different instrument has different timeframes, so the list is rebuilt
    // before the fetch that reads the chosen one.
    fillTimeframes(el("symbol").value);
    // Everything about the old series goes before the fetch: candles, window
    // and indicator. A fetch that fails now shows an empty chart and its
    // error, not the previous instrument under the new one's name.
    resetSeries();
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
      // Following the edge again means the ladder should live again too.
      startFootprintPoll();
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

    /// Attach a generated indicator to this chart. A revision preview whose
    /// `concepts` survived is attached **live**: the engine re-detects its
    /// concepts on whatever series this pane loads -- any symbol, any
    /// timeframe, every new candle -- until it is removed. A snapshot-only
    /// payload (no concepts) still attaches as the one-window preview it is.
    /// The caller may only pass a preview returned by the workspace revision
    /// API; source itself never runs in the browser.
    attachIndicator(output) {
      indicator = output || null;
      diffIndicator = null;
      renderNow();
      syncIndicatorChip();
    },

    /// Attach a vetted Pine-lite script (docs/23) to this chart as a LAYER
    /// (docs/25). The caller passes { source, inputs } -- source already
    /// accepted by `POST /scripts/vet`; the browser never runs or parses it,
    /// it only sends it back so the engine can. Layers compose: attaching a
    /// new script adds a layer beside the existing ones; re-attaching the
    /// SAME source updates its layer in place, so saving the revision under
    /// edit stays one gesture instead of stacking a duplicate per save.
    attachScript(spec) {
      if (!spec || !spec.source) return;
      const existing = attachedScripts.findIndex((s) => s.source === spec.source);
      // `inputsSpec` is the settings form's shape (docs/25): the input
      // declarations the vet recorded, carried by the revision's validation
      // record so an attach never re-vets just to learn the knobs.
      const layer = {
        source: spec.source,
        // On a re-attach (re-saving the revision under edit) the tuned
        // values survive -- wiping them would make the settings popover
        // unusable with the editor's own save-and-run gesture.
        inputs: existing >= 0 ? attachedScripts[existing].inputs : (spec.inputs || {}),
        name: spec.name,
        inputsSpec: Array.isArray(spec.inputsSpec) ? spec.inputsSpec : [],
        visible: true,
      };
      if (existing >= 0) attachedScripts[existing] = layer;
      else attachedScripts.push(layer);
      renderNow();
      syncIndicatorChip();
    },

    /// Every attached script layer, for the chips and the attach paths.
    attachedScripts() {
      return attachedScripts;
    },

    /// Show or hide one script layer (docs/25). Hiding withholds the layer
    /// from the scene request rather than deleting it: the layer list is the
    /// pane's state, and the eye toggles what the engine is asked to draw.
    setScriptVisible(index, visible) {
      const layer = attachedScripts[index];
      if (!layer) return;
      layer.visible = !!visible;
      renderNow();
      syncIndicatorChip();
    },

    /// Remove one script layer by index.
    removeScriptAt(index) {
      if (index < 0 || index >= attachedScripts.length) return;
      attachedScripts.splice(index, 1);
      // The open popover follows its layer: removing the layer under it
      // closes it, removing an earlier one shifts it.
      if (layerSettingsFor === index) layerSettingsFor = null;
      else if (layerSettingsFor !== null && layerSettingsFor > index) layerSettingsFor -= 1;
      renderNow();
      syncIndicatorChip();
      renderLayerSettingsPopover();
    },

    /// Open (or toggle) one layer's settings popover (docs/25). Only one is
    /// open at a time -- two forms editing two layers at once is two sources
    /// of truth for one chart.
    toggleLayerSettings(index) {
      const layer = attachedScripts[index];
      if (!layer || !layer.inputsSpec || !layer.inputsSpec.length) return;
      layerSettingsFor = layerSettingsFor === index ? null : index;
      renderLayerSettingsPopover();
    },

    /// The index of the layer whose settings popover is open, or null.
    layerSettingsOpen: () => layerSettingsFor,

    /// Take every attached script off this chart.
    clearScripts() {
      if (!attachedScripts.length) return;
      attachedScripts = [];
      layerSettingsFor = null;
      renderNow();
      syncIndicatorChip();
      renderLayerSettingsPopover();
    },

    /// Overlay an older revision under the attached one, for the diff view:
    /// the old revision's zones draw faded behind the live layer, so "what
    /// changed between rev 3 and rev 4" is a picture instead of two mental
    /// models. Only snapshot zones diff well -- a live old layer would
    /// re-detect on today's series and make the comparison lie.
    diffIndicatorAgainst(oldPreview) {
      diffIndicator = oldPreview && Array.isArray(oldPreview.zones) ? oldPreview : null;
      if (diffIndicator && Array.isArray(oldPreview.concepts) && oldPreview.concepts.length) {
        // A live old layer would move; the diff wants the frozen coordinates.
        diffIndicator = { ...oldPreview, concepts: [] };
      }
      renderNow();
    },

    /// Clear the diff overlay.
    clearDiffIndicator() {
      if (diffIndicator === null) return;
      diffIndicator = null;
      renderNow();
    },

    /// Take the attached indicator off this chart.
    ///
    /// Called with every series change. A **live** indicator (one carrying its
    /// concepts) survives: its definition is symbol-agnostic, so it keeps
    /// detecting on the new series by design. A snapshot-only payload is
    /// cleared -- coordinates against the old series are meaningless on the
    /// new one, which is the "the old chart's indicator is on my new chart"
    /// report. The workspace still holds both; the picker re-attaches.
    clearIndicator() {
      if (indicator === null) return;
      if (indicator && Array.isArray(indicator.concepts) && indicator.concepts.length) return;
      indicator = null;
      renderNow();
      syncIndicatorChip();
    },

    /// This pane's attached indicator, for the picker and the chip. The page
    /// cannot reach the pane's `indicator` local directly -- that is the point
    /// of the pane boundary -- so the answer comes through the API.
    attachedIndicator: () => indicator,

    /// The pane's diff overlay, if one is set.
    diffIndicator: () => diffIndicator,

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
    /// Read-only state getters for the global toolbar, which derives every
    /// button's pressed state from the active pane rather than keeping its
    /// own copy of any of it.
    currentTool: () => tool,
    magnetOn: () => magnet,
    aiLayerOn: () => aiLayerOn,
    profileLeft: () => profileLeft,
    canUndo: () => undoStack.length > 0,
    canRedo: () => redoStack.length > 0,
    deleteSelected,
    /// Undo/redo for the page's Ctrl+Z / Ctrl+Y routing. Takes the direction
    /// as a string because the router has no reason to hold two references.
    history(direction) {
      if (direction === "redo") redo();
      else undo();
      refreshGlobalTools();
    },
    toggleMagnet,
    toggleAiLayer,
    toggleProfileAnchor,
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
  // The global toolbar follows activation: it is bound to whichever chart the
  // user last touched, so an activation that changes nothing else may still
  // change what the toolbar's buttons would act on.
  refreshGlobalTools();
  syncIndicatorChip();
}

/// The global drawing toolbar: the standalone twin of the right-click menu's
/// hosted one (`docs/21`).
///
/// ## Why it is bound, not hosted
///
/// The right-click menu *hosts* a pane's own toolbar -- the real buttons move
/// into the menu and back, so there is exactly one wiring of each control. The
/// global toolbar cannot use that trick: it must survive the pane it was built
/// for being closed, and its buttons must not move out of it while a menu is
/// open. So it is the inverse arrangement: the toolbar is one permanent row of
/// buttons owned by the page, and every interaction is **delegated** to the
/// active pane through its existing API. Two toolbars, one state -- each button
/// here acts on the same `tool`/`selectedDrawing`/undo stacks the hosted one
/// does, because they are the same functions.
///
/// ## Why it only works on the active pane
///
/// A drawing tool aimed at "some chart" is aimed at none: the user must know
/// which canvas the next click lands on. Activation is the same gesture the
/// aside already follows -- press anything on a chart and it becomes the one
/// the controls mean -- and the toolbar's label names the chart it will draw
/// on, so the binding is stated where the click happens.
const globalTools = {
  node: null,
  built: false,

  /// Build the row once, from the same registry the pane toolbars use. The
  /// buttons are *labels*, not per-pane state: their pressed state is set by
  /// `refreshGlobalTools` from the active pane's tool, so a toolbar can never
  /// disagree with the chart it acts on.
  build() {
    if (this.built || !toolRegistry || !toolRegistry.length) return;
    const node = document.getElementById("globalTools");
    if (!node) return;
    this.node = node;
    const frag = document.createDocumentFragment();
    const which = document.createElement("span");
    which.className = "globalToolsWhich";
    which.id = "globalToolsWhich";
    frag.appendChild(which);

    const cursor = document.createElement("button");
    cursor.dataset.tool = "cursor";
    cursor.textContent = "Cursor";
    cursor.title = "Select a drawing, move its anchors, or pan the chart";
    frag.appendChild(cursor);

    // The same grouping `buildToolbarFromRegistry` renders, so the two
    // toolbars read identically -- same groups, same order, same labels.
    const groups = [];
    for (const entry of toolRegistry) {
      let group = groups.find((g) => g.name === entry.group);
      if (!group) {
        group = { name: entry.group, label: entry.group_label || entry.group, tools: [] };
        groups.push(group);
      }
      group.tools.push(entry);
    }
    for (const group of groups) {
      const wrap = document.createElement("div");
      wrap.className = "toolGroup";
      const trigger = document.createElement("button");
      trigger.className = "toolGroupTrigger";
      trigger.textContent = group.label;
      trigger.title = group.tools.map((t) => t.label).join(", ");
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
        flyout.appendChild(button);
      }
      trigger.addEventListener("click", (event) => {
        event.stopPropagation();
        const open = !flyout.hidden;
        for (const other of node.querySelectorAll(".toolFlyout")) other.hidden = true;
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

    // The page-level controls. Delegated, like the tools: the active pane's
    // own undo stack, magnet and AI layer are the ones that move, because
    // drawings are per pane and a global toggle over "all panes" would be a
    // different feature.
    const controls = [
      ["button[data-magnet]", "Magnet", "Snap new drawings to nearby open, high, low, close and value-area prices", "magnet"],
      ["button[data-ai-layer]", "AI", "Show objects the AI agent drew on the active chart, with the reason it gave for each", "aiLayer"],
      ["button[data-profile-anchor]", "Profile", "Anchor the volume profile to the plot's left edge instead of the right", "profileAnchor"],
      ["button[data-undo]", "Undo", "Undo the last drawing change on the active chart (Ctrl+Z)", "undo"],
      ["button[data-redo]", "Redo", "Redo an undone drawing change on the active chart (Ctrl+Y)", "redo"],
    ];
    for (const [attr, label, title] of controls) {
      const button = document.createElement("button");
      button.setAttribute(attr.match(/data-[a-z-]+/)[0], "");
      button.textContent = label;
      button.title = title;
      frag.appendChild(button);
    }

    node.replaceChildren(frag);
    // One delegated listener for the whole row, at the row: buttons are
    // generated, so binding each would be a rebuild away from drifting.
    node.addEventListener("click", (event) => {
      const button = event.target.closest("button");
      if (!button || !activePane) return;
      if (button.dataset.tool) {
        activePane.selectTool(button.dataset.tool);
        refreshGlobalTools();
      } else if (button.hasAttribute("data-magnet")) {
        activePane.toggleMagnet();
        refreshGlobalTools();
      } else if (button.hasAttribute("data-ai-layer")) {
        activePane.toggleAiLayer();
        refreshGlobalTools();
      } else if (button.hasAttribute("data-profile-anchor")) {
        activePane.toggleProfileAnchor();
        refreshGlobalTools();
      } else if (button.hasAttribute("data-undo")) {
        activePane.history("undo");
      } else if (button.hasAttribute("data-redo")) {
        activePane.history("redo");
      }
    });
    this.built = true;
    refreshGlobalTools();
  },

  /// Hide when there is no pane to act on. Shown the moment one exists.
  setHidden(hidden) {
    if (this.node) this.node.hidden = hidden;
  },
};

/// A page-level toast, for messages from outside any pane: the workspace chat's
/// attach confirmations, picker errors. One element, one timer -- the same
/// discipline as a pane's own toast, at page scope because the workspace panel
/// is not part of any chart.
let pageToastTimer = 0;
function pageToast(message) {
  if (!message) return;
  let box = document.getElementById("pageToast");
  if (!box) {
    box = document.createElement("div");
    box.id = "pageToast";
    box.className = "pageToast";
    box.setAttribute("role", "status");
    document.body.appendChild(box);
  }
  box.textContent = message;
  box.classList.add("show");
  if (pageToastTimer) clearTimeout(pageToastTimer);
  pageToastTimer = setTimeout(() => {
    box.classList.remove("show");
    pageToastTimer = 0;
  }, 4000);
}

/// Sync every pane's generated-indicator chip to what that pane holds.
///
/// Derived, never stored: each pane's chip shows that pane's own attachment
/// (multi-chart means many chips), with the live ones marked. Called wherever
/// an attachment changes and wherever the active pane changes -- cheap, and it
/// cannot drift the way a flag set in one place and read in another does.
function syncIndicatorChip() {
  for (const pane of panes) {
    // The concept-layer chip: only the attached indicator document, never a
    // script -- script layers have their own chips below (docs/25), so one
    // chip never stands in for (or removes) a different layer's output.
    const chip = pane.root.querySelector(".indicatorChip");
    if (chip) {
      const attached = pane.attachedIndicator();
      const live = attached && Array.isArray(attached.concepts) && attached.concepts.length > 0;
      chip.hidden = !attached;
      if (attached) {
        chip.querySelector(".indicatorChipName").textContent =
          (attached.name || "generated indicator") + (live ? " · live" : "");
        chip.title = live
          ? "Detects live on every candle of this chart until removed"
          : "Static preview from the generator window";
      }
    }
    // One chip per script layer, derived from the pane's layer list on every
    // sync -- never stored in the DOM between syncs, so the chips and the
    // request cannot disagree about what is attached or visible.
    const layersEl = pane.root.querySelector(".layerChips");
    if (layersEl) {
      const scripts = pane.attachedScripts ? pane.attachedScripts() : [];
      layersEl.hidden = scripts.length === 0;
      layersEl.innerHTML = scripts
        .map((s, i) => {
          const off = s.visible === false;
          const hasSettings = Array.isArray(s.inputsSpec) && s.inputsSpec.length > 0;
          return (
            `<span class="layerChip${off ? " off" : ""}" title="A pine-lite layer — runs on this chart's candles every frame">` +
            `<button type="button" class="layerChipEye" data-layer="${i}" aria-pressed="${!off}" ` +
            `title="${off ? "Show this layer" : "Hide this layer"}">${off ? "○" : "●"}</button>` +
            `<span class="indicatorChipName">${escapeHtml(s.name || "script")} · live</span>` +
            (hasSettings
              ? `<button type="button" class="layerChipGear" data-layer="${i}" title="Layer settings (its declared inputs)">⚙</button>`
              : "") +
            `<button type="button" class="indicatorChipRemove layerChipRemove" data-layer="${i}" title="Remove this layer">×</button>` +
            `</span>`
          );
        })
        .join("");
    }
  }
}

/// Remove the active pane's attached CONCEPT-layer indicator. Script layers
/// are untouched: each has its own chip with its own × (docs/25) -- one chip
/// removing a different layer's output was the old all-or-nothing model.
function detachIndicator() {
  if (!activePane) return;
  const output = activePane.attachedIndicator();
  activePane.clearIndicator();
  // A live definition survives series changes by design, so `clearIndicator`
  // skips it; the chip's × means *remove*, so it goes through the full reset
  // by handing the pane a bare null payload.
  if (output && Array.isArray(output.concepts) && output.concepts.length) {
    activePane.attachIndicator(null);
  }
  syncIndicatorChip();
}

/// Re-derive every global button's state from the active pane. Derived,
/// never stored -- the same rule the pane toolbars follow for `aria-pressed`.
function refreshGlobalTools() {
  const node = globalTools.node;
  // No pane, no binding: the row hides rather than aiming at nothing.
  if (!node) return;
  if (!activePane) {
    node.hidden = true;
    return;
  }
  node.hidden = false;
  const which = document.getElementById("globalToolsWhich");
  if (which) {
    which.textContent = `${activePane.symbol() || "—"} · ${activePane.timeframe() || "—"}`;
  }
  const tool = activePane.currentTool();
  for (const button of node.querySelectorAll("button[data-tool]")) {
    button.setAttribute("aria-pressed", String(button.dataset.tool === tool));
  }
  // Group triggers relabel themselves with the armed tool, as the pane
  // toolbars' `refreshGroupTriggers` does: the trigger carries its group's
  // name, and the armed tool belongs to at most one group.
  for (const trigger of node.querySelectorAll(".toolGroupTrigger")) {
    const base = trigger.dataset.groupLabel;
    if (!base) continue;
    const armed = toolRegistry.some(
      (entry) => entry.group === trigger.dataset.group && entry.kind === tool
    );
    trigger.textContent = armed ? `${base} · ${tool}` : base;
  }
  const magnet = node.querySelector("[data-magnet]");
  if (magnet) magnet.setAttribute("aria-pressed", String(activePane.magnetOn()));
  const ai = node.querySelector("[data-ai-layer]");
  if (ai) ai.setAttribute("aria-pressed", String(activePane.aiLayerOn()));
  const profile = node.querySelector("[data-profile-anchor]");
  if (profile) profile.setAttribute("aria-pressed", String(activePane.profileLeft()));
  const undo = node.querySelector("[data-undo]");
  if (undo) undo.disabled = !activePane.canUndo();
  const redo = node.querySelector("[data-redo]");
  if (redo) redo.disabled = !activePane.canRedo();
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

  ws.onmessage = async (event) => {
    let frame;
    try {
      // Binary frames arrive as Blobs -- see `decodeFrame`; this channel's
      // candles are sent binary exactly like the market channel's.
      frame = await decodeFrame(event);
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

async function onAgentFrame(event) {
  let frame;
  try {
    frame = await decodeFrame(event);
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

  ws.onmessage = async (event) => {
    let frame;
    try {
      frame = await decodeFrame(event);
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
  // Sweep buttons are rendered inside message bubbles, so the click is caught
  // on the stream and dispatched -- one listener instead of one per bubble.
  const chatStream = document.getElementById("wsChat");
  if (chatStream) {
    chatStream.addEventListener("click", (e) => {
      const btn = e.target.closest(".ws-sweep-btn");
      if (btn && btn.dataset.strategy) runParameterSweep(btn.dataset.strategy, btn);
      const reviewBtn = e.target.closest(".ws-review-btn");
      if (reviewBtn && reviewBtn.dataset.revision) reviewRevisionWithChart(reviewBtn.dataset.revision, reviewBtn);
    });
  }
  // Screenshot attachment: the file input is hidden and the paperclip opens
  // it, so the composer stays one row tall until images are actually chosen.
  const attachBtn = document.getElementById("wsChatAttach");
  const imageInput = document.getElementById("wsChatImage");
  if (attachBtn && imageInput) {
    attachBtn.onclick = () => imageInput.click();
    imageInput.onchange = async () => {
      const files = Array.from(imageInput.files || []).slice(0, 4);
      for (const file of files) {
        try {
          const shot = await readImageFile(file);
          if (wsPendingImages.length < 4) wsPendingImages.push(shot);
        } catch {
          pageToast(`Could not read ${file.name}`);
        }
      }
      imageInput.value = "";
      renderPendingImages();
    };
  }
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
  // The global toolbar is built from the same registry, once, at the same
  // moment: before the engine there is nothing to build it from.
  globalTools.build();

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
  // Leaving the conversation also stops waiting (docs/29).
  if (wsGenAbort) wsGenAbort.abort();
  wsActiveId = null;
  el("wsActive").hidden = true;
  el("wsListView").hidden = false;
  loadWorkspaces();
}

async function selectWorkspace(id) {
  // Switching chats stops waiting for the previous chat's in-flight answer
  // (docs/29): the generation may still finish server-side, and its message
  // lands the next time that workspace loads.
  if (wsGenAbort && id !== wsActiveId) wsGenAbort.abort();
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
  // Auto-attach the active revision to the active chart, if one exists.
  //
  // A live revision (one carrying its concepts) attaches to any chart -- a
  // concept is a shape, and the engine detects it on whatever series the pane
  // holds, including a different symbol or timeframe than the generator used.
  // A snapshot-only preview only ever attaches to the symbol it was computed
  // from: its coordinates are meaningless over another instrument.
  if (ws.active_revision_id && activePane) {
    try {
      const rev = await api(`/indicator-workspaces/${id}/revisions/${ws.active_revision_id}`);
      const preview = rev.preview;
      // A pine-lite revision attaches as CODE: the source goes back to the
      // engine, which re-runs it on this chart's candles every frame. A
      // document revision attaches as the concepts/preview pair, as before.
      if (rev.validation && rev.validation.engine === "pine-lite-v1" && rev.source) {
        // A layer (docs/25): joins whatever is already on the chart. The
        // validation record carries the input declarations, so the layer's
        // settings form needs no re-vet.
        activePane.attachScript({
          source: rev.source,
          inputs: {},
          name: (preview && preview.name) || ws.name,
          inputsSpec: Array.isArray(rev.validation.inputs) ? rev.validation.inputs : [],
        });
        syncIndicatorChip();
        pageToast(`Attached script "${(preview && preview.name) || ws.name}" to ${activePane.symbol()} ${activePane.timeframe()} — it re-runs on every candle`);
      } else {
        const live = preview && Array.isArray(preview.concepts) && preview.concepts.length > 0;
        if (preview && (live || !ws.symbol || ws.symbol === activePane.symbol())) {
          activePane.attachIndicator(preview);
          syncIndicatorChip();
          const liveNote = live
            ? " — it keeps detecting on every new candle of any chart it is attached to"
            : "";
          pageToast(`Attached "${preview.name || ws.name}" to ${activePane.symbol()} ${activePane.timeframe()}${liveNote}`);
        }
      }
    } catch (e) {
      pageToast(`Failed to attach indicator: ${e.message}`);
    }
  }
}

async function loadRevisions(wsId) {
  const out = el("wsRevisions");
  try {
    wsRevisions = await api(`/indicator-workspaces/${wsId}/revisions`);
  } catch (e) {
    out.innerHTML = `<p class="error">${e.message}</p>`;
    renderCodeFiles();
    return;
  }
  if (!wsRevisions.length) {
    out.innerHTML = `<p class="empty">No revisions yet.</p>`;
    renderCodeFiles();
    return;
  }
  const ws = wsWorkspaces.find(w => w.id === wsId);
  const activeId = ws ? ws.active_revision_id : null;
  out.innerHTML = wsRevisions.map(r => {
    const isActive = r.id === activeId;
    const evidence = r.preview && r.preview.evidence ? r.preview.evidence.length : 0;
    const live = r.preview && Array.isArray(r.preview.concepts) && r.preview.concepts.length > 0;
    return `
      <div class="ws-revision" style="padding:6px 0;border-bottom:1px solid var(--line)">
        <div class="row">
          <strong>#${r.revision_number}</strong>
          <span class="muted">${escapeHtml(r.summary)}</span>
          ${isActive ? '<span class="up">active</span>' : ''}
          <span class="muted">${evidence} evidence</span>
          ${live ? '<span class="up" title="This indicator detects live on any chart it is attached to">live</span>' : ''}
        </div>
        <div class="muted" style="font-size:11px">${escapeHtml(r.change_summary)}</div>
        <div class="row" style="margin-top:4px">
          <button onclick="attachRevisionToChart('${wsId}','${r.id}')" title="Attach this indicator to a chart">Attach to chart</button>
          <button onclick="restoreRevision('${wsId}','${r.id}')" title="Set as active">Restore</button>
          <button onclick="viewRevision('${wsId}','${r.id}')" title="View source and preview">View</button>
          <button onclick="openCodeFile('${wsId}','${r.id}')" title="Open this revision's source in the code panel">Code</button>
          ${isActive ? '' : `<button onclick="diffRevision('${wsId}','${r.id}')" title="Draw this revision faded under the active one, to see what changed">Diff vs active</button>`}
        </div>
      </div>
    `;
  }).join("");
  renderCodeFiles();

// ---------------------------------------------------------------------------
// The code panel: the workspace's revisions as files, Pine-editor style.
// ---------------------------------------------------------------------------

// The file currently open in the editor, { revisionId, name, source, fresh }.
// `fresh` marks a never-saved "New file" draft, which has no revision to
// point at yet.
let wsCodeFile = null;

/// Render the file list from `wsRevisions` (already fetched) and restore the
/// editor's visibility. Cheap enough to re-run on every revision load.
function renderCodeFiles() {
  const host = el("wsCodeFiles");
  if (!host) return;
  if (!wsRevisions.length) {
    host.innerHTML = `<p class="empty">No files yet — generate a revision first.</p>`;
    const editor = el("wsCodeEditor");
    if (editor) editor.hidden = true;
    return;
  }
  const activeId = (wsWorkspaces.find((w) => w.id === wsActiveId) || {}).active_revision_id;
  host.innerHTML = wsRevisions.map((r) => `
    <div class="row" style="padding:2px 0;border-bottom:1px solid var(--line)">
      <button type="button" class="ws-code-file" data-revision="${r.id}" title="Open in the code panel">${escapeHtml(r.summary || "indicator")}</button>
      <span class="muted" style="font-size:11px">rev #${r.revision_number}${r.id === activeId ? " · active" : ""}</span>
    </div>`).join("");
}

/// Open one revision's source in the code panel.
async function openCodeFile(wsId, revId) {
  try {
    const rev = await api(`/indicator-workspaces/${wsId}/revisions/${revId}`);
    if (!rev.source) { alert("This revision has no stored source."); return; }
    wsCodeFile = { wsId, revisionId: revId, name: rev.summary || `rev #${rev.revision_number}`, source: rev.source, fresh: false };
    paintCodeEditor(false);
  } catch (e) {
    alert(`Could not open: ${e.message}`);
  }
}

/// Paint the editor for `wsCodeFile`. `readOnlyView` keeps the textarea
/// read-only until Edit is pressed: looking at code and changing code are
/// different intents, and a textarea that always edits makes a stray
/// keystroke a silent rewrite of history.
function paintCodeEditor(readOnlyView) {
  const editor = el("wsCodeEditor");
  const source = el("wsCodeSource");
  const title = el("wsCodeTitle");
  const msg = el("wsCodeMsg");
  if (!editor || !source || !title) return;
  editor.hidden = false;
  title.textContent = wsCodeFile.fresh ? "new file (unsaved)" : wsCodeFile.name;
  source.value = wsCodeFile.source;
  source.readOnly = readOnlyView;
  if (msg) msg.textContent = "";
  const editBtn = el("wsCodeEdit");
  if (editBtn) editBtn.hidden = wsCodeFile.fresh;
  const saveBtn = el("wsCodeSave");
  if (saveBtn) saveBtn.disabled = readOnlyView;
}

/// Start a blank document: same kind rules the chat generator enforces are
/// the user's to get right, and the save path validates before storing.
function newCodeFile() {
  if (!wsActiveId) { alert("Select the workspace first."); return; }
  wsCodeFile = { wsId: wsActiveId, revisionId: null, name: "new file (unsaved)", source: "", fresh: true };
  paintCodeEditor(false);
  el("wsCodeSource").focus();
}

/// Save the editor's contents as pine-lite: the same vet gate the generator's
/// output passes, the same revision store, and -- on success -- the script
/// attaches to the chart immediately. A failed vet lists EVERY issue with its
/// line and column, TradingView-editor style: fix, resubmit, no model in the
/// loop.
async function saveCodeAsRevision() {
  if (!wsCodeFile) { alert("Nothing to save."); return; }
  const source = el("wsCodeSource").value;
  if (!source.trim()) { alert("The file is empty."); return; }
  const msg = el("wsCodeMsg");
  if (msg) msg.textContent = "Validating…";
  try {
    const resp = await api(`/indicator-workspaces/${wsCodeFile.wsId}/scripts`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        source,
        origin: wsCodeFile.fresh ? "Pasted script" : `Edited ${wsCodeFile.name}`,
      }),
    });
    const stats = resp.preview_stats;
    const statNote = stats ? ` — ${stats.bars} bars replayed, ${stats.plots} plot(s), ${stats.shapes} marker(s)` : "";
    if (msg) msg.textContent = `Saved as rev #${resp.revision_number}${statNote}.`;
    wsCodeFile = { wsId: wsCodeFile.wsId, revisionId: resp.revision_id, name: resp.title, source, fresh: false };
    await loadRevisions(wsCodeFile.wsId);
    // Attach straight to the chart: save and run are one gesture, the way
    // the Pine editor does it. As a LAYER (docs/25): the script joins any
    // layers already on the chart instead of replacing them -- re-saving the
    // same source updates its own layer, and the layer chips toggle or remove
    // each one.
    if (activePane) {
      activePane.attachScript({
        source,
        inputs: {},
        name: resp.title || "script",
        inputsSpec: Array.isArray(resp.inputs) ? resp.inputs : [],
      });
      syncIndicatorChip();
      pageToast(`Attached "${resp.title || "script"}" to ${activePane.symbol()} ${activePane.timeframe()} — it re-runs on every candle; manage it from the layer chips`);
    }
    if (wsActiveId) await selectWorkspace(wsActiveId);
  } catch (e) {
    // The 422's details carry every vet issue as {path, message} pairs;
    // render them as an error list, not a wall of JSON.
    if (msg) {
      const issues = e.details && e.details.issues;
      if (Array.isArray(issues) && issues.length) {
        msg.innerHTML = `<ul class="ws-vet-errors">${issues.map(i => `<li>${escapeHtml(String(i.message))}</li>`).join("")}</ul>`;
      } else {
        msg.textContent = `Save failed: ${e.message}`;
      }
    }
  }
}

function wireCodePanel() {
  const files = el("wsCodeFiles");
  if (files && !files.dataset.wired) {
    files.addEventListener("click", (e) => {
      const file = e.target.closest(".ws-code-file");
      if (file && file.dataset.revision && wsActiveId) openCodeFile(wsActiveId, file.dataset.revision);
    });
    files.dataset.wired = "1";
  }
  const copy = el("wsCodeCopy");
  if (copy && !copy.dataset.wired) {
    copy.onclick = async () => {
      if (!el("wsCodeSource")) return;
      const text = el("wsCodeSource").value;
      try {
        await navigator.clipboard.writeText(text);
        if (el("wsCodeMsg")) el("wsCodeMsg").textContent = "Copied.";
      } catch {
        // Clipboard permission denied: select instead, Ctrl+C still works.
        el("wsCodeSource").select();
        if (el("wsCodeMsg")) el("wsCodeMsg").textContent = "Clipboard refused — selection made, press Ctrl+C.";
      }
    };
    copy.dataset.wired = "1";
  }
  const edit = el("wsCodeEdit");
  if (edit && !edit.dataset.wired) {
    edit.onclick = () => paintCodeEditor(false);
    edit.dataset.wired = "1";
  }
  const fresh = el("wsCodeNew");
  if (fresh && !fresh.dataset.wired) {
    fresh.onclick = newCodeFile;
    fresh.dataset.wired = "1";
  }
  const save = el("wsCodeSave");
  if (save && !save.dataset.wired) {
    save.onclick = saveCodeAsRevision;
    save.dataset.wired = "1";
  }
}
wireCodePanel();}

/// Attach a revision's indicator to a chart the user picks.
///
/// A generated indicator is not bound to the symbol it was generated on: a
/// concept is a shape, and the engine detects that shape on whatever series a
/// pane holds. The picker lists every open chart; a single-chart workspace
/// skips the question and attaches straight away.
async function attachRevisionToChart(wsId, revId) {
  try {
    const rev = await api(`/indicator-workspaces/${wsId}/revisions/${revId}`);
    if (!rev.preview) { alert("This revision has no chart preview."); return; }
    let target = activePane;
    if (panes.length > 1) {
      const labels = panes.map((p, i) => `${i + 1}. ${p.symbol()} ${p.timeframe()}${p === activePane ? " (active)" : ""}`);
      const pick = prompt(`Attach "${rev.preview.name || "indicator"}" to which chart?\n${labels.join("\n")}\nEnter a number:`, "1");
      if (pick === null) return;
      const index = parseInt(pick, 10) - 1;
      if (!(index >= 0 && index < panes.length)) { alert("No such chart."); return; }
      target = panes[index];
    }
    if (!target) { alert("Open a chart first."); return; }
    // A pine-lite revision attaches as code; a document revision as its
    // concepts/preview pair -- see `selectWorkspace`'s same fork.
    if (rev.validation && rev.validation.engine === "pine-lite-v1" && rev.source) {
      // A layer (docs/25): joins whatever is already on the chart.
      target.attachScript({
        source: rev.source,
        inputs: {},
        name: rev.preview.name || "script",
        inputsSpec: Array.isArray(rev.validation.inputs) ? rev.validation.inputs : [],
      });
    } else {
      target.attachIndicator(rev.preview);
    }
    setActive(target);
    syncIndicatorChip();
    const isScript = rev.validation && rev.validation.engine === "pine-lite-v1";
    pageToast(`Attached "${rev.preview.name || "indicator"}" to ${target.symbol()} ${target.timeframe()}${isScript ? " — the script re-runs on every candle" : ""}`);
  } catch (e) {
    pageToast(`Could not attach: ${e.message}`);
  }
}

/// Draw an older revision faded under the active one: "what changed" as a
/// picture. Both previews are already on the client; the diff is frozen
/// coordinates, so it draws even when the two revisions were generated on
/// different windows.
async function diffRevision(wsId, revId) {
  try {
    if (!activePane) { alert("Open a chart first."); return; }
    const attached = activePane.attachedIndicator();
    if (!attached) { alert("Attach the active revision to a chart first, then diff another against it."); return; }
    const rev = await api(`/indicator-workspaces/${wsId}/revisions/${revId}`);
    if (!rev.preview || !Array.isArray(rev.preview.zones)) { alert("That revision has no zones to diff."); return; }
    activePane.diffIndicatorAgainst(rev.preview);
    syncIndicatorChip();
    pageToast(`Diffing rev #${rev.revision_number} (faded) against the attached layer`);
  } catch (e) {
    pageToast(`Could not diff: ${e.message}`);
  }
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
    // Attaching is the point of the button: a script attaches as code, a
    // live document definition works on any chart, a snapshot attaches to
    // the active pane as the one-window view of what the generator saw.
    if (rev.preview && activePane) {
      if (rev.validation && rev.validation.engine === "pine-lite-v1" && rev.source) {
        // A layer (docs/25): joins whatever is already on the chart.
        activePane.attachScript({
          source: rev.source,
          inputs: {},
          name: rev.preview.name || "script",
          inputsSpec: Array.isArray(rev.validation.inputs) ? rev.validation.inputs : [],
        });
      } else {
        activePane.attachIndicator(rev.preview);
      }
      syncIndicatorChip();
      pageToast(`Attached "${rev.preview.name || "indicator"}" to ${activePane.symbol()} ${activePane.timeframe()}`);
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

  /// The auto-backtest card under a generated revision's chat message.
  ///
  /// The gateway computed these from the generation replay's own trades -- the
  /// same `compute_metrics` a real backtest runs -- so the card describes a
  /// one-week sandboxed preview, and says so. A detector layer has no trades
  /// by design; its card reports the detection counts instead of inventing R
  /// numbers that do not exist.
  function previewStatsCard(stats) {
    if (!stats || typeof stats !== "object") return "";
    const days = Math.max(1, Math.round(Number(stats.window_days) || 7));
    const fmt = (v, suffix = "") =>
      v === null || v === undefined ? "—" : `${Number(v).toFixed(2)}${suffix}`;
    // A pine-lite script's preview: what the VM ran over and what it drew.
    if (typeof stats.bars === "number") {
      return `
        <div class="ws-stats" title="The script ran over the preview window's candles on the server">
          <span class="muted">script replay · ${days}d</span>
          <span><strong>${stats.bars}</strong> bars</span>
          <span><strong>${Number(stats.plots) || 0}</strong> plots</span>
          <span><strong>${Number(stats.levels) || 0}</strong> levels</span>
          <span><strong>${Number(stats.shapes) || 0}</strong> markers</span>
        </div>`;
    }
    if (stats.kind === "detector") {
      return `
        <div class="ws-stats" title="Detection counts from the generation replay over the last ${days} days">
          <span class="muted">preview replay · ${days}d</span>
          <span><strong>${Number(stats.fires) || 0}</strong> detections</span>
          <span class="muted">detector layer — no trades by design</span>
        </div>`;
    }
    return `
      <div class="ws-stats" title="Auto-backtest of the generated document over the last ${days} days (sandboxed replay, R multiples)">
        <span class="muted">preview backtest · ${days}d</span>
        <span><strong>${Number(stats.fires) || 0}</strong> signals</span>
        <span><strong>${Number(stats.trades) || 0}</strong> trades</span>
        <span>win <strong>${stats.win_rate === null || stats.win_rate === undefined ? "—" : `${(Number(stats.win_rate) * 100).toFixed(0)}%`}</strong></span>
        <span>avg <strong>${fmt(stats.average_r, "R")}</strong></span>
        <span>net <strong>${fmt(stats.net_r, "R")}</strong></span>
        <span>maxDD <strong>${fmt(stats.max_drawdown_r, "R")}</strong></span>
      </div>`;
  }

  /// Parameter sweep: call the sweep route and render each parameter's grid
  /// as a compact table. The "stable" read is the point -- a parameter whose
  /// neighbours all lose money is a lucky spike, not an edge.
  /// Screenshot self-review: capture the chart the revision is attached to
  /// (the ACTIVE pane -- it must be the one showing the indicator) and ask
  /// the model to compare the rendered result against the document it wrote.
  /// The review lands in the sweep/result panel so it can be read at leisure
  /// and closed; the chat bubble's own button says "Review on chart".
  async function reviewRevisionWithChart(revisionId, btn) {
    if (!activePane) { alert("Open a chart and attach the revision to it first."); return; }
    if (btn) { btn.disabled = true; btn.textContent = "Reviewing…"; }
    try {
      const shot = captureChart(activePane.canvas, activePane.timeframe());
      if (!shot) { alert("Could not capture the active chart."); return; }
      if (!wsActiveId) { alert("Select the workspace this indicator belongs to first."); return; }
      const resp = await api(`/indicator-workspaces/${wsActiveId}/revisions/${revisionId}/review`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ images: [{ media_type: shot.media_type, data: shot.data }] }),
      });
      const host = el("wsSweepResult");
      if (host) {
        host.hidden = false;
        host.innerHTML = `<div class="ws-review"><strong>AI review of the rendered chart</strong>
          <pre style="white-space:pre-wrap;margin:6px 0 0;font-size:12px">${escapeHtml(resp.review || "(empty review)")}</pre></div>`;
      } else {
        pageToast("Review ready — see the workspace panel");
      }
    } catch (e) {
      pageToast(`Review failed: ${e.message}`);
    } finally {
      if (btn) { btn.disabled = false; btn.textContent = "Review on chart"; }
    }
  }

  async function runParameterSweep(strategyId, btn) {
    const card = btn && btn.parentElement;
    if (card) {
      btn.disabled = true;
      btn.textContent = "Sweeping…";
    }
    try {
      const to = new Date().toISOString().slice(0, 10);
      const from = new Date(Date.now() - 7 * 86400e3).toISOString().slice(0, 10);
      const resp = await api(`/strategies/${strategyId}/sweep`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ symbol: activePane ? activePane.symbol() : "BTCUSDT", from, to, source_timeframe: "1m" }),
      });
      const host = el("wsSweepResult");
      if (!host) { pageToast("No sweep panel on this view"); return; }
      host.hidden = false;
      host.innerHTML = (resp.parameters || []).map((p) => {
        if (!p.points || !p.points.length) {
          return `<div class="ws-sweep"><strong>threshold(${p.base_value})</strong> <span class="muted">no variant ran: ${escapeHtml((p.skipped || []).join("; ") || "unknown")}</span></div>`;
        }
        const best = p.points.reduce((a, b) => (b.net_r > a.net_r ? b : a), p.points[0]);
        // Colour by net R: red below 0, green above, brightest at the best.
        const cell = (pt) => {
          const r = pt.net_r;
          const tone = r >= 0
            ? `rgba(45, 212, 191, ${Math.min(0.55, 0.12 + r * 0.3)})`
            : `rgba(251, 113, 133, ${Math.min(0.55, 0.12 + Math.abs(r) * 0.3)})`;
          const isBest = pt === best && p.points.length > 1;
          return `<td style="background:${tone}">${r.toFixed(2)}R${isBest ? ' ★' : ''}</td>`;
        };
        return `
          <div class="ws-sweep">
            <div class="row"><strong>threshold(${p.base_value})</strong><span class="muted">${escapeHtml(p.condition)}</span></div>
            <table>
              <thead><tr><th>×${p.points.map((pt) => pt.multiplier).join("</th><th>×")}</th></tr></thead>
              <tbody><tr><td>${p.points.map(cell).join("</td><td>")}</td></tr></tbody>
            </table>
            <div class="muted" style="font-size:11px">net R per grid point · trades ${p.points.map((pt) => pt.trades).join("/")} · ${best.multiplier}× (${best.value}) was best${(p.skipped || []).length ? ` · ${p.skipped.length} variant(s) refused` : ""}</div>
          </div>`;
      }).join("") || "<p class=\"muted\">This document has no threshold() parameters to sweep.</p>";
      if (card && btn) {
        btn.disabled = false;
        btn.textContent = "Sweep parameters";
      }
      // Open the tab that hosts the result, so the click always has a visible
      // effect even when the button lives in a collapsed message.
      host.scrollIntoView({ behavior: "smooth", block: "nearest" });
    } catch (e) {
      if (card && btn) {
        btn.disabled = false;
        btn.textContent = "Sweep parameters";
      }
      pageToast(`Sweep failed: ${e.message}`);
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
    // The auto-backtest card: the generation replay's own numbers, so "is
    // this idea even viable" is answered in the same breath that produced it.
    const statsCard = !isUser && m.payload && m.payload.preview_stats
      ? previewStatsCard(m.payload.preview_stats) : "";
    // Parameter sweep button: only for kind: strategy, which is the kind the
    // backtester runs. A detector layer has no thresholds to sweep.
    const sweepBtn = !isUser && m.payload && m.payload.kind === "strategy" && m.payload.strategy_id
      ? `<button type="button" class="ws-sweep-btn" data-strategy="${m.payload.strategy_id}" title="Sweep every threshold() over a grid and show what each value did">Sweep parameters</button>`
      : "";
    // Code revisions carry their source too; the summary line names the
    // representation so the transcript reads "code", not "document".
    // Self-review button: sends a screenshot of the chart the revision is
    // attached to, and the model reads what it actually drew off the picture.
    const reviewBtn = !isUser && m.payload && m.payload.revision_id
      ? `<button type="button" class="ws-review-btn" data-revision="${m.payload.revision_id}" title="Screenshot the chart this indicator is attached to and have the AI check what it drew">Review on chart</button>`
      : "";
    const when = m.created_at ? new Date(m.created_at / 1e6).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : "";
    return `
      <div class="msg ${isUser ? "user" : "ai"}">
        <div class="avatar" aria-hidden="true">${isUser ? "🧑" : "✦"}</div>
        <div>
          <div class="bubble">${escapeHtml(m.content)}${statsCard}${sweepBtn}${reviewBtn}${srcBlock}</div>
          <div class="meta">${isUser ? "You" : "AI"}${when ? ` · ${when}` : ""}</div>
        </div>
      </div>
    `;
  }).join("");
  out.scrollTop = out.scrollHeight;
}

  /// Screenshots pending attachment to the next workspace message.
  ///
  /// Read to data URLs immediately (the file input's value does not survive
  /// the round trip) and sent as base64 with the next message -- the same
  /// shape the chart-capture path already sends.
  let wsPendingImages = [];

  function readImageFile(file) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve({
        media_type: file.type,
        data: String(reader.result).split(",")[1] || "",
        name: file.name,
      });
      reader.onerror = () => reject(reader.error);
      reader.readAsDataURL(file);
    });
  }

  function renderPendingImages() {
    const row = el("wsImageRow");
    if (!wsPendingImages.length) {
      row.hidden = true;
      row.innerHTML = "";
      return;
    }
    row.hidden = false;
    row.innerHTML = wsPendingImages
      .map((img, i) => `<span>📎 ${escapeHtml(img.name || "screenshot")} <button type="button" data-idx="${i}" class="ws-img-remove" title="Remove">✕</button></span>`)
      .join(" ");
    row.querySelectorAll(".ws-img-remove").forEach((btn) => {
      btn.onclick = () => {
        wsPendingImages.splice(Number(btn.dataset.idx), 1);
        renderPendingImages();
      };
    });
  }

  /// The in-flight generation, if any (docs/29): while a message is being
  /// answered the send button becomes the stop button, and a pending bubble
  /// in the transcript shows the elapsed time.
  let wsGenAbort = null;
  let wsGenTimer = null;

  /// The pending bubble is honest about what is knowable mid-flight: that
  /// the pipeline is running and how long it has taken. The stages (model
  /// draft, vet, repair, preview) are NOT observable without streaming, so
  /// the bubble describes the pipeline once and counts seconds -- never a
  /// fake per-stage progress bar.
  function wsGenBubbleShow(withImage) {
    wsGenBubbleClear();
    const stream = el("wsChat");
    if (!stream) return;
    const bubble = document.createElement("div");
    bubble.className = "msg ai wsGenPending";
    bubble.innerHTML =
      `<div class="avatar" aria-hidden="true">✦</div>` +
      `<div><div class="bubble">` +
      `${withImage ? "Reading the screenshot and generating" : "Generating"} — the model drafts, the vet checks and repairs, usually 15–60s · <b class="wsGenElapsed">0s</b>` +
      `</div><div class="meta">➤ is now ■ — click it (or press Enter) to stop waiting</div></div>`;
    stream.appendChild(bubble);
    stream.scrollTop = stream.scrollHeight;
    const started = Date.now();
    const elapsedEl = bubble.querySelector(".wsGenElapsed");
    wsGenTimer = setInterval(() => {
      // The element can be detached by a transcript re-render; stop then.
      if (!elapsedEl.isConnected) {
        clearInterval(wsGenTimer);
        wsGenTimer = null;
        return;
      }
      elapsedEl.textContent = `${Math.round((Date.now() - started) / 1000)}s`;
    }, 1000);
  }

  function wsGenBubbleClear() {
    if (wsGenTimer) {
      clearInterval(wsGenTimer);
      wsGenTimer = null;
    }
    document.querySelectorAll(".wsGenPending").forEach((n) => n.remove());
  }

  async function sendWorkspaceMessage() {
  if (!wsActiveId) return;
  // While a generation is in flight, sending means stopping.
  if (wsGenAbort) {
    wsGenAbort.abort();
    return;
  }
  const input = el("wsChatInput");
  const content = input.value.trim();
  if (!content && !wsPendingImages.length) return;
  if (!content) {
    el("wsChatMsg").textContent = "Describe what to build along with the screenshot.";
    return;
  }
  input.value = "";
  input.style.height = "auto";
  const images = wsPendingImages.map(({ media_type, data }) => ({ media_type, data }));
  wsPendingImages = [];
  renderPendingImages();
  const msg = el("wsChatMsg");
  const sendBtn = el("wsChatSend");
  msg.textContent = "";
  wsGenAbort = new AbortController();
  wsGenBubbleShow(images.length > 0);
  sendBtn.textContent = "■";
  sendBtn.title = "Stop waiting — the generation may still finish server-side and appear on the next refresh";
  try {
    const resp = await api(`/indicator-workspaces/${wsActiveId}/messages`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ content, images }),
      signal: wsGenAbort.signal,
    });
    // Show the revision info from the response before refreshing.
    if (resp && resp.revision) {
      msg.textContent = `Revision #${resp.revision.revision_number} created — attaching to chart…`;
    }
    await selectWorkspace(wsActiveId);
    msg.textContent = "Done.";
  } catch (e) {
    msg.textContent = e.name === "AbortError"
      ? "Stopped waiting — if the generation finishes server-side it appears on the next refresh."
      : e.message;
  } finally {
    wsGenAbort = null;
    wsGenBubbleClear();
    sendBtn.textContent = "➤";
    sendBtn.title = "Send (Enter)";
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
