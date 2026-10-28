/* The instrument bar: one strip of controls for the active chart.
 *
 * A projection, not a second source of truth. Every value it shows is read
 * back from the active pane -- `HRZApp.activePaneInfo()` -- and every control
 * it draws calls a named command on that pane. The pane's own selects and
 * buttons stay where they were, hidden in `.chartBar` and hosted by the
 * right-click menu, and the two cannot disagree: both drive the same `apply*`
 * functions inside the pane and both read the same selects back.
 *
 * The strip narrows by *moving* groups into the More panel, never by cloning
 * them. `#split` and the layout selects belong to `app.js` and carry its
 * listeners; a copy would be a control that looks live and does nothing.
 *
 * Loaded before `app.js` (it only defines `window.InstrumentBar`), initialised
 * from `main()`, and driven afterwards by `hrz:pane` -- the same event the
 * status bar listens to -- and by clicks.
 */
(function () {
  "use strict";

  // The bar's preferred ladder. A frame appears only when the pane actually
  // has it, so the bar can never offer a rung the chart cannot fill; the
  // active frame is swapped in for the last one when it is not on this list.
  const FRAMES = ["1m", "5m", "15m", "1h", "4h"];
  // Short labels for the chart types. Anything not named here uses the pane's
  // own option text, so a mode added to the select still reaches the bar.
  const MODE_LABELS = {
    candles: "Candles", heikin_ashi: "H-A", bars: "Bars",
    line: "Line", area: "Area", footprint: "Footprint",
  };
  const SEARCH_DELAY = 160;
  const SEARCH_SHOWN = 12;
  // Where the groups go, in the order the spec collapses them: Indicators and
  // Panes under 900px, then View under 720. Symbol, timeframe, Fit and the
  // panel toggles never collapse.
  const WIDE_MIN = 900;
  const TIGHT_BELOW = 720;
  // The priority order from the spec: Indicators and Panes go first, View
  // last -- at the tight width all three are out. More lists them in that
  // order, so the control that just left the bar is the first thing in the
  // panel that replaced it.
  const GROUP_ORDER = ["indicators", "panes", "view"];
  const COLLAPSED_COMPACT = ["indicators", "panes"];

  const app = () => window.HRZApp || {};
  const shell = () => window.Shell || {};
  const info = () => (app().activePaneInfo ? app().activePaneInfo() : null);
  const byId = (id) => document.getElementById(id);

  const state = {
    open: null,      // the panel currently shown, by id
    trigger: null,   // the button that opened it
    searchTimer: 0,
    searchSeq: 0,
    frame: 0,        // the pending repaint
    moved: new Set(),// the groups currently inside More
  };

  let wired = false;
  let observer = null;
  let framesSig = "";
  let modesSig = "";

  /* ---- Panels -------------------------------------------------------------- */

  function place(panel, trigger) {
    if (!panel || !trigger) return;
    const rect = trigger.getBoundingClientRect();
    const width = panel.offsetWidth || 260;
    const height = panel.offsetHeight || 240;
    const left = Math.max(8, Math.min(window.innerWidth - width - 8, rect.left));
    const below = rect.bottom + 6;
    const flip = below + height > window.innerHeight - 8;
    const top = flip ? Math.max(8, rect.top - height - 6) : below;
    panel.style.setProperty("--pop-left", `${Math.round(left)}px`);
    panel.style.setProperty("--pop-top", `${Math.round(top)}px`);
  }

  function reposition() {
    if (!state.open) return;
    place(byId(state.open), state.trigger);
  }

  function show(panelId, trigger) {
    if (!trigger) return;
    if (state.open && state.open !== panelId) close();
    const panel = byId(panelId);
    if (!panel) return;
    state.open = panelId;
    state.trigger = trigger;
    trigger.setAttribute("aria-expanded", "true");
    panel.hidden = false;
    place(panel, trigger);
  }

  function close() {
    if (!state.open) return;
    const panel = byId(state.open);
    // Whether the caret is inside the panel has to be read before it goes: a
    // hidden field that still holds focus is a focus that no longer exists, and
    // the keyboard user who just picked something should be standing on the
    // control they opened rather than on a node that is no longer drawn.
    const inside = Boolean(panel) && panel.contains(document.activeElement);
    if (panel) panel.hidden = true;
    if (state.trigger) state.trigger.setAttribute("aria-expanded", "false");
    if (inside && state.trigger) state.trigger.focus();
    state.open = null;
    state.trigger = null;
  }

  function toggle(panelId, triggerId) {
    if (state.open === panelId) {
      close();
      return;
    }
    const trigger = byId(triggerId);
    if (!trigger) return;
    show(panelId, trigger);
    if (panelId === "layersPop") renderLayers(info());
    if (panelId === "attachPop") loadAttach();
    if (panelId === "framesPop") renderFrames();
    if (panelId === "symbolPop") focusSearch();
  }

  function onPointerDown(event) {
    if (!state.open) return;
    const panel = byId(state.open);
    if (panel && panel.contains(event.target)) return;
    // A press on the trigger is the toggle's own gesture; closing here would
    // make the button reopen the panel it just closed.
    if (state.trigger && state.trigger.contains(event.target)) return;
    close();
  }

  function onKeydown(event) {
    if (!state.open || event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    const trigger = state.trigger;
    close();
    if (trigger) trigger.focus();
  }

  /* ---- Reading the pane ---------------------------------------------------- */

  function framesFor(current) {
    const available = new Set(current.frames.map((frame) => frame.value));
    const shown = FRAMES.filter((frame) => available.has(frame));
    if (current.timeframe && !shown.includes(current.timeframe)) {
      // The active frame is always visible, even when it is not one of the
      // five the bar prefers (3m, 30m, 1d). It takes the last slot, next to
      // the overflow button, which is where "the one you are on" belongs.
      if (shown.length >= FRAMES.length) shown.splice(shown.length - 1, 1, current.timeframe);
      else shown.push(current.timeframe);
    }
    return shown;
  }

  function schedule() {
    if (state.frame) return;
    state.frame = requestAnimationFrame(() => {
      state.frame = 0;
      sync();
    });
  }

  function sync() {
    const node = byId("instrumentBar");
    if (!node) return;
    const current = info();
    node.dataset.state = current ? "ready" : "empty";
    if (!current) return;

    const name = byId("barSymbolName");
    if (name) name.textContent = current.symbol || "—";

    paintFrames(current);
    paintModes(current);

    const bars = byId("barBars");
    if (bars && String(bars.value) !== String(current.bars)) bars.value = String(current.bars);

    const zones = byId("barZones");
    if (zones) zones.setAttribute("aria-pressed", String(Boolean(current.zones)));

    const maximize = byId("barMaximize");
    if (maximize) {
      maximize.setAttribute("aria-pressed", String(Boolean(current.maximized)));
      maximize.title = current.maximized ? "Back to the grid" : "Expand this chart";
      maximize.setAttribute("aria-label", maximize.title);
    }

    paintIndicator(current);
    if (state.open === "layersPop") renderLayers(current);
    if (state.open === "framesPop") renderFrames();
    if (state.open) reposition();
  }

  function paintFrames(current) {
    const host = byId("barFrames");
    if (!host) return;
    const shown = framesFor(current);
    const sig = shown.join(",");
    if (sig !== framesSig) {
      framesSig = sig;
      host.textContent = "";
      for (const frame of shown) {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "hrz-seg__item";
        button.dataset.frame = frame;
        button.textContent = frame;
        host.appendChild(button);
      }
    }
    for (const button of host.children) {
      button.setAttribute("aria-pressed", String(button.dataset.frame === current.timeframe));
    }
  }

  function paintModes(current) {
    const host = byId("barModes");
    if (!host) return;
    const sig = current.modes.map((mode) => mode.value).join(",");
    if (sig !== modesSig) {
      modesSig = sig;
      host.textContent = "";
      for (const mode of current.modes) {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "hrz-seg__item";
        button.dataset.mode = mode.value;
        button.textContent = MODE_LABELS[mode.value] || mode.label;
        host.appendChild(button);
      }
    }
    for (const button of host.children) {
      const active = button.dataset.mode === current.mode;
      button.setAttribute("aria-pressed", String(active));
      // Heikin-Ashi is a real mode with a narrow audience; it stays reachable
      // through the pane's own chart-type select, and the bar shows it only
      // while this chart is in it -- so a mode can never be silently applied
      // by a chart the user is not looking at.
      if (button.dataset.mode === "heikin_ashi") button.hidden = !active;
    }
  }

  function paintIndicator(current) {
    const name = byId("barIndicatorName");
    const chip = byId("barIndicator");
    const detach = byId("barIndicatorDetach");
    const layers = byId("barLayers");
    const count = (current.layers || []).length;
    let label = "";
    if (current.indicator) {
      label = current.indicator.name + (current.indicator.live ? " · live" : "");
    }
    if (count) label = label ? `${label} +${count}` : `${count} layer${count === 1 ? "" : "s"}`;
    if (name) name.textContent = label;
    if (chip) chip.hidden = !label;
    if (detach) {
      detach.hidden = !current.indicator;
      detach.title = current.indicator ? `Detach ${current.indicator.name}` : "";
    }
    if (layers) {
      layers.hidden = count === 0;
      layers.textContent = `Layers (${count})`;
    }
  }

  /* ---- The panels' contents ------------------------------------------------ */

  function focusSearch() {
    const input = byId("symbolPopInput");
    if (input) input.focus();
  }

  function renderFrames() {
    const list = byId("framesPopList");
    const current = info();
    if (!list || !current) return;
    list.textContent = "";
    for (const frame of current.frames) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "barPopItem";
      button.dataset.frame = frame.value;
      button.setAttribute("aria-pressed", String(frame.value === current.timeframe));
      const name = document.createElement("span");
      name.className = "barPopMain";
      name.textContent = frame.label;
      button.appendChild(name);
      button.addEventListener("click", () => {
        close();
        if (app().setActiveTimeframe) app().setActiveTimeframe(frame.value);
      });
      list.appendChild(button);
    }
    reposition();
  }

  function renderLayers(current) {
    const list = byId("layersPopList");
    if (!list) return;
    list.textContent = "";
    const layers = (current && current.layers) || [];
    if (!layers.length) {
      const empty = document.createElement("p");
      empty.className = "barPopNote";
      empty.textContent = "No script layers on this chart. Attach one from Studio.";
      list.appendChild(empty);
      return;
    }
    for (const layer of layers) {
      const row = document.createElement("div");
      row.className = "barPopRow";
      const name = document.createElement("span");
      name.className = "barPopMain";
      name.textContent = layer.name;
      const eye = document.createElement("button");
      eye.type = "button";
      eye.className = "hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon";
      eye.setAttribute("aria-pressed", String(layer.visible));
      eye.setAttribute("aria-label", layer.visible ? `Hide ${layer.name}` : `Show ${layer.name}`);
      eye.title = layer.visible ? "Hide this layer" : "Show this layer";
      eye.appendChild(window.HRZUI.icon(layer.visible ? "eye" : "eye-off", { size: 14 }));
      eye.addEventListener("click", () => {
        if (app().setLayerVisible) app().setLayerVisible(layer.index, !layer.visible);
      });
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon";
      remove.setAttribute("aria-label", `Remove ${layer.name}`);
      remove.title = "Remove this layer";
      remove.appendChild(window.HRZUI.icon("trash", { size: 14 }));
      remove.addEventListener("click", () => {
        if (app().removeLayer) app().removeLayer(layer.index);
      });
      row.append(name, eye, remove);
      list.appendChild(row);
    }
  }

  async function loadAttach() {
    const list = byId("attachPopList");
    const note = byId("attachPopNote");
    if (!list || !note) return;
    list.textContent = "";
    note.textContent = "Reading your indicators…";
    let workspaces = [];
    try {
      workspaces = (await app().workspaces()) || [];
    } catch (error) {
      if (state.open === "attachPop") note.textContent = `Could not read your indicators: ${error.message}`;
      return;
    }
    if (state.open !== "attachPop") return;
    const attachable = workspaces.filter((ws) => ws.active_revision_id);
    note.textContent = attachable.length ? "" : "No indicator has an active revision yet.";
    for (const ws of attachable) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "barPopItem";
      button.dataset.workspace = ws.id;
      const name = document.createElement("span");
      name.className = "barPopMain";
      name.textContent = ws.name;
      const meta = document.createElement("span");
      meta.className = "barPopMeta";
      meta.textContent = `${ws.symbol} · ${ws.timeframe}`;
      button.append(name, meta);
      button.addEventListener("click", async () => {
        close();
        const message = await app().attachWorkspace(ws.id);
        if (message) {
          window.HRZUI.toast(message, {
            tone: message.startsWith("Could not") ? "error" : "info",
          });
        }
      });
      list.appendChild(button);
    }
    if (state.open === "attachPop") reposition();
  }

  function pickSymbol(symbol) {
    close();
    if (app().setActiveSymbol) app().setActiveSymbol(symbol);
  }

  async function runSearch(query) {
    const seq = ++state.searchSeq;
    const list = byId("symbolPopList");
    const note = byId("symbolPopNote");
    if (!list) return;
    let results = [];
    try {
      results = await app().searchSymbols(query);
    } catch (error) {
      if (seq === state.searchSeq && state.open === "symbolPop") {
        note.textContent = `Search failed: ${error.message}`;
      }
      return;
    }
    // A stale answer is dropped, not shown: two keystrokes can land out of
    // order, and a list for "bt" under "eth" would chart the wrong thing.
    if (seq !== state.searchSeq || state.open !== "symbolPop") return;
    list.textContent = "";
    for (const instrument of results.slice(0, SEARCH_SHOWN)) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "barPopItem";
      button.dataset.symbol = instrument.symbol;
      const name = document.createElement("span");
      name.className = "barPopMain";
      name.textContent = instrument.symbol;
      const meta = document.createElement("span");
      meta.className = "barPopMeta";
      meta.textContent = instrument.trading
        ? `${instrument.base} / ${instrument.quote}`
        : "not trading";
      button.append(name, meta);
      button.addEventListener("click", () => pickSymbol(instrument.symbol));
      list.appendChild(button);
    }
    note.textContent = results.length ? "" : "No instrument matches that.";
    reposition();
  }

  /* ---- Narrowing ----------------------------------------------------------- */

  function collapsedFor(mode) {
    if (mode === "tight") return GROUP_ORDER;
    if (mode === "compact") return COLLAPSED_COMPACT;
    return [];
  }

  function moveGroup(name, group) {
    const more = byId("moreGroups");
    if (!more) return;
    if (state.trigger && group.contains(state.trigger)) close();
    const marker = document.createElement("span");
    marker.className = "barMarker";
    marker.dataset.barMarker = name;
    group.parentElement.insertBefore(marker, group.nextSibling);
    more.appendChild(group);
    state.moved.add(name);
  }

  function restoreGroup(name, group) {
    const marker = document.querySelector(`#instrumentBar [data-bar-marker="${name}"]`);
    if (state.trigger && group.contains(state.trigger)) close();
    if (marker) marker.replaceWith(group);
    else byId("instrumentBar").appendChild(group);
    state.moved.delete(name);
  }

  function collapse(mode) {
    const wanted = collapsedFor(mode);
    for (const name of GROUP_ORDER) {
      // Queried from the document, not from the bar: a group that is already
      // inside More is not a descendant of `#instrumentBar` any more, and a
      // bar-scoped lookup would leave it there forever -- the strip could get
      // wider without ever getting its controls back.
      const group = document.querySelector(`#instrumentBar [data-bar-group="${name}"], #moreGroups [data-bar-group="${name}"]`);
      if (!group) continue;
      const shouldMove = wanted.includes(name);
      if (shouldMove && !state.moved.has(name)) moveGroup(name, group);
      else if (!shouldMove && state.moved.has(name)) restoreGroup(name, group);
    }
    const section = byId("moreSection");
    if (section) section.hidden = state.moved.size === 0;
    const more = byId("chartMoreBtn");
    if (more) more.title = state.moved.size ? "More controls" : "More";
  }

  function measure() {
    const node = byId("instrumentBar");
    const column = byId("chartColumn");
    if (!node || !column) return;
    // The bar's own box, not the column's: `#chartColumn` wraps the whole chart
    // area and jsdom lays nothing out, so its `clientWidth` is 0 in the harness
    // while the bar still reports its authored width. A zero means "not laid
    // out yet" -- a hidden frame at boot, or a headless DOM -- and collapsing on
    // that number would move the groups into More on a wide screen and never
    // move them back.
    const width = node.clientWidth || column.clientWidth || 0;
    const mode = !width || width >= WIDE_MIN ? "wide" : width >= TIGHT_BELOW ? "compact" : "tight";
    if (node.dataset.width === mode) return;
    node.dataset.width = mode;
    collapse(mode);
  }

  function observe() {
    const column = byId("chartColumn");
    if (!column) return;
    if (typeof ResizeObserver === "function") {
      observer = new ResizeObserver(() => measure());
      observer.observe(column);
    } else {
      // No ResizeObserver (jsdom): the window's resize asks the same question
      // less precisely, and the harness calls `measure()` directly.
      window.addEventListener("resize", measure);
    }
  }

  /* ---- Wiring -------------------------------------------------------------- */

  function wire() {
    if (wired) return;
    wired = true;

    byId("barSymbol").addEventListener("click", () => toggle("symbolPop", "barSymbol"));
    byId("barFrameMore").addEventListener("click", () => toggle("framesPop", "barFrameMore"));
    byId("barAttach").addEventListener("click", () => toggle("attachPop", "barAttach"));
    byId("barLayers").addEventListener("click", () => toggle("layersPop", "barLayers"));
    byId("barLayout").addEventListener("click", () => toggle("layoutPop", "barLayout"));
    byId("chartMoreBtn").addEventListener("click", () => {
      toggle("chartMore", "chartMoreBtn");
      if (state.open === "chartMore") reposition();
    });

    byId("barBars").addEventListener("change", (event) => {
      if (app().setActiveBars) app().setActiveBars(Number(event.currentTarget.value));
    });
    byId("barFrames").addEventListener("click", (event) => {
      const button = event.target.closest("button[data-frame]");
      if (button && app().setActiveTimeframe) app().setActiveTimeframe(button.dataset.frame);
    });
    byId("barModes").addEventListener("click", (event) => {
      const button = event.target.closest("button[data-mode]");
      if (button && app().setActiveMode) app().setActiveMode(button.dataset.mode);
    });
    byId("barZones").addEventListener("click", () => {
      const on = byId("barZones").getAttribute("aria-pressed") !== "true";
      if (app().setActiveZones) app().setActiveZones(on);
    });
    byId("barFit").addEventListener("click", () => {
      if (app().fitActivePane) app().fitActivePane();
    });
    byId("barReload").addEventListener("click", () => {
      if (app().reloadActivePane) app().reloadActivePane();
    });
    byId("barMaximize").addEventListener("click", () => {
      if (app().toggleMaximize) app().toggleMaximize();
    });
    byId("barIndicatorDetach").addEventListener("click", () => {
      if (app().detachActiveIndicator) app().detachActiveIndicator();
    });

    byId("attachStudio").addEventListener("click", () => {
      close();
      if (shell().go) shell().go("studio", { studioPane: "indicator" });
    });
    byId("attachCommunity").addEventListener("click", () => {
      close();
      if (shell().go) shell().go("community");
    });
    byId("moreShot").addEventListener("click", async () => {
      close();
      const message = await app().saveScreenshot();
      if (message) {
        window.HRZUI.toast(message, {
          tone: message.startsWith("Saved") ? "info" : "error",
        });
      }
    });
    byId("moreKeys").addEventListener("click", () => {
      close();
      if (window.HRZPalette) window.HRZPalette.shortcuts();
    });
    // Deliberately no `close()`: you flip the colour scheme, look at the chart
    // behind the panel, and flip it back if it is worse.
    byId("moreCvd").addEventListener("click", () => {
      const on = byId("moreCvd").getAttribute("aria-pressed") !== "true";
      if (app().setChartSetting) app().setChartSetting("cvd", on);
    });
    byId("moreChartSettings").addEventListener("click", () => {
      close();
      if (app().openChartSettings) app().openChartSettings();
    });

    const input = byId("symbolPopInput");
    input.addEventListener("input", () => {
      const query = input.value.trim();
      clearTimeout(state.searchTimer);
      if (!query) {
        byId("symbolPopList").textContent = "";
        byId("symbolPopNote").textContent = "";
        return;
      }
      state.searchTimer = setTimeout(() => runSearch(query), SEARCH_DELAY);
    });
    input.addEventListener("keydown", (event) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      const first = document.querySelector("#symbolPopList [data-symbol]");
      if (first) pickSymbol(first.dataset.symbol);
    });

    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeydown);
    document.addEventListener("hrz:pane", schedule);
    document.addEventListener("hrz:auth", () => {
      measure();
      schedule();
    });
    window.addEventListener("resize", close);
  }

  function init() {
    if (!byId("instrumentBar")) return;
    wire();
    observe();
    measure();
    sync();
  }

  window.InstrumentBar = {
    init,
    sync,
    measure,
    close,
  };
})();
