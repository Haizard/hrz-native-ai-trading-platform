/* HRZUI — the behaviour layer.
 *
 * CSS-first: a class is the contract and the state lives in an ARIA attribute.
 * This file adds only what CSS cannot: focus, keyboard, timers, positioning.
 * No component here concatenates a caller's string into HTML.
 */
(function () {
  const UI = (window.HRZUI = window.HRZUI || {});

  const esc = (value) =>
    String(value ?? "").replace(/[&<>"']/g, (c) => ({
      "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
    })[c]);

  const node = (tag, className, text) => {
    const el = document.createElement(tag);
    if (className) el.className = className;
    if (text !== undefined) el.textContent = text;
    return el;
  };

  let seq = 0;
  const uid = (prefix) => `${prefix}-${++seq}`;

  UI.escape = esc;

  /* ---- Formatting -------------------------------------------------------- */
  const priceFormat = new Intl.NumberFormat("en-US", {
    minimumFractionDigits: 2, maximumFractionDigits: 2,
  });
  const compactFormat = new Intl.NumberFormat("en-US", {
    notation: "compact", maximumFractionDigits: 1,
  });

  UI.format = {
    price(value, decimals) {
      const n = Number(value);
      if (!Number.isFinite(n)) return "—";
      if (decimals !== undefined) {
        return n.toLocaleString("en-US", {
          minimumFractionDigits: decimals, maximumFractionDigits: decimals,
        });
      }
      return priceFormat.format(n);
    },
    percent(value, digits = 2) {
      const n = Number(value);
      if (!Number.isFinite(n)) return "—";
      const sign = n > 0 ? "+" : n < 0 ? "−" : "";
      return `${sign}${Math.abs(n).toFixed(digits)}%`;
    },
    volume(value) {
      const n = Number(value);
      return Number.isFinite(n) ? compactFormat.format(n).toUpperCase() : "—";
    },
    compact(value) {
      const n = Number(value);
      return Number.isFinite(n) ? compactFormat.format(n).toUpperCase() : "—";
    },
    time(value) {
      const d = value instanceof Date ? value : new Date(value);
      if (Number.isNaN(d.getTime())) return "—";
      return d.toLocaleTimeString("en-GB", { hour12: false });
    },
  };

  /* ---- Toast ------------------------------------------------------------- */
  const TOAST_MAX = 3;
  const toasts = () => {
    let host = document.getElementById("hrzToasts");
    if (!host) {
      host = node("div", "hrz-toasts");
      host.id = "hrzToasts";
      document.body.appendChild(host);
    }
    return host;
  };

  UI.toast = function toast(message, options = {}) {
    const { tone = "info", action, timeout = 4000 } = options;
    const host = toasts();
    while (host.children.length >= TOAST_MAX) host.firstElementChild.remove();

    const el = node("div", "hrz-toast");
    if (tone === "error" || tone === "warn" || tone === "up") {
      el.classList.add(`hrz-toast--${tone}`);
    }
    el.setAttribute("role", tone === "error" ? "alert" : "status");
    el.appendChild(node("span", "u-grow", String(message)));

    let timer = 0;
    const dismiss = () => {
      if (timer) clearTimeout(timer);
      if (el.isConnected) el.remove();
    };
    if (action && action.label) {
      const button = node("button", "hrz-toast__action", action.label);
      button.type = "button";
      button.addEventListener("click", () => {
        action.onSelect?.();
        dismiss();
      });
      el.appendChild(button);
    }
    const close = node("button", "hrz-btn hrz-btn--sm hrz-btn--ghost hrz-btn--icon", "×");
    close.type = "button";
    close.setAttribute("aria-label", "Dismiss");
    close.addEventListener("click", dismiss);
    el.appendChild(close);

    host.appendChild(el);
    if (timeout > 0) timer = setTimeout(dismiss, timeout);
    return { dismiss };
  };

  /* ---- Tooltip ----------------------------------------------------------- */
  let tipTimer = 0;
  let tipNode = null;

  function hideTip() {
    if (tipTimer) clearTimeout(tipTimer);
    tipTimer = 0;
    tipNode?.remove();
    tipNode = null;
  }

  function showTip(target, text) {
    hideTip();
    tipNode = node("div", "hrz-tooltip", text);
    tipNode.id = uid("tip");
    tipNode.setAttribute("role", "tooltip");
    document.body.appendChild(tipNode);
    const rect = target.getBoundingClientRect();
    const tipRect = tipNode.getBoundingClientRect();
    const below = rect.top < tipRect.height + 8;
    tipNode.style.setProperty("--x", `${Math.max(8, rect.left + rect.width / 2 - tipRect.width / 2)}px`);
    tipNode.style.setProperty("--y", `${below ? rect.bottom + 6 : rect.top - tipRect.height - 6}px`);
  }

  UI.tooltip = function tooltip(target, text) {
    target.setAttribute("aria-describedby", target.getAttribute("aria-describedby") || "");
    const enter = () => {
      tipTimer = setTimeout(() => {
        showTip(target, text);
        target.setAttribute("aria-describedby", tipNode.id);
      }, 300);
    };
    const leave = () => {
      hideTip();
      target.removeAttribute("aria-describedby");
    };
    target.addEventListener("mouseenter", enter);
    target.addEventListener("mouseleave", leave);
    target.addEventListener("focus", enter);
    target.addEventListener("blur", leave);
    return {
      destroy() {
        leave();
        target.removeEventListener("mouseenter", enter);
        target.removeEventListener("mouseleave", leave);
        target.removeEventListener("focus", enter);
        target.removeEventListener("blur", leave);
      },
    };
  };

  /* ---- Anchored overlay -------------------------------------------------- */
  function place(overlay, anchor, align = "start") {
    const rect = anchor.getBoundingClientRect();
    overlay.style.visibility = "hidden";
    overlay.style.setProperty("--x", "0px");
    overlay.style.setProperty("--y", "0px");
    document.body.appendChild(overlay);
    const box = overlay.getBoundingClientRect();
    const x = align === "end" ? rect.right - box.width : rect.left;
    const flipUp = rect.bottom + box.height + 8 > window.innerHeight;
    overlay.style.setProperty("--x", `${Math.max(8, Math.min(x, window.innerWidth - box.width - 8))}px`);
    overlay.style.setProperty("--y", `${flipUp ? Math.max(8, rect.top - box.height - 4) : rect.bottom + 4}px`);
    overlay.style.visibility = "";
  }

  function dismissOnOutside(overlay, onClose) {
    const handler = (event) => {
      if (!overlay.contains(event.target)) onClose();
    };
    // `setTimeout` defers the listener past the click that opened the overlay.
    const id = setTimeout(() => document.addEventListener("pointerdown", handler), 0);
    return () => {
      clearTimeout(id);
      document.removeEventListener("pointerdown", handler);
    };
  }

  /* ---- Menu -------------------------------------------------------------- */
  UI.menu = function menu(trigger, items) {
    let overlay = null;
    let removeOutside = null;

    const close = () => {
      if (!overlay) return;
      removeOutside?.();
      overlay.remove();
      overlay = null;
      // A menu is opened by its trigger, so the trigger is where focus goes
      // back -- not wherever the page happened to be focused before.
      trigger.focus?.();
    };

    const open = () => {
      if (overlay) return;
      overlay = node("div", "hrz-menu");
      overlay.setAttribute("role", "menu");
      overlay.setAttribute("aria-label", "Actions");

      for (const item of items) {
        if (item.separator) {
          overlay.appendChild(node("div", "hrz-menu__sep"));
          continue;
        }
        const button = node("button", "hrz-menu__item");
        button.type = "button";
        button.setAttribute("role", "menuitem");
        if (item.danger) button.classList.add("hrz-menu__item--danger");
        if (item.disabled) button.disabled = true;
        if (item.icon && UI.icon) button.appendChild(UI.icon(item.icon, { size: 14 }));
        button.appendChild(node("span", "u-grow", item.label));
        if (item.hint) button.appendChild(node("span", "hrz-menu__hint", item.hint));
        button.addEventListener("click", () => {
          close();
          item.onSelect?.();
        });
        overlay.appendChild(button);
      }

      place(overlay, trigger);
      removeOutside = dismissOnOutside(overlay, close);
      overlay.addEventListener("keydown", (event) => {
        const rows = [...overlay.querySelectorAll('[role="menuitem"]:not([disabled])')];
        const at = rows.indexOf(document.activeElement);
        if (event.key === "ArrowDown") {
          event.preventDefault();
          rows[(at + 1) % rows.length]?.focus();
        } else if (event.key === "ArrowUp") {
          event.preventDefault();
          rows[(at - 1 + rows.length) % rows.length]?.focus();
        } else if (event.key === "Home") {
          event.preventDefault();
          rows[0]?.focus();
        } else if (event.key === "End") {
          event.preventDefault();
          rows[rows.length - 1]?.focus();
        } else if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          close();
        } else if (event.key === "Tab") {
          close();
        }
      });
      overlay.querySelector('[role="menuitem"]:not([disabled])')?.focus();
    };

    trigger.setAttribute("aria-haspopup", "menu");
    trigger.setAttribute("aria-expanded", "false");
    const toggle = () => {
      if (overlay) close();
      else open();
      trigger.setAttribute("aria-expanded", String(Boolean(overlay)));
    };
    trigger.addEventListener("click", toggle);
    return {
      open: () => { open(); trigger.setAttribute("aria-expanded", "true"); },
      close: () => { close(); trigger.setAttribute("aria-expanded", "false"); },
    };
  };

  /* ---- Popover ----------------------------------------------------------- */
  UI.popover = function popover(trigger, content, options = {}) {
    let overlay = null;
    let removeOutside = null;
    let previousFocus = null;

    const close = () => {
      if (!overlay) return;
      removeOutside?.();
      overlay.remove();
      overlay = null;
      (previousFocus || trigger).focus?.();
    };

    const open = () => {
      if (overlay) return;
      previousFocus = document.activeElement;
      overlay = node("div", "hrz-popover");
      overlay.setAttribute("role", "dialog");
      if (options.label) overlay.setAttribute("aria-label", options.label);
      if (options.head) overlay.appendChild(node("div", "hrz-popover__head", options.head));
      if (typeof content === "string") overlay.appendChild(node("div", "", content));
      else overlay.appendChild(content);
      place(overlay, trigger, options.align);
      removeOutside = dismissOnOutside(overlay, close);
      overlay.addEventListener("keydown", (event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          close();
        }
      });
      overlay.querySelector("button, a, input, select, textarea")?.focus();
    };

    trigger.setAttribute("aria-haspopup", "dialog");
    trigger.setAttribute("aria-expanded", "false");
    trigger.addEventListener("click", () => {
      if (overlay) close();
      else open();
      trigger.setAttribute("aria-expanded", String(Boolean(overlay)));
    });
    return { open, close, element: () => overlay };
  };

  /* ---- Dialog ------------------------------------------------------------ */
  UI.dialog = function dialog({ title, body, actions = [], tone = "" }) {
    return new Promise((resolve) => {
      const previousFocus = document.activeElement;
      const el = node("dialog", "hrz-dialog");
      if (tone) el.dataset.tone = tone;
      el.setAttribute("aria-labelledby", uid("dialog-title"));
      const heading = node("h2", "hrz-dialog__title", title);
      heading.id = el.getAttribute("aria-labelledby");
      const bodyNode = node("div", "hrz-dialog__body");
      if (typeof body === "string") bodyNode.textContent = body;
      else if (body) bodyNode.appendChild(body);
      const foot = node("div", "hrz-dialog__foot");

      const settle = (value) => {
        el.close();
        el.remove();
        previousFocus?.focus?.();
        resolve(value);
      };

      for (const action of actions) {
        const button = node(
          "button",
          `hrz-btn ${action.tone === "primary" ? "hrz-btn--primary" : action.tone === "danger" ? "hrz-btn--danger" : "hrz-btn--secondary"}`,
          action.label
        );
        button.type = "button";
        button.addEventListener("click", () => settle(action.value));
        foot.appendChild(button);
      }

      el.append(heading, bodyNode, foot);
      el.addEventListener("cancel", (event) => {
        event.preventDefault();
        settle(null);
      });
      document.body.appendChild(el);
      el.showModal();
      const first = el.querySelector(".hrz-btn--danger, .hrz-btn--primary, .hrz-btn--secondary");
      first?.focus();
    });
  };

  UI.confirm = function confirm({ title, body, confirmLabel = "Confirm", tone = "danger" }) {
    return UI.dialog({
      title,
      body,
      actions: [
        { label: "Cancel", value: false },
        { label: confirmLabel, value: true, tone },
      ],
    }).then(Boolean);
  };

  /* ---- Panel ------------------------------------------------------------- */
  /// The subpanel the chart platforms float over the chart: a modal whose body
  /// is one of several hosts that already exist on the page.
  ///
  /// Deliberately dumb. It knows a name, a title, a width and which host is on
  /// screen; it does not know that `community` is a destination or that closing
  /// should change the address. That belongs to whoever owns the panel, and
  /// keeping it out here is what lets the hosts stay exactly where their
  /// builders put them -- this only says which of them is showing.
  ///
  /// Native `<dialog>` carries the modal contract that cannot be written by
  /// hand: the top layer above the chart, the scrim, the focus trap, and input
  /// frozen while the clocks keep running. So Escape and the close button do not
  /// close anything themselves -- they ask the owner, which is the only way the
  /// address and the panel can never disagree about what is open.
  const panelApis = new WeakMap();

  UI.panel = function panel(root, options = {}) {
    let api = panelApis.get(root);
    if (!api) {
      let opener = null;
      let current = null;

      const titleNode = root.querySelector(".hrz-dialog__title");
      const bodies = [...root.querySelectorAll("[data-body]")];
      const tabs = [...root.querySelectorAll(".hrz-dialog__head [data-tab]")];

      const show = (name, { title, width = "medium", opener: trigger } = {}) => {
        // Where closing hands focus back to. An owner that names a control wins
        // on every call, because the panel is one modal wearing several faces:
        // switching from Community to Account while it stays open has to move the
        // return address too, or Escape would hand focus to a door that is no
        // longer the one showing. With no name given, only a panel that was shut
        // may capture the focused element -- reopening the one already on screen
        // must not move it.
        if (trigger) opener = trigger;
        else if (!root.open) {
          const candidate = document.activeElement;
          opener = candidate && candidate !== document.body && candidate !== document.documentElement
            ? candidate
            : null;
        }
        current = name;
        root.dataset.width = width;
        if (title !== undefined && titleNode) titleNode.textContent = title;
        for (const body of bodies) body.hidden = body.dataset.body !== name;
        for (const tab of tabs) {
          tab.setAttribute("aria-selected", String(tab.dataset.tab === name));
        }
        if (!root.open) root.showModal();
      };

      const hide = () => {
        if (root.open) root.close();
      };

      const ask = () => {
        if (api.onRequestClose) api.onRequestClose(current);
        else hide();
      };

      root.querySelector(".hrz-dialog__close")?.addEventListener("click", ask);
      root.addEventListener("cancel", (event) => {
        // Escape would close without asking, which can leave the address
        // claiming a panel nobody can see.
        event.preventDefault();
        ask();
      });
      root.addEventListener("click", (event) => {
        if (event.target !== root) return;
        // A backdrop click is delivered to the dialog itself; the box is the
        // only way to tell it from a click on the panel's own padding.
        const box = root.getBoundingClientRect();
        const inBox =
          event.clientX >= box.left && event.clientX <= box.right &&
          event.clientY >= box.top && event.clientY <= box.bottom;
        if (!inBox) ask();
      });
      for (const tab of tabs) {
        // Selecting a host is the owner's decision, not this file's: the shell
        // writes the address, and the round trip through `show` is the only way
        // a host becomes visible. So a click here never changes the panel.
        tab.addEventListener("click", () => api.onSelect?.(tab.dataset.tab));
      }
      root.addEventListener("close", () => {
        current = null;
        // Where focus came from is where it goes, the promise `UI.menu` and
        // `UI.popover` already make. The dialog's own `close` event is the one
        // place it happens, so the button, Escape and the owner all hand focus
        // back to the same control.
        opener?.focus?.();
        opener = null;
      });

      api = {
        show,
        hide,
        isOpen: () => root.open,
        name: () => current,
        onRequestClose: null,
        onSelect: null,
      };
      panelApis.set(root, api);
    }
    // What closing and selecting *mean* belongs to the owner, and the owner may
    // be rebuilt while the panel keeps its identity -- so the handlers are set
    // on every call rather than captured once.
    if (options.onRequestClose) api.onRequestClose = options.onRequestClose;
    if (options.onSelect) api.onSelect = options.onSelect;
    return api;
  };

  /* ---- Drawer ------------------------------------------------------------ */
  UI.drawer = function drawer({ side = "right", title, content, onClose }) {
    const bottom = side === "bottom";
    const host = node("div", `hrz-drawer${bottom ? " hrz-drawer--bottom" : ""}`);
    host.setAttribute("role", "dialog");
    host.setAttribute("aria-modal", "true");
    if (title) {
      host.setAttribute("aria-labelledby", uid("drawer-title"));
      const heading = node("h2", "hrz-panel__title", title);
      heading.id = host.getAttribute("aria-labelledby");
      heading.style.padding = "var(--space-3) var(--space-3) 0";
      if (bottom) host.appendChild(node("div", "hrz-drawer__handle"));
      host.appendChild(heading);
    }
    const body = node("div", "u-grow");
    body.style.overflow = "auto";
    body.style.padding = "var(--space-3)";
    if (typeof content === "string") body.textContent = content;
    else body.appendChild(content);
    host.appendChild(body);

    let previousFocus = null;
    const onKey = (event) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        close();
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = [...host.querySelectorAll("button, a[href], input, select, textarea, [tabindex]:not([tabindex='-1'])")];
      if (!focusable.length) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    const close = () => {
      if (!host.isConnected) return;
      document.removeEventListener("keydown", onKey);
      host.remove();
      previousFocus?.focus?.();
      onClose?.();
    };

    return {
      open() {
        previousFocus = document.activeElement;
        document.body.appendChild(host);
        document.addEventListener("keydown", onKey);
        host.querySelector("button, a, input, select, textarea")?.focus();
      },
      close,
      element: () => host,
    };
  };

  /* ---- Tabs -------------------------------------------------------------- */
  UI.tabs = function tabs(root) {
    const list = [...root.querySelectorAll('[role="tab"]')];
    // Panels are siblings of the tablist, not document-wide: a shell with six
    // tab strips must not let one strip hide another's panels.
    const scope = root.parentElement || root;
    const panels = [...scope.querySelectorAll("[data-panel]")];

    const select = (name) => {
      for (const panel of panels) {
        if (!panel.id) panel.id = uid("panel");
        panel.setAttribute("role", "tabpanel");
        const tab = list.find((entry) => entry.dataset.tab === panel.dataset.panel);
        if (!tab) continue;
        if (!tab.id) tab.id = uid("tab");
        tab.setAttribute("aria-controls", panel.id);
        panel.setAttribute("aria-labelledby", tab.id);
      }
      for (const tab of list) {
        const on = tab.dataset.tab === name;
        tab.setAttribute("aria-selected", String(on));
        tab.tabIndex = on ? 0 : -1;
        if (on) tab.removeAttribute("aria-hidden");
      }
      for (const panel of panels) {
        const match = list.find((tab) => tab.dataset.tab === panel.dataset.panel);
        panel.hidden = panel.dataset.panel !== name || !match;
      }
      root.dispatchEvent(new CustomEvent("hrz:tabs", { detail: { name }, bubbles: true }));
    };

    list.forEach((tab, index) => {
      if (!tab.id) tab.id = uid("tab");
      tab.addEventListener("click", () => select(tab.dataset.tab));
      tab.addEventListener("keydown", (event) => {
        const at = list.indexOf(tab);
        let next = null;
        if (event.key === "ArrowRight") next = list[(at + 1) % list.length];
        else if (event.key === "ArrowLeft") next = list[(at - 1 + list.length) % list.length];
        else if (event.key === "Home") next = list[0];
        else if (event.key === "End") next = list[list.length - 1];
        if (next) {
          event.preventDefault();
          next.focus();
          select(next.dataset.tab);
        }
      });
    });

    const initial = list.find((tab) => tab.getAttribute("aria-selected") === "true") || list[0];
    if (initial) select(initial.dataset.tab);
    return { select };
  };

  /* ---- Tabs, keyboard ----------------------------------------------------- */
  /// One Tab stop and four arrow keys for a strip whose owner already knows
  /// what selection means.
  ///
  /// `UI.tabs` above owns its panels and can therefore move the selection
  /// itself. The shell's strips cannot: `#inspectorTabs` swaps an inspector
  /// pane, the dock's swaps a workbench tab, and the dashboard's re-sorts a
  /// table. So this helper stays out of the meaning entirely -- it moves focus
  /// and dispatches a click, and the host's own click handler remains the one
  /// thing that decides what a selection does.
  ///
  /// The Tab stop follows `aria-selected`, so a strip is still one stop for a
  /// keyboard, which is what `role="tablist"` promises. Idempotent: the shell
  /// calls it after every sync, and a second call only re-reads the selection.
  const tabKeyboards = new WeakMap();
  UI.tabKeyboard = function tabKeyboard(root) {
    let keyboard = tabKeyboards.get(root);
    if (!keyboard) {
      const tabs = () => [...root.querySelectorAll('[role="tab"]')];
      keyboard = {
        refresh() {
          const list = tabs();
          const on = list.find((tab) => tab.getAttribute("aria-selected") === "true") || list[0];
          for (const tab of list) tab.tabIndex = tab === on ? 0 : -1;
        },
      };
      root.addEventListener("keydown", (event) => {
        const list = tabs();
        const at = list.indexOf(document.activeElement);
        if (at < 0) return;
        let next = null;
        if (event.key === "ArrowRight") next = list[(at + 1) % list.length];
        else if (event.key === "ArrowLeft") next = list[(at - 1 + list.length) % list.length];
        else if (event.key === "Home") next = list[0];
        else if (event.key === "End") next = list[list.length - 1];
        if (!next) return;
        event.preventDefault();
        next.focus();
        next.click();
      });
      tabKeyboards.set(root, keyboard);
    }
    keyboard.refresh();
    return keyboard;
  };

  /* ---- Table ------------------------------------------------------------- */
  UI.table = function table(root) {
    const sort = (key) => {
      const head = root.querySelector(`th[data-key="${key}"]`);
      if (!head) return;
      const next = head.getAttribute("aria-sort") === "ascending" ? "descending" : "ascending";
      for (const th of root.querySelectorAll("th[aria-sort]")) th.setAttribute("aria-sort", "none");
      head.setAttribute("aria-sort", next);

      const body = root.querySelector("tbody");
      const rows = [...body.querySelectorAll("tr")];
      const index = [...head.parentElement.children].indexOf(head);
      const value = (row) => {
        const cell = row.children[index];
        return cell?.dataset.sort ?? cell?.textContent ?? "";
      };
      const numeric = rows.every((row) => Number.isFinite(Number(value(row).replace(/[^\d.-]/g, ""))));
      rows.sort((a, b) => {
        const av = value(a);
        const bv = value(b);
        const cmp = numeric
          ? Number(av.replace(/[^\d.-]/g, "")) - Number(bv.replace(/[^\d.-]/g, ""))
          : av.localeCompare(bv);
        return next === "ascending" ? cmp : -cmp;
      });
      for (const row of rows) body.appendChild(row);
      body.dispatchEvent(new CustomEvent("hrz:sort", { bubbles: true }));
    };

    root.querySelectorAll("th[data-key]").forEach((head) => {
      head.setAttribute("aria-sort", head.getAttribute("aria-sort") || "none");
      const button = head.querySelector(".hrz-table__sort");
      button?.addEventListener("click", () => sort(head.dataset.key));
    });

    return {
      sort,
      setRows(rows) {
        const body = root.querySelector("tbody");
        body.replaceChildren();
        for (const row of rows) {
          const tr = node("tr");
          for (const cell of row) {
            const td = node("td", cell?.className || "", cell?.text ?? String(cell));
            if (cell?.sort !== undefined) td.dataset.sort = cell.sort;
            tr.appendChild(td);
          }
          body.appendChild(tr);
        }
      },
    };
  };

  /* ---- Sparkline --------------------------------------------------------- */
  UI.sparkline = function sparkline(canvas, values, options = {}) {
    const trend = options.trend || "up";
    const width = 64;
    const height = 20;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = width * dpr;
    canvas.height = height * dpr;
    canvas.classList.add("hrz-spark");
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.scale(dpr, dpr);
    ctx.clearRect(0, 0, width, height);
    if (!values || values.length < 2) return;

    const min = Math.min(...values);
    const max = Math.max(...values);
    const span = max - min || 1;
    const x = (i) => (i / (values.length - 1)) * width;
    const y = (v) => height - 2 - ((v - min) / span) * (height - 4);
    const token = trend === "down" ? "--down" : "--up";
    // A canvas paints an unresolved `var(--up)` as nothing at all. Resolve the
    // token against this canvas's own tree, so a light theme gets its green and
    // a canvas that is not in the document still gets the literal.
    const resolved = window.getComputedStyle(canvas).getPropertyValue(token).trim();
    const stroke = resolved || (trend === "down" ? "#ea3943" : "#16c784");

    ctx.beginPath();
    values.forEach((v, i) => (i ? ctx.lineTo(x(i), y(v)) : ctx.moveTo(x(i), y(v))));
    ctx.strokeStyle = stroke;
    ctx.lineWidth = 1.25;
    ctx.stroke();

    ctx.lineTo(width, height);
    ctx.lineTo(0, height);
    ctx.closePath();
    ctx.globalAlpha = 0.1;
    ctx.fillStyle = stroke;
    ctx.fill();
    ctx.globalAlpha = 1;

    canvas.setAttribute("role", "img");
    canvas.setAttribute(
      "aria-label",
      `${options.label || "Trend"}: min ${UI.format.price(min)}, max ${UI.format.price(max)}, last ${UI.format.price(values[values.length - 1])}`
    );
  };

  /* ---- Meter ------------------------------------------------------------- */
  UI.meter = function meter(el, { value, max = 100, tone = "", label }) {
    const ratio = max > 0 ? Math.max(0, Math.min(1, value / max)) : 0;
    el.classList.add("hrz-meter");
    if (tone) el.classList.add(`hrz-meter--${tone}`);
    el.setAttribute("role", "progressbar");
    el.setAttribute("aria-valuemin", "0");
    el.setAttribute("aria-valuemax", String(max));
    el.setAttribute("aria-valuenow", String(value));
    if (label) el.setAttribute("aria-label", label);
    el.style.setProperty("--value", String(ratio));
    el.replaceChildren();
    if (label) el.appendChild(node("span", "hrz-meter__label", label));
    el.appendChild(node("span", "hrz-meter__value", `${Math.round(ratio * 100)}%`));
    const bar = node("div", "hrz-meter__bar");
    bar.appendChild(node("div", "hrz-meter__fill"));
    el.appendChild(bar);
  };

  /* ---- Skeleton, empty, error -------------------------------------------- */
  UI.skeleton = function skeleton(el, { rows = 3, kind = "text" } = {}) {
    el.classList.add("hrz-skeleton", `hrz-skeleton--${kind}`);
    el.replaceChildren();
    for (let i = 0; i < rows; i += 1) el.appendChild(node("div"));
    el.setAttribute("aria-hidden", "true");
  };

  UI.empty = function empty(el, { icon, title, body, action } = {}) {
    el.classList.add("hrz-empty");
    el.replaceChildren();
    if (icon && UI.icon) el.appendChild(UI.icon(icon, { size: 20 }));
    if (title) el.appendChild(node("div", "hrz-empty__title", title));
    if (body) el.appendChild(node("div", "hrz-empty__body", body));
    if (action?.label) {
      const button = node("button", "hrz-btn hrz-btn--secondary hrz-btn--sm hrz-empty__action", action.label);
      button.type = "button";
      button.addEventListener("click", () => action.onClick?.());
      el.appendChild(button);
    }
  };

  UI.errorState = function errorState(el, { title = "Something went wrong", message, retry } = {}) {
    el.classList.add("hrz-error");
    el.replaceChildren();
    if (UI.icon) el.appendChild(UI.icon("warning", { size: 20, label: "" }));
    el.appendChild(node("div", "hrz-error__title", title));
    if (message) el.appendChild(node("div", "hrz-error__msg", message));
    if (retry) {
      const button = node("button", "hrz-btn hrz-btn--secondary hrz-btn--sm hrz-error__retry", "Retry");
      button.type = "button";
      button.addEventListener("click", retry);
      el.appendChild(button);
    }
  };
})();
