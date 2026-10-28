/* The command palette and the shortcut sheet.
 *
 * One field answers three questions -- where can I go, which instrument, which
 * chat -- and a listbox answers with the closest match. The pattern is the
 * combobox/listbox pair, not a menu: the selection moves while focus stays in
 * the field, which is what lets a screen reader read each option as it passes
 * (aria-activedescendant) and what lets the user keep typing.
 *
 * The sheet is a static dialog. Its labels are rewritten from the platform at
 * load, because a Mac user reading "Ctrl" learns nothing about their keyboard.
 */
(function () {
  "use strict";

  const IS_APPLE = /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent);
  const MOD = IS_APPLE ? "⌘" : "Ctrl";
  const SYMBOL_LIMIT = 25;
  const SEARCH_DELAY = 160;
  const CHATS_MAX_AGE = 60_000;

  const app = () => window.HRZApp;
  const shell = () => window.Shell;
  const signedIn = () => Boolean(app() && app().signedIn && app().signedIn());

  let dialog = null;
  let input = null;
  let list = null;
  let empty = null;
  let status = null;
  let sheet = null;

  let isOpen = false;
  let items = [];
  let active = 0;
  let activeId = "";
  let query = "";
  let symbols = [];
  let searching = false;
  let searchSeq = 0;
  let searchTimer = 0;
  let chats = null;
  let chatsAt = 0;
  let returnFocus = null;
  let sheetReturn = null;

  /* ---- What the list can hold --------------------------------------------- */

  function destinations() {
    return (shell() && shell().destinations && shell().destinations()) || [];
  }

  function navItems() {
    return destinations().map((destination, index) => ({
      id: `nav:${destination.id}`,
      label: destination.label,
      hint: `Alt+${index + 1}`,
      run: () => shell() && shell().go(destination.id),
    }));
  }

  function actionItems() {
    const state = (shell() && shell().state()) || {};
    const theme = window.Theme ? window.Theme.current() : "dark";
    const actions = [
      {
        id: "action:pane",
        label: "New chart pane",
        hint: "Charts",
        run: () => {
          if (shell()) shell().go("charts");
          if (app() && app().newPane) app().newPane();
        },
      },
      {
        id: "action:chat",
        // The builder creates a chat from its own name field. The palette does
        // not invent a name: it opens the builder and puts the caret where the
        // user has to type anyway.
        label: "New indicator chat",
        hint: "Studio",
        run: () => {
          if (shell()) shell().go("studio", { studioPane: "indicator" });
          if (app() && app().newWorkspace) app().newWorkspace();
        },
      },
      {
        id: "action:theme",
        label: `Switch to the ${theme === "dark" ? "light" : "dark"} theme`,
        run: () => {
          if (window.Theme) window.Theme.set(theme === "dark" ? "light" : "dark");
        },
      },
      {
        id: "action:shortcuts",
        label: "Keyboard shortcuts",
        hint: `${MOD}/`,
        run: openShortcuts,
      },
    ];
    // The panel toggles exist in the top bar and only change something the user
    // can see on Charts. Offering them elsewhere would be a command whose
    // result is off screen -- and a subpanel floating over the chart hides the
    // very columns they move, so they wait until the modal is closed.
    if (state.destination === "charts" && !state.panel) {
      actions.unshift({
        id: "action:watchlist",
        label: state.watchlistCollapsed ? "Show the watchlist" : "Hide the watchlist",
        run: () => shell() && shell().setWatchlistCollapsed(!state.watchlistCollapsed),
      });
      actions.unshift({
        id: "action:inspector",
        label: state.inspectorOpen ? "Hide the inspector" : "Show the inspector",
        run: () => shell() && shell().setInspectorOpen(!state.inspectorOpen),
      });
    }
    actions.push({
      id: "action:signout",
      label: "Sign out",
      run: () => { if (app() && app().signOut) app().signOut(); },
    });
    return actions;
  }

  function chatItems() {
    if (!Array.isArray(chats)) return [];
    return chats.map((chat) => ({
      id: `chat:${chat.id}`,
      label: chat.name || "Untitled chat",
      hint: [chat.symbol, chat.timeframe].filter(Boolean).join(" "),
      run: () => shell() && shell().go("studio", { workspaceId: chat.id }),
    }));
  }

  function symbolItems() {
    return symbols.map((result) => ({
      id: `symbol:${result.symbol}`,
      label: result.symbol,
      hint: result.trading === false ? "orders closed" : "Charts",
      run: () => {
        if (shell()) shell().go("charts");
        if (app() && app().chartSymbol) app().chartSymbol(result.symbol);
      },
    }));
  }

  const matches = (item, needle) =>
    !needle ||
    item.label.toLowerCase().includes(needle) ||
    String(item.hint || "").toLowerCase().includes(needle);

  function groups() {
    const needle = query.toLowerCase();
    const out = [];
    const nav = navItems().filter((item) => matches(item, needle));
    if (nav.length) out.push({ title: "Navigate", items: nav });
    // Symbols and chats need a query. With an empty field the palette is a
    // launcher; with one it is a search. Two modes, one field.
    if (needle) {
      const found = symbolItems();
      if (found.length) out.push({ title: "Symbols", items: found });
      const conversations = chatItems().filter((item) => matches(item, needle));
      if (conversations.length) out.push({ title: "Chats", items: conversations });
    }
    const actions = actionItems().filter((item) => matches(item, needle));
    if (actions.length) out.push({ title: "Actions", items: actions });
    return out;
  }

  /* ---- Rendering ----------------------------------------------------------- */

  function optionNode(item, index) {
    const node = document.createElement("div");
    node.className = "paletteOpt";
    node.id = `paletteOpt${index}`;
    node.dataset.index = String(index);
    node.setAttribute("role", "option");
    node.setAttribute("aria-selected", "false");

    const label = document.createElement("span");
    label.className = "paletteOpt__label";
    label.textContent = item.label;
    node.appendChild(label);
    if (item.hint) {
      const hint = document.createElement("span");
      hint.className = "paletteOpt__hint";
      hint.textContent = item.hint;
      node.appendChild(hint);
    }
    node.addEventListener("pointermove", () => {
      if (active !== index) selectActive(index, false);
    });
    node.addEventListener("click", () => run(index));
    return node;
  }

  function selectActive(index, scroll = true) {
    if (!items.length) return;
    active = (index + items.length) % items.length;
    activeId = items[active].id;
    input.setAttribute("aria-activedescendant", `paletteOpt${active}`);
    for (const node of list.querySelectorAll('[role="option"]')) {
      node.setAttribute("aria-selected", String(node.dataset.index === String(active)));
    }
    if (scroll) {
      const node = list.querySelector(`[data-index="${active}"]`);
      if (node && node.scrollIntoView) node.scrollIntoView({ block: "nearest" });
    }
  }

  function render() {
    if (!list) return;
    const previous = activeId;
    const built = groups();
    const flat = [];
    let groupIndex = 0;
    list.textContent = "";

    for (const group of built) {
      const wrap = document.createElement("div");
      wrap.className = "paletteGroup";
      wrap.setAttribute("role", "group");
      const title = document.createElement("div");
      title.className = "paletteGroup__title";
      title.id = `paletteGroup${groupIndex++}`;
      title.textContent = group.title;
      wrap.setAttribute("aria-labelledby", title.id);
      wrap.appendChild(title);
      for (const item of group.items) {
        const index = flat.length;
        flat.push(item);
        wrap.appendChild(optionNode(item, index));
      }
      list.appendChild(wrap);
    }

    items = flat;
    if (!flat.length) {
      active = -1;
      activeId = "";
      list.hidden = true;
      empty.hidden = false;
      empty.textContent = searching
        ? "Searching instruments…"
        : query
          ? `No matches for “${query}”.`
          : "Nothing to jump to.";
      input.removeAttribute("aria-activedescendant");
    } else {
      list.hidden = false;
      empty.hidden = true;
      const found = previous ? flat.findIndex((item) => item.id === previous) : -1;
      selectActive(found >= 0 ? found : 0, false);
    }

    status.textContent = query
      ? `${flat.length} result${flat.length === 1 ? "" : "s"} for ${query}`
      : `${flat.length} commands`;
  }

  /* ---- Running a result ---------------------------------------------------- */

  function run(index) {
    const item = items[index];
    if (!item) return;
    close();
    // After the dialog is gone, so a command that moves focus (the builder's
    // name field) is not fought by the dialog restoring focus to its trigger.
    queueMicrotask(() => {
      try {
        Promise.resolve(item.run()).catch((error) => {
          if (window.HRZUI) {
            window.HRZUI.toast(error && error.message ? error.message : "That action failed", {
              tone: "error",
            });
          }
        });
      } catch (error) {
        if (window.HRZUI) {
          window.HRZUI.toast(error && error.message ? error.message : "That action failed", {
            tone: "error",
          });
        }
      }
    });
  }

  /* ---- Search -------------------------------------------------------------- */

  function onInput() {
    const value = input.value.trim();
    if (value !== query) {
      symbols = [];
      searching = Boolean(value);
    }
    query = value;
    clearTimeout(searchTimer);
    render();
    if (!query) {
      searchSeq += 1;
      searching = false;
      return;
    }
    const ticket = ++searchSeq;
    searchTimer = setTimeout(async () => {
      try {
        const results = app() && app().searchSymbols ? await app().searchSymbols(query) : [];
        if (!isOpen || ticket !== searchSeq) return;
        symbols = Array.isArray(results) ? results.slice(0, SYMBOL_LIMIT) : [];
      } catch {
        if (!isOpen || ticket !== searchSeq) return;
        symbols = [];
      }
      searching = false;
      render();
    }, SEARCH_DELAY);
  }

  async function loadChats() {
    if (!app() || !app().workspaces) return;
    try {
      const response = await app().workspaces();
      chats = Array.isArray(response) ? response : [];
    } catch {
      // A failed list is an empty section, not an error over the field: the
      // user came here to go somewhere, and the destinations still work.
      if (chats === null) chats = [];
    }
    chatsAt = Date.now();
    if (isOpen) render();
  }

  /* ---- The two dialogs ----------------------------------------------------- */

  function openDialog(node) {
    if (!node || node.hasAttribute("open")) return;
    if (typeof node.showModal === "function") {
      try {
        node.showModal();
        return;
      } catch {
        // A DOM without modal-dialog support falls through to the attribute.
      }
    }
    node.setAttribute("open", "");
  }

  function closeDialog(node) {
    if (!node || !node.hasAttribute("open")) return;
    if (typeof node.close === "function" && node.open) node.close();
    else node.removeAttribute("open");
  }

  function openPalette(trigger) {
    if (!signedIn() || !dialog) return;
    if (isOpen) {
      input.focus();
      input.select();
      return;
    }
    returnFocus = trigger || document.activeElement;
    isOpen = true;
    query = "";
    symbols = [];
    searching = false;
    searchSeq += 1;
    activeId = "";
    input.value = "";
    openDialog(dialog);
    render();
    input.focus();
    if (!chats || Date.now() - chatsAt > CHATS_MAX_AGE) loadChats();
  }

  function close() {
    if (!isOpen || !dialog) return;
    isOpen = false;
    clearTimeout(searchTimer);
    closeDialog(dialog);
    const back = returnFocus;
    returnFocus = null;
    if (back && back.focus) back.focus();
  }

  function openShortcuts(trigger) {
    if (!sheet) return;
    sheetReturn = trigger || document.activeElement;
    openDialog(sheet);
    const button = sheet.querySelector("[data-close]");
    if (button) button.focus();
  }

  function closeShortcuts() {
    if (!sheet) return;
    closeDialog(sheet);
    const back = sheetReturn;
    sheetReturn = null;
    if (back && back.focus) back.focus();
  }

  /* ---- Keys ---------------------------------------------------------------- */

  function onPaletteKey(event) {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      selectActive(active + 1);
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      selectActive(active - 1);
      return;
    }
    if (event.key === "Home") {
      event.preventDefault();
      selectActive(0);
      return;
    }
    if (event.key === "End") {
      event.preventDefault();
      selectActive(items.length - 1);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      run(active);
      return;
    }
    if (event.key !== "Tab" || !dialog) return;
    // The field and the close button are the only stops. Native modals trap
    // Tab; this keeps the fallback honest too.
    const button = dialog.querySelector("[data-close]");
    if (!button) return;
    if (event.shiftKey && document.activeElement === input) {
      event.preventDefault();
      button.focus();
    } else if (!event.shiftKey && document.activeElement === button) {
      event.preventDefault();
      input.focus();
    }
  }

  function cycle(step) {
    // Views only. A panel is a modal, and a modal is not a stop on a tour of the
    // page behind it: stepping right from Charts would leave the user staring at
    // a Community list they did not ask to walk past.
    const ids = (shell() && shell().pages && shell().pages()) || [];
    if (!ids.length || !shell()) return;
    const current = (shell().state() || {}).destination || "dashboard";
    const index = Math.max(0, ids.indexOf(current));
    shell().go(ids[(index + step + ids.length) % ids.length]);
  }

  function onKeydown(event) {
    if (event.defaultPrevented || event.isComposing) return;
    // Any open dialog owns the keyboard, except the work panel. That one is the
    // modal the platform keeps *over* the page, and the palette is the door out
    // of anywhere -- a panel the user cannot ⌘K away from is a room, not a door.
    // The palette's own dialog stacks above it, and closes back onto it.
    if (document.querySelector("dialog[open]:not(#workPanel)")) return;

    const target = event.target;
    const tag = target && target.tagName;
    if (
      tag === "INPUT" ||
      tag === "TEXTAREA" ||
      tag === "SELECT" ||
      (target && target.isContentEditable)
    ) {
      return;
    }

    const mod = event.metaKey || event.ctrlKey;
    const key = String(event.key || "").toLowerCase();

    if (mod && !event.altKey && key === "k") {
      if (!signedIn()) return;
      event.preventDefault();
      // The chart's key handler is on the window; stopping here keeps ⌘K from
      // reaching it even if a later change gives it an opinion about the key.
      event.stopPropagation();
      openPalette(document.activeElement);
      return;
    }
    if (mod && !event.altKey && event.key === "/") {
      event.preventDefault();
      event.stopPropagation();
      openShortcuts(document.activeElement);
      return;
    }
    if (!signedIn()) return;
    if (event.key === "?" && !mod && !event.altKey) {
      event.preventDefault();
      event.stopPropagation();
      openShortcuts(document.activeElement);
      return;
    }
    if (!event.altKey || event.metaKey || event.ctrlKey) return;

    // `event.code`, not `event.key`: Option+1 on macOS produces "¡", and the
    // physical key is what the shortcut names.
    const digit = /^Digit([1-6])$/.exec(event.code || "");
    if (digit) {
      event.preventDefault();
      event.stopPropagation();
      const destination = destinations()[Number(digit[1]) - 1];
      if (destination && shell()) shell().go(destination.id);
      return;
    }
    if (event.code === "ArrowLeft" || event.code === "ArrowRight") {
      event.preventDefault();
      event.stopPropagation();
      cycle(event.code === "ArrowRight" ? 1 : -1);
    }
  }

  /* ---- Wiring -------------------------------------------------------------- */

  function wire() {
    dialog = document.getElementById("palette");
    input = document.getElementById("paletteInput");
    list = document.getElementById("paletteList");
    empty = document.getElementById("paletteEmpty");
    status = document.getElementById("paletteStatus");
    sheet = document.getElementById("shortcuts");
    if (!dialog || !input || !list || !empty || !status || !sheet) return;

    for (const node of [dialog, sheet]) {
      node.addEventListener("keydown", (event) => {
        // Escape and every other key inside a modal belong to the modal. The
        // chart's window-level handler must not cancel a drawing behind it.
        event.stopPropagation();
        if (event.key === "Escape") {
          event.preventDefault();
          if (node === dialog) close();
          else closeShortcuts();
        }
      });
      node.addEventListener("cancel", (event) => {
        event.preventDefault();
        if (node === dialog) close();
        else closeShortcuts();
      });
      node.addEventListener("click", (event) => {
        if (event.target !== node) return;
        if (node === dialog) close();
        else closeShortcuts();
      });
    }

    input.addEventListener("input", onInput);
    input.addEventListener("keydown", onPaletteKey);
    const closePaletteButton = dialog.querySelector("[data-close]");
    if (closePaletteButton) closePaletteButton.addEventListener("click", close);
    const closeSheetButton = sheet.querySelector("[data-close]");
    if (closeSheetButton) closeSheetButton.addEventListener("click", closeShortcuts);

    const openButton = document.getElementById("paletteBtn");
    if (openButton) openButton.addEventListener("click", () => openPalette(openButton));
    const help = document.getElementById("shortcutsBtn");
    if (help) help.addEventListener("click", () => openShortcuts(help));

    for (const node of document.querySelectorAll("[data-mod]")) node.textContent = MOD;
    const hint = document.querySelector("#paletteBtn .paletteKey");
    if (hint) hint.textContent = `${MOD}K`;

    document.addEventListener("keydown", onKeydown);
  }

  window.HRZPalette = {
    open: () => openPalette(document.activeElement),
    shortcuts: () => openShortcuts(document.activeElement),
    close: () => {
      close();
      closeShortcuts();
    },
  };

  wire();
})();
