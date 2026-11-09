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
  const ROUTES = ["home", "features", "markets", "pricing", "community", "docs", "signin", "signup"];
  // Auth pages don't show the footer or the marketing nav links.
  const AUTH_PAGES = ["signin", "signup"];

  const TITLES = {
    home: "HRZ — AI-native order-flow trading terminal",
    features: "Features — HRZ",
    markets: "Markets & Brokers — HRZ",
    pricing: "Pricing — HRZ",
    community: "Community — HRZ",
    docs: "Docs — HRZ",
    signin: "Sign in — HRZ",
    signup: "Create account — HRZ",
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
    document.body.dataset.publicView = page;
    // On the dedicated auth pages hide the header nav + footer so users see
    // nothing but the auth form — identical to how Vercel, Linear, Notion do it.
    const isAuth = AUTH_PAGES.includes(page);
    const pubFooter = document.querySelector(".pubFooter");
    const publicNav  = document.querySelector(".publicNav");
    const publicMenuBtn = el("publicMenuBtn");
    if (pubFooter)    pubFooter.hidden    = isAuth;
    if (publicNav)    publicNav.hidden    = isAuth;
    if (publicMenuBtn) publicMenuBtn.hidden = isAuth;

    closeMenu({ restoreFocus: false });

    const home = el("home");
    if (home && !opts.keepScroll) home.scrollTop = 0;

    const announce = el("publicAnnounce");
    if (announce && started && page !== HOME) {
      const label = page.charAt(0).toUpperCase() + page.slice(1);
      announce.textContent = `${label} page`;
    }

    if (page === "pricing") loadPlans();
    if (page === "markets") loadMarkets();

    // Re-observe reveal targets each time a page becomes visible, since the
    // observer ran at boot when non-home pages were still hidden.
    requestAnimationFrame(reObserveCurrentPage);

    // Focus the first input on auth pages for keyboard users.
    if (AUTH_PAGES.includes(page)) focusAuthInput();

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
    // Navigate to the dedicated auth page — industry-standard pattern.
    location.hash = opts.register ? "#/signup" : "#/signin";
  }

  function focusAuthInput() {
    // Focus the first visible input on whichever auth page is shown.
    const input = document.querySelector("[data-public-page='signin']:not([hidden]) .authCard__input, [data-public-page='signup']:not([hidden]) .authCard__input");
    if (input) requestAnimationFrame(() => input.focus({ preventScroll: true }));
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

  /* ---- Markets page: live tickers + brokers from the API ------------------ */

  const TICKER_SYMBOLS = [
    "BTCUSDT","ETHUSDT","SOLUSDT","BNBUSDT","XRPUSDT",
    "DOGEUSDT","ADAUSDT","AVAXUSDT","LINKUSDT","DOTUSDT",
    "MATICUSDT","TRXUSDT","LTCUSDT","ATOMUSDT","UNIUSDT",
    "XLMUSDT","BCHUSDT","NEARUSDT","ICPUSDT","FILUSDT",
  ];

  let marketsLoaded = false;
  let tickerInterval = null;

  function fmtPrice(p) {
    if (p >= 1000)  return p.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
    if (p >= 1)     return p.toLocaleString("en-US", { minimumFractionDigits: 4, maximumFractionDigits: 4 });
    return p.toLocaleString("en-US", { minimumFractionDigits: 6, maximumFractionDigits: 6 });
  }

  function fmtVol(v) {
    if (v >= 1e9)  return (v / 1e9).toFixed(2) + "B";
    if (v >= 1e6)  return (v / 1e6).toFixed(1) + "M";
    if (v >= 1e3)  return (v / 1e3).toFixed(1) + "K";
    return v.toFixed(0);
  }

  // Generate a simple deterministic sparkline SVG from a ticker's 24h range + last price.
  // Since we only have 24h high/low/last we synthesise 8 plausible points that are visually
  // consistent with the direction (pos = trending up, neg = trending down).
  function sparkline(ticker, pos) {
    const h = ticker.high_price;
    const l = ticker.low_price;
    const p = ticker.last_price;
    const range = h - l || 1;
    // 8 normalised y-values (0 = bottom, 28 = top of 30px viewBox)
    const norm = (v) => Math.round(28 - ((v - l) / range) * 24);
    // Synthesise a plausible path: start near open (opposite side), drift to last
    const pts = pos
      ? [l + range * 0.3, l + range * 0.25, l + range * 0.35, l + range * 0.45, l + range * 0.55, l + range * 0.70, l + range * 0.85, p]
      : [l + range * 0.75, l + range * 0.80, l + range * 0.68, l + range * 0.58, l + range * 0.48, l + range * 0.35, l + range * 0.22, p];
    const color = pos ? "#16c784" : "#ea3943";
    const xs = [2, 12, 22, 32, 42, 52, 62, 72];
    const polyPoints = xs.map((x, i) => `${x},${norm(pts[i])}`).join(" ");
    return `<svg viewBox="0 0 74 30" width="74" height="30" aria-hidden="true" class="sparkSvg">
      <polyline points="${polyPoints}" fill="none" stroke="${color}" stroke-width="1.5" stroke-linejoin="round" opacity=".85"/>
      <circle cx="${xs[7]}" cy="${norm(p)}" r="2" fill="${color}" opacity=".9"/>
    </svg>`;
  }

  function renderTickers(tickers) {
    const table = el("mktTickerGrid");
    const meta  = el("mktTickerMeta");
    if (!table) return;
    // Remove skeleton rows but keep the header row
    const head = table.querySelector(".mktTable__head");
    table.innerHTML = "";
    if (head) table.appendChild(head);
    for (const t of tickers) {
      const pos = t.price_change_percent >= 0;
      const row = document.createElement("div");
      row.className = "tickerRow" + (pos ? " tickerRow--up" : " tickerRow--dn");
      row.innerHTML = `
        <span class="tickerRow__symbol">${t.symbol.replace("USDT","")}<em>/ USDT</em></span>
        <span class="tickerRow__price">${fmtPrice(t.last_price)}</span>
        <span class="tickerRow__change ${pos ? "up" : "down"}">${pos ? "+" : ""}${t.price_change_percent.toFixed(2)}%</span>
        <span class="tickerRow__range"><i>${fmtPrice(t.low_price)}</i> – <i>${fmtPrice(t.high_price)}</i></span>
        <span class="tickerRow__vol">${fmtVol(t.quote_volume)}</span>
        <span class="tickerRow__spark">${sparkline(t, pos)}</span>
      `;
      table.appendChild(row);
    }
    if (meta) meta.textContent = `Updated ${new Date().toLocaleTimeString()} · ${tickers.length} symbols · Source: Binance`;
  }

  function fetchTickers() {
    const syms = TICKER_SYMBOLS.join(",");
    fetch(`/tickers?symbols=${syms}`, { headers: { accept: "application/json" } })
      .then((r) => { if (!r.ok) throw new Error(r.status); return r.json(); })
      .then((body) => { if (body && Array.isArray(body.tickers)) renderTickers(body.tickers); })
      .catch(() => { /* keep whatever was showing */ });
  }

  function renderBrokers(brokers) {
    const grid = el("mktBrokerGrid");
    if (!grid) return;
    grid.innerHTML = "";
    for (const b of brokers) {
      const card = document.createElement("div");
      card.className = "brokerCard";
      const guidance = (b.guidance || "").replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
      const referral = b.referral_url
        ? `<a class="pubBtn pubBtn--primary brokerCard__cta" href="${b.referral_url}" target="_blank" rel="noopener noreferrer">Sign up for ${b.name}</a>`
        : "";
      card.innerHTML = `
        <div class="brokerCard__head">
          <span class="brokerCard__avatar">${b.name[0]}</span>
          <div>
            <b>${b.name}</b>
            <span class="brokerCard__live">Live trading enabled</span>
          </div>
        </div>
        <div class="brokerCard__guidance">${guidance}</div>
        <div class="brokerCard__actions">
          <a class="pubBtn brokerCard__keys" href="${b.keys_url}" target="_blank" rel="noopener noreferrer">
            Create API key on ${b.name} →
          </a>
          ${referral}
        </div>
      `;
      grid.appendChild(card);
    }
  }

  function loadMarkets() {
    fetchTickers();
    if (!marketsLoaded) {
      marketsLoaded = true;
      fetch("/brokers/available", { headers: { accept: "application/json" } })
        .then((r) => { if (!r.ok) throw new Error(r.status); return r.json(); })
        .then((body) => { if (Array.isArray(body)) renderBrokers(body); })
        .catch(() => {
          const g = el("mktBrokerGrid");
          if (g) g.innerHTML = '<p class="mktBrokerError">Could not load broker list — refresh to try again.</p>';
        });
    }
    // Refresh tickers every 5 seconds while the markets page is visible
    if (!tickerInterval) {
      tickerInterval = setInterval(() => {
        if (current === "markets") fetchTickers();
        else {
          clearInterval(tickerInterval);
          tickerInterval = null;
        }
      }, 5000);
    }
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

    // Any CTA marked `data-public-auth` navigates to the right auth page.
    // CTAs whose text contains "Create" / "Start" / "free" → signup page.
    document.addEventListener("click", (event) => {
      const trigger = event.target.closest("[data-public-auth]");
      if (trigger) {
        event.preventDefault();
        const wantsRegister = /create|start|free|signup/i.test(trigger.textContent || "");
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
