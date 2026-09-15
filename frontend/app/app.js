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

let wasm = null; // the chart engine instance
let scene = null; // the last scene the engine produced
let thesis = null; // the last thesis, for the chart overlay
let socket = null; // the live candle channel
let bookSocket = null; // the order-book channel
let botSocket = null; // the channel for the one bot being watched
let watchedBot = null; // its id, or null when watching none
let bots = []; // the last bot list read from the API
let botLog = []; // frames from `botSocket`, oldest first

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
  el("signin").hidden = Boolean(value);
  el("signinToggle").textContent = value ? "Sign out" : "Sign in";
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

async function signIn(register) {
  const email = el("email").value.trim();
  const password = el("password").value;
  const msg = el("signinMsg");
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
  } catch (e) {
    msg.textContent = e.message;
  }
}

// ---------------------------------------------------------------------------
// The chart engine
// ---------------------------------------------------------------------------

const ENGINE_URL = "/chart_engine.wasm";

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
  return instance.exports;
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
// Drawing
//
// Every coordinate below comes from the scene. The only arithmetic is the
// device-pixel-ratio scale, which is a display concern rather than a market one.
// ---------------------------------------------------------------------------

const COLORS = {
  up: "#26a69a",
  down: "#ef5350",
  wick: "#8b949e",
  profile: "#30363d",
  value: "#58a6ff",
  grid: "#21262d",
  text: "#8b949e",
  vwap: "#d29922",
  poc: "#e6edf3",
  vah: "#8b949e",
  val: "#8b949e",
  entry: "#58a6ff",
  stop: "#ef5350",
  target: "#26a69a",
  demand: "#26a69a",
  supply: "#ef5350",
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
  drawAxis(ctx, scene);
  if (thesis) drawThesis(ctx, scene, thesis);
}

/// The footprint ladder: bid x ask per level, per candle.
///
/// Every coordinate and every string comes from the engine. This function picks
/// colours and calls fillText -- nothing else.
/// Supply/demand zones -- the chart's only *area* overlay.
///
/// Every coordinate, every price and the label itself come from the engine.
/// This function picks a colour from the zone's kind and fills a rectangle,
/// which is the same contract `drawProfile` and `drawLevels` follow. Note there
/// is no subtraction here: the band's height arrives as `h`, so a fill is
/// `fillRect(x, y_top, w, h)` and nothing is computed from prices.
///
/// A fresh zone is drawn solid and a mitigated one faded. That distinction is
/// the whole reason the concept is worth drawing: a zone price has already
/// traded back through is not a level any more, and rendering the two the same
/// is how a chart teaches someone to buy something that no longer exists.
function drawRegions(ctx, scene) {
  if (!scene.regions.length) return;
  ctx.font = "10px ui-monospace, monospace";

  for (const zone of scene.regions) {
    const colour = COLORS[zone.kind] || COLORS.text;
    ctx.globalAlpha = zone.fresh ? 0.16 : 0.07;
    ctx.fillStyle = colour;
    ctx.fillRect(zone.x, zone.y_top, zone.w, zone.h);
    ctx.globalAlpha = 1;

    // The outline, so a zone in a quiet stretch of chart is still visible.
    ctx.strokeStyle = colour;
    ctx.globalAlpha = zone.fresh ? 0.7 : 0.35;
    ctx.setLineDash([3, 3]);
    ctx.strokeRect(zone.x + 0.5, zone.y_top + 0.5, zone.w - 1, zone.h - 1);
    ctx.setLineDash([]);
    ctx.globalAlpha = 1;

    ctx.fillStyle = colour;
    ctx.fillText(zone.label, zone.x + 4, zone.y_top + 11);
  }
}

function drawFootprintGrid(ctx, scene) {
  const grid = scene.footprint;
  const font = Math.max(6, Math.min(11, grid.font_px));
  const showText = font >= 7;

  // The value-area band, behind everything, so the eye finds it first.
  for (const row of grid.rows) {
    const inside = grid.columns.some((column) =>
      column.cells.some((cell) => cell.y === row.y && cell.in_value_area)
    );
    if (!inside) continue;
    ctx.fillStyle = "rgba(88, 166, 255, 0.05)";
    ctx.fillRect(scene.plot.x, row.y, scene.plot.w, row.h);
  }

  ctx.font = `${font}px ui-monospace, monospace`;
  ctx.textBaseline = "middle";

  for (const column of grid.columns) {
    const half = column.w / 2;

    for (const cell of column.cells) {
      // A diagonal imbalance is the signal a footprint exists to show, so it
      // gets the only saturated fill on the chart. Buy-aggressed volume is the
      // ask side winning; sell-aggressed is the bid side.
      if (cell.side === "buy") {
        ctx.fillStyle = "rgba(194, 100, 216, 0.28)";
        ctx.fillRect(cell.x + 1, cell.y, cell.w - 2, cell.h);
      } else if (cell.side === "sell") {
        ctx.fillStyle = "rgba(74, 158, 218, 0.28)";
        ctx.fillRect(cell.x + 1, cell.y, cell.w - 2, cell.h);
      }

      if (cell.is_poc) {
        ctx.strokeStyle = "rgba(230, 237, 243, 0.55)";
        ctx.strokeRect(cell.x + 1, cell.y + 0.5, cell.w - 2, Math.max(1, cell.h - 1));
      }

      if (!showText) continue;
      const mid = cell.y + cell.h / 2;
      ctx.fillStyle = cell.bid >= cell.ask ? "#e6edf3" : "#8b949e";
      ctx.textAlign = "right";
      ctx.fillText(cell.bid_text, cell.x + half - 3, mid);
      ctx.fillStyle = cell.ask >= cell.bid ? "#e6edf3" : "#8b949e";
      ctx.textAlign = "left";
      ctx.fillText(cell.ask_text, cell.x + half + 3, mid);
    }

    // The candle summary: total volume over the delta, under the ladder.
    const summary = column.summary;
    ctx.fillStyle = "#1c2129";
    ctx.fillRect(summary.x + 1, summary.y, summary.w - 2, summary.h);
    if (showText) {
      ctx.textAlign = "center";
      ctx.fillStyle = "#e6edf3";
      ctx.fillText(summary.volume_text, summary.x + summary.w / 2, summary.y + font * 0.9);
      ctx.fillStyle = summary.delta_positive ? COLORS.up : COLORS.down;
      ctx.fillText(summary.delta_text, summary.x + summary.w / 2, summary.y + summary.h - font * 0.7);
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

function drawAxis(ctx, scene) {
  ctx.fillStyle = COLORS.text;
  ctx.font = "10px ui-monospace, monospace";
  const from = new Date(scene.from / 1e6);
  const to = new Date(scene.to / 1e6);
  ctx.fillText(from.toISOString().slice(0, 16).replace("T", " "), scene.plot.x, scene.height - 8);
  const label = to.toISOString().slice(0, 16).replace("T", " ");
  ctx.fillText(label, scene.plot.x + scene.plot.w - ctx.measureText(label).width, scene.height - 8);
}

/// Highlight the thesis's own levels.
///
/// docs/14: "the ability to highlight the exact chart region(s) referenced in
/// the AI's explanation (map thesis fields like `entry_price`/timestamps back to
/// chart coordinates)". So the stop-to-target band is shaded and the three
/// prices are drawn -- the numbers come from the thesis, the coordinates come
/// from the engine's own price scale.
function drawThesis(ctx, scene, thesis) {
  const span = scene.price_max - scene.price_min;
  if (!(span > 0)) return;
  const y = (price) =>
    scene.plot.y + scene.plot.h - ((price - scene.price_min) / span) * scene.plot.h;

  const stop = y(thesis.stop_price);
  const target = y(thesis.target_price);
  const entry = y(thesis.entry_price);

  ctx.globalAlpha = 0.12;
  ctx.fillStyle = thesis.direction === "long" ? COLORS.target : COLORS.stop;
  ctx.fillRect(scene.plot.x, Math.min(stop, target), scene.plot.w, Math.abs(target - stop));
  ctx.globalAlpha = 1;

  for (const [price, colour, label] of [
    [thesis.stop_price, COLORS.stop, "stop"],
    [thesis.entry_price, COLORS.entry, "entry"],
    [thesis.target_price, COLORS.target, "target"],
  ]) {
    const yy = y(price);
    ctx.strokeStyle = colour;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(scene.plot.x, yy);
    ctx.lineTo(scene.plot.x + scene.plot.w, yy);
    ctx.stroke();
    ctx.fillStyle = colour;
    ctx.font = "bold 10px ui-monospace, monospace";
    const text = `${label} ${price.toFixed(2)}`;
    ctx.fillText(text, scene.plot.x + scene.plot.w - ctx.measureText(text).width - 4, yy - 3);
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
  return response.candles || [];
}

let candles = [];
// The trade-level ladders, when the mode asks for them and the window has
// trades. Kept beside `candles` rather than inside them: a footprint needs both
// the OHLC for the axis and the ladders for the grid, and they come from two
// routes.
let footprint = null;

async function refresh() {
  const message = el("chartMsg");
  footprint = null;

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

function render() {
  if (!wasm) return;
  const wrap = el("chart").parentElement;
  scene = buildScene({
    candles,
    width: wrap.clientWidth,
    height: wrap.clientHeight,
    mode: el("mode").value,
    lines: ["vwap", "poc", "vah", "val"],
    // Whether to detect and draw the supply/demand zones. The engine does the
    // detecting, on the candles this request already carries, so switching this
    // on costs no extra round trip.
    zones: zonesOn(),
    footprint: footprint ? footprint.candles : [],
    footprint_trades: footprint ? footprint.trades : 0,
  });
  el("chartNote").textContent = scene.note || "";
  renderFootprintStats(scene.footprint);
  draw();
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
/// ## Why the column count comes from the viewport
///
/// A ladder cell has to fit `0.44 x 2.75` -- about nine characters. Below ~54px
/// per column the two numbers collide and the ladder stops being readable, which
/// is what the first version of this looked like: 40 columns of smeared text.
/// The column count is therefore a layout decision, which is the shell's to
/// make, and the engine is told how many candles to expect.
async function loadFootprint() {
  const symbol = el("symbol").value;
  const timeframe = el("timeframe").value;
  const width = el("chart").parentElement.clientWidth || 900;

  const MIN_COLUMN_PX = 54;
  const columns = Math.max(8, Math.min(40, Math.floor(width / MIN_COLUMN_PX)));

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

/// Follow the live candle channel.
///
/// The token goes in the query string because a browser cannot set a header on
/// a WebSocket handshake. The channel is public anyway, but passing the token
/// when we have one keeps the code honest about which channels need it.
function connectLive() {
  if (socket) socket.close();
  const symbol = el("symbol").value;
  const timeframe = el("timeframe").value;
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const query = token() ? `?token=${encodeURIComponent(token())}` : "";

  socket = new WebSocket(`${scheme}://${location.host}/ws/market/${symbol}/${timeframe}${query}`);
  socket.onmessage = (event) => {
    let frame;
    try {
      frame = JSON.parse(typeof event.data === "string" ? event.data : new TextDecoder().decode(event.data));
    } catch { return; }

    if (frame.type === "data") {
      // Replace the last candle if it is the same bucket, else append. This is
      // the only market-data decision this file makes, and it is about
      // identity, not value.
      const incoming = frame.payload;
      const last = candles[candles.length - 1];
      if (last && last.open_time === incoming.open_time) candles[candles.length - 1] = incoming;
      else candles.push(incoming);
      if (candles.length > Number(el("limit").value) + 50) candles.shift();
      render();
    } else if (frame.type === "lagged") {
      el("chartNote").textContent =
        `the live feed dropped ${frame.dropped} candles; reload to resynchronise`;
    }
  };
  socket.onclose = () => { socket = null; };
}

/// Follow the order-book channel.
///
/// This panel derives nothing. The ladder arrives with each level's cumulative
/// size and a bar width already worked out, both in Rust, because summing
/// depth here would be a second implementation of a number -- and then two
/// parts of this shell could disagree about the same book. Formatting is all
/// that is left to do, and that is all this does.
function connectBook() {
  if (bookSocket) bookSocket.close();
  const symbol = el("symbol").value;
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${scheme}://${location.host}/ws/orderbook/${symbol}`);
  bookSocket = ws;

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
    else if (frame.type === "notice") el("bookMsg").textContent = frame.message;
    else if (frame.type === "lagged") {
      el("bookMsg").textContent = `the book dropped ${frame.dropped} update(s)`;
    }
  };
  ws.onclose = () => {
    // Only the socket we are still meant to be using may speak for the panel.
    if (bookSocket !== ws) return;
    bookSocket = null;
    el("bookMsg").textContent = "The book disconnected.";
  };
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
    ws.onerror = () => reject(new Error("could not reach the agent channel"));
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
    draw();
  } else if (frame.type === "notice") {
    // A notice is a refusal or a failure -- a rate limit, a bad request, a
    // model error -- and it ends this question.
    turn.error = frame.message;
    thesis = null;
    asking = false;
    paintAsk();
    renderTranscript();
    draw();
  }
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
    ws.send(
      JSON.stringify({
        symbol: el("symbol").value,
        question,
        timeframes: [el("timeframe").value],
      })
    );
  } catch (e) {
    turn.error = e.message;
    asking = false;
    paintAsk();
    renderTranscript();
  }
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
        market: el("symbol").value,
        timeframe: el("timeframe").value,
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

function renderBuilderForm() {
  const keys = builderForm.kind === "indicator" ? [] : StrategyBuilder.GROUP_KEYS;
  el("builderForm").innerHTML =
    documentCard() +
    timeframesCard() +
    (builderForm.kind === "indicator" ? "" : riskCard()) +
    keys.map(groupCard).join("");
  renderBuilderNote();
}

/// What the form is missing, and what it could not model.
///
/// Separate from [`renderBuilderForm`] because this runs on every keystroke and
/// rebuilding the controls would throw away whatever the user was typing.
function renderBuilderNote() {
  // Say what is wrong before the round trip, and be honest about the
  // conditions held as text because the builder cannot model them.
  const issues = StrategyBuilder.formIssues(builderForm, builderSchema);
  const raw = StrategyBuilder.rawRowCount(builderForm);
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
  el("builderMsg").innerHTML = note;
}

/// Write the form back into the textarea, which is the one source of truth.
function applyBuilderToSource() {
  try {
    const yaml = StrategyBuilder.toYaml(
      StrategyBuilder.documentFromForm(builderForm, builderSchema)
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
        symbol: el("symbol").value,
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
      return `<dl class="kv">
          <dt>id</dt><dd>${escapeHtml(bot.id.slice(0, 8))}</dd>
          <dt>status</dt><dd>${escapeHtml(bot.status)}</dd>
          <dt>supervised</dt><dd>${bot.supervised_here}</dd>
          <dt>trades</dt><dd>${bot.activity?.trades ?? 0}</dd>
          <dt>decisions</dt><dd>${bot.activity?.decisions ?? 0}</dd>
          <dt>cumulative R</dt><dd>${(bot.activity?.cumulative_r ?? 0).toFixed(3)}</dd>
        </dl>
        <div class="row">
          <button data-bot="${bot.id}" data-act="${watching ? "unwatch" : "watch"}">${
            watching ? "Stop watching" : "Watch"
          }</button>
          <button data-bot="${bot.id}" data-act="pause">Pause</button>
          <button data-bot="${bot.id}" data-act="resume">Resume</button>
          <button data-bot="${bot.id}" data-act="delete">Delete</button>
        </div>
        ${log}`;
    })
    .join("<hr />");
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

async function botAction(id, act) {
  if (act === "watch") {
    watchBot(id);
    return;
  }
  if (act === "unwatch") {
    unwatchBot();
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
// Wiring
// ---------------------------------------------------------------------------

function selectPane(name) {
  for (const button of document.querySelectorAll(".tabs button")) {
    button.setAttribute("aria-selected", String(button.dataset.pane === name));
  }
  for (const pane of document.querySelectorAll(".pane")) {
    pane.hidden = pane.id !== `pane-${name}`;
  }
}

async function main() {
  paintSession();

  document.querySelectorAll(".tabs button").forEach((button) =>
    button.addEventListener("click", () => selectPane(button.dataset.pane))
  );

  el("signinToggle").addEventListener("click", () => {
    if (token()) { setToken(""); el("signinMsg").textContent = ""; }
    else { el("signin").hidden = false; el("email").focus(); }
  });
  el("signinGo").addEventListener("click", () => signIn(false));
  el("registerGo").addEventListener("click", () => signIn(true));
  el("password").addEventListener("keydown", (e) => { if (e.key === "Enter") signIn(false); });

  el("load").addEventListener("click", () => { refresh().then(connectLive); connectBook(); });
  // A redraw, not a refetch: the zones are detected from the candles the engine
  // already has, so there is nothing new to ask the backend for.
  el("zones").addEventListener("click", (e) => {
    const button = e.currentTarget;
    const on = button.getAttribute("aria-pressed") !== "true";
    button.setAttribute("aria-pressed", on ? "true" : "false");
    render();
  });
  // Changing the chart type can change the *window* (a footprint uses the
  // span that has trades), so it refetches rather than just redrawing.
  el("mode").addEventListener("change", () => { refresh(); });
  el("timeframe").addEventListener("change", () => { refresh().then(connectLive); });
  // The book is per symbol, so it follows the same change.
  el("symbol").addEventListener("change", () => { refresh().then(connectLive); connectBook(); });
  el("ask").addEventListener("click", ask);
  el("question").addEventListener("keydown", (e) => { if (e.key === "Enter") ask(); });

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
  el("refreshBots").addEventListener("click", refreshBots);
  el("botsOut").addEventListener("click", (e) => {
    const button = e.target.closest("button[data-bot]");
    if (button) botAction(button.dataset.bot, button.dataset.act);
  });

  window.addEventListener("resize", () => { if (scene) render(); });

  try {
    wasm = await loadEngine();
    el("chartMsg").textContent = "";
  } catch (e) {
    el("chartMsg").textContent = e.message;
    return;
  }

  // The editor is never empty: a saved strategy if there is one, otherwise the
  // reference document. An empty box makes Validate and Save look broken when
  // they are only being sent nothing.
  await seedEditor();
  // The editor may have opened on a saved strategy, which has runs. They are
  // read on load so the panel is never blank when there is something to show.
  await refreshBacktestRuns();

  await refresh();
  connectLive();
  // The DOM opens its own socket rather than riding the chart's: the two have
  // different reconnection stories, and the book has to be able to say "no
  // depth feed" without the chart looking broken.
  connectBook();
}

main();
