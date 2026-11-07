/* The public site's router.
 *
 * The workstation owns `#/dashboard`, `#/charts` and the panel segments. This
 * file owns the five pages a signed-out visitor can reach, and nothing else:
 * the markup is already in `index.html`, so routing a page is showing one
 * section and hiding the rest. No page is fetched, built or cloned.
 *
 * It is deliberately separate from `views/shell.js`. The shell's router refuses
 * to run without a token -- a signed-out address must not name an app
 * destination -- and the public pages need exactly the opposite gate. Two
 * routers with one condition each is simpler than one router with a mode, and
 * it keeps the marketing surface from growing branches inside the workstation.
 *
 * The auth card stays on the home page, and `app.js` still owns the request.
 * This file only moves the user to it.
 */
(function () {
  "use strict";

  const TOKEN_KEY = "atp.token";
  const HOME = "home";
  const ROUTES = ["home", "features", "pricing", "community", "docs"];

  const TITLES = {
    home: "HRZ — AI-native order-flow trading terminal",
    features: "Features — HRZ",
    pricing: "Pricing — HRZ",
    community: "Community — HRZ",
    docs: "Docs — HRZ",
  };

  const el = (id) => document.getElementById(id);
  const signedIn = () => Boolean(localStorage.getItem(TOKEN_KEY));

  let current = null;
  let started = false;
  let pendingAuth = false;
  let menuOpen = false;
  let menuReturnFocus = null;
  // Set when a navigation came from the phone sheet. The destination's heading
  // takes focus once it is on screen, because the link that was focused is now
  // inside a hidden menu.
  let focusPageOnShow = false;

  // The pricing cards are painted from the server's matrix the first time the
  // pricing page is opened. The fallback is the same matrix written down, so a
  // gateway that is down shows today's prices instead of an empty grid; the
  // fetch is what keeps the page honest when a plan changes.
  let plans = null;
  let plansLoading = null;
  let cycle = "monthly";
  const FALLBACK_PLANS = [
    { id: "free", name: "Free", tagline: "Chart, paper trade, and try the agent.", pricing: { monthly_cents: 0, annual_cents: 0, annual_effective_monthly_cents: 0 }, limits: { max_panes: 4, max_layouts: 3, max_alerts: 5, max_watchers: 1, max_ai_credits: 25, max_backtests: 5, history_days: 31, max_paper_bots: 1, max_live_bots: 0 }, features: { orderflow: false, orderflow_advanced: false, byo_model_key: false, priority_support: false, dedicated_onboarding: false, community_indicators: "included" } },
    { id: "core", name: "Core", tagline: "Live order flow and a daily workflow.", pricing: { monthly_cents: 1200, annual_cents: 10800, annual_effective_monthly_cents: 900 }, limits: { max_panes: 16, max_layouts: 25, max_alerts: 50, max_watchers: 5, max_ai_credits: 250, max_backtests: 50, history_days: 365, max_paper_bots: 3, max_live_bots: 0 }, features: { orderflow: true, orderflow_advanced: false, byo_model_key: false, priority_support: false, dedicated_onboarding: false, community_indicators: "included" } },
    { id: "pro", name: "Pro", tagline: "The full analytical desk, with one live bot.", pricing: { monthly_cents: 2400, annual_cents: 22800, annual_effective_monthly_cents: 1900 }, limits: { max_panes: 50, max_layouts: 100, max_alerts: 200, max_watchers: 20, max_ai_credits: 1200, max_backtests: 300, history_days: 1095, max_paper_bots: 10, max_live_bots: 1 }, features: { orderflow: true, orderflow_advanced: true, byo_model_key: false, priority_support: true, dedicated_onboarding: false, community_indicators: "included" } },
    { id: "elite", name: "Elite", tagline: "More of everything, plus your own model key.", pricing: { monthly_cents: 3600, annual_cents: 34800, annual_effective_monthly_cents: 2900 }, limits: { max_panes: 100, max_layouts: -1, max_alerts: 500, max_watchers: 50, max_ai_credits: 2500, max_backtests: -1, history_days: -1, max_paper_bots: 25, max_live_bots: 3 }, features: { orderflow: true, orderflow_advanced: true, byo_model_key: true, priority_support: true, dedicated_onboarding: false, community_indicators: "included" } },
    { id: "desk", name: "Desk", tagline: "For teams running several live books.", pricing: { monthly_cents: 5500, annual_cents: 52800, annual_effective_monthly_cents: 4400 }, limits: { max_panes: 100, max_layouts: -1, max_alerts: 1000, max_watchers: 100, max_ai_credits: 5000, max_backtests: -1, history_days: -1, max_paper_bots: 50, max_live_bots: 5 }, features: { orderflow: true, orderflow_advanced: true, byo_model_key: true, priority_support: true, dedicated_onboarding: true, community_indicators: "priority" } },
  ];

  /* ---- Address ------------------------------------------------------------ */

  /// The page the address names, or null when the hash is a fragment rather
  /// than a route. `#home` is the skip link and must not move the page.
  function fromHash() {
    const raw = String(location.hash || "");
    if (!raw || raw === "#" || raw === "#/") return HOME;
    if (!raw.startsWith("#/")) return null;
    const page = raw.slice(2).split("/").filter(Boolean)[0] || HOME;
    return ROUTES.includes(page) ? page : HOME;
  }

  function canonical(page) {
    return page === HOME ? "#/" : `#/${page}`;
  }

  /// Show one page and put the address in its canonical form.
  ///
  /// The rewrite is a replace, not a push: `#/features` and `#/features/` are
  /// one screen, and Back should leave the page rather than cycle spellings.
  function route(opts = {}) {
    if (signedIn()) return;
    const page = fromHash();
    if (page === null) return; // a fragment inside the current page
    const canonicalHash = canonical(page);
    if (location.hash !== canonicalHash) history.replaceState(null, "", canonicalHash);
    show(page, opts);
  }

  function show(page, opts = {}) {
    if (!ROUTES.includes(page)) page = HOME;
    current = page;

    let shown = null;
    for (const section of document.querySelectorAll("[data-public-page]")) {
      const on = section.dataset.publicPage === page;
      section.hidden = !on;
      if (on) shown = section;
    }
    for (const link of document.querySelectorAll("[data-public-route]")) {
      if (link.dataset.publicRoute === page) link.setAttribute("aria-current", "page");
      else link.removeAttribute("aria-current");
    }

    document.title = TITLES[page] || TITLES.home;
    // `data-public-view`, not `data-public-page`: the page sections are found by
    // the latter, and a body carrying the same attribute would match first and
    // hand every "the page's heading" query the home page's hidden h1.
    document.body.dataset.publicView = page;
    closeMenu({ restoreFocus: false });

    const home = el("home");
    if (home && !opts.keepScroll) home.scrollTop = 0;

    const announce = el("publicAnnounce");
    if (announce && started && page !== HOME) {
      const label = page.charAt(0).toUpperCase() + page.slice(1);
      announce.textContent = `${label} page`;
    }

    if (page === "pricing") loadPlans();

    // Re-observe reveal targets each time a page becomes visible, since the
    // observer ran at boot when non-home pages were still hidden.
    requestAnimationFrame(reObserveCurrentPage);

    if (pendingAuth && page === HOME) {
      pendingAuth = false;
      const pendingOpts = openAuth._pendingOpts || {};
      openAuth._pendingOpts = null;
      focusAuth(pendingOpts);
    }

    if (focusPageOnShow) {
      focusPageOnShow = false;
      const heading = shown && shown.querySelector("h1");
      if (heading) {
        heading.setAttribute("tabindex", "-1");
        // After the frame, because the browser's own fragment navigation for
        // the new hash runs behind this event and would reset focus to the
        // body. The heading is still what the keyboard should continue from.
        requestAnimationFrame(() => heading.focus({ preventScroll: true }));
      }
    }
  }

  /* ---- Auth --------------------------------------------------------------- */

  /// Send a signed-out visitor to the one auth card, wherever they are.
  ///
  /// A CTA on Pricing that leaves the user on Pricing with a focused field they
  /// cannot see is worse than no CTA, so the route moves first and the focus
  /// waits for the home page to be on screen.
  function openAuth(opts = {}) {
    if (signedIn()) return;
    closeMenu({ restoreFocus: false });
    pendingAuth = true;
    if (current !== HOME) {
      // Store the opts so we can pass them once the home page is on screen.
      openAuth._pendingOpts = opts;
      location.hash = "#/";
      return;
    }
    pendingAuth = false;
    focusAuth(opts);
  }

  function focusAuth(opts = {}) {
    const card = el("homeAuth");
    const reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (card && card.scrollIntoView) {
      card.scrollIntoView({ block: "center", behavior: reduce ? "auto" : "smooth" });
    }
    // If caller wants to pre-select the register tab, switch it now.
    if (opts.register) {
      const tabRegister = el("authTabRegister");
      if (tabRegister) tabRegister.click();
      const email = el("homeRegEmail");
      if (email) { email.focus({ preventScroll: true }); return; }
    }
    const email = el("homeEmail");
    if (email) email.focus({ preventScroll: true });
  }

  /* ---- The mobile menu ---------------------------------------------------- */

  function setMenu(open, opts = {}) {
    const restoreFocus = opts.restoreFocus !== false;
    menuOpen = Boolean(open);
    const sheet = el("publicMenu");
    const button = el("publicMenuBtn");
    if (sheet) sheet.hidden = !menuOpen;
    if (button) {
      button.setAttribute("aria-expanded", String(menuOpen));
      button.setAttribute("aria-label", menuOpen ? "Close the site menu" : "Open the site menu");
    }
    if (menuOpen) {
      menuReturnFocus = document.activeElement;
      const first = sheet && sheet.querySelector("a, button");
      if (first) first.focus();
    } else if (restoreFocus && menuReturnFocus && menuReturnFocus.focus) {
      menuReturnFocus.focus();
      menuReturnFocus = null;
    } else if (!restoreFocus) {
      // A navigation, not a dismissal: the user is going somewhere, so the
      // button must not take focus back from the page they asked for.
      menuReturnFocus = null;
    }
  }

  function closeMenu(opts = {}) {
    if (menuOpen) setMenu(false, opts);
  }

  /* ---- Pricing ------------------------------------------------------------ */

  function formatMoney(cents) {
    const value = Number(cents) || 0;
    const dollars = value / 100;
    return Number.isInteger(dollars) ? `$${dollars}` : `$${dollars.toFixed(2)}`;
  }

  function limitLabel(value, unit) {
    if (value < 0) return `Unlimited ${unit}`;
    return `${value.toLocaleString("en-US")} ${unit}`;
  }

  function planFeatures(plan) {
    const limit = plan.limits || {};
    const feature = plan.features || {};
    const items = [
      `${limit.max_panes} panes per layout`,
      limitLabel(limit.max_layouts, "saved layouts"),
      `${limit.max_alerts} alerts · ${limit.max_watchers} zone watchers`,
      `${limit.max_ai_credits.toLocaleString("en-US")} AI credits / month`,
      limitLabel(limit.max_backtests, "backtests / month"),
      limit.history_days < 0
        ? "Unlimited history"
        : limit.history_days >= 365
          ? `${Math.round(limit.history_days / 365)} year${limit.history_days >= 730 ? "s" : ""} of history`
          : `${limit.history_days} days of history`,
      `${limit.max_paper_bots} paper bot${limit.max_paper_bots === 1 ? "" : "s"}${
        limit.max_live_bots ? ` · ${limit.max_live_bots} live bot${limit.max_live_bots === 1 ? "" : "s"}` : ""
      }`,
    ];
    if (feature.orderflow) items.push("Live order flow");
    if (feature.orderflow_advanced) items.push("Advanced order flow");
    if (feature.byo_model_key) items.push("Bring your own model key");
    if (feature.priority_support) items.push("Priority support");
    if (feature.dedicated_onboarding) items.push("Dedicated onboarding");
    items.push(
      feature.community_indicators === "priority"
        ? "Community indicators · priority placement"
        : "Community indicators"
    );
    return items;
  }

  function priceFor(plan) {
    const pricing = plan.pricing || {};
    if (!pricing.monthly_cents) {
      return { amount: "$0", per: "forever", note: "" };
    }
    if (cycle === "annual") {
      return {
        amount: formatMoney(pricing.annual_effective_monthly_cents),
        per: "/mo billed yearly",
        note: `${formatMoney(pricing.annual_cents)} charged once a year`,
      };
    }
    return { amount: formatMoney(pricing.monthly_cents), per: "/mo", note: "" };
  }

  function renderPlans() {
    const grid = el("publicPriceGrid");
    if (!grid || !plans) return;
    grid.replaceChildren();
    for (const plan of plans) {
      const card = document.createElement("article");
      card.className = "priceCard" + (plan.id === "pro" ? " priceCard--featured" : "");
      card.dataset.plan = plan.id;

      if (plan.id === "pro") {
        const flag = document.createElement("span");
        flag.className = "priceCard__flag";
        flag.textContent = "Most popular";
        card.appendChild(flag);
      }

      const head = document.createElement("header");
      head.className = "priceCard__head";
      const name = document.createElement("h2");
      name.textContent = plan.name;
      const tagline = document.createElement("p");
      tagline.textContent = plan.tagline;
      head.append(name, tagline);

      const price = document.createElement("p");
      price.className = "priceCard__price";
      const amount = document.createElement("span");
      amount.className = "priceCard__amount";
      const per = document.createElement("span");
      per.className = "priceCard__per";
      price.append(amount, per);

      const note = document.createElement("p");
      note.className = "priceCard__note";

      const cta = document.createElement("button");
      cta.type = "button";
      cta.className = "pubBtn priceCard__cta" + (plan.id === "pro" ? " pubBtn--primary" : "");
      cta.dataset.publicAuth = "";
      cta.textContent = plan.id === "free" ? "Start free" : `Choose ${plan.name}`;

      const list = document.createElement("ul");
      list.className = "priceCard__list";
      for (const item of planFeatures(plan)) {
        const li = document.createElement("li");
        li.textContent = item;
        list.appendChild(li);
      }

      card.append(head, price, note, cta, list);
      card._paint = () => {
        const shown = priceFor(plan);
        amount.textContent = shown.amount;
        per.textContent = shown.per;
        note.textContent = shown.note;
      };
      card._paint();
      grid.appendChild(card);
    }
  }

  function paintCycle() {
    for (const button of document.querySelectorAll("[data-billing-cycle]")) {
      button.setAttribute("aria-pressed", String(button.dataset.billingCycle === cycle));
    }
    for (const card of document.querySelectorAll("#publicPriceGrid .priceCard")) {
      if (card._paint) card._paint();
    }
  }

  function loadPlans() {
    // A failed fetch leaves the fallback in place, so the next visit to the
    // page tries again rather than pinning yesterday's fallback for the whole
    // session.
    if (plans && plans !== FALLBACK_PLANS) return;
    if (plansLoading) return;
    plansLoading = fetch("/billing/plans", { headers: { accept: "application/json" } })
      .then((response) => {
        if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
        return response.json();
      })
      .then((body) => {
        if (!body || !Array.isArray(body.plans) || !body.plans.length) throw new Error("empty catalog");
        plans = body.plans;
        renderPlans();
      })
      .catch(() => {
        // Keep the page usable when the API is unreachable. The fallback is the
        // same matrix; the next visit retries the fetch.
        plans = FALLBACK_PLANS;
        renderPlans();
      })
      .finally(() => {
        plansLoading = null;
      });
  }

  /* ---- Auth tab switching ------------------------------------------------- */

  function wireAuthTabs() {
    const tabSignin   = el("authTabSignin");
    const tabRegister = el("authTabRegister");
    const panelSignin = el("authPanelSignin");
    const panelRegister = el("authPanelRegister");
    if (!tabSignin || !tabRegister) return;

    function activateTab(which) {
      const isSignin = which === "signin";
      tabSignin.setAttribute("aria-selected",   String(isSignin));
      tabRegister.setAttribute("aria-selected",  String(!isSignin));
      tabSignin.classList.toggle("homeAuth__tab--active",   isSignin);
      tabRegister.classList.toggle("homeAuth__tab--active", !isSignin);
      if (panelSignin)   panelSignin.hidden   = !isSignin;
      if (panelRegister) panelRegister.hidden =  isSignin;
    }

    tabSignin.addEventListener("click",   () => activateTab("signin"));
    tabRegister.addEventListener("click", () => activateTab("register"));

    // Sync the separate register email/password fields to the IDs app.js
    // already reads (homeEmail / homePassword) when the register panel fires.
    const regEmail = el("homeRegEmail");
    const regPass  = el("homeRegPassword");
    if (regEmail) regEmail.addEventListener("input", () => {
      const src = el("homeEmail");
      if (src && !el("authPanelSignin").hidden) return; // signin panel active
      // Keep homeEmail in sync so app.js sign-in path also works if swapped
    });
    // app.js reads #homeRegister click; the button is still present in the
    // register panel and the IDs didn't change, so no further wiring needed.
  }

  /* ---- Scroll-reveal via IntersectionObserver ----------------------------- */

  let _scrollRevealIo = null;

  function wireScrollReveal() {
    if (!("IntersectionObserver" in window)) {
      for (const node of document.querySelectorAll(".pubSection, .pubFeatureRow")) {
        node.classList.add("anim-visible");
      }
      return;
    }
    if (_scrollRevealIo) _scrollRevealIo.disconnect();
    _scrollRevealIo = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            entry.target.classList.add("anim-visible");
            _scrollRevealIo.unobserve(entry.target);
          }
        }
      },
      { threshold: 0.06, rootMargin: "0px 0px -30px 0px" }
    );
    for (const target of document.querySelectorAll(".pubSection, .pubFeatureRow")) {
      if (!target.classList.contains("anim-visible")) {
        _scrollRevealIo.observe(target);
      }
    }
  }

  // Re-observe newly visible sections when the user switches pages.
  // Sections on non-home pages are hidden (display:none via `hidden` attr) when
  // the IntersectionObserver first runs, so they never fire — their class must be
  // added once they become visible.
  function reObserveCurrentPage() {
    if (!_scrollRevealIo) return;
    for (const target of document.querySelectorAll(".pubSection, .pubFeatureRow")) {
      if (!target.classList.contains("anim-visible") && !target.hidden) {
        _scrollRevealIo.observe(target);
      }
    }
  }

  /* ---- Wiring ------------------------------------------------------------- */

  function wire() {
    const menuButton = el("publicMenuBtn");
    if (menuButton) {
      menuButton.addEventListener("click", () => setMenu(!menuOpen));
    }
    const menuSignin = el("publicMenuSignin");
    if (menuSignin) menuSignin.addEventListener("click", openAuth);

    // Any CTA marked `data-public-auth` lands on the one auth card. Delegated,
    // because the pricing cards are replaced whenever the catalog is fetched.
    // CTAs whose text contains "Create" pre-select the register tab.
    document.addEventListener("click", (event) => {
      const trigger = event.target.closest("[data-public-auth]");
      if (trigger) {
        event.preventDefault();
        const wantsRegister = /create/i.test(trigger.textContent || "");
        openAuth(wantsRegister ? { register: true } : {});
        return;
      }
      // A link inside the sheet is a choice: close the menu before the route
      // paints, and hand focus to the destination heading rather than back to
      // the button the user just left.
      const menuLink = event.target.closest("#publicMenu a");
      if (menuLink) {
        if (menuLink.getAttribute("href") !== location.hash) focusPageOnShow = true;
        closeMenu({ restoreFocus: false });
        return;
      }
      // A click outside the sheet closes it, the way a menu should.
      if (menuOpen && !event.target.closest("#publicMenu, #publicMenuBtn")) {
        setMenu(false);
      }
    });

    for (const button of document.querySelectorAll("[data-billing-cycle]")) {
      button.addEventListener("click", () => {
        cycle = button.dataset.billingCycle === "annual" ? "annual" : "monthly";
        paintCycle();
      });
    }

    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && menuOpen) {
        event.preventDefault();
        setMenu(false);
      }
    });

    window.addEventListener("hashchange", () => {
      if (!started || signedIn()) return;
      route();
    });

    document.addEventListener("hrz:auth", (event) => {
      const detail = event.detail || {};
      if (detail.signedIn) {
        closeMenu();
        return;
      }
      // A sign-out leaves the workstation's address behind. The public site is
      // the only valid screen now, and the unknown hash becomes the home page.
      started = true;
      route();
    });
  }

  function init() {
    if (started && document.body.dataset.publicReady === "true") return;
    document.body.dataset.publicReady = "true";
    const year = el("publicYear");
    if (year) year.textContent = String(new Date().getFullYear());

    // The nav is hidden in the markup so a signed-in boot never flashes it.
    // Showing it here happens before `app.js` runs, which is what lets the
    // marketing header paint in its real shape on the first frame.
    const nav = el("publicNav");
    const menuButton = el("publicMenuBtn");
    if (!signedIn()) {
      if (nav) nav.hidden = false;
      if (menuButton) menuButton.hidden = false;
    }

    wire();
    wireAuthTabs();
    wireScrollReveal();
    started = true;
    if (!signedIn()) route();
  }

  window.HRZPublic = {
    init,
    show,
    openAuth,
    current: () => current,
  };

  // Scripts sit at the end of the body, so the markup this reads is already
  // parsed. `app.js` runs after this file and owns every byte of auth.
  init();
})();
