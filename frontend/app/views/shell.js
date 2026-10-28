/* The shell: destinations, the hash router, and one host for every pane.
 *
 * `app.js` owns the instruments -- charts, streams, panes and the panels that
 * read them. This file owns the page they sit on: which destination shows,
 * which inspector tab, whether the watchlist, inspector and dock are open,
 * and where each panel is mounted. Panels move between hosts; they are never
 * cloned, so the listeners `app.js` bound to them keep firing and there is
 * never a second conversation or a second builder in the DOM.
 *
 * Loaded before `app.js` (it only defines `window.Shell`), initialised and
 * started from `main()`, and driven afterwards by hash changes and clicks.
 * It reaches the app through the `HRZApp` facade, so the dependency runs one
 * way: this file never touches `panes`, `token` or `billingState`.
 */
(function () {
  "use strict";

  const DESTINATIONS = ["dashboard", "charts", "studio", "community", "bots", "account"];
  const TITLES = {
    dashboard: "Dashboard", charts: "Charts", studio: "Studio",
    community: "Community", bots: "Bots", account: "Account",
  };
  const INSPECTOR_TABS = ["ai", "indicator", "book", "trade", "details"];
  const INSPECTOR_VIEWS = {
    ai: "pane-chat", indicator: "inspectorIndicator",
    book: "pane-book", trade: "pane-trade", details: "pane-thesis",
  };
  const TAB_FROM_LEGACY = { chat: "ai", thesis: "details", book: "book", trade: "trade" };
  const LEGACY_FROM_TAB = { ai: "chat", book: "book", trade: "trade", details: "thesis" };
  const STUDIO_PANES = {
    indicator: "pane-indicator", scan: "pane-scan",
    strategy: "pane-strategy", skills: "pane-skills",
  };
  const ACCOUNT_SECTIONS = ["plan", "brokers", "ai", "profile"];
  const ACCOUNT_PANE = { plan: "pane-billing", brokers: "pane-brokers", ai: "pane-aimodel" };
  const ACCOUNT_FROM_LEGACY = { billing: "plan", brokers: "brokers", aimodel: "ai" };
  const LEGACY_FROM_ACCOUNT = { plan: "billing", brokers: "brokers", ai: "aimodel", profile: "profile" };
  const DEST_FROM_LEGACY = {
    chat: "charts", thesis: "charts", book: "charts", trade: "charts",
    watchlist: "charts",
    indicator: "studio", scan: "studio", strategy: "studio", skills: "studio",
    community: "community", bots: "bots",
    billing: "account", brokers: "account", aimodel: "account",
  };
  const HOSTS = {
    chat: "inspectorPaneHost", book: "inspectorPaneHost",
    trade: "inspectorPaneHost", thesis: "inspectorPaneHost",
    indicator: "studioHost", scan: "studioHost",
    strategy: "studioHost", skills: "studioHost",
    community: "communityHost", bots: "botsHost",
    billing: "accountHost", brokers: "accountHost", aimodel: "accountHost",
  };
  const DOCK_TABS = ["studio", "alerts", "orders", "console"];
  const DOCK_PLACEHOLDER = {
    alerts: "Watch monitors and recent watch events arrive in M4.",
    orders: "Today's practice fills and open positions arrive in M4.",
    console: "The client event log arrives in M4.",
  };
  const DOCK_DEFAULT_HEIGHT = 240;
  const DOCK_MIN_HEIGHT = 160;

  const state = {
    destination: null,
    inspectorTab: "ai",
    inspectorOpen: true,
    watchlistCollapsed: false,
    dockOpen: false,
    dockTab: "studio",
    dockHeight: DOCK_DEFAULT_HEIGHT,
    studioPane: "indicator",
    accountSection: "plan",
  };

  let ready = false;
  let started = false;
  let applyingRestore = false;
  let drawer = null;
  // The chat the current route already asked the app to open. `go` applies the
  // destination directly and the hashchange applies it again a moment later;
  // without this, one link is two loads of the same conversation.
  let openedWorkspaceId = "";

  const el = (id) => document.getElementById(id);
  const app = () => window.HRZApp || {};
  const isSignedIn = () => Boolean(app().signedIn && app().signedIn());
  const clamp = (value, lo, hi) => Math.min(hi, Math.max(lo, value));

  const managed = () =>
    [...document.querySelectorAll(".pane")].filter((pane) => pane.id !== "pane-watchlist");

  function clampDock(value) {
    const max = Math.max(DOCK_MIN_HEIGHT, Math.round(window.innerHeight * 0.45));
    return clamp(Math.round(Number(value) || DOCK_DEFAULT_HEIGHT), DOCK_MIN_HEIGHT, max);
  }

  function applyDockHeight() {
    document.documentElement.style.setProperty("--dock-h", `${state.dockHeight}px`);
  }

  function setWidth(property, value, lo, hi) {
    if (!Number.isFinite(value)) return;
    document.documentElement.style.setProperty(property, `${clamp(Math.round(value), lo, hi)}px`);
  }

  /* ---- Mounting ----------------------------------------------------------- */

  function mount() {
    for (const [name, hostId] of Object.entries(HOSTS)) {
      const pane = el(`pane-${name}`);
      const host = el(hostId);
      if (pane && host && pane.parentElement !== host) host.appendChild(pane);
    }
    const dockBody = el("dockBody");
    if (dockBody && !el("dockPanel")) {
      const panel = document.createElement("div");
      panel.id = "dockPanel";
      panel.className = "dockPlaceholder";
      panel.hidden = true;
      dockBody.appendChild(panel);
    }
    for (const pane of managed()) pane.hidden = true;
  }

  function mountIndicator() {
    const pane = el("pane-indicator");
    if (!pane) return;
    const inDock = state.destination === "charts" && state.dockOpen && state.dockTab === "studio";
    const host = el(inDock ? "dockBody" : "studioHost");
    if (host && pane.parentElement !== host) host.appendChild(pane);
  }

  /* ---- Visibility --------------------------------------------------------- */

  function showPanes() {
    const visible = new Set();
    if (state.destination === "charts") {
      if (state.inspectorOpen) visible.add(INSPECTOR_VIEWS[state.inspectorTab]);
      if (state.dockOpen && state.dockTab === "studio") visible.add("pane-indicator");
    } else if (state.destination === "studio") {
      visible.add(STUDIO_PANES[state.studioPane]);
    } else if (state.destination === "account") {
      visible.add(ACCOUNT_PANE[state.accountSection] || "accountProfile");
    } else if (state.destination === "community") {
      visible.add("pane-community");
    } else if (state.destination === "bots") {
      visible.add("pane-bots");
    }
    for (const pane of managed()) {
      // A pane moved into the AI drawer is on screen wherever its host says.
      const inDrawer = drawer && drawer.element().contains(pane);
      pane.hidden = inDrawer ? false : !visible.has(pane.id);
    }
    // The projection panel is not a `.pane` (it is a view of the instrument,
    // not a panel of its own), so it is toggled by the same visible set.
    const projection = el("inspectorIndicator");
    if (projection) projection.hidden = !visible.has("inspectorIndicator");
    const profile = el("accountProfile");
    if (profile) profile.hidden = !visible.has("accountProfile");
  }

  function syncWatchlist() {
    const rail = el("sideWatch");
    const splitter = el("splitWatch");
    const reveal = el("railShow");
    if (rail) rail.classList.toggle("hideRail", state.watchlistCollapsed);
    if (splitter) splitter.classList.toggle("hideRail", state.watchlistCollapsed);
    if (reveal) reveal.hidden = !state.watchlistCollapsed;
    const toggle = el("watchlistToggle");
    if (toggle) toggle.setAttribute("aria-pressed", String(state.watchlistCollapsed));
  }

  function syncInspector() {
    const open = state.destination === "charts" && state.inspectorOpen;
    const host = el("inspectorHost");
    const splitter = el("splitAside");
    const reveal = el("asideShow");
    if (host) host.hidden = !open;
    if (splitter) splitter.hidden = !open;
    if (reveal) reveal.hidden = open || state.destination !== "charts";
    for (const button of document.querySelectorAll("#inspectorTabs [data-inspector]")) {
      button.setAttribute("aria-selected", String(button.dataset.inspector === state.inspectorTab));
    }
    const ai = el("aiBtn");
    if (ai) ai.setAttribute("aria-pressed", String(open && state.inspectorTab === "ai"));
    const toggle = el("inspectorToggle");
    if (toggle) toggle.setAttribute("aria-pressed", String(open));
  }

  function syncStudio() {
    for (const button of document.querySelectorAll("#studioTabs [data-studio]")) {
      button.setAttribute("aria-selected", String(button.dataset.studio === state.studioPane));
    }
  }

  function syncAccount() {
    for (const button of document.querySelectorAll("#accountTabs [data-account]")) {
      button.setAttribute("aria-selected", String(button.dataset.account === state.accountSection));
    }
  }

  function syncDock() {
    const open = state.destination === "charts" && state.dockOpen;
    const dock = el("dock");
    if (dock) dock.hidden = !open;
    applyDockHeight();
    for (const button of document.querySelectorAll("#dock [data-dock]")) {
      button.setAttribute("aria-selected", String(button.dataset.dock === state.dockTab));
    }
    const panel = el("dockPanel");
    if (panel) {
      panel.hidden = state.dockTab === "studio";
      if (!panel.hidden) panel.textContent = DOCK_PLACEHOLDER[state.dockTab] || "";
    }
    const toggle = el("dockToggle");
    if (toggle) toggle.setAttribute("aria-pressed", String(open));
  }

  function showDestination() {
    for (const destination of DESTINATIONS) {
      const section = el(`dest-${destination}`);
      if (section) section.hidden = destination !== state.destination;
    }
    const title = el("destTitle");
    if (title) {
      title.textContent = TITLES[state.destination] || "";
      title.hidden = !state.destination;
    }
    for (const button of document.querySelectorAll("#rail [data-dest]")) {
      if (button.dataset.dest === state.destination) button.setAttribute("aria-current", "page");
      else button.removeAttribute("aria-current");
    }
    syncWatchlist();
    syncInspector();
    syncStudio();
    syncAccount();
    syncDock();
    mountIndicator();
    showPanes();
    refreshStatus();
    if (state.destination === "charts" && app().resizePanes) app().resizePanes();
  }

  /* ---- Status and chrome -------------------------------------------------- */

  function refreshStatus() {
    const info = app().activePaneInfo ? app().activePaneInfo() : null;
    const symbol = el("sbSymbol");
    if (symbol) symbol.textContent = info && info.symbol ? info.symbol : "—";
    const timeframe = el("sbTimeframe");
    if (timeframe) timeframe.textContent = info && info.timeframe ? info.timeframe : "";
    // The badge's states are the app's own readings -- "live", "stale" and
    // "behind" included -- so the map mirrors them rather than inventing a
    // vocabulary of its own.
    const claim = {
      live: ["Live", "live"],
      stale: ["Quiet", "wait"],
      behind: ["Delayed", "wait"],
      paused: ["Paused", "wait"],
      nofeed: ["No feed", "wait"],
      idle: ["Connecting", "wait"],
      connecting: ["Connecting", "wait"],
      offline: ["Offline", "down"],
    }[(info && info.feed) || "offline"] || ["Offline", "down"];
    const label = el("sbConnLabel");
    if (label) label.textContent = claim[0];
    const dot = el("sbConnection");
    if (dot) dot.dataset.state = claim[1];
    const top = el("connDotTop");
    if (top) top.dataset.state = claim[1];
  }

  function paintProfile() {
    const email = el("profileEmail");
    if (email) email.textContent = (app().email && app().email()) || "—";
    const plan = el("profilePlan");
    if (plan) plan.textContent = (app().planName && app().planName()) || "—";
    const theme = el("profileTheme");
    if (theme && window.Theme) theme.value = window.Theme.preference();
  }

  function syncChrome() {
    const signedIn = isSignedIn();
    for (const button of document.querySelectorAll("#rail [data-dest]")) button.disabled = !signedIn;
    for (const id of [
      "aiBtn",
      "paletteBtn",
      "watchlistToggle",
      "inspectorToggle",
      "dockToggle",
      "railHelp",
    ]) {
      const button = el(id);
      if (button) button.disabled = !signedIn;
    }
    refreshStatus();
    paintProfile();
  }

  function setSaveState(name, text) {
    const node = el("sbSave");
    if (!node) return;
    node.dataset.state = name || "";
    node.textContent = text || "Saved";
  }

  function setPlan(name) {
    const node = el("sbPlan");
    if (node) node.textContent = name || "";
    paintProfile();
  }

  function note() {
    if (app().noteChange) app().noteChange();
  }

  /* ---- Routing ------------------------------------------------------------ */

  function parse(hash) {
    const raw = String(hash || "").replace(/^#\/?/, "");
    const parts = raw.split("/").filter(Boolean);
    if (!parts.length || !DESTINATIONS.includes(parts[0])) return null;
    const route = { destination: parts[0] };
    if (parts[0] === "account") {
      route.section = ACCOUNT_SECTIONS.includes(parts[1]) ? parts[1] : "plan";
    }
    if (parts[0] === "studio" && parts[1]) {
      // An id is user data from the address bar. A malformed escape is a bad
      // link, not a router crash: fall back to the raw segment and let the
      // workspace lookup reject it.
      try {
        route.workspaceId = decodeURIComponent(parts[1]);
      } catch {
        route.workspaceId = parts[1];
      }
    }
    return route;
  }

  function hashFor(destination, opts = {}) {
    if (destination === "account") {
      return `#/account/${opts.section || state.accountSection || "plan"}`;
    }
    if (destination === "studio" && opts.workspaceId) {
      return `#/studio/${encodeURIComponent(opts.workspaceId)}`;
    }
    return `#/${destination}`;
  }

  function setDestination(destination, opts = {}) {
    if (!DESTINATIONS.includes(destination)) destination = "dashboard";
    const previous = state.destination;
    const changed = previous !== destination;
    state.destination = destination;
    let load = changed;
    if (opts.inspectorTab && INSPECTOR_TABS.includes(opts.inspectorTab) &&
        opts.inspectorTab !== state.inspectorTab) {
      state.inspectorTab = opts.inspectorTab;
      state.inspectorOpen = true;
      load = true;
    }
    if (destination === "studio" && opts.studioPane && STUDIO_PANES[opts.studioPane] &&
        opts.studioPane !== state.studioPane) {
      state.studioPane = opts.studioPane;
      load = true;
    }
    if (destination === "studio" && opts.workspaceId) {
      // A chat lives in the builder. A link to one is a link to that tab.
      state.studioPane = "indicator";
      load = true;
    }
    if (destination === "account") {
      const section = opts.section && ACCOUNT_SECTIONS.includes(opts.section)
        ? opts.section
        : changed ? "plan" : state.accountSection;
      if (section !== state.accountSection) {
        state.accountSection = section;
        load = true;
      }
    }
    showDestination();
    // Opening a chat is the indicator loader's own work: it fetches the same
    // list and then selects. Running both would race two lists, and the loser
    // could repaint the list over the conversation the other just opened.
    const openingChat = destination === "studio" && Boolean(opts.workspaceId) &&
      Boolean(app().openWorkspace);
    if (!openingChat) openedWorkspaceId = "";
    if (openingChat) {
      const id = String(opts.workspaceId);
      if (id !== openedWorkspaceId) {
        openedWorkspaceId = id;
        app().openWorkspace(id);
      }
    } else if (load) {
      runDestinationLoaders();
    }
    note();
  }

  function go(destination, opts = {}) {
    if (!DESTINATIONS.includes(destination)) destination = "dashboard";
    setDestination(destination, opts);
    const hash = hashFor(destination, opts);
    if (opts.replace) history.replaceState(null, "", hash);
    else if (location.hash !== hash) location.hash = hash;
  }

  function onHash() {
    if (!started || !isSignedIn()) return;
    const route = parse(location.hash);
    if (!route) {
      history.replaceState(null, "", "#/dashboard");
      setDestination("dashboard");
      return;
    }
    setDestination(route.destination, {
      section: route.section,
      workspaceId: route.workspaceId,
    });
  }

  function runDestinationLoaders() {
    const load = app().runPaneLoaders;
    if (!load) return;
    if (state.destination === "studio") load(state.studioPane);
    else if (state.destination === "community") load("community");
    else if (state.destination === "bots") load("bots");
    else if (state.destination === "account") {
      const legacy = LEGACY_FROM_ACCOUNT[state.accountSection];
      if (legacy) load(legacy);
    }
  }

  /* ---- Panels ------------------------------------------------------------- */

  function selectInspector(tab) {
    if (!INSPECTOR_TABS.includes(tab)) return;
    state.inspectorTab = tab;
    state.inspectorOpen = true;
    showDestination();
    note();
  }

  function setInspectorOpen(open) {
    state.inspectorOpen = Boolean(open);
    showDestination();
    note();
  }

  function setWatchlistCollapsed(collapsed) {
    state.watchlistCollapsed = Boolean(collapsed);
    showDestination();
    note();
  }

  function selectStudio(pane) {
    if (!STUDIO_PANES[pane]) return;
    state.studioPane = pane;
    showDestination();
    if (app().runPaneLoaders) app().runPaneLoaders(pane);
    note();
  }

  function selectAccount(section) {
    if (!ACCOUNT_SECTIONS.includes(section)) return;
    state.accountSection = section;
    showDestination();
    const legacy = LEGACY_FROM_ACCOUNT[section];
    if (legacy && app().runPaneLoaders) app().runPaneLoaders(legacy);
    note();
  }

  function loadDock(tab) {
    if (tab === "studio" && app().runPaneLoaders) app().runPaneLoaders("indicator");
  }

  function selectDockTab(tab) {
    if (!DOCK_TABS.includes(tab)) return;
    state.dockTab = tab;
    state.dockOpen = true;
    showDestination();
    loadDock(tab);
    note();
  }

  function openDock(tab) {
    if (tab && DOCK_TABS.includes(tab)) state.dockTab = tab;
    state.dockOpen = true;
    showDestination();
    loadDock(state.dockTab);
    note();
  }

  function closeDock() {
    state.dockOpen = false;
    showDestination();
    note();
  }

  function openAi() {
    if (state.destination === "charts") {
      state.inspectorTab = "ai";
      state.inspectorOpen = true;
      showDestination();
      note();
      return;
    }
    if (drawer || !window.HRZUI || !window.HRZUI.drawer) return;
    const pane = el("pane-chat");
    if (!pane) return;
    const body = document.createElement("div");
    body.className = "aiDrawerBody";
    body.appendChild(pane);
    pane.hidden = false;
    drawer = window.HRZUI.drawer({
      side: "right",
      title: "AI analyst",
      content: body,
      onClose: () => {
        drawer = null;
        const host = el("inspectorPaneHost");
        if (host) host.appendChild(pane);
        showPanes();
        syncInspector();
      },
    });
    drawer.open();
  }

  /* ---- Legacy entry point ------------------------------------------------- */

  function selectLegacyPane(name) {
    if (!ready || applyingRestore) return;
    const destination = DEST_FROM_LEGACY[name];
    if (!destination) return;
    if (destination === "charts") {
      if (name === "watchlist") state.watchlistCollapsed = false;
      const tab = TAB_FROM_LEGACY[name];
      go("charts", tab ? { inspectorTab: tab } : {});
      return;
    }
    if (destination === "studio") {
      go("studio", { studioPane: name });
      return;
    }
    if (destination === "account") {
      go("account", { section: ACCOUNT_FROM_LEGACY[name] || "plan" });
      return;
    }
    go(destination);
  }

  function legacyTab() {
    if (state.destination === "charts") return LEGACY_FROM_TAB[state.inspectorTab] || "chat";
    if (state.destination === "studio") return state.studioPane;
    if (state.destination === "account") return LEGACY_FROM_ACCOUNT[state.accountSection] || "billing";
    if (state.destination === "community") return "community";
    if (state.destination === "bots") return "bots";
    return "chat";
  }

  /* ---- Snapshot ----------------------------------------------------------- */

  function snapshot() {
    const styles = getComputedStyle(document.documentElement);
    const width = (name) => {
      const value = parseFloat(styles.getPropertyValue(name));
      return Number.isFinite(value) && value > 0 ? Math.round(value) : undefined;
    };
    return {
      destination: state.destination || "dashboard",
      inspectorTab: state.inspectorTab,
      inspectorOpen: state.inspectorOpen,
      watchlistWidth: width("--watch-w"),
      watchlistCollapsed: state.watchlistCollapsed,
      inspectorWidth: width("--inspector-w"),
      dockOpen: state.dockOpen,
      dockTab: state.dockTab,
      dockHeight: state.dockHeight,
    };
  }

  function restore(saved) {
    if (!saved || typeof saved !== "object") return;
    let destination = DESTINATIONS.includes(saved.destination) ? saved.destination : null;
    if (!destination) {
      if (saved.activeTab && DEST_FROM_LEGACY[saved.activeTab]) {
        destination = DEST_FROM_LEGACY[saved.activeTab];
      } else if (Array.isArray(saved.panes) && saved.panes.length) {
        destination = "charts";
      } else {
        destination = "dashboard";
      }
    }
    state.destination = destination;
    const legacy = saved.activeTab;
    if (legacy && destination === "studio" && STUDIO_PANES[legacy]) {
      state.studioPane = legacy;
    }
    if (legacy && destination === "account" && ACCOUNT_FROM_LEGACY[legacy]) {
      state.accountSection = ACCOUNT_FROM_LEGACY[legacy];
    }
    if (INSPECTOR_TABS.includes(saved.inspectorTab)) state.inspectorTab = saved.inspectorTab;
    else if (legacy && TAB_FROM_LEGACY[legacy]) state.inspectorTab = TAB_FROM_LEGACY[legacy];
    if (typeof saved.inspectorOpen === "boolean") state.inspectorOpen = saved.inspectorOpen;
    if (typeof saved.watchlistCollapsed === "boolean") {
      state.watchlistCollapsed = saved.watchlistCollapsed;
    }
    if (typeof saved.dockOpen === "boolean") state.dockOpen = saved.dockOpen;
    if (DOCK_TABS.includes(saved.dockTab)) state.dockTab = saved.dockTab;
    if (Number.isFinite(saved.dockHeight)) state.dockHeight = clampDock(saved.dockHeight);
    setWidth("--watch-w", saved.watchlistWidth, 200, 360);
    setWidth("--inspector-w", saved.inspectorWidth, 300, 480);
    // `restoreWorkspace` calls `selectPane(saved.activeTab)` one statement
    // later; that echo must not overwrite the tab the snapshot named.
    applyingRestore = true;
    queueMicrotask(() => { applyingRestore = false; });
    if (started) {
      showDestination();
      runDestinationLoaders();
      history.replaceState(null, "", hashFor(state.destination));
    }
  }

  /* ---- Lifecycle ---------------------------------------------------------- */

  function init() {
    if (ready) return;
    ready = true;
    const media = (query) => Boolean(window.matchMedia && window.matchMedia(query).matches);
    state.inspectorOpen = media("(min-width: 1024px)");
    state.watchlistCollapsed = !media("(min-width: 1280px)");
    mount();
    wire();
    syncChrome();
  }

  function start() {
    if (started) return;
    started = true;
    if (!isSignedIn()) {
      syncChrome();
      return;
    }
    const route = parse(location.hash);
    if (route) {
      setDestination(route.destination, {
        section: route.section,
        workspaceId: route.workspaceId,
      });
      const hash = hashFor(route.destination, route);
      if (location.hash !== hash) history.replaceState(null, "", hash);
      return;
    }
    if (!state.destination) {
      setDestination("dashboard");
      history.replaceState(null, "", "#/dashboard");
      return;
    }
    showDestination();
    runDestinationLoaders();
    syncChrome();
    history.replaceState(null, "", hashFor(state.destination));
  }

  function onAuth(signedIn) {
    // A drawer belongs to the signed-in frame. Left open, it would hang over
    // the home page owned by a session that is gone.
    if (!signedIn && drawer) drawer.close();
    syncChrome();
    if (!signedIn || !started) return;
    if (!state.destination) {
      const route = parse(location.hash);
      setDestination(route ? route.destination : "dashboard", {
        section: route && route.section,
        workspaceId: route && route.workspaceId,
      });
      history.replaceState(null, "", hashFor(state.destination));
      return;
    }
    showDestination();
  }

  /* ---- Wiring ------------------------------------------------------------- */

  function wire() {
    for (const button of document.querySelectorAll("#rail [data-dest]")) {
      button.addEventListener("click", () => go(button.dataset.dest));
    }
    const ai = el("aiBtn");
    if (ai) ai.addEventListener("click", openAi);
    const watchlist = el("watchlistToggle");
    if (watchlist) {
      watchlist.addEventListener("click", () => setWatchlistCollapsed(!state.watchlistCollapsed));
    }
    const inspector = el("inspectorToggle");
    if (inspector) {
      inspector.addEventListener("click", () => {
        if (state.destination !== "charts") {
          go("charts");
          setInspectorOpen(true);
          return;
        }
        setInspectorOpen(!state.inspectorOpen);
      });
    }
    const dock = el("dockToggle");
    if (dock) {
      dock.addEventListener("click", () => {
        if (state.destination !== "charts") {
          go("charts");
          openDock(state.dockTab);
          return;
        }
        if (state.dockOpen) closeDock();
        else openDock(state.dockTab);
      });
    }
    const asideHide = el("asideHide");
    if (asideHide) asideHide.addEventListener("click", () => setInspectorOpen(false));
    const asideShow = el("asideShow");
    if (asideShow) asideShow.addEventListener("click", () => setInspectorOpen(true));
    const railHide = el("railHide");
    if (railHide) railHide.addEventListener("click", () => setWatchlistCollapsed(true));
    const railShow = el("railShow");
    if (railShow) railShow.addEventListener("click", () => setWatchlistCollapsed(false));

    for (const button of document.querySelectorAll("#inspectorTabs [data-inspector]")) {
      button.addEventListener("click", () => selectInspector(button.dataset.inspector));
    }
    for (const button of document.querySelectorAll("#studioTabs [data-studio]")) {
      button.addEventListener("click", () => selectStudio(button.dataset.studio));
    }
    for (const button of document.querySelectorAll("#accountTabs [data-account]")) {
      button.addEventListener("click", () => selectAccount(button.dataset.account));
    }
    for (const button of document.querySelectorAll("#dock [data-dock]")) {
      button.addEventListener("click", () => selectDockTab(button.dataset.dock));
    }
    const collapse = el("dockCollapse");
    if (collapse) collapse.addEventListener("click", closeDock);
    const dockTabs = document.querySelector("#dock .dockTabs");
    if (dockTabs) {
      dockTabs.addEventListener("dblclick", () => {
        state.dockHeight = DOCK_DEFAULT_HEIGHT;
        applyDockHeight();
        if (app().resizePanes) app().resizePanes();
        note();
      });
    }
    const grip = el("dockResize");
    if (grip) {
      grip.addEventListener("pointerdown", (event) => {
        event.preventDefault();
        const startY = event.clientY;
        const startHeight = state.dockHeight;
        if (grip.setPointerCapture) grip.setPointerCapture(event.pointerId);
        const move = (moveEvent) => {
          state.dockHeight = clampDock(startHeight + (startY - moveEvent.clientY));
          applyDockHeight();
          if (app().resizePanes) app().resizePanes();
        };
        const stop = () => {
          grip.removeEventListener("pointermove", move);
          grip.removeEventListener("pointerup", stop);
          grip.removeEventListener("pointercancel", stop);
          note();
        };
        grip.addEventListener("pointermove", move);
        grip.addEventListener("pointerup", stop);
        grip.addEventListener("pointercancel", stop);
      });
    }
    const theme = el("profileTheme");
    if (theme) {
      theme.addEventListener("change", () => {
        if (window.Theme) window.Theme.set(theme.value);
      });
    }
    const signout = el("profileSignout");
    if (signout) {
      signout.addEventListener("click", () => {
        const toggle = el("signinToggle");
        if (toggle) toggle.click();
      });
    }

    document.addEventListener("hrz:pane", refreshStatus);
    document.addEventListener("hrz:auth", (event) =>
      onAuth(Boolean(event.detail && event.detail.signedIn))
    );
    document.addEventListener("hrz:save", (event) => {
      const detail = event.detail || {};
      setSaveState(detail.state || "saved", detail.text || "Saved");
    });
    window.addEventListener("hashchange", onHash);
  }

  window.Shell = {
    init,
    start,
    go,
    restore,
    snapshot,
    selectLegacyPane,
    legacyTab,
    setSaveState,
    setPlan,
    refreshStatus,
    openAi,
    openInspector: selectInspector,
    setInspectorOpen,
    setWatchlistCollapsed,
    openDock,
    closeDock,
    selectDockTab,
    destinations: () => DESTINATIONS.map((id) => ({ id, label: TITLES[id] })),
    state: () => ({ ...state }),
  };
})();
