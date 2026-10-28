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

  /* The things the address bar can name, in the order Alt+1..6 counts them.
   *
   * Three are pages and three are panels that float over whichever page is
   * open. The difference is not cosmetic: a page owns the frame under the chart,
   * a panel owns a modal and nothing else. One table says which, and every list
   * the shell needs -- the router's, the top bar's, the palette's -- is read out
   * of it, so an id cannot be a destination in one place and a panel in another.
   *
   * `width` is the panel's promise to its own content: a library and a bot desk
   * are read as tables, an account is a stack of cards.
   */
  const NAV = [
    { id: "dashboard", kind: "page", label: "Dashboard" },
    { id: "charts", kind: "page", label: "Charts" },
    { id: "studio", kind: "page", label: "Studio" },
    { id: "community", kind: "panel", label: "Community", width: "wide" },
    { id: "bots", kind: "panel", label: "Bots", width: "wide" },
    { id: "account", kind: "panel", label: "Account", width: "medium" },
  ];
  const NAV_BY_ID = new Map(NAV.map((entry) => [entry.id, entry]));
  const PAGES = NAV.filter((entry) => entry.kind === "page").map((entry) => entry.id);
  const PANELS = NAV.filter((entry) => entry.kind === "panel").map((entry) => entry.id);
  const TITLES = Object.fromEntries(NAV.map((entry) => [entry.id, entry.label]));
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
  /// The account sections with content to fetch, by the name the app's loader
  /// answers to. Profile is static markup, and `runPaneLoaders` with a name it
  /// does not know means "no pane is on screen" -- which would stop the
  /// watchlist's clock while the panel floats over the chart.
  const ACCOUNT_LOADERS = { plan: "billing", brokers: "brokers", ai: "aimodel" };
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
  /// The dock tabs whose body is a panel in `#dockBody`. Studio has no entry:
  /// its body is `pane-indicator`, moved in by `mountIndicator`.
  const DOCK_PANELS = { alerts: "dockAlerts", orders: "dockOrders", console: "dockConsole" };
  const DOCK_DEFAULT_HEIGHT = 240;
  const DOCK_MIN_HEIGHT = 160;

  /// How each feed state reads in words and as a dot tone. One table, so the
  /// same state cannot be called two things on one screen.
  ///
  /// The keys are exactly what the pane's own badge writes to
  /// `feedStatus.dataset.state` -- `live`, `stale` and `behind` included. They
  /// are not decorations: the badge is a measurement, and a status strip that
  /// maps a healthy feed to "Offline" (or an unknown one to "Live") is the
  /// claim the badge was built to avoid.
  const FEED_CLAIMS = {
    live: ["Live", "live"],
    stale: ["Quiet", "wait"],
    behind: ["Delayed", "wait"],
    paused: ["Paused", "wait"],
    connecting: ["Connecting", "wait"],
    idle: ["Connecting", "wait"],
    nofeed: ["No feed", "wait"],
    offline: ["Offline", "down"],
  };

  const state = {
    destination: null,
    panel: null,
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
  // The modal itself. Built once in `wire`, from markup that already holds the
  // three hosts; `syncPanel` is the only thing that asks it to show one.
  let workPanel = null;
  let applyingRestore = false;
  let drawer = null;
  // The chat the current route already asked the app to open. `go` applies the
  // destination directly and the hashchange applies it again a moment later;
  // without this, one link is two loads of the same conversation.
  let openedWorkspaceId = "";

  const el = (id) => document.getElementById(id);
  /// The top bar and the phone's bottom nav are the same navigation. One
  /// selector names both, so a view cannot be current in one place and not the
  /// other -- the promise the two lists made to each other when the shell still
  /// had a rail to keep.
  const destButtons = () =>
    document.querySelectorAll("#topNav [data-dest], #phoneNav [data-dest]");
  /// The three icons that open a panel: two in the top bar's second nav, and the
  /// account chip at its right end. They are one set because they are one
  /// question -- is this the panel you are looking at?
  const panelTriggers = () => document.querySelectorAll("[data-open-panel]");
  /// The icon a panel is opened from, which is also where closing hands focus
  /// back to -- including when the panel was opened by an address, not a click.
  const panelTrigger = (name) => document.querySelector(`[data-open-panel="${name}"]`);
  const paint = (id, text) => {
    const node = el(id);
    if (node) node.textContent = text;
  };
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
    }
    // Three of the six ids the address names are not pages: their content shows
    // because the panel is open, whatever is running underneath it.
    if (state.panel === "community") visible.add("pane-community");
    else if (state.panel === "bots") visible.add("pane-bots");
    else if (state.panel === "account") {
      visible.add(ACCOUNT_PANE[state.accountSection] || "accountProfile");
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

  /// Set a strip's selection and make it one Tab stop with four arrow keys.
  ///
  /// Four strips, one rule: the state names the selection, `HRZUI.tabKeyboard`
  /// makes the strip navigable, and no strip grows its own copy of the same
  /// four `if`s.
  function syncTabStrip(root, attribute, selected) {
    if (!root) return;
    for (const button of root.querySelectorAll('[role="tab"]')) {
      button.setAttribute("aria-selected", String(button.dataset[attribute] === selected));
    }
    HRZUI.tabKeyboard(root);
  }

  function syncInspector() {
    const open = state.destination === "charts" && state.inspectorOpen;
    const host = el("inspectorHost");
    const splitter = el("splitAside");
    const reveal = el("asideShow");
    if (host) host.hidden = !open;
    if (splitter) splitter.hidden = !open;
    if (reveal) reveal.hidden = open || state.destination !== "charts";
    syncTabStrip(el("inspectorTabs"), "inspector", state.inspectorTab);
    const ai = el("aiBtn");
    if (ai) ai.setAttribute("aria-pressed", String(open && state.inspectorTab === "ai"));
    const toggle = el("inspectorToggle");
    if (toggle) toggle.setAttribute("aria-pressed", String(open));
  }

  function syncStudio() {
    syncTabStrip(el("studioTabs"), "studio", state.studioPane);
  }

  function syncAccount() {
    syncTabStrip(el("accountTabs"), "account", state.accountSection);
  }

  /// The one place the modal is told what to be.
  ///
  /// The route already decided it: this reads `state.panel` and asks the dialog
  /// to show that host, or to close. Nothing else calls `show` or `hide`, which
  /// is why the address and the panel cannot drift apart -- and why clicking the
  /// second icon while the first is open is a route change, not a second modal.
  function syncPanel() {
    const entry = state.panel ? NAV_BY_ID.get(state.panel) : null;
    if (workPanel) {
      if (entry) {
        workPanel.show(entry.id, {
          title: entry.label,
          width: entry.width,
          opener: panelTrigger(entry.id),
        });
      } else workPanel.hide();
    }
    for (const button of panelTriggers()) {
      const on = state.panel === button.dataset.openPanel;
      button.setAttribute("aria-expanded", String(on));
    }
    // The head tabs are a fourth tab strip, and `show` only marks the selected
    // one; arrow-key navigation lives in `syncTabStrip`, which is also where the
    // strip is re-taught after a host swap.
    syncTabStrip(el("workPanelTabs"), "tab", state.panel);
    syncAccount();
  }

  /// Unread lines since the Console was last looked at. Reset by `loadDock`,
  /// not by the paint: a log line arriving while the tab is open must not
  /// raise a badge for output the user can see.
  let consoleUnread = 0;

  function paintConsole() {
    const list = el("consoleRows");
    if (!list) return;
    const entries = window.HRZLog ? HRZLog.entries() : [];
    list.replaceChildren(...entries.map((entry) => {
      const row = document.createElement("li");
      row.className = `consoleRow consoleRow--${entry.level}`;
      const time = document.createElement("span");
      time.className = "consoleTime";
      time.textContent = new Date(entry.at).toLocaleTimeString([], { hour12: false });
      const message = document.createElement("span");
      message.className = "consoleMsg";
      message.textContent = entry.message;
      row.append(time, message);
      return row;
    }));
    const empty = el("consoleEmpty");
    if (empty) empty.hidden = entries.length > 0;
    // Newest at the bottom, and the view follows the bottom: the line that
    // explains the last line is the one above it.
    list.scrollTop = list.scrollHeight;
  }

  function paintConsoleBadge() {
    const tab = document.querySelector('#dock [data-dock="console"]');
    if (!tab) return;
    const unread = consoleUnread > 0;
    tab.dataset.unread = unread ? "true" : "false";
    tab.setAttribute(
      "aria-label",
      unread
        ? `Console, ${consoleUnread} new ${consoleUnread === 1 ? "message" : "messages"}`
        : "Console"
    );
  }

  function noteLog(entry) {
    const looking =
      state.destination === "charts" && state.dockOpen && state.dockTab === "console";
    if (looking) {
      paintConsole();
      return;
    }
    // A clear is not news. The event carries a null entry for it, and the
    // badge is about lines that arrived, not about lines that left.
    if (!entry) return;
    consoleUnread += 1;
    paintConsoleBadge();
  }

  function syncDock() {
    const onCharts = state.destination === "charts";
    const open = onCharts && state.dockOpen;
    const dock = el("dock");
    if (dock) {
      dock.hidden = !onCharts;
      dock.dataset.open = open ? "true" : "false";
    }
    // The body is hidden by attribute, not only by height: the panels inside it
    // are focusable, and a 0-height scroller is still in the tab order.
    const body = el("dockBody");
    if (body) body.hidden = !open;
    applyDockHeight();
    syncTabStrip(el("dockTablist"), "dock", state.dockTab);
    // One panel per tab, none rebuilt on switch: the console keeps its scroll
    // position and the alerts keep the rows their last poll produced.
    for (const [tab, id] of Object.entries(DOCK_PANELS)) {
      const panel = el(id);
      if (panel) panel.hidden = state.dockTab !== tab;
    }
    const collapse = el("dockCollapse");
    if (collapse) {
      collapse.setAttribute("aria-expanded", String(open));
      collapse.setAttribute("aria-label", open ? "Collapse the dock" : "Expand the dock");
      collapse.title = open ? "Collapse" : "Expand";
    }
    const toggle = el("dockToggle");
    if (toggle) toggle.setAttribute("aria-pressed", String(open));
  }

  function showDestination() {
    for (const page of PAGES) {
      const section = el(`dest-${page}`);
      if (section) section.hidden = page !== state.destination;
    }
    const title = el("destTitle");
    if (title) {
      title.textContent = TITLES[state.destination] || "";
      title.hidden = !state.destination;
    }
    for (const button of destButtons()) {
      if (button.dataset.dest === state.destination) button.setAttribute("aria-current", "page");
      else button.removeAttribute("aria-current");
    }
    syncWatchlist();
    syncInspector();
    syncStudio();
    syncPanel();
    syncDock();
    mountIndicator();
    showPanes();
    refreshPane();
    if (state.destination === "charts" && app().resizePanes) app().resizePanes();
    // The dashboard keeps a clock, and it is told when it is on screen rather
    // than asking the DOM. Called last, so the destination is fully mounted
    // before the first paint lands on it.
    if (app().setDestinationVisible) app().setDestinationVisible(state.destination);
  }

  /* ---- Status and chrome -------------------------------------------------- */

  /// What the chart's live region last said, compared by series and never by
  /// price: the status bar repaints on every tick, and a region that re-read
  /// on every tick would read the market aloud all day.
  let announcedSeries = "";

  function announceSeries(info) {
    const node = el("chartAnnounce");
    if (!node) return;
    const series = info && info.symbol ? `${info.symbol} ${info.timeframe || ""}`.trim() : "";
    if (series === announcedSeries) return;
    const first = !announcedSeries;
    announcedSeries = series;
    // The first series a page names is the one it loaded with; boot is not a
    // change the user made, and saying it would greet every reload.
    if (first) return;
    node.textContent = series ? `Chart on ${series}` : "No chart";
  }

  function refreshStatus() {
    const info = app().activePaneInfo ? app().activePaneInfo() : null;
    const symbol = el("sbSymbol");
    if (symbol) symbol.textContent = info && info.symbol ? info.symbol : "—";
    const timeframe = el("sbTimeframe");
    if (timeframe) timeframe.textContent = info && info.timeframe ? info.timeframe : "";
    // The badge's states are the app's own readings -- "live", "stale" and
    // "behind" included -- so the map mirrors them rather than inventing a
    // vocabulary of the strip's own.
    const claim = FEED_CLAIMS[(info && info.feed) || "offline"] || ["Offline", "down"];
    const label = el("sbConnLabel");
    // `role="status"`: write only on a change, so a retry every five seconds is
    // not read out every five seconds.
    if (label && label.textContent !== claim[0]) label.textContent = claim[0];
    const dot = el("sbConnection");
    if (dot) dot.dataset.state = claim[1];
    const top = el("connDotTop");
    if (top) top.dataset.state = claim[1];
    announceSeries(info);
  }

  /// Repaint the Indicator tab from the active chart.
  ///
  /// A projection, like the instrument bar: the pane owns the indicator and
  /// this tab only reads it, on every `hrz:pane`. Nothing is stored here, so
  /// detaching from the pane's chip, the bar's chip and this tab cannot drift.
  function paintIndicatorTab() {
    const projection = el("indicatorProjection");
    const empty = el("indicatorProjectionEmpty");
    if (!projection || !empty) return;
    const info = app().activePaneInfo ? app().activePaneInfo() : null;
    const indicator = info && info.indicator;
    projection.hidden = !indicator;
    empty.hidden = Boolean(indicator);
    if (!indicator) return;
    paint("indicatorName", indicator.name);
    paint("indicatorMode", indicator.live ? "Live — re-detects every candle" : "Static preview");
    paint("indicatorConcepts", String(indicator.concepts));
    paint("indicatorZones", String(indicator.zones));
    paint("indicatorMarkers", String(indicator.markers));
    paint(
      "indicatorNote",
      indicator.live
        ? `Following ${info.symbol} ${info.timeframe}: it re-detects on whatever series the chart loads.`
        : "Drawn for one window; it does not re-detect on a new series."
    );
  }

  /// Repaint the Details tab's session and data facts.
  function paintDetails() {
    const info = app().activePaneInfo ? app().activePaneInfo() : null;
    const claim = info && info.feed ? FEED_CLAIMS[info.feed] : null;
    paint("dfChart", info && info.symbol ? `${info.symbol} · ${info.timeframe}` : "—");
    paint("dfBars", info && info.bars ? `${info.bars} bars` : "—");
    paint("dfFeed", claim ? claim[0] : "—");
    paint("dfAccount", (app().email && app().email()) || "—");
    paint("dfPlan", (app().planName && app().planName()) || "—");
  }

  /// Everything that reads the active chart, repainted together.
  ///
  /// The status bar, the Indicator tab and the Details facts are three
  /// projections of one question, so one event repaints all three and none is
  /// ever an event behind the others.
  function refreshPane() {
    refreshStatus();
    paintIndicatorTab();
    paintDetails();
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
    for (const button of destButtons()) button.disabled = !signedIn;
    // The panel triggers are chrome too: a signed-out visitor must not be handed
    // a door into a modal with nothing to load.
    for (const button of panelTriggers()) button.disabled = !signedIn;
    // The bottom nav is outside #frame, so the frame's own hide does not reach
    // it: a signed-out phone must not list destinations into a frame that is
    // not there. CSS shows it only below 768; this attribute is the other half.
    const phone = el("phoneNav");
    if (phone) phone.hidden = !signedIn;
    for (const id of [
      "aiBtn",
      "paletteBtn",
      "watchlistToggle",
      "inspectorToggle",
      "dockToggle",
      "shortcutsBtn",
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
    const label = text || "Saved";
    // Also a `role="status"` region, and the workspace saves on every debounced
    // change: the same word twice is not news.
    if (node.textContent !== label) node.textContent = label;
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

  /// The address bar, read as a page and the panel floating over it.
  ///
  /// Two shapes are accepted. `#/<page>` and `#/<page>/<panel>[/<section>]` are
  /// what the shell writes; `#/<panel>` is the shape the bar had when Community,
  /// Bots and Account were destinations, and it still means the same screen --
  /// that panel over Charts, the page it used to sit beside. Every shape is
  /// written back in the first form, so a reload, a pasted link and a Back press
  /// cannot name one screen three ways.
  function parse(hash) {
    const raw = String(hash || "").replace(/^#\/?/, "");
    const parts = raw.split("/").filter(Boolean);
    if (!parts.length) return null;
    if (PANELS.includes(parts[0])) {
      const route = { page: "charts", panel: parts[0] };
      if (parts[0] === "account") {
        route.section = ACCOUNT_SECTIONS.includes(parts[1]) ? parts[1] : "plan";
      }
      return route;
    }
    if (!PAGES.includes(parts[0])) return null;
    const route = { page: parts[0], panel: null };
    const second = parts[1];
    if (second && PANELS.includes(second)) {
      route.panel = second;
      if (second === "account") {
        route.section = ACCOUNT_SECTIONS.includes(parts[2]) ? parts[2] : "plan";
      }
    } else if (route.page === "studio" && second) {
      // An id is user data from the address bar. A malformed escape is a bad
      // link, not a router crash: fall back to the raw segment and let the
      // workspace lookup reject it. A workspace named exactly like a panel is
      // the one link this grammar cannot make; the builder's list still reaches it.
      try {
        route.workspaceId = decodeURIComponent(second);
      } catch {
        route.workspaceId = second;
      }
    }
    return route;
  }

  /// The address for a page, with the panel it carries if none is given.
  function hashFor(page, opts = {}) {
    const panel = opts.panel === undefined ? state.panel : opts.panel;
    if (page === "studio" && opts.workspaceId && !panel) {
      return `#/studio/${encodeURIComponent(opts.workspaceId)}`;
    }
    if (!panel) return `#/${page}`;
    if (panel === "account") {
      return `#/${page}/account/${opts.section || state.accountSection || "plan"}`;
    }
    return `#/${page}/${panel}`;
  }

  /// The address follows the choice, and a push is what gives Back something to
  /// do: closing a panel from inside it is the same step as pressing Back.
  function writeHash(hash, replace) {
    if (replace) history.replaceState(null, "", hash);
    else if (location.hash !== hash) location.hash = hash;
  }

  function setDestination(destination, opts = {}) {
    if (!PAGES.includes(destination)) destination = "dashboard";
    const previous = state.destination;
    const changed = previous !== destination;
    state.destination = destination;
    // A page carries no panel unless the route names one: the modal belongs to
    // the page it floats over, and taking it along to another would leave the
    // address naming a combination nobody asked for.
    const panel = opts.panel === undefined ? null : opts.panel;
    const panelChanged = state.panel !== panel;
    state.panel = panel;
    let load = changed;
    let panelLoad = panelChanged;
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
    if (state.panel === "account") {
      const section = opts.section && ACCOUNT_SECTIONS.includes(opts.section)
        ? opts.section
        : changed || panelChanged ? "plan" : state.accountSection;
      if (section !== state.accountSection) {
        state.accountSection = section;
        panelLoad = true;
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
    } else {
      if (load) runPageLoaders();
      if (panelLoad) runPanelLoaders();
    }
    note();
  }

  /// The one door every click, palette verb and Alt digit walks through.
  ///
  /// A page id switches the page and puts the panel away; a panel id leaves the
  /// page exactly where it is and floats the modal over it. Both write the
  /// address, because both are things a reload has to be able to repeat.
  function go(id, opts = {}) {
    const entry = NAV_BY_ID.get(id) || NAV_BY_ID.get("dashboard");
    if (entry.kind === "panel") {
      openPanel(entry.id, opts);
      return;
    }
    setDestination(entry.id, opts);
    writeHash(hashFor(entry.id, { panel: null, workspaceId: opts.workspaceId }), opts.replace);
  }

  function openPanel(name, opts = {}) {
    if (!PANELS.includes(name)) return;
    const page = opts.page || state.destination || "charts";
    setDestination(page, { ...opts, panel: name });
    writeHash(hashFor(page, { panel: name, section: state.accountSection }), opts.replace);
  }

  function closePanel() {
    if (!state.panel) return;
    const page = state.destination || "dashboard";
    setDestination(page, { panel: null });
    writeHash(hashFor(page, { panel: null }));
  }

  /// Apply the address, then say it back in the form the shell writes.
  ///
  /// Returns whether the hash named anything at all. The rewrite is a replace,
  /// never a push: the old shape is a second name for the same screen, and a
  /// Back press should leave the screen, not one spelling of it.
  function fromHash() {
    const route = parse(location.hash);
    if (!route) return false;
    setDestination(route.page, {
      panel: route.panel,
      section: route.section,
      workspaceId: route.workspaceId,
    });
    const canonical = hashFor(route.page, route);
    if (location.hash !== canonical) history.replaceState(null, "", canonical);
    return true;
  }

  /// Where a user lands when the address names nothing they can be sent to.
  ///
  /// `go` applies the route itself and the hashchange applies it again a moment
  /// later, so both paths have to end in the same place: `fromHash` is the only
  /// reading of the bar either of them does.
  function onHash() {
    if (!started || !isSignedIn()) return;
    if (fromHash()) return;
    // A fragment that names something already on the page -- the skip link's
    // `#mainView` -- is not a route. Sending it here would drop a keyboard
    // user on Dashboard from the chart they were reading, so the address is
    // put back and the destination the user is on stays.
    const fragment = String(location.hash || "").slice(1);
    if (state.destination && fragment && document.getElementById(fragment)) {
      history.replaceState(null, "", hashFor(state.destination));
      return;
    }
    history.replaceState(null, "", "#/dashboard");
    setDestination("dashboard", { panel: null });
  }

  /* ---- What a route makes the app fetch ----------------------------------- */

  /// The page's own content. A page switch asks for this; nothing else does.
  function runPageLoaders() {
    const load = app().runPaneLoaders;
    if (!load) return;
    // The watchlist prices itself only while it is on screen: the ticker has no
    // reason to keep running behind Studio, and the way back to Charts is
    // supposed to find it fresh rather than waiting for the next tick.
    if (state.destination === "charts") {
      load("watchlist");
      // A restored workspace can open on Alerts, Orders or Console, and no tab
      // click happens in that case -- the panel would sit empty until the user
      // switched away and back.
      if (state.dockOpen) loadDock(state.dockTab);
    } else if (state.destination === "studio") load(state.studioPane);
    else if (state.destination === "dashboard") {
      // No pane is on screen -- and the loader's own `else` is what stops the
      // watchlist ticker, so the dashboard's page clock is the only one left
      // running. `runPaneLoaders()` with no name means exactly that.
      load();
    }
  }

  /// The panel's content, asked for when the panel opens and never before.
  ///
  /// This is what a modal buys over the page it replaced: the library is not
  /// fetched, sorted or scrolled until somebody opens it, and closing it costs
  /// nothing because the host keeps what it loaded for the next open.
  function runPanelLoaders() {
    const load = app().runPaneLoaders;
    if (!load || !state.panel) return;
    if (state.panel === "community") load("community");
    else if (state.panel === "bots") load("bots");
    else if (state.panel === "account") {
      const legacy = ACCOUNT_LOADERS[state.accountSection];
      if (legacy) load(legacy);
    }
  }

  function runDestinationLoaders() {
    runPageLoaders();
    runPanelLoaders();
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
    // A section is part of the address while the panel is open -- the third
    // segment a reload, a pasted link and a Back press all read. With the panel
    // shut the shell only remembers the tab, because the hash that opens it
    // names the panel and its default section.
    if (state.panel === "account") openPanel("account", { section });
    else showDestination();
    const legacy = ACCOUNT_LOADERS[section];
    if (legacy && app().runPaneLoaders) app().runPaneLoaders(legacy);
    note();
  }

  function loadDock(tab) {
    if (tab === "console") {
      // Opening the console is reading it: the dot that said "there is
      // something new" is answered by the paint below.
      consoleUnread = 0;
      paintConsoleBadge();
      paintConsole();
      return;
    }
    if (app().runPaneLoaders) {
      app().runPaneLoaders(tab === "studio" ? "indicator" : tab);
    }
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
    // The panel is what the user is looking at, so it is answered before the
    // page underneath it. The workspace snapshot remembers the tab, and a
    // reload of the builder must not find the Community list written as Charts.
    if (state.panel === "community") return "community";
    if (state.panel === "bots") return "bots";
    if (state.panel === "account") return LEGACY_FROM_ACCOUNT[state.accountSection] || "billing";
    if (state.destination === "charts") return LEGACY_FROM_TAB[state.inspectorTab] || "chat";
    if (state.destination === "studio") return state.studioPane;
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
    // A snapshot from before the panels went modal names a panel where a page
    // belongs, and `DEST_FROM_LEGACY` hands back the same three ids for the
    // same reason. Either way the screen underneath is Charts: a snapshot is
    // allowed to say which page to land on, but opening a modal is the hash's
    // work, and nothing here reads or writes `state.panel`.
    const named = saved.destination || DEST_FROM_LEGACY[saved.activeTab];
    let destination = PAGES.includes(saved.destination) ? saved.destination : null;
    if (!destination && PANELS.includes(named)) destination = "charts";
    if (!destination) {
      if (PAGES.includes(named)) destination = named;
      else if (Array.isArray(saved.panes) && saved.panes.length) destination = "charts";
      else destination = "dashboard";
    }
    state.destination = destination;
    const legacy = saved.activeTab;
    if (legacy && destination === "studio" && STUDIO_PANES[legacy]) {
      state.studioPane = legacy;
    }
    // The account section is a page of its own even when the panel that holds it
    // is shut, so a snapshot naming `billing` still decides which tab the avatar
    // opens on.
    if (ACCOUNT_FROM_LEGACY[legacy]) state.accountSection = ACCOUNT_FROM_LEGACY[legacy];
    else if (ACCOUNT_SECTIONS.includes(saved.accountSection)) {
      state.accountSection = saved.accountSection;
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
    if (fromHash()) return;
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
    // A drawer and a panel belong to the signed-in frame. Left open, either
    // would hang over the home page owned by a session that is gone.
    if (!signedIn) {
      if (drawer) drawer.close();
      if (state.panel) {
        state.panel = null;
        if (workPanel) workPanel.hide();
      }
    }
    syncChrome();
    if (!signedIn || !started) return;
    if (fromHash()) return;
    if (!state.destination) {
      setDestination("dashboard");
      history.replaceState(null, "", "#/dashboard");
      return;
    }
    showDestination();
  }

  /* ---- Wiring ------------------------------------------------------------- */

  function wire() {
    for (const button of destButtons()) {
      button.addEventListener("click", () => go(button.dataset.dest));
    }
    for (const button of panelTriggers()) {
      button.addEventListener("click", () => go(button.dataset.openPanel));
    }
    // The primitive owns the scrim, the close button, Escape and focus return.
    // All three of its decisions are handed back to the router, because a panel
    // closing is a route change: the hash loses the segment, and Back finds the
    // page underneath rather than a second screen.
    if (window.HRZUI && window.HRZUI.panel) {
      workPanel = window.HRZUI.panel(el("workPanel"), {
        onRequestClose: closePanel,
        onSelect: (name) => openPanel(name),
      });
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
    if (collapse) {
      // The chevron collapses the body, not the bar: the bar is where the
      // Console's dot lives when the dock is shut.
      collapse.addEventListener("click", () => {
        if (state.dockOpen) closeDock();
        else openDock(state.dockTab);
      });
    }
    const consoleClear = el("consoleClear");
    if (consoleClear) {
      consoleClear.addEventListener("click", () => {
        if (window.HRZLog) HRZLog.clear();
      });
    }
    document.addEventListener("hrz:log", (event) => {
      noteLog(event.detail && event.detail.entry);
    });
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
    const detach = el("indicatorDetach");
    if (detach) {
      detach.addEventListener("click", () => {
        if (app().detachActiveIndicator) app().detachActiveIndicator();
      });
    }
    const toStudio = el("indicatorStudio");
    if (toStudio) {
      toStudio.addEventListener("click", () => go("studio", { studioPane: "indicator" }));
    }
    // Alerts is the dock's tab: the same watch events that fire for every
    // chart, not a list owned by this layer. The dock paints them from the
    // server's monitors, so this button opens a tab that already has content.
    const alerts = el("indicatorAlerts");
    if (alerts) alerts.addEventListener("click", () => openDock("alerts"));
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

    document.addEventListener("hrz:pane", refreshPane);
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
    destinations: () => NAV.map((entry) => ({
      id: entry.id,
      label: entry.label,
      kind: entry.kind,
    })),
    pages: () => PAGES.slice(),
    openPanel,
    closePanel,
    state: () => ({ ...state }),
  };
})();
