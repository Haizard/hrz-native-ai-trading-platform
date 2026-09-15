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

function render() {
  if (!wasm) return;
  const wrap = el("chart").parentElement;
  scene = buildScene({
    candles,
    width: wrap.clientWidth,
    height: wrap.clientHeight,
    mode: el("mode").value,
    lines: ["vwap", "poc", "vah", "val"],
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

function renderThesis(response) {
  const t = response.thesis;
  thesis = t;

  const checks = [...(t.higher_timeframe_checks || []), ...(t.order_flow_checks || [])]
    .map(
      (check) => `<li>
        <span class="status ${STATUS_CLASS[check.status] || "unknown"}">${check.status}</span>
        <span><strong>${escapeHtml(check.label)}</strong><br />
        <span class="muted">${escapeHtml(check.detail)}</span></span>
      </li>`
    )
    .join("");

  el("thesis").innerHTML = `
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
    <pre>${escapeHtml(t.narrative || "")}</pre>
  `;
  // The levels are on the chart now, which is the point of asking.
  draw();
}

function escapeHtml(value) {
  return String(value == null ? "" : value).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
  );
}

async function ask() {
  const question = el("question").value.trim();
  if (!question) return;
  const button = el("ask");
  button.disabled = true;
  button.textContent = "Thinking…";
  el("thesis").innerHTML = `<p class="empty">Asking the agent…</p>`;
  try {
    const response = await api("/agent/ask", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        symbol: el("symbol").value,
        question,
        timeframes: [el("timeframe").value],
      }),
    });
    renderThesis(response);
  } catch (e) {
    thesis = null;
    el("thesis").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
    draw();
  } finally {
    button.disabled = false;
    button.textContent = "Ask";
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
  } catch (e) {
    // 409 is the interesting one: a bot is still running this document, and the
    // server refuses rather than pulling it out from under the bot.
    message.innerHTML = `<span class="fail">${escapeHtml(e.message)}</span>`;
  }
}

async function runBacktest() {
  if (!savedStrategyId) {
    el("backtestOut").innerHTML = `<p class="fail">Save the strategy first.</p>`;
    return;
  }
  el("backtestOut").innerHTML = `<p class="empty">Running…</p>`;
  try {
    const result = await api(`/strategies/${savedStrategyId}/backtest`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        symbol: el("symbol").value,
        from: el("btFrom").value,
        to: el("btTo").value,
      }),
    });
    const r = result.report;
    el("backtestOut").innerHTML = `
      <dl class="kv">
        <dt>trades</dt><dd>${r.total_trades}</dd>
        <dt>win rate</dt><dd>${(r.win_rate * 100).toFixed(1)}%</dd>
        <dt>average R</dt><dd>${r.average_r.toFixed(3)}</dd>
        <dt>net</dt><dd>${r.net_return_pct.toFixed(2)}R</dd>
        <dt>max DD</dt><dd>${r.max_drawdown_pct.toFixed(2)}R</dd>
      </dl>
      <p class="muted">${escapeHtml(r.assumptions?.return_units || "")}</p>`;
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
    el("botsOut").innerHTML = `<p class="pass">started ${escapeHtml(bot.id)}</p>`;
    refreshBots();
  } catch (e) {
    el("botsOut").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
  }
}

async function refreshBots() {
  try {
    const bots = await api("/bots");
    if (!bots.length) {
      el("botsOut").innerHTML = `<p class="empty">No bots yet.</p>`;
      return;
    }
    el("botsOut").innerHTML = bots
      .map(
        (bot) => `<dl class="kv">
          <dt>id</dt><dd>${escapeHtml(bot.id.slice(0, 8))}</dd>
          <dt>status</dt><dd>${escapeHtml(bot.status)}</dd>
          <dt>supervised</dt><dd>${bot.supervised_here}</dd>
          <dt>trades</dt><dd>${bot.activity?.trades ?? 0}</dd>
          <dt>decisions</dt><dd>${bot.activity?.decisions ?? 0}</dd>
          <dt>cumulative R</dt><dd>${(bot.activity?.cumulative_r ?? 0).toFixed(3)}</dd>
        </dl>
        <div class="row">
          <button data-bot="${bot.id}" data-act="pause">Pause</button>
          <button data-bot="${bot.id}" data-act="resume">Resume</button>
          <button data-bot="${bot.id}" data-act="delete">Delete</button>
        </div>`
      )
      .join("<hr />");
  } catch (e) {
    el("botsOut").innerHTML = `<p class="fail">${escapeHtml(e.message)}</p>`;
  }
}

async function botAction(id, act) {
  try {
    if (act === "delete") await api(`/bots/${id}`, { method: "DELETE" });
    else await api(`/bots/${id}/${act}`, { method: "POST" });
    refreshBots();
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

  await refresh();
  connectLive();
  // The DOM opens its own socket rather than riding the chart's: the two have
  // different reconnection stories, and the book has to be able to say "no
  // depth feed" without the chart looking broken.
  connectBook();
}

main();
