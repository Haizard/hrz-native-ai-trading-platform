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
let botSocket = null; // the channel for the one bot being watched
let watchedBot = null; // its id, or null when watching none
let bots = []; // the last bot list read from the API
let botLog = []; // frames from `botSocket`, oldest first
let botNotifications = {}; // bot id -> notifications, once asked for
let notificationsOpen = null; // the bot whose notifications are shown
let venues = []; // the last venue list, so opt-in state has one source

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
    // The drawings belong to the account, so they arrive with it. Everything
    // else on the page is public market data and was already there.
    refresh();
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
  "load", "close", "clearDrawings",
]);

/// A timeframe's length in minutes, for ordering the options.
///
/// `GET /symbols` reports its timeframes in its own order, which is alphabetical
/// -- `15m, 1h, 1m, 4h, 5m` on this deployment. That is not a ladder anyone can
/// read and it makes "the next timeframe up" mean nothing, so the shell sorts
/// them. A suffix the server adds later sorts last rather than throwing: a new
/// unit should cost an odd-looking position, not a blank chart.
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
  // This pane's own controls, under the names the chart code already used.
  // Shadowing one function is the whole of the boundary -- every call site below
  // reads exactly as it did when there was a single chart, and the one line that
  // decides what "the chart" means is here.
  const el = (name) =>
    PANE_ELS.has(name) ? root.querySelector(`.${name}`) : document.getElementById(name);

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
    // The drawing tools, one colour per kind so two shapes on the same chart are
    // told apart by what they are rather than by which was drawn first.
    trendline: "#4aa3ff",
    hline: "#d29922",
    rect: "#a371f7",
    fib: "#3fb950",
    // The two bands the engine ships with. A concept a client defined has no
    // entry here -- it cannot, we have never heard of it -- which is what the
    // side fallback below is for.
    demand: "#26a69a",
    supply: "#ef5350",
    // Keyed by the region's `side`, the direction expected to react from the
    // band. Every region carries one, so an unfamiliar band reads as a direction
    // instead of as grey.
    buy: "#26a69a",
    sell: "#ef5350",
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
    // The user's own marks, above the levels and below the axis: a drawing that
    // could cover the price labels would be a drawing that hides the scale it is
    // read against.
    drawDrawings(ctx, scene);
    drawAxis(ctx, scene);
    // The thesis's levels are prices, so they mean something on any window of the
    // instrument they were computed for -- and nothing at all on another one. A
    // BTCUSDT entry drawn across an ETHUSDT chart would be a level that looks
    // plausible and is not, and with more than one chart that is one click away.
    // The timeframe is deliberately not part of the test: reading a 5m thesis
    // against the 4h chart is the reason to have a second chart at all.
    if (thesis && thesis.symbol === el("symbol").value) drawThesis(ctx, scene, thesis);
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
    };
    // Assigned rather than sent as `null`: a null is not a missing field, and the
    // engine's `Viewport` is a struct rather than an option, so `viewport: null`
    // would be a deserialization error rather than a default. Absent means
    // "everything, fitted", which is where a chart starts.
    if (viewport) request.viewport = viewport;
    if (gesture) request.gesture = gesture;

    scene = buildScene(request);
    viewport = scene.viewport;
    el("chartNote").textContent = scene.note || "";
    renderFootprintStats(scene.footprint);
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
    else if (finished.mode === "move") finishMoving(finished.target.drawing);
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
  /// screen and the window is still the one the user chose.
  function resetViewport() {
    viewport = null;
  }

  // ---------------------------------------------------------------------------
  // Chart drawings: placing, moving, storing
  //
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
  function finishPlacing() {
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
    void createDrawing(resolved);
  }

  /// Store a drawing the user has just moved.
  function finishMoving(id) {
    renderNow();
    const resolved = resolvedDrawing(id);
    const stored = drawings.find((d) => d.id === id);
    if (!resolved || !stored) return;

    // The list takes the engine's numbers, so what is on screen and what is about
    // to be stored are the same two points rather than two roundings of one.
    stored.a1 = resolved.a1;
    stored.a2 = resolved.a2;
    void putDrawing(stored);
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

  /// Remove the selected drawing.
  async function deleteSelected() {
    const id = selectedDrawing;
    if (!id) return;
    // Off the chart first and out of the database second. The managed instance
    // costs about a second a statement, and a delete button that does nothing for
    // a second reads as broken.
    selectedDrawing = null;
    drawings = drawings.filter((d) => d.id !== id);
    renderNow();
    try {
      await api(`/drawings/${id}`, { method: "DELETE" });
    } catch (e) {
      note(`the drawing left the chart but not the database: ${e.message}`);
    }
  }

  /// Remove every drawing on this symbol.
  async function clearDrawings() {
    const ids = drawings.map((d) => d.id);
    if (!ids.length) return;
    selectedDrawing = null;
    placing = null;
    drawings = [];
    renderNow();
    // One at a time. A burst of concurrent deletes over a ten-connection pool is
    // how a request path starts failing for somebody else, and nothing here is in
    // a hurry.
    for (const id of ids) {
      try {
        await api(`/drawings/${id}`, { method: "DELETE" });
      } catch (e) {
        note(`some drawings were not removed from the database: ${e.message}`);
        return;
      }
    }
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
  // ---------------------------------------------------------------------------
  // This pane's own listeners
  // ---------------------------------------------------------------------------

  /// Attach the listeners that are about *this* pane: its canvas, its tool
  /// buttons, its series selects, its own reload. The two that are not -- the
  /// keyboard and the resize -- stay on the window and are routed to the active
  /// pane, because a keyboard has no way to say which canvas it means.
  function wire() {
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
    el("mode").addEventListener("change", () => { refresh(); });
    // These change the series itself, so the window means nothing afterwards -- a
    // bar index into the old series is not a bar in the new one, and the limit
    // select changes how many exist at all.
    el("timeframe").addEventListener("change", () => {
      resetViewport();
      refresh().then(connectLive);
    });
    el("symbol").addEventListener("change", () => {
      // A different instrument has different timeframes, so the list is rebuilt
      // before the fetch that reads the chosen one.
      fillTimeframes(el("symbol").value);
      resetViewport();
      refresh().then(connectLive);
      if (hooks.onSymbolChange) hooks.onSymbolChange(paneApi);
    });
    el("fit").addEventListener("click", () => applyGesture({ kind: "fit" }));
    for (const button of root.querySelectorAll(".tools button[data-tool]")) {
      button.addEventListener("click", () => selectTool(button.dataset.tool));
    }
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
  /// Ordered by length, because the server's order is its own -- alphabetical,
  /// as it happens, which reads `15m, 1h, 1m, 4h, 5m`. That is not a ladder
  /// anyone can use, and it makes "the next timeframe up" mean nothing.
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
    },

    /// Put a freshly cloned pane back to its starting state.
    ///
    /// A clone carries whatever the pane it came from was showing -- the active
    /// outline, the note the user was reading, the footprint stats, the tool. A
    /// new chart that starts by claiming something about itself is worse than one
    /// that starts blank, so each of them is cleared rather than inherited.
    reset() {
      root.classList.remove("active");
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

    /// Take this pane off the page. Its own listeners go with its elements; the
    /// two things that outlive them are the channel and a frame waiting for one.
    destroy() {
      if (socket) socket.close();
      if (gestureFrame) {
        cancelAnimationFrame(gestureFrame);
        gestureFrame = 0;
      }
      socket = null;
    },
  };

  wire();
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
  el("charts").appendChild(node);

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
  pane.destroy();
  pane.root.remove();
  panes.splice(at, 1);
  refreshCloseButtons();

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
}

/// Build the first pane from the markup, and make it the active one.
function openFirstPane() {
  const node = el("charts").querySelector(".chartPane");
  const pane = createChartPane(node, {
    onSymbolChange: (which) => { if (which === activePane) connectBook(); },
  });
  panes.push(pane);
  setActive(pane);
  return pane;
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
  if (bookSocket) bookSocket.close();
  // The book follows the *active* pane, because there is one book and one aside.
  // A pane that is not active has no claim on it, however recently it changed.
  const symbol = activePane ? activePane.symbol() : "";
  if (!symbol) return;
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
        symbol: activeSymbol(),
        question,
        timeframes: [activeTimeframe()],
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
// Wiring
// ---------------------------------------------------------------------------

function selectPane(name) {
  for (const button of document.querySelectorAll(".tabs button")) {
    button.setAttribute("aria-selected", String(button.dataset.pane === name));
  }
  for (const pane of document.querySelectorAll(".pane")) {
    pane.hidden = pane.id !== `pane-${name}`;
  }
  // The live-trading panel is read when it is opened rather than at startup:
  // opt-in state is changed from elsewhere (the API, another tab) and a value
  // cached at page load would show a venue as revoked after it was re-enabled.
  if (name === "bots") refreshVenues();
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

  // -------------------------------------------------------------------------
  // The page: a list of panes, and which one the aside is about
  // -------------------------------------------------------------------------

  // The first pane, built from the markup. Before the engine is loaded, because a
  // failed engine load is something a pane has to be able to say.
  openFirstPane();
  refreshCloseButtons();

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
  // drawing in progress first, and the tool after that. Bound to the window
  // rather than to a canvas, because a canvas is not focusable: a keyboard user
  // would have to click it first, and a click on the chart is already a
  // selection. Routed to the *active* pane for the same reason a keyboard has no
  // way to say which canvas it means -- which is why touching a pane marks it.
  window.addEventListener("keydown", (event) => {
    // Not while the user is typing. The strategy editor and the question box are
    // on the same page, and a Backspace in a textarea has to delete a character.
    const tag = document.activeElement && document.activeElement.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA") return;
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

  if (activePane.symbol()) {
    await activePane.refresh();
    activePane.connectLive();
  }
  // The DOM opens its own socket rather than riding the chart's: the two have
  // different reconnection stories, and the book has to be able to say "no
  // depth feed" without the chart looking broken.
  connectBook();
}

main();
