# HRZ Foundation & Shell — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the HRZ workstation's foundation and shell — tokens, both themes, `HRZUI`, the app frame, the router, the docked chart workspace and the dashboard — without breaking any existing flow.

**Architecture:** Split the single `index.html` stylesheet into twelve files served by one new `/assets/{*path}` route, build a CSS-first component layer in classic scripts loaded before `app.js`, then restructure the frame around destinations while moving — never cloning — the existing `pane-*` nodes into their new hosts. `app.js` keeps ownership of market data, charts, chat, billing, community and bots and exposes a small `HRZApp` facade.

**Tech Stack:** Rust/Axum 0.8 for one static-asset route, vanilla JavaScript classic scripts (no modules, no bundler), CSS custom properties, jsdom for the existing shell harness, PostgreSQL-backed gateway tests.

**Spec:** `docs/superpowers/specs/2026-10-27-hrz-foundation-and-shell-design.md`

## Global Constraints

- Every ID and class listed in the spec under "What the shell must preserve" stays in the markup and keeps working.
- No module bundler, no ES modules, no framework. New files are classic scripts loaded in `<head>`/end-of-`<body>` order; every new script gets a route check in `packaging.rs`.
- No inline `<style>` block in `index.html` after Task 3; no hardcoded colour outside `styles/tokens.css` and the canvas palette in `app.js`.
- Every text pair meets 4.5:1 and every control boundary/focus ring meets 3:1 (WCAG 2.1 AA).
- No user or API string reaches `innerHTML`; use `textContent` or `HRZUI.escape`.
- Keep `#split`, `#layoutMode`, `#chartCols`, `.symbol`, `.timeframe`, `.limit`, `.mode`, `.close`, `.load`, `.clearDrawings`, `.deleteDrawing`, `.footprintStats`, `.chartMsg`, `.feedStatus`, `.chartWrap`, `.chartNote`, `.paneHead`, `#globalTools`, `#splitWatch`, `#splitAside`, `#railHide`, `#railShow`, `#asideHide`, `#asideShow`, `#session`, `#signinToggle`, `#home*` and every `pane-*` ID.
- Snapshot stays `version: 1`; new keys are optional and old clients ignore them.
- Never run `cargo fmt --all` — the repository is not rustfmt-clean and it rewrites 100+ unrelated files.
- Never print, commit or expose `.env` contents, `DATABASE_URL`, JWT secrets, provider keys or broker credentials.
- The user tests on the local gateway at `:8080`. Do not restart or rebuild it, and never run `tools/guard_check.py`, while they are testing; ask first.
- Each task ends with a green verification and one commit.

## File Structure

```
frontend/app/
  index.html                 shell markup only (no <style>)
  styles/
    tokens.css               literals, @font-face, theme blocks, compatibility aliases
    base.css                 reset, typography, focus, scrollbars, skip link
    components.css           HRZUI classes and states
    shell.css                top bar, rail, status, destination frame, palette, shortcuts
    charts.css               wall, panes, instrument bar, watchlist, dock, drawing rail
    dashboard.css            pulse cards, markets table, watchlist panel, heatmap
    inspector.css            inspector tabs and panels
    studio.css               Studio pane styles (moved and tokenised)
    community.css            Community pane styles (moved and tokenised)
    account.css              billing, brokers, AI model, profile
    public.css               home and auth (rebuilt in Spec 4)
    responsive.css           breakpoints, loaded last
  ui/
    theme.js                 blocking theme boot + Theme API
    icons.js                 generated icon sprite + HRZUI.icon
    components.js            HRZUI behaviour
    LUCIDE-LICENSE.txt       icon licence notice
  views/
    shell.js                 frame, router, pane host, palette, shortcuts, state
  fonts/
    inter-var.woff2
    jetbrains-mono-var.woff2
    OFL-Inter.txt
    OFL-JetBrainsMono.txt
  favicon.svg
  builder.js                 unchanged route and path
  app.js                     unchanged route and path
tools/
  fetch_icons.mjs            one-time icon generator (not served, not shipped)
```

Load order in `index.html`:

```
tokens, base, components, shell, charts, dashboard, inspector,
studio, community, account, public, responsive
theme.js (head, blocking) · icons.js · components.js · builder.js ·
views/shell.js · app.js
```

The dashboard has no file of its own: every input it reads — the ticker batch,
favourites, bots, billing — is already owned by `app.js`, so its modules live
there (Task 16) and the shell reaches them through the one hook
`HRZApp.setDestinationVisible(destination)`. A separate file would have been a
second reader of state it does not own.

---

## M1 — Tokens, type, icons, themes, asset route

### Task 1: Serve `/assets/{*path}`

**Files:**
- Modify: `crates/api-gateway/src/lib.rs` (router at the five explicit frontend routes; add `asset` near `app_js`)
- Create: `frontend/app/styles/tokens.css` (token block only in this task)
- Create: `frontend/app/ui/theme.js` (minimal, completed in Task 2)
- Create: `frontend/app/favicon.svg`
- Test: `crates/api-gateway/tests/assets_flow.rs`

**Interfaces:**
- Produces: `GET /assets/{*path}` → `frontend/app/{path}` for `.css`, `.js`, `.woff2`, `.svg`, with `no-cache` and `nosniff`.
- Produces: `api_gateway::asset` handler (public, no auth).

- [ ] **Step 1: Write the failing test**

Create `crates/api-gateway/tests/assets_flow.rs`:

```rust
//! The split shell loads its styles, scripts, fonts and icon over
//! `/assets/{*path}`. This proves the route serves them with the types the
//! browser needs, and refuses everything that is not one of those files.

mod common;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).expect("request"))
        .await
        .expect("the router must answer");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("the body must be readable")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

#[tokio::test]
async fn assets_are_served_with_the_type_the_browser_needs() {
    let Some(harness) = common::Harness::new().await else {
        return;
    };
    for (uri, content_type) in [
        ("/assets/styles/tokens.css", "text/css; charset=utf-8"),
        ("/assets/ui/theme.js", "text/javascript; charset=utf-8"),
        ("/assets/favicon.svg", "image/svg+xml"),
    ] {
        let (status, headers, body) = get(&harness.app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} was not served");
        assert_eq!(
            headers[header::CONTENT_TYPE].to_str().unwrap_or(""),
            content_type,
            "{uri} has the wrong content type"
        );
        assert_eq!(
            headers[header::CACHE_CONTROL].to_str().unwrap_or(""),
            "no-cache",
            "{uri} may be cached stale"
        );
        assert_eq!(
            headers["x-content-type-options"].to_str().unwrap_or(""),
            "nosniff",
            "{uri} is missing nosniff"
        );
        assert!(!body.is_empty(), "{uri} served an empty body");
    }
}

#[tokio::test]
async fn assets_refuse_everything_that_is_not_one_of_the_four_types() {
    let Some(harness) = common::Harness::new().await else {
        return;
    };
    for uri in [
        "/assets/../Cargo.toml",
        "/assets/%2e%2e/%2e%2e/Cargo.toml",
        "/assets/.env",
        "/assets/Cargo.toml",
        "/assets/styles/missing.css",
    ] {
        let (status, _, _) = get(&harness.app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} should not be served");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p api-gateway --test assets_flow`
Expected: FAIL — every URI answers 404 because the route does not exist.

- [ ] **Step 3: Create the three files the test reads**

`frontend/app/favicon.svg`:

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <rect width="32" height="32" rx="7" fill="#0a0d12"/>
  <path d="M10 8v16M22 8v16M8 16h16" stroke="#22d3ee" stroke-width="3" stroke-linecap="round"/>
</svg>
```

`frontend/app/ui/theme.js` — the first half of the Theme API (Task 2 completes it):

```js
(function () {
  const KEY = "hrz.theme";
  let pref = "system";
  try {
    const saved = localStorage.getItem(KEY);
    if (saved === "light" || saved === "dark" || saved === "system") pref = saved;
  } catch { /* private mode: the system preference still decides */ }
  window.Theme = {
    current() {
      if (pref !== "system") return pref;
      return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
    },
    preference: () => pref,
    set(next) {
      pref = next === "light" || next === "dark" ? next : "system";
      try { localStorage.setItem(KEY, pref); } catch { /* the theme still applies */ }
      document.documentElement.dataset.theme = this.current();
    },
  };
  document.documentElement.dataset.theme = window.Theme.current();
})();
```

`frontend/app/styles/tokens.css` — the token block (Task 2 adds `@font-face`):

```css
/* The one file with colour literals. Everything else uses these names. */
:root {
  --bg-canvas: #0a0d12;
  --bg-panel: #0f141b;
  --bg-raised: #141b24;
  --bg-overlay: #1a222d;
  --line: #1b2430;
  --line-strong: #2a3644;
  --control-line: #5c6b7c;
  --text-1: #e9edf3;
  --text-2: #aeb8c6;
  --text-3: #7c8898;
  --accent: #22d3ee;
  --accent-ink: #04222b;
  --accent-soft: rgba(34, 211, 238, 0.12);
  --accent-line: rgba(34, 211, 238, 0.35);
  --focus: #22d3ee;
  --ai: #a78bfa;
  --ai-ink: #1b1030;
  --ai-soft: rgba(167, 139, 250, 0.12);
  --up: #16c784;
  --down: #ea3943;
  --warn: #f5a524;
  --up-fill: #16c784;
  --down-fill: #ea3943;
  --up-soft: rgba(22, 199, 132, 0.12);
  --down-soft: rgba(234, 57, 67, 0.12);
  --warn-soft: rgba(245, 165, 36, 0.12);
  --shadow-1: 0 2px 8px rgba(4, 8, 12, 0.28);
  --shadow-2: 0 8px 24px rgba(4, 8, 12, 0.35);

  --font-ui: "Inter", system-ui, -apple-system, "Segoe UI", sans-serif;
  --font-mono: "JetBrains Mono", ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  --text-2xs: 11px;
  --text-xs: 12px;
  --text-sm: 13px;
  --text-base: 14px;
  --text-md: 16px;
  --text-lg: 20px;
  --text-xl: 28px;
  --text-2xl: 40px;

  --space-1: 4px;
  --space-2: 8px;
  --space-3: 12px;
  --space-4: 16px;
  --space-5: 20px;
  --space-6: 24px;
  --space-7: 32px;
  --space-8: 40px;

  --radius-1: 4px;
  --radius-2: 6px;
  --radius-3: 10px;
  --dur-1: 120ms;
  --dur-2: 180ms;
  --dur-3: 240ms;
  --ease: cubic-bezier(0.2, 0.8, 0.2, 1);

  --topbar-h: 44px;
  --rail-w: 48px;
  --status-h: 24px;
  --watch-w: 260px;
  --aside-w: 360px;
  --inspector-w: var(--aside-w);
  --dock-h: 240px;

  /* Compatibility layer: the existing stylesheet inherits the new palette
     before it is migrated. Remove an alias only after its last consumer. */
  --bg: var(--bg-canvas);
  --bg-elev: var(--bg-panel);
  --panel: var(--bg-panel);
  --panel-2: var(--bg-raised);
  --border-subtle: var(--line);
  --border-strong: var(--line-strong);
  --text: var(--text-1);
  --text-dim: var(--text-2);
  --muted: var(--text-2);
  --faint: var(--text-3);
  --up-dim: var(--up-soft);
  --down-dim: var(--down-soft);
  --buy: var(--up);
  --sell: var(--down);
  --accent-dim: var(--accent-soft);
  --radius-s: var(--radius-1);
  --radius-m: var(--radius-2);
  --radius-l: var(--radius-3);
  --shadow-3: var(--shadow-2);
  --mono: var(--font-mono);
}

[data-theme="light"] {
  --bg-canvas: #f6f8fa;
  --bg-panel: #ffffff;
  --bg-raised: #f2f5f8;
  --bg-overlay: #ffffff;
  --line: #dfe5ec;
  --line-strong: #c6d0db;
  --control-line: #7a8794;
  --text-1: #0d1520;
  --text-2: #465364;
  --text-3: #5f6b7a;
  --accent: #0e7490;
  --accent-ink: #ffffff;
  --accent-soft: rgba(14, 116, 144, 0.10);
  --accent-line: rgba(14, 116, 144, 0.30);
  --focus: #0e7490;
  --ai: #7c3aed;
  --ai-ink: #ffffff;
  --ai-soft: rgba(124, 58, 237, 0.10);
  --up: #067a4d;
  --down: #d42a3c;
  --warn: #b45309;
  --up-fill: #0a8f5b;
  --down-fill: #d92d3f;
  --up-soft: rgba(6, 122, 77, 0.10);
  --down-soft: rgba(217, 45, 63, 0.10);
  --warn-soft: rgba(180, 83, 9, 0.10);
  --shadow-1: 0 2px 8px rgba(16, 24, 40, 0.08);
  --shadow-2: 0 8px 24px rgba(16, 24, 40, 0.12);
}
```

- [ ] **Step 4: Add the handler and the route**

In `crates/api-gateway/src/lib.rs`, above `read_frontend`, add:

```rust
/// Content type for one of the four asset kinds the shell loads.
///
/// An allowlist rather than a guess: `read_frontend` will read any name it is
/// given, and this is the one route that takes a path from the URL.
fn asset_content_type(path: &str) -> Option<&'static str> {
    match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "css" => Some("text/css; charset=utf-8"),
        "js" => Some("text/javascript; charset=utf-8"),
        "woff2" => Some("font/woff2"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// Whether a decoded asset path is a plain relative path.
///
/// Rejects empty segments, `.`/`..`, dotfiles and backslashes. The `Path`
/// extractor has already percent-decoded, so `%2e%2e` arrives here as `..`
/// and is refused by the same rule as a literal one.
fn valid_asset_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 200
        && !path.contains('\\')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != ".." && !segment.starts_with('.'))
}

/// The split shell's styles, scripts, fonts and icon at `/assets/{*path}`.
///
/// # Errors
/// 404 for a disallowed extension, an invalid path, or a missing file.
pub async fn asset(
    axum::extract::Path(path): axum::extract::Path<String>,
) -> Result<axum::response::Response, StatusCode> {
    let path = path.trim_start_matches('/');
    let Some(content_type) = asset_content_type(path).filter(|_| valid_asset_path(path)) else {
        warn!(path = %path, "asset path refused (404)");
        return Err(StatusCode::NOT_FOUND);
    };
    let bytes = read_frontend(path)?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, content_type),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}
```

Add the route beside the existing frontend routes:

```rust
.route("/assets/{*path}", get(asset))
```

- [ ] **Step 5: Run the test and the existing packaging test**

Run: `cargo test -p api-gateway --test assets_flow`
Expected: PASS (2 tests; both skip early without `DATABASE_URL`, which is the harness's existing contract).

Run: `cargo test -p api-gateway --test packaging`
Expected: PASS — no `<script src>` has changed yet.

- [ ] **Step 6: Commit**

```bash
git add crates/api-gateway/src/lib.rs crates/api-gateway/tests/assets_flow.rs \
  frontend/app/styles/tokens.css frontend/app/ui/theme.js frontend/app/favicon.svg
git commit -m "feat: serve the shell's split assets from one validated route"
```

---

### Task 2: Fonts, base styles, complete Theme API

**Files:**
- Modify: `frontend/app/styles/tokens.css` (`@font-face` block)
- Create: `frontend/app/styles/base.css`
- Create: `frontend/app/fonts/*` (two woff2, two licence texts)
- Modify: `frontend/app/ui/theme.js` (system-change listener, `onChange`)
- Modify: `frontend/app/index.html` (theme toggle in the current header, temporary until Task 8)
- Modify: `crates/api-gateway/tests/assets_flow.rs` (woff2 row)

**Interfaces:**
- Produces: `Theme.current()`, `Theme.preference()`, `Theme.set(pref)`, `Theme.onChange(cb)`, and the `hrz:theme` event with `{ theme, pref }`.
- Produces: `.sr-only`, `.skipLink`, `:focus-visible`, scrollbar styling, base typography.

- [ ] **Step 1: Add the failing font case**

In `assets_flow.rs`, add to the table:

```rust
("/assets/fonts/inter-var.woff2", "font/woff2"),
```

Run: `cargo test -p api-gateway --test assets_flow`
Expected: FAIL — the file does not exist (404).

- [ ] **Step 2: Fetch the fonts and licences**

```bash
mkdir -p frontend/app/fonts
curl -sfL -o frontend/app/fonts/inter-var.woff2 \
  https://cdn.jsdelivr.net/npm/@fontsource-variable/inter@5.2.8/files/inter-latin-wght-normal.woff2
curl -sfL -o frontend/app/fonts/jetbrains-mono-var.woff2 \
  https://cdn.jsdelivr.net/npm/@fontsource-variable/jetbrains-mono@5.2.8/files/jetbrains-mono-latin-wght-normal.woff2
curl -sfL -o frontend/app/fonts/OFL-Inter.txt \
  https://cdn.jsdelivr.net/npm/@fontsource-variable/inter@5.2.8/LICENSE
curl -sfL -o frontend/app/fonts/OFL-JetBrainsMono.txt \
  https://cdn.jsdelivr.net/npm/@fontsource-variable/jetbrains-mono@5.2.8/LICENSE
ls -l frontend/app/fonts
```

Expected: both woff2 files are non-empty (about 48KB and 40KB) and start with the woff2 magic. Check:

```bash
head -c 4 frontend/app/fonts/inter-var.woff2 | xxd
```

Expected: `0000 774f 4632` (`wOF2`).

- [ ] **Step 3: Add `@font-face` to tokens.css**

At the top of `frontend/app/styles/tokens.css`, above `:root`:

```css
@font-face {
  font-family: "Inter";
  src: url("/assets/fonts/inter-var.woff2") format("woff2");
  font-weight: 100 900;
  font-style: normal;
  font-display: swap;
}

@font-face {
  font-family: "JetBrains Mono";
  src: url("/assets/fonts/jetbrains-mono-var.woff2") format("woff2");
  font-weight: 100 800;
  font-style: normal;
  font-display: swap;
}
```

- [ ] **Step 4: Complete theme.js**

Replace `frontend/app/ui/theme.js` with:

```js
(function () {
  const KEY = "hrz.theme";
  const media = window.matchMedia("(prefers-color-scheme: light)");
  const read = () => {
    try {
      const saved = localStorage.getItem(KEY);
      if (saved === "light" || saved === "dark" || saved === "system") return saved;
    } catch { /* private mode: the system preference still decides */ }
    return "system";
  };
  let pref = read();
  const resolve = () => (pref === "system" ? (media.matches ? "light" : "dark") : pref);
  const announce = () => {
    window.dispatchEvent(
      new CustomEvent("hrz:theme", { detail: { theme: resolve(), pref } })
    );
  };
  const apply = () => {
    document.documentElement.dataset.theme = resolve();
    announce();
  };
  // jsdom (the harness) exposes only the legacy `addListener`; a browser
  // exposes both. Calling the missing one would throw before `Theme` exists.
  const watchScheme = (callback) => {
    if (typeof media.addEventListener === "function") media.addEventListener("change", callback);
    else if (typeof media.addListener === "function") media.addListener(callback);
  };
  watchScheme(() => {
    if (pref === "system") apply();
  });
  window.Theme = {
    current: resolve,
    preference: () => pref,
    set(next) {
      pref = next === "light" || next === "dark" ? next : "system";
      try { localStorage.setItem(KEY, pref); } catch { /* the theme still applies */ }
      apply();
    },
    onChange(callback) {
      window.addEventListener("hrz:theme", (event) => callback(event.detail.theme));
      callback(resolve());
    },
  };
  apply();
})();
```

- [ ] **Step 5: Create base.css**

`frontend/app/styles/base.css`:

```css
*, *::before, *::after { box-sizing: border-box; }
html, body { height: 100%; margin: 0; }
body {
  background: var(--bg-canvas);
  color: var(--text-1);
  font: var(--text-sm)/20px var(--font-ui);
  display: flex;
  flex-direction: column;
  -webkit-font-smoothing: antialiased;
}
h1, h2, h3, h4 { font-family: var(--font-ui); font-weight: 600; letter-spacing: -0.01em; }
h1 { font-size: var(--text-lg); margin: 0; }
h2 { font-size: var(--text-md); margin: 0; }
h3 { font-size: var(--text-sm); margin: 0; }
p { margin: 0; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
button, input, select, textarea { font: inherit; color: inherit; }
button { cursor: pointer; }
code, pre, kbd, samp { font-family: var(--font-mono); }
.num { font-variant-numeric: tabular-nums; }
.muted { color: var(--text-2); }
.faint { color: var(--text-3); }
.spacer { flex: 1 1 auto; }

:focus-visible {
  outline: 2px solid var(--focus);
  outline-offset: 2px;
}

::selection { background: var(--accent-soft); }
* { scrollbar-width: thin; scrollbar-color: var(--line-strong) transparent; }
*::-webkit-scrollbar { width: 10px; height: 10px; }
*::-webkit-scrollbar-track { background: transparent; }
*::-webkit-scrollbar-thumb { background: var(--line-strong); border: 3px solid transparent; background-clip: content-box; border-radius: 999px; }

.sr-only {
  position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px;
  overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; border: 0;
}

.skipLink {
  position: fixed; top: 8px; left: 8px; z-index: 100;
  padding: 8px 12px; border-radius: var(--radius-1);
  background: var(--bg-overlay); color: var(--text-1);
  border: 1px solid var(--control-line);
  transform: translateY(-200%);
}
.skipLink:focus { transform: none; }

@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    animation-duration: 0.001ms !important;
    animation-iteration-count: 1 !important;
    transition-duration: 0.001ms !important;
    scroll-behavior: auto !important;
  }
}
```

- [ ] **Step 6: Add the temporary theme toggle**

In `frontend/app/index.html`, inside the current `<header>` before `#session`:

```html
<button id="themeToggle" type="button" title="Switch between dark and light" aria-label="Switch theme">◐</button>
```

Move the `◐` replacement to `HRZUI.icon` in Task 4.

At the end of `main()` in `app.js`, next to the other page-level wiring, add:

```js
el("themeToggle").addEventListener("click", () => {
  Theme.set(Theme.current() === "dark" ? "light" : "dark");
});
```

- [ ] **Step 7: Verify**

Run: `cargo test -p api-gateway --test assets_flow`
Expected: PASS, now including the woff2 row (and still skipping without `DATABASE_URL`).

Run: `node --check frontend/app/ui/theme.js`
Expected: no output.

Run the contrast ledger (the numbers must match spec §1.2):

```bash
node -e '
const lum = (hex) => { const c = hex.replace("#","").match(/../g).map((h) => parseInt(h,16)/255).map((v) => v <= 0.03928 ? v/12.92 : ((v+0.055)/1.055)**2.4); return 0.2126*c[0]+0.7152*c[1]+0.0722*c[2]; };
const ratio = (a,b) => { const [x,y] = [lum(a),lum(b)].sort((m,n)=>n-m); return (x+0.05)/(y+0.05); };
const pairs = [
  ["dark text-3","#7c8898","#0a0d12",4.5], ["dark text-2","#aeb8c6","#0a0d12",4.5],
  ["dark accent","#22d3ee","#0a0d12",4.5], ["accent ink","#04222b","#22d3ee",4.5],
  ["dark up","#16c784","#0a0d12",4.5], ["dark down","#ea3943","#0a0d12",4.5],
  ["light text-3","#5f6b7a","#f6f8fa",4.5], ["light text-2","#465364","#f6f8fa",4.5],
  ["light accent","#0e7490","#f6f8fa",4.5], ["white on accent","#ffffff","#0e7490",4.5],
  ["light up","#067a4d","#f6f8fa",4.5], ["light down","#d42a3c","#f6f8fa",4.5],
  ["light warn","#b45309","#f6f8fa",4.5], ["dark control","#5c6b7c","#0a0d12",3],
  ["light control","#7a8794","#f6f8fa",3],
];
let bad = 0;
for (const [name,a,b,min] of pairs) { const r = ratio(a,b); if (r < min) { bad++; console.log("FAIL", name, r.toFixed(2)); } else { console.log("ok  ", name, r.toFixed(2)); } }
process.exit(bad ? 1 : 0);
'
```

Expected: every line `ok`, exit 0.

- [ ] **Step 8: Commit**

```bash
git add frontend/app/styles/tokens.css frontend/app/styles/base.css \
  frontend/app/fonts frontend/app/ui/theme.js frontend/app/index.html \
  frontend/app/app.js crates/api-gateway/tests/assets_flow.rs
git commit -m "feat: self-host the UI fonts and add the theme API"
```

---

### Task 3: Split the stylesheet, load the files, teach the guards

**Files:**
- Create: `frontend/app/styles/{components,shell,charts,dashboard,inspector,studio,community,account,public,responsive}.css`
- Modify: `frontend/app/index.html` (remove `<style>…</style>` at lines 19–1819, add links and preloads)
- Modify: `crates/api-gateway/tests/packaging.rs`
- Modify: `tools/shell_check.mjs`

**Interfaces:**
- Consumes: `/assets/{*path}` from Task 1.
- Produces: `index.html` with no `<style>`; twelve linked stylesheets in the spec's order.

- [ ] **Step 1: Make the harness load stylesheets (it will fail)**

In `tools/shell_check.mjs`, replace the single line:

```js
const styleText = html.slice(html.indexOf("<style>"), html.indexOf("</style>"));
```

with:

```js
const styleFiles = [
  "tokens", "base", "components", "shell", "charts", "dashboard",
  "inspector", "studio", "community", "account", "public", "responsive",
];
const styleText = (
  await Promise.all(
    styleFiles.map((name) =>
      readFile(join(root, `frontend/app/styles/${name}.css`), "utf8")
    )
  )
).join("\n");
```

Run: `NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs`
Expected: FAIL to start with a missing `tokens.css` (or exit 2 if jsdom is absent — install it as the file's header documents).

- [ ] **Step 2: Move the CSS into the twelve files**

Cut the entire block from `<style>` (line 19) through `</style>` (line 1819). Distribute it by the section comments and selector ownership:

| File | Contents |
| --- | --- |
| `tokens.css` | the existing `:root` custom properties, merged into the token block from Task 1 (tokens win; keep only aliases that are still referenced) |
| `base.css` | `*`, `html, body`, headings, links, focus, scrollbars, `.spacer`, `.sr-only` |
| `components.css` | form controls, `.primary`, buttons, `.tabs`, `.pane`, `.empty`, `.error`, `.row`, `.muted`, chips/badges, `#pageToast`, `.skeleton`, the motion and reduced-motion rules |
| `shell.css` | `header`, `#layoutCtl`, `#statusbar`, `.rail*`, `.splitter` collapse helpers, footer |
| `charts.css` | `main`, `#sideWatch`, `#chartColumn`, `#charts`, `.chartPane`, `.chartRow`, `.chartWrap`, `.paneHead`, `.paneBtns`, `.globalTools`, `.row-splitter`, `#chartMenu`, `.layerSettings`, `.toolFlyout` |
| `inspector.css` | `aside`, `.tabs` inside the inspector, `.chat-*`, `#chatStream`, `#pane-thesis`, `.book*`, `#pane-trade`, `.trade*`, `.chat-history*` |
| `studio.css` | `#pane-indicator`, `#ws*`, `#scan*`, `#strategy*`, `#skill*`, `#author*`, builder and backtest rules |
| `community.css` | `#pane-community`, `.community*`, library filters |
| `account.css` | `#pane-billing`, `#pane-brokers`, `#pane-aimodel`, billing/broker/model rules |
| `public.css` | `#home`, `.home*`, `.market*`, auth card |
| `dashboard.css` | empty in this task; Task 15 fills it |
| `responsive.css` | the existing `@media (max-width: 1100px)` and `@media (max-width: 900px)` blocks, unchanged and in that order |

Rules shared by several panes go to `components.css`; a pane-specific rule must not be duplicated. Keep every selector exactly as it is — this is a move, not a rewrite.

- [ ] **Step 3: Replace the style block with links**

In `<head>`, remove `<style>…</style>` and add, in this order:

```html
<link rel="stylesheet" href="/assets/styles/tokens.css">
<link rel="stylesheet" href="/assets/styles/base.css">
<link rel="stylesheet" href="/assets/styles/components.css">
<link rel="stylesheet" href="/assets/styles/shell.css">
<link rel="stylesheet" href="/assets/styles/charts.css">
<link rel="stylesheet" href="/assets/styles/dashboard.css">
<link rel="stylesheet" href="/assets/styles/inspector.css">
<link rel="stylesheet" href="/assets/styles/studio.css">
<link rel="stylesheet" href="/assets/styles/community.css">
<link rel="stylesheet" href="/assets/styles/account.css">
<link rel="stylesheet" href="/assets/styles/public.css">
<link rel="stylesheet" href="/assets/styles/responsive.css">
<link rel="preload" as="font" type="font/woff2" href="/assets/fonts/inter-var.woff2" crossorigin>
<link rel="preload" as="font" type="font/woff2" href="/assets/fonts/jetbrains-mono-var.woff2" crossorigin>
<link rel="icon" type="image/svg+xml" href="/assets/favicon.svg">
<script src="/assets/ui/theme.js"></script>
```

`theme.js` stays in `<head>` and blocking so `data-theme` is set before first paint.

- [ ] **Step 4: Update the packaging guard**

Replace `every_file_the_shell_loads_is_served_by_a_route_and_present` in `crates/api-gateway/tests/packaging.rs` with:

```rust
/// Every file the shell loads is served, present, and in the order the
/// cascade depends on.
#[test]
fn every_file_the_shell_loads_is_served_by_a_route_and_present() {
    let root = repo_root();
    let index = std::fs::read_to_string(root.join("frontend/app/index.html"))
        .expect("frontend/app/index.html must exist");
    assert!(
        !index.contains("<style>"),
        "index.html still carries a <style> block; the styles live in frontend/app/styles"
    );

    let mut scripts: Vec<String> = Vec::new();
    let mut styles: Vec<String> = Vec::new();
    let mut fonts: Vec<String> = Vec::new();
    let mut icons: Vec<String> = Vec::new();
    for line in index.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("<script src=\"") {
            if let Some(src) = rest.split('"').next() { scripts.push(src.to_string()); }
        } else if let Some(rest) = line.strip_prefix("<link rel=\"stylesheet\" href=\"") {
            if let Some(href) = rest.split('"').next() { styles.push(href.to_string()); }
        } else if let Some(rest) = line.strip_prefix("<link rel=\"preload\" as=\"font\" type=\"font/woff2\" href=\"") {
            if let Some(href) = rest.split('"').next() { fonts.push(href.to_string()); }
        } else if let Some(rest) = line.strip_prefix("<link rel=\"icon\" type=\"image/svg+xml\" href=\"") {
            if let Some(href) = rest.split('"').next() { icons.push(href.to_string()); }
        }
    }

    let router = std::fs::read_to_string(root.join("crates/api-gateway/src/lib.rs"))
        .expect("the gateway's lib.rs must exist");
    assert!(
        router.contains("\"/assets/{*path}\""),
        "the router does not expose /assets/{{*path}}"
    );

    for reference in scripts.iter().chain(styles.iter()).chain(fonts.iter()).chain(icons.iter()) {
        let relative = reference
            .strip_prefix("/assets/")
            .unwrap_or(reference.as_str());
        if reference.starts_with("/assets/") {
            assert!(
                root.join("frontend/app").join(relative).is_file(),
                "index.html loads `{reference}` but frontend/app/{relative} does not exist"
            );
        } else {
            assert!(
                router.contains(&format!("\"/{reference}\"")),
                "index.html loads `/{reference}` but no route serves it"
            );
            assert!(
                root.join("frontend/app").join(reference).is_file(),
                "index.html loads `/{reference}` but frontend/app/{reference} does not exist"
            );
        }
    }

    assert!(
        scripts.iter().any(|s| s == "builder.js"),
        "index.html does not load builder.js: {scripts:?}"
    );
    let expected = [
        "/assets/styles/tokens.css",
        "/assets/styles/base.css",
        "/assets/styles/components.css",
        "/assets/styles/shell.css",
        "/assets/styles/charts.css",
        "/assets/styles/dashboard.css",
        "/assets/styles/inspector.css",
        "/assets/styles/studio.css",
        "/assets/styles/community.css",
        "/assets/styles/account.css",
        "/assets/styles/public.css",
        "/assets/styles/responsive.css",
    ];
    assert_eq!(styles, expected, "the stylesheet order changed");

    for directory in ["styles", "ui", "views", "fonts"] {
        for path in walk(&root.join("frontend/app").join(directory)) {
            let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(extension, "css" | "js" | "woff2") {
                continue;
            }
            let relative = path
                .strip_prefix(root.join("frontend/app"))
                .expect("a file under frontend/app")
                .to_string_lossy()
                .to_string();
            let reference = format!("/assets/{relative}");
            assert!(
                scripts.contains(&format!("/assets/{relative}"))
                    || styles.contains(&reference)
                    || fonts.contains(&reference),
                "frontend/app/{relative} is never loaded by index.html, so it is dead weight"
            );
        }
    }
}
```

The orphan check deliberately skips `.txt`: the two OFL licence texts are shipped for attribution, not loaded.

- [ ] **Step 5: Verify**

Run: `cargo test -p api-gateway --test packaging`
Expected: the shell-assets test passes; the other packaging tests are unchanged.

Run: `node --check frontend/app/app.js` (unchanged) and:

```bash
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
```

Expected: every check that passed before still passes; the narrow-width assertions now read from `responsive.css`.

- [ ] **Step 6: Commit**

```bash
git add frontend/app/index.html frontend/app/styles \
  crates/api-gateway/tests/packaging.rs tools/shell_check.mjs
git commit -m "refactor: split the shell stylesheet into twelve owned files"
```

---

### Task 4: Icon sprite

**Files:**
- Create: `tools/fetch_icons.mjs`
- Create: `frontend/app/ui/icons.js` (generated)
- Create: `frontend/app/ui/LUCIDE-LICENSE.txt`
- Modify: `frontend/app/index.html` (load `icons.js` before `app.js`)
- Modify: shell chrome markup: header buttons, rail labels, status bar, aside collapse buttons

**Interfaces:**
- Produces: `HRZUI.icon(name, { size = 16, label }) -> SVGElement`.
- Produces: the icon inventory in spec §1.6; `HRZUI.icon("circle")` is the fallback.

- [ ] **Step 1: Write the generator**

Create `tools/fetch_icons.mjs`:

```js
// Generate ui/icons.js from a pinned Lucide release.
//
// Lucide is ISC-licensed and drawn on the 24px grid the design system uses.
// The generated file is committed: nothing downloads icons at runtime, and the
// page carries no dependency on a CDN.
import { writeFile } from "node:fs/promises";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const VERSION = "0.468.0";
const BASE = `https://cdn.jsdelivr.net/npm/lucide-static@${VERSION}/icons`;

const MAP = {
  dashboard: "layout-dashboard",
  chart: "chart-candlestick",
  studio: "wand-sparkles",
  community: "users",
  bots: "bot",
  account: "circle-user-round",
  search: "search",
  sun: "sun",
  moon: "moon",
  monitor: "monitor",
  plus: "plus",
  minus: "minus",
  close: "x",
  "chevron-up": "chevron-up",
  "chevron-down": "chevron-down",
  "chevron-left": "chevron-left",
  "chevron-right": "chevron-right",
  play: "play",
  pause: "pause",
  stop: "square",
  alert: "bell",
  book: "book-open",
  trade: "arrow-left-right",
  settings: "settings",
  upload: "upload",
  download: "download",
  copy: "copy",
  check: "check",
  external: "external-link",
  filter: "filter",
  sort: "arrow-up-down",
  "more-horizontal": "ellipsis",
  "more-vertical": "ellipsis-vertical",
  "panel-left": "panel-left",
  "panel-right": "panel-right",
  "panel-bottom": "panel-bottom",
  refresh: "refresh-cw",
  trash: "trash-2",
  edit: "pencil",
  eye: "eye",
  "eye-off": "eye-off",
  lock: "lock",
  globe: "globe",
  user: "user",
  "log-out": "log-out",
  command: "command",
  wifi: "wifi",
  zap: "zap",
  layers: "layers",
  clock: "clock",
  "arrow-up-right": "arrow-up-right",
  "arrow-down-right": "arrow-down-right",
  maximize: "maximize-2",
  minimize: "minimize-2",
  split: "columns-2",
  grid: "grid-3x3",
  sparkles: "sparkles",
  image: "image",
  file: "file-text",
  link: "link",
  warning: "triangle-alert",
  info: "info",
  "check-circle": "circle-check",
  "x-circle": "circle-x",
  loader: "loader-circle",
  crosshair: "crosshair",
  camera: "camera",
  activity: "activity",
  sliders: "sliders-horizontal",
};

const missing = [];
const entries = [];
for (const [name, file] of Object.entries(MAP)) {
  const response = await fetch(`${BASE}/${file}.svg`);
  if (!response.ok) {
    missing.push(`${name} (${file})`);
    continue;
  }
  const svg = await response.text();
  const body = svg
    .replace(/^[\s\S]*?<svg[^>]*>/, "")
    .replace(/<\/svg>[\s\S]*$/, "")
    .replace(/\s*\n\s*/g, "")
    .trim();
  entries.push([name, body]);
}
if (missing.length) {
  console.error(`Lucide ${VERSION} is missing: ${missing.join(", ")}`);
  process.exit(1);
}
entries.push(["circle", '<circle cx="12" cy="12" r="9"/>']);

const output = `// Generated by tools/fetch_icons.mjs from Lucide ${VERSION} (ISC).
// Do not edit by hand: change the map in the generator and rerun it.
(function () {
  const paths = {
${entries.map(([name, body]) => `    ${JSON.stringify(name)}: ${JSON.stringify(body)},`).join("\n")}
  };
  const NS = "http://www.w3.org/2000/svg";
  window.HRZUI = window.HRZUI || {};
  window.HRZUI.icon = function icon(name, options) {
    const { size = 16, label } = options || {};
    const svg = document.createElementNS(NS, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("width", String(size));
    svg.setAttribute("height", String(size));
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "1.5");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    if (label) {
      svg.setAttribute("role", "img");
      svg.setAttribute("aria-label", label);
    } else {
      svg.setAttribute("aria-hidden", "true");
    }
    svg.innerHTML = paths[name] || paths.circle;
    return svg;
  };
})();
`;
await writeFile(join(root, "frontend/app/ui/icons.js"), output, "utf8");
console.log(`wrote ui/icons.js with ${entries.length} icons`);
```

- [ ] **Step 2: Generate and licence**

```bash
node tools/fetch_icons.mjs
curl -sfL -o frontend/app/ui/LUCIDE-LICENSE.txt \
  https://cdn.jsdelivr.net/npm/lucide-static@0.468.0/LICENSE
node --check frontend/app/ui/icons.js
```

Expected: `wrote ui/icons.js with 61 icons`, no syntax errors, and the licence file starts with `ISC License`.

- [ ] **Step 3: Load it and replace the shell emoji**

In `index.html`, before `builder.js`:

```html
<script src="/assets/ui/icons.js"></script>
```

The generated `HRZUI` object is then augmented by `components.js` in `app.js`'s load order.

Replace these shell-chrome glyphs:

| Location | Replace | With |
| --- | --- | --- |
| `#split` text `＋ Panels` | `＋` | `<span data-icon="plus"></span> Panels` |
| `#railHide` text `×` | `×` | `<span data-icon="close"></span>` |
| `#asideHide` text `»` | `»` | `<span data-icon="chevron-right"></span>` |
| `#asideShow` text `«` | `«` | `<span data-icon="chevron-left"></span>` |
| `#railShow` text `◀` | `◀` | `<span data-icon="chevron-left"></span>` |
| `#themeToggle` text `◐` | `◐` | replaced by `HRZUI.icon(Theme.current() === "dark" ? "sun" : "moon")` |

Add one paint function near `main()` in `app.js`:

```js
/// Replace every `[data-icon]` placeholder with the icon it names. Called once
/// after the DOM is parsed, and again after any markup that ships placeholders.
function paintIcons(scope = document) {
  for (const host of scope.querySelectorAll("[data-icon]")) {
    const name = host.dataset.icon;
    if (!name || host.firstElementChild) continue;
    host.appendChild(HRZUI.icon(name, { size: Number(host.dataset.iconSize) || 16 }));
  }
  const toggle = document.getElementById("themeToggle");
  if (toggle) {
    toggle.replaceChildren(HRZUI.icon(Theme.current() === "dark" ? "sun" : "moon"));
    toggle.setAttribute("aria-label", Theme.current() === "dark" ? "Switch to light theme" : "Switch to dark theme");
  }
}
```

Call `paintIcons(document)` once in `main()` after the DOM is wired, and repaint on theme changes:

```js
Theme.onChange(() => paintIcons(document));
```

- [ ] **Step 4: Verify**

Run: `node --check frontend/app/ui/icons.js && node --check frontend/app/app.js`
Expected: no output.

Run: `cargo test -p api-gateway --test packaging`
Expected: PASS — `ui/icons.js` is referenced and present.

Manual: the header buttons render 16px stroke icons, no emoji left in the header, rail or status bar.

- [ ] **Step 5: Commit**

```bash
git add tools/fetch_icons.mjs frontend/app/ui/icons.js \
  frontend/app/ui/LUCIDE-LICENSE.txt frontend/app/index.html frontend/app/app.js
git commit -m "feat: one icon family for the shell, generated and committed"
```

---

### Task 5: Tokenise the moved styles

**Files:**
- Modify: every `frontend/app/styles/*.css` except `tokens.css`

**Interfaces:**
- Consumes: the compatibility aliases from `tokens.css`.
- Produces: `grep -nE '#[0-9a-fA-F]{3,8}|rgba?\(' frontend/app/styles/` returning only `tokens.css`.

- [ ] **Step 1: Prove the extent of the work**

```bash
grep -rnE '#[0-9a-fA-F]{3,8}|rgba?\(' frontend/app/styles/ | grep -v tokens.css | wc -l
```

Expected: a nonzero count (roughly 80–120 occurrences).

- [ ] **Step 2: Replace each literal with a token**

Use this table; when a literal has no exact token, pick the nearest semantic role in the same row:

| Old literal(s) | Token |
| --- | --- |
| `#0b0e13`, `#0a0d12` | `var(--bg-canvas)` |
| `#10141c`, `#0f141b` | `var(--bg-panel)` / `var(--panel)` |
| `#151a24`, `#141b24` | `var(--bg-raised)` / `var(--panel-2)` |
| `#1a222d`, `#171b26` | `var(--bg-overlay)` |
| `#1c2230`, `#1b2430`, `#232a38`, `#262b3a` | `var(--line)` |
| `#2a3242`, `#2a3644` | `var(--line-strong)` |
| `#e8ebf0`, `#e9edf3`, `#d6dbe4`, `#ffffff`, `#fff` (text on dark) | `var(--text-1)` |
| `#b3bccb`, `#aeb8c6`, `#8a93a6` (secondary text) | `var(--text-2)` |
| `#5b6577`, `#7c8898`, `#787b86` (hints) | `var(--text-3)` |
| `#4a9eff`, `#60a5fa`, `#5ea9ff`, `#2962ff` | `var(--accent)` |
| `rgba(74,158,255,…)`, `rgba(96,165,250,…)` | `var(--accent-soft)`, `var(--accent-line)` by alpha |
| `#2ebd85`, `#16c784`, `#4caf8e`, `#26a69a`, `#06130d` | `var(--up)` / `var(--up-soft)` / `var(--up-fill)` |
| `#f6465d`, `#ea3943`, `#fb7185`, `#f23645`, `#06121f` | `var(--down)` / `var(--down-soft)` / `var(--down-fill)` |
| `#edb541`, `#f5a524`, `#f59e0b` | `var(--warn)` |
| `#a78bfa`, `#9564e2`, `rgba(167,139,250,…)` | `var(--ai)` / `var(--ai-soft)` |
| `#6d8cff` (brand gradient) | delete with the old mark in Task 8 |
| `rgba(0,0,0,.35/.45/.6)`, `rgba(13,17,26,.6)`, `rgba(23,27,38,…)` | `var(--shadow-1)` / `var(--shadow-2)` |

Alpha tints that are not shadows become the matching `*-soft` token. `transparent`, `currentColor` and `inherit` stay.

- [ ] **Step 3: Gate it**

```bash
grep -rnE '#[0-9a-fA-F]{3,8}|rgba?\(' frontend/app/styles/ | grep -v tokens.css
```

Expected: no output.

Run the harness again:

```bash
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
```

Expected: PASS — a tokenised stylesheet changes values, not selectors.

- [ ] **Step 4: Commit**

```bash
git add frontend/app/styles
git commit -m "refactor: every shell colour now comes from a token"
```

---

## M2 — Components

M2 turns the token vocabulary into the component layer every destination will
use. The CSS lands first (Task 6) because it is the contract, then the
behaviour that CSS cannot express (Task 7). Neither task restyles a page; M2
ends with the shell's own controls migrated so M3 builds on real components.

### Task 6: `components.css` and its static contract

**Files:**
- Create: `frontend/app/styles/components.css`
- Create: `tools/component_check.mjs`
- Modify: `frontend/app/styles/tokens.css` (one new token, both themes)

**Interfaces:**
- Consumes: every token from Task 1, `base.css`'s `.sr-only` and focus rule.
- Produces: the class vocabulary used by M3–M6 (`hrz-btn`, `hrz-field`,
  `hrz-seg`, `hrz-tabs`, `hrz-panel`, `hrz-table`, `hrz-badge`, `hrz-price`,
  `hrz-menu`, `hrz-dialog`, `hrz-drawer`, `hrz-toast`, `hrz-skeleton`,
  `hrz-empty`, `hrz-error`, `hrz-spark`, `hrz-meter`, `hrz-tooltip`,
  `hrz-banner`) and the `u-*` layout helpers.

- [ ] **Step 1: Write the failing contract check**

`tools/component_check.mjs` audits the stylesheet the way `wasm_abi_check.mjs`
audits the module: it cannot judge taste, so it checks the promises that are
mechanical — every documented class exists, every `var(--x)` resolves, and no
colour literal escapes `tokens.css`.

```js
#!/usr/bin/env node
// Static contract for the component layer. Visual quality and keyboard
// behaviour are verified by hand (M2's verification block); this catches the
// failure that is invisible in a screenshot: a class the plan promises that
// the stylesheet never defined, or a token typo that silently falls back.

import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const styles = join(root, "frontend/app/styles");

const REQUIRED = [
  "hrz-btn", "hrz-btn--primary", "hrz-btn--secondary", "hrz-btn--ghost",
  "hrz-btn--danger", "hrz-btn--ai", "hrz-btn--sm", "hrz-btn--lg",
  "hrz-btn--icon", "hrz-spinner",
  "hrz-field", "hrz-field__hint", "hrz-field__error", "hrz-input",
  "hrz-select", "hrz-seg", "hrz-seg__item", "hrz-switch", "hrz-slider",
  "hrz-tabs", "hrz-tab", "hrz-tabpanel",
  "hrz-panel", "hrz-panel__head", "hrz-panel__title", "hrz-panel__actions",
  "hrz-panel__body", "hrz-panel--flush",
  "hrz-table", "hrz-table--compact", "hrz-table--roomy", "hrz-table__sort",
  "hrz-table__num", "hrz-table__row--selected",
  "hrz-price", "hrz-price--up", "hrz-price--down", "hrz-price--flat",
  "hrz-spark", "hrz-meter", "hrz-meter__bar", "hrz-meter__fill",
  "hrz-meter__label", "hrz-meter__value",
  "hrz-badge", "hrz-badge--neutral", "hrz-badge--accent", "hrz-badge--up",
  "hrz-badge--down", "hrz-badge--warn", "hrz-badge--ai",
  "hrz-menu", "hrz-menu__item", "hrz-menu__item--danger", "hrz-menu__hint",
  "hrz-menu__sep", "hrz-popover", "hrz-popover__head",
  "hrz-dialog", "hrz-dialog__title", "hrz-dialog__body", "hrz-dialog__foot",
  "hrz-drawer", "hrz-drawer--bottom", "hrz-drawer__handle",
  "hrz-toasts", "hrz-toast", "hrz-toast--error", "hrz-toast--warn",
  "hrz-toast--up", "hrz-toast__action",
  "hrz-skeleton", "hrz-skeleton--text", "hrz-skeleton--card", "hrz-skeleton--row",
  "hrz-empty", "hrz-empty__title", "hrz-empty__body", "hrz-empty__action",
  "hrz-error", "hrz-error__title", "hrz-error__msg", "hrz-error__retry",
  "hrz-banner", "hrz-tooltip",
  "u-row", "u-col", "u-between", "u-center", "u-grow",
  "u-gap-1", "u-gap-2", "u-gap-3", "u-gap-4",
];

const css = await readFile(join(styles, "components.css"), "utf8");
const tokens = await readFile(join(styles, "tokens.css"), "utf8");

const missing = REQUIRED.filter(
  (name) => !new RegExp(`\\.${name.replace(/[-]/g, "\\-")}(?![\\w-])`).test(css)
);

const declared = new Set([...(tokens + css).matchAll(/--([\w-]+)\s*:/g)].map((m) => m[1]));
const unresolved = [...new Set([...css.matchAll(/var\(--([\w-]+)/g)].map((m) => m[1]))].filter(
  (name) => !declared.has(name)
);

const literals = [...css.matchAll(/#[0-9a-fA-F]{3,8}\b|rgba?\(/g)].map((m) => m[0]);

let failed = false;
const report = (ok, message, detail) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${message}`);
  if (!ok) {
    failed = true;
    for (const line of detail) console.log(`       ${line}`);
  }
};
report(missing.length === 0, "every documented class is defined", missing);
report(unresolved.length === 0, "every var() resolves to a declared token", unresolved);
report(literals.length === 0, "no colour literal outside tokens.css", literals);
process.exit(failed ? 1 : 0);
```

Run it:

```bash
node tools/component_check.mjs
```

Expected: FAIL with `ENOENT` for `components.css` — the file the CSS contract
describes does not exist yet.

- [ ] **Step 2: Add the scrim token**

A modal backdrop is the one surface that must stay dark in the light theme.
Add to the dark block of `tokens.css`, after `--shadow-2`:

```css
  --scrim: rgba(3, 7, 12, 0.6);
```

and to the light block, after its `--shadow-2`:

```css
  --scrim: rgba(15, 23, 42, 0.35);
```

- [ ] **Step 3: Write `components.css`**

`frontend/app/styles/components.css`:

```css
/* ---------------------------------------------------------------------------
   HRZUI — the component layer. Class-first: markup carries the class and the
   state attribute; `components.js` only does what CSS cannot (focus, timers,
   positioning). Every colour is a token.
   --------------------------------------------------------------------------- */

/* ---- Button -------------------------------------------------------------- */
.hrz-btn {
  display: inline-flex; align-items: center; justify-content: center;
  gap: var(--space-1); height: 28px; padding: 0 10px;
  border: 1px solid transparent; border-radius: var(--radius-1);
  background: transparent; color: var(--text-1);
  font: 500 var(--text-xs)/1 var(--font-ui); white-space: nowrap;
  transition: background var(--dur-1) var(--ease), border-color var(--dur-1) var(--ease);
}
.hrz-btn:hover { background: var(--bg-raised); }
.hrz-btn:active { background: var(--bg-panel); }
.hrz-btn[disabled], .hrz-btn[aria-disabled="true"] { color: var(--text-3); cursor: not-allowed; }
.hrz-btn[disabled]:hover { background: transparent; }
.hrz-btn--primary { background: var(--accent); color: var(--accent-ink); }
.hrz-btn--primary:hover { background: var(--accent); filter: brightness(1.08); }
.hrz-btn--secondary { background: var(--bg-raised); border-color: var(--control-line); }
.hrz-btn--secondary:hover { border-color: var(--text-3); }
.hrz-btn--ghost { color: var(--text-2); }
.hrz-btn--ghost:hover { color: var(--text-1); }
.hrz-btn--danger { color: var(--down); border-color: var(--down); }
.hrz-btn--danger:hover { background: var(--down-soft); }
.hrz-btn--ai { color: var(--ai); border-color: var(--ai); }
.hrz-btn--ai:hover { background: var(--ai-soft); }
.hrz-btn--sm { height: 24px; padding: 0 8px; }
.hrz-btn--lg { height: 32px; padding: 0 12px; font-size: var(--text-sm); }
.hrz-btn--icon { width: 28px; padding: 0; }
.hrz-btn--sm.hrz-btn--icon { width: 24px; }
.hrz-btn--lg.hrz-btn--icon { width: 32px; }
.hrz-btn[aria-busy="true"] { color: transparent; position: relative; }
.hrz-btn[aria-busy="true"] > * { visibility: hidden; }
.hrz-btn[aria-busy="true"]::after {
  content: ""; position: absolute; width: 12px; height: 12px; border-radius: 50%;
  border: 2px solid var(--bg-raised); border-top-color: var(--accent);
  animation: hrz-spin 0.7s linear infinite;
}
.hrz-spinner {
  display: inline-block; width: 12px; height: 12px; border-radius: 50%;
  border: 2px solid var(--line-strong); border-top-color: var(--accent);
  animation: hrz-spin 0.7s linear infinite;
}
@keyframes hrz-spin { to { transform: rotate(360deg); } }

/* ---- Field, select ------------------------------------------------------- */
.hrz-field { display: flex; flex-direction: column; gap: var(--space-1); min-width: 0; }
.hrz-field > label, .hrz-label {
  font: 500 var(--text-2xs)/16px var(--font-ui); color: var(--text-2); letter-spacing: 0.01em;
}
.hrz-input, .hrz-select, .hrz-field input:not([type="checkbox"]):not([type="radio"]),
.hrz-field select, .hrz-field textarea {
  width: 100%; height: 28px; padding: 0 var(--space-2);
  background: var(--bg-panel); color: var(--text-1);
  border: 1px solid var(--control-line); border-radius: var(--radius-1);
  font: var(--text-xs)/1 var(--font-ui);
}
.hrz-field textarea { height: auto; min-height: 72px; padding: var(--space-2); line-height: 20px; resize: vertical; }
.hrz-input:hover, .hrz-select:hover { border-color: var(--text-3); }
.hrz-input:focus-visible, .hrz-select:focus-visible { border-color: var(--accent); outline-offset: 0; }
.hrz-input::placeholder { color: var(--text-3); }
.hrz-input[aria-invalid="true"] { border-color: var(--down); }
.hrz-select {
  appearance: none; padding-right: 28px;
  background-image: linear-gradient(45deg, transparent 50%, currentColor 50%),
    linear-gradient(135deg, currentColor 50%, transparent 50%);
  background-position: right 12px center, right 8px center;
  background-size: 4px 4px, 4px 4px; background-repeat: no-repeat;
}
.hrz-field__hint { font-size: var(--text-2xs); color: var(--text-3); }
.hrz-field__error { font-size: var(--text-2xs); color: var(--down); }

/* ---- Segmented control --------------------------------------------------- */
.hrz-seg {
  display: inline-flex; align-items: center; padding: 2px; gap: 2px;
  height: 28px; border: 1px solid var(--line-strong); border-radius: var(--radius-1);
  background: var(--bg-panel);
}
.hrz-seg__item {
  display: inline-flex; align-items: center; justify-content: center;
  height: 22px; padding: 0 var(--space-2); border: 0; border-radius: 3px;
  background: transparent; color: var(--text-2);
  font: 500 var(--text-2xs)/1 var(--font-ui); white-space: nowrap;
}
.hrz-seg__item:hover { color: var(--text-1); }
.hrz-seg__item[aria-pressed="true"] {
  background: var(--bg-raised); color: var(--text-1); box-shadow: var(--shadow-1);
}
.hrz-seg--sm { height: 24px; }
.hrz-seg--sm .hrz-seg__item { height: 18px; }

/* ---- Switch -------------------------------------------------------------- */
.hrz-switch {
  position: relative; width: 32px; height: 18px; flex: 0 0 auto;
  padding: 0; border: 1px solid var(--control-line); border-radius: 999px;
  background: var(--bg-panel);
  transition: background var(--dur-1) var(--ease), border-color var(--dur-1) var(--ease);
}
.hrz-switch::after {
  content: ""; position: absolute; top: 2px; left: 2px; width: 12px; height: 12px;
  border-radius: 50%; background: var(--text-2);
  transition: transform var(--dur-1) var(--ease), background var(--dur-1) var(--ease);
}
.hrz-switch[aria-checked="true"] { background: var(--accent); border-color: var(--accent); }
.hrz-switch[aria-checked="true"]::after { transform: translateX(14px); background: var(--accent-ink); }

/* ---- Slider -------------------------------------------------------------- */
.hrz-slider { width: 100%; height: 20px; padding: 0; background: transparent; appearance: none; }
.hrz-slider::-webkit-slider-runnable-track { height: 4px; border-radius: 999px; background: var(--line-strong); }
.hrz-slider::-moz-range-track { height: 4px; border-radius: 999px; background: var(--line-strong); }
.hrz-slider::-webkit-slider-thumb {
  appearance: none; width: 12px; height: 12px; margin-top: -4px; border: 0;
  border-radius: 50%; background: var(--accent);
}
.hrz-slider::-moz-range-thumb { width: 12px; height: 12px; border: 0; border-radius: 50%; background: var(--accent); }

/* ---- Tabs ---------------------------------------------------------------- */
.hrz-tabs { display: flex; align-items: stretch; gap: 2px; height: 32px; border-bottom: 1px solid var(--line); }
.hrz-tab {
  position: relative; display: inline-flex; align-items: center; gap: var(--space-1);
  height: 32px; padding: 0 10px; border: 0; background: transparent;
  color: var(--text-2); font: 500 var(--text-xs)/1 var(--font-ui); white-space: nowrap;
}
.hrz-tab:hover { color: var(--text-1); }
.hrz-tab[aria-selected="true"] { color: var(--text-1); }
.hrz-tab[aria-selected="true"]::after {
  content: ""; position: absolute; left: 8px; right: 8px; bottom: -1px;
  height: 2px; background: var(--accent); border-radius: 2px 2px 0 0;
}
.hrz-tabpanel[hidden] { display: none; }

/* ---- Panel --------------------------------------------------------------- */
.hrz-panel { display: flex; flex-direction: column; min-width: 0; background: var(--bg-panel); border-radius: var(--radius-2); }
.hrz-panel__head {
  display: flex; align-items: center; gap: var(--space-2); height: 32px;
  padding: 0 var(--space-3); border-bottom: 1px solid var(--line);
}
.hrz-panel__title { font: 600 var(--text-xs)/1 var(--font-ui); color: var(--text-1); }
.hrz-panel__actions { display: flex; align-items: center; gap: var(--space-1); margin-left: auto; }
.hrz-panel__body { padding: var(--space-3); min-width: 0; }
.hrz-panel--flush .hrz-panel__body { padding: 0; }

/* ---- Table --------------------------------------------------------------- */
.hrz-table { width: 100%; border-collapse: collapse; font: var(--text-xs)/1 var(--font-ui); }
.hrz-table th {
  position: sticky; top: 0; z-index: 1; height: 28px; padding: 0 var(--space-3);
  text-align: left; font: 500 var(--text-2xs)/1 var(--font-ui); color: var(--text-3);
  text-transform: uppercase; letter-spacing: 0.06em;
  background: var(--bg-panel); border-bottom: 1px solid var(--line);
}
.hrz-table td {
  height: 30px; padding: 0 var(--space-3); border-bottom: 1px solid var(--line);
  color: var(--text-1); white-space: nowrap;
}
.hrz-table tbody tr { transition: background var(--dur-1) var(--ease); }
.hrz-table tbody tr:hover { background: var(--bg-raised); }
.hrz-table tbody tr:last-child td { border-bottom: 0; }
.hrz-table__row--selected { background: var(--accent-soft); }
.hrz-table__row--selected td:first-child { box-shadow: inset 2px 0 0 var(--accent); }
.hrz-table__num { text-align: right; font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
.hrz-table--compact td { height: 28px; }
.hrz-table--roomy td { height: 36px; }
.hrz-table__sort {
  display: inline-flex; align-items: center; gap: 4px; padding: 0;
  border: 0; background: transparent; color: inherit; font: inherit;
  text-transform: inherit; letter-spacing: inherit;
}
.hrz-table__sort:hover { color: var(--text-1); }
.hrz-table th[aria-sort="ascending"] .hrz-table__sort::after,
.hrz-table th[aria-sort="descending"] .hrz-table__sort::after {
  content: ""; width: 0; height: 0; margin-left: 2px;
  border-left: 3px solid transparent; border-right: 3px solid transparent;
}
.hrz-table th[aria-sort="ascending"] .hrz-table__sort::after { border-bottom: 4px solid currentColor; }
.hrz-table th[aria-sort="descending"] .hrz-table__sort::after { border-top: 4px solid currentColor; }

/* ---- Price, badge -------------------------------------------------------- */
.hrz-price {
  font-family: var(--font-mono); font-variant-numeric: tabular-nums;
  color: var(--text-1); border-radius: var(--radius-1);
}
.hrz-price--up { color: var(--up); }
.hrz-price--down { color: var(--down); }
.hrz-price--flat { color: var(--text-2); }
@keyframes hrz-flash { from { background: var(--flash, var(--bg-raised)); } to { background: transparent; } }
.hrz-price--flash { animation: hrz-flash var(--dur-3) var(--ease); }

.hrz-badge {
  display: inline-flex; align-items: center; height: 18px; padding: 0 6px;
  border-radius: var(--radius-1); font: 600 var(--text-2xs)/1 var(--font-ui);
  text-transform: uppercase; letter-spacing: 0.06em; white-space: nowrap;
}
.hrz-badge--neutral { background: var(--bg-raised); color: var(--text-2); }
.hrz-badge--accent { background: var(--accent-soft); color: var(--accent); }
.hrz-badge--up { background: var(--up-soft); color: var(--up); }
.hrz-badge--down { background: var(--down-soft); color: var(--down); }
.hrz-badge--warn { background: var(--warn-soft); color: var(--warn); }
.hrz-badge--ai { background: var(--ai-soft); color: var(--ai); }

/* ---- Sparkline, meter ---------------------------------------------------- */
.hrz-spark { display: block; width: 64px; height: 20px; }
.hrz-meter { display: grid; grid-template-columns: 1fr auto; gap: var(--space-1) var(--space-2); align-items: center; }
.hrz-meter__label { font-size: var(--text-2xs); color: var(--text-2); }
.hrz-meter__value { font: var(--text-2xs)/1 var(--font-mono); color: var(--text-1); text-align: right; }
.hrz-meter__bar { grid-column: 1 / -1; height: 4px; border-radius: 999px; background: var(--line); overflow: hidden; }
.hrz-meter__fill {
  height: 100%; width: calc(var(--value, 0) * 100%);
  background: var(--accent); border-radius: inherit;
  transition: width var(--dur-2) var(--ease);
}
.hrz-meter--up .hrz-meter__fill { background: var(--up); }
.hrz-meter--down .hrz-meter__fill { background: var(--down); }
.hrz-meter--warn .hrz-meter__fill { background: var(--warn); }

/* ---- Menu, popover ------------------------------------------------------- */
.hrz-menu, .hrz-popover {
  position: fixed; left: var(--x, 0px); top: var(--y, 0px); z-index: 60;
  background: var(--bg-overlay); border: 1px solid var(--line-strong);
  border-radius: var(--radius-2); box-shadow: var(--shadow-2);
  animation: hrz-overlay-in var(--dur-2) var(--ease);
}
.hrz-menu { min-width: 240px; max-width: 320px; padding: var(--space-1); }
.hrz-menu__item {
  display: flex; align-items: center; gap: var(--space-2); width: 100%;
  height: 28px; padding: 0 var(--space-2); border: 0; border-radius: var(--radius-1);
  background: transparent; color: var(--text-1); font: var(--text-xs)/1 var(--font-ui);
  text-align: left;
}
.hrz-menu__item:hover, .hrz-menu__item:focus-visible { background: var(--bg-raised); outline: none; }
.hrz-menu__item[disabled] { color: var(--text-3); cursor: default; }
.hrz-menu__item[disabled]:hover { background: transparent; }
.hrz-menu__item--danger { color: var(--down); }
.hrz-menu__item--danger:hover { background: var(--down-soft); }
.hrz-menu__hint { margin-left: auto; font: var(--text-2xs)/1 var(--font-mono); color: var(--text-3); }
.hrz-menu__sep { height: 1px; margin: var(--space-1) 0; background: var(--line); }
.hrz-popover { width: 320px; padding: var(--space-3); }
.hrz-popover__head {
  display: flex; align-items: center; gap: var(--space-2); margin: calc(-1 * var(--space-3));
  margin-bottom: var(--space-3); padding: 0 var(--space-3); height: 32px;
  border-bottom: 1px solid var(--line); font: 600 var(--text-xs)/1 var(--font-ui);
}
@keyframes hrz-overlay-in {
  from { opacity: 0; transform: translateY(-2px) scale(0.98); }
  to { opacity: 1; transform: none; }
}

/* ---- Dialog, drawer ------------------------------------------------------ */
.hrz-dialog {
  width: min(480px, calc(100vw - var(--space-7))); padding: 0; border: 0;
  border-radius: var(--radius-3); background: var(--bg-overlay); color: var(--text-1);
  box-shadow: var(--shadow-2);
}
.hrz-dialog::backdrop { background: var(--scrim); }
.hrz-dialog__title { margin: 0; padding: var(--space-4) var(--space-4) 0; font: 600 var(--text-md)/24px var(--font-ui); }
.hrz-dialog__body { padding: var(--space-3) var(--space-4); font-size: var(--text-sm); color: var(--text-2); line-height: 20px; }
.hrz-dialog__foot {
  display: flex; align-items: center; justify-content: flex-end; gap: var(--space-2);
  padding: var(--space-3) var(--space-4); border-top: 1px solid var(--line);
}

.hrz-drawer {
  position: fixed; top: 0; right: 0; bottom: 0; z-index: 55;
  width: min(400px, 90vw); display: flex; flex-direction: column;
  background: var(--bg-panel); border-left: 1px solid var(--line-strong);
  box-shadow: var(--shadow-2); animation: hrz-drawer-in var(--dur-2) var(--ease);
}
.hrz-drawer--bottom {
  top: auto; left: 0; right: 0; width: auto; height: min(90vh, 640px);
  border-left: 0; border-top: 1px solid var(--line-strong);
  border-radius: var(--radius-3) var(--radius-3) 0 0;
  animation-name: hrz-sheet-in;
}
.hrz-drawer__handle {
  display: none; width: 36px; height: 4px; margin: var(--space-2) auto var(--space-1);
  border: 0; padding: 0; border-radius: 999px; background: var(--line-strong);
}
.hrz-drawer--bottom .hrz-drawer__handle { display: block; }
@keyframes hrz-drawer-in { from { transform: translateX(24px); opacity: 0; } to { transform: none; opacity: 1; } }
@keyframes hrz-sheet-in { from { transform: translateY(24px); opacity: 0; } to { transform: none; opacity: 1; } }

/* ---- Toasts -------------------------------------------------------------- */
.hrz-toasts {
  position: fixed; right: var(--space-4); bottom: calc(var(--status-h) + var(--space-4));
  z-index: 80; display: flex; flex-direction: column; align-items: flex-end; gap: var(--space-2);
  pointer-events: none;
}
.hrz-toast {
  display: flex; align-items: center; gap: var(--space-3); max-width: 360px;
  padding: 10px var(--space-3); border: 1px solid var(--line-strong);
  border-left: 3px solid var(--accent); border-radius: var(--radius-2);
  background: var(--bg-overlay); color: var(--text-1);
  font-size: var(--text-xs); box-shadow: var(--shadow-2);
  pointer-events: auto; animation: hrz-overlay-in var(--dur-2) var(--ease);
}
.hrz-toast--error { border-left-color: var(--down); }
.hrz-toast--warn { border-left-color: var(--warn); }
.hrz-toast--up { border-left-color: var(--up); }
.hrz-toast__action {
  border: 0; padding: 0; background: transparent; color: var(--accent);
  font: 600 var(--text-2xs)/1 var(--font-ui); white-space: nowrap;
}
.hrz-toast__action:hover { text-decoration: underline; }

/* ---- Skeleton, empty, error --------------------------------------------- */
.hrz-skeleton { display: flex; flex-direction: column; gap: var(--space-2); }
.hrz-skeleton > * {
  border-radius: var(--radius-1);
  background: linear-gradient(90deg, var(--bg-raised) 25%, var(--bg-overlay) 50%, var(--bg-raised) 75%);
  background-size: 400% 100%;
  animation: hrz-shimmer 1.4s var(--ease) infinite;
}
.hrz-skeleton--text > * { height: 12px; }
.hrz-skeleton--row > * { height: 30px; }
.hrz-skeleton--card > * { height: 96px; border-radius: var(--radius-3); }
@keyframes hrz-shimmer { from { background-position: 100% 0; } to { background-position: 0 0; } }

.hrz-empty, .hrz-error {
  display: flex; flex-direction: column; align-items: center; justify-content: center;
  gap: var(--space-2); padding: var(--space-7) var(--space-4); text-align: center;
}
.hrz-empty__title, .hrz-error__title { font: 600 var(--text-sm)/20px var(--font-ui); color: var(--text-1); }
.hrz-empty__body, .hrz-error__msg { max-width: 48ch; font-size: var(--text-xs); color: var(--text-2); }
.hrz-error__title { color: var(--warn); }
.hrz-empty__action, .hrz-error__retry { margin-top: var(--space-1); }

/* ---- Connection banner --------------------------------------------------- */
.hrz-banner {
  display: flex; align-items: center; gap: var(--space-2);
  height: 28px; padding: 0 var(--space-3);
  background: var(--warn-soft); color: var(--warn);
  font: 500 var(--text-2xs)/1 var(--font-ui);
  border-bottom: 1px solid var(--line);
}
.hrz-banner[hidden] { display: none; }

/* ---- Tooltip ------------------------------------------------------------- */
.hrz-tooltip {
  position: fixed; left: var(--x, 0px); top: var(--y, 0px); z-index: 90;
  max-width: 260px; padding: 4px var(--space-2);
  border-radius: var(--radius-1); background: var(--bg-overlay);
  border: 1px solid var(--line-strong); box-shadow: var(--shadow-1);
  color: var(--text-1); font: var(--text-2xs)/16px var(--font-ui);
  pointer-events: none;
}

/* ---- Layout helpers ------------------------------------------------------ */
.u-row { display: flex; align-items: center; gap: var(--space-2); min-width: 0; }
.u-col { display: flex; flex-direction: column; gap: var(--space-2); min-width: 0; }
.u-between { justify-content: space-between; }
.u-center { justify-content: center; }
.u-grow { flex: 1 1 auto; min-width: 0; }
.u-gap-1 { gap: var(--space-1); }
.u-gap-2 { gap: var(--space-2); }
.u-gap-3 { gap: var(--space-3); }
.u-gap-4 { gap: var(--space-4); }
```

- [ ] **Step 4: Run the contract check**

```bash
node tools/component_check.mjs
```

Expected: PASS — three `ok` lines and exit 0.

- [ ] **Step 5: Look at it**

Build a throwaway gallery (not committed) and open it in the browser:

```bash
cat > /tmp/hrz-components.html <<'HTML'
<!doctype html><html data-theme="dark"><head><meta charset="utf-8">
<link rel="stylesheet" href="http://127.0.0.1:8080/assets/styles/tokens.css">
<link rel="stylesheet" href="http://127.0.0.1:8080/assets/styles/base.css">
<link rel="stylesheet" href="http://127.0.0.1:8080/assets/styles/components.css">
</head><body style="padding:24px;display:grid;gap:16px">
<div class="u-row"><button class="hrz-btn hrz-btn--primary">Primary</button>
<button class="hrz-btn hrz-btn--secondary">Secondary</button>
<button class="hrz-btn hrz-btn--ghost">Ghost</button>
<button class="hrz-btn hrz-btn--danger">Delete</button>
<button class="hrz-btn hrz-btn--ai">Ask AI</button>
<button class="hrz-btn hrz-btn--secondary" aria-busy="true">Saving</button></div>
<div class="u-row"><span class="hrz-badge hrz-badge--up">Live</span>
<span class="hrz-badge hrz-badge--warn">Delayed</span>
<span class="hrz-price hrz-price--up">67,412.50</span>
<span class="hrz-price hrz-price--down">−1.24%</span></div>
<div class="hrz-seg"><button class="hrz-seg__item" aria-pressed="false">1m</button>
<button class="hrz-seg__item" aria-pressed="true">5m</button>
<button class="hrz-seg__item" aria-pressed="false">1h</button></div>
<div class="hrz-panel" style="width:420px"><div class="hrz-panel__head">
<span class="hrz-panel__title">Markets</span><div class="hrz-panel__actions">
<button class="hrz-btn hrz-btn--sm hrz-btn--ghost">Sort</button></div></div>
<div class="hrz-panel__body hrz-panel--flush" style="padding:0"><table class="hrz-table hrz-table--compact">
<thead><tr><th aria-sort="ascending"><button class="hrz-table__sort">Symbol</button></th>
<th class="hrz-table__num">Last</th></tr></thead>
<tbody><tr class="hrz-table__row--selected"><td>BTCUSDT</td><td class="hrz-table__num">67,412.50</td></tr>
<tr><td>ETHUSDT</td><td class="hrz-table__num">2,481.10</td></tr></tbody></table></div></div>
<div class="hrz-field" style="width:240px"><label for="demo">Symbol</label>
<input class="hrz-input" id="demo" placeholder="Search"><span class="hrz-field__hint">Venue listing</span></div>
<div class="hrz-skeleton hrz-skeleton--row" style="width:240px"><div></div><div></div><div></div></div>
<div class="hrz-empty" style="width:320px"><div class="hrz-empty__title">No alerts yet</div>
<div class="hrz-empty__body">Add a watch monitor from Details.</div></div>
</body></html>
HTML
python3 -m http.server 8099 --directory /tmp >/dev/null 2>&1 &
```

Open `http://127.0.0.1:8099/hrz-components.html` with the browser tools at
1440×900, then repeat with `data-theme="light"`. Both themes must read as the
same product: equal hierarchy, no unreadable label, no control that disappears
into its ground. Delete `/tmp/hrz-components.html` and stop the server after
the review.

- [ ] **Step 6: Commit**

```bash
git add tools/component_check.mjs frontend/app/styles/components.css frontend/app/styles/tokens.css
git commit -m "feat: the component stylesheet every destination will use"
```

---

### Task 7: `ui/components.js` — the behaviour CSS cannot provide

**Files:**
- Create: `frontend/app/ui/components.js`
- Create: `tools/ui_check.mjs`
- Modify: `frontend/app/app.js` (`pageToast` delegates to `HRZUI.toast`)
- Modify: `frontend/app/index.html` (load `components.js` before `builder.js`)

**Interfaces:**
- Consumes: `HRZUI.icon` from Task 4, the classes from Task 6, `Theme.current()`.
- Produces: the full `HRZUI` API in spec §3.1 —
  `icon, tooltip, menu, popover, dialog, confirm, drawer, toast, tabs, table,
  sparkline, meter, skeleton, empty, errorState, format, escape`.
- Later tasks call only these names; nothing in M3–M6 builds an ad-hoc menu,
  toast or table.

- [ ] **Step 1: Write the failing behaviour test**

`tools/ui_check.mjs` runs the real files in jsdom, the way `shell_check.mjs`
does, but with a fixture document instead of the whole app:

```js
#!/usr/bin/env node
// Behaviour tests for HRZUI in jsdom. jsdom computes no layout, so nothing
// here asserts pixels; it asserts the promises that are logic: escaping,
// focus return, keyboard handling, timer discipline, ARIA state.

import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
let JSDOM;
try {
  ({ JSDOM } = require("jsdom"));
} catch {
  console.error("jsdom is not installed; set NODE_PATH as shell_check.mjs does");
  process.exit(2);
}

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const dom = new JSDOM("<!doctype html><html><body></body></html>", {
  url: "http://127.0.0.1:8080/",
  runScripts: "outside-only",
  pretendToBeVisual: true,
});
const { window } = dom;
window.matchMedia = (query) => ({
  matches: false, media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});

for (const file of ["frontend/app/ui/theme.js", "frontend/app/ui/icons.js", "frontend/app/ui/components.js"]) {
  window.eval(await readFile(join(root, file), "utf8"));
}
const { document, HRZUI } = window;

let failures = 0;
const check = (name, ok, detail = "") => {
  if (ok) console.log(`ok   ${name}`);
  else { failures += 1; console.log(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

check("escape neutralises markup", HRZUI.escape("<img src=x onerror=1>") === "&lt;img src=x onerror=1&gt;");
check("price formats with separators", HRZUI.format.price(67412.5) === "67,412.50", HRZUI.format.price(67412.5));
check("percent carries direction", HRZUI.format.percent(-1.2) === "−1.20%", HRZUI.format.percent(-1.2));

const trigger = document.createElement("button");
document.body.appendChild(trigger);
const menu = HRZUI.menu(trigger, [
  { label: "Rename", hint: "F2", onSelect: () => {} },
  { separator: true },
  { label: "Delete", danger: true, onSelect: () => {} },
]);
menu.open();
check("menu opens with role=menu", document.querySelector('[role="menu"]') !== null);
check("menu items are buttons", document.querySelectorAll('[role="menuitem"]').length === 2);
menu.close();
check("menu closes and empties", document.querySelector('[role="menu"]') === null);
check("focus returns to the trigger", document.activeElement === trigger);

const toastOne = HRZUI.toast("Saved", { timeout: 10 });
const toastTwo = HRZUI.toast("Save failed", { tone: "error", timeout: 10 });
check("toasts are announced", document.querySelector('[role="alert"]') !== null);
check("toast limit is three", document.querySelectorAll(".hrz-toast").length <= 3);
await wait(30);
toastOne.dismiss(); toastTwo.dismiss();
check("dismiss removes the node", document.querySelectorAll(".hrz-toast").length === 0);

const tabs = document.createElement("div");
tabs.className = "hrz-tabs";
tabs.setAttribute("role", "tablist");
tabs.innerHTML = '<button role="tab" aria-selected="true" data-tab="a">A</button>' +
  '<button role="tab" aria-selected="false" data-tab="b">B</button>';
const panelA = document.createElement("div");
panelA.dataset.panel = "a";
const panelB = document.createElement("div");
panelB.dataset.panel = "b";
document.body.append(tabs, panelA, panelB);
const tabApi = HRZUI.tabs(tabs);
tabApi.select("b");
check("selecting a tab flips aria-selected",
  tabs.querySelector('[data-tab="b"]').getAttribute("aria-selected") === "true");
check("selecting a tab hides the other panel", panelA.hidden === true && panelB.hidden === false);

const meter = document.createElement("div");
document.body.appendChild(meter);
HRZUI.meter(meter, { value: 42, max: 100, tone: "up" });
check("meter exposes progressbar values",
  meter.getAttribute("role") === "progressbar" && meter.getAttribute("aria-valuenow") === "42");

const empty = document.createElement("div");
document.body.appendChild(empty);
let clicked = false;
HRZUI.empty(empty, { title: "No bots yet", body: "Build one from a strategy.", action: { label: "New bot", onClick: () => { clicked = true; } } });
check("empty state renders its action as a button", empty.querySelector("button").textContent === "New bot");
empty.querySelector("button").click();
check("empty action runs", clicked);

console.log(failures === 0 ? "\nPASS" : `\n${failures} FAILED`);
process.exit(failures === 0 ? 0 : 1);
```

Run it:

```bash
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/ui_check.mjs
```

Expected: FAIL — `components.js` does not exist.

- [ ] **Step 2: Write `components.js`**

`frontend/app/ui/components.js`:

```js
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
    const stroke = trend === "down" ? "var(--down)" : "var(--up)";

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
```

- [ ] **Step 3: Load the file and migrate `pageToast`**

In `frontend/app/index.html`, before the `builder.js` script tag:

```html
<script src="/assets/ui/components.js"></script>
```

In `app.js`, replace the body of `pageToast` (and delete `pageToastTimer`):

```js
/// Page-level messages go through the shared toast stack, so a message from
/// the workspace chat and one from a chart pane cannot occupy two different
/// corners of the screen.
function pageToast(message) {
  if (!message) return;
  HRZUI.toast(message);
}
```

- [ ] **Step 4: Run the behaviour test and the harness**

```bash
node --check frontend/app/ui/components.js
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/ui_check.mjs
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
```

Expected: `ui_check` prints PASS; `shell_check` prints PASS. If `shell_check`
fails because `pageToast` no longer creates `#pageToast`, find the assertion
with `grep -n "pageToast" tools/shell_check.mjs` and update it to assert a
`.hrz-toast` with the same text — the visible behaviour is unchanged.

- [ ] **Step 5: Commit**

```bash
git add frontend/app/ui/components.js frontend/app/index.html frontend/app/app.js tools/ui_check.mjs
git commit -m "feat: the HRZUI behaviour layer, and one toast for the whole page"
```

---

## M3 — Frame, router, pane host

M3 is the first milestone that changes what a signed-in user sees: the top bar,
the destination rail, the status bar, and a real hash router. It is sequenced so
the page is usable after every commit — the frame lands around the existing
Charts markup first (Task 8), then routing and the pane host switch on (Task 9),
then the palette and shortcuts (Task 10).

### Task 8: The frame markup and `shell.css`

**Files:**
- Modify: `frontend/app/index.html:1823-1861` (header → top bar)
- Modify: `frontend/app/index.html:1937-2912` (`main` → frame + destinations)
- Modify: `frontend/app/index.html:2920-2927` (status bar)
- Modify: `frontend/app/styles/shell.css` (frame block; replaces the old header rules)
- Modify: `frontend/app/styles/charts.css` (charts destination layout, inspector wrapper)
- Modify: `frontend/app/app.js` (`applyAuthGate` toggles the frame; splitter selector)

**Interfaces:**
- Consumes: `tokens.css`, `base.css`, `components.css`, `HRZUI.icon` via `[data-icon]`.
- Produces: `#topbar`, `#frame`, `#rail[data-dest]`, `#mainView`, the six
  `#dest-*` sections, `#panePark`, `#inspectorHost`/`#inspector`,
  `#chartToolbar`, `#dock`, and the status-bar spans Task 9 fills.

- [ ] **Step 1: Replace the header with the top bar**

Replace the whole `<header>…</header>` block (`index.html:1823-1861`) with:

```html
<header id="topbar">
  <a class="brand" href="#/dashboard" aria-label="HRZ dashboard">
    <span class="brandMark" aria-hidden="true"></span>
    <span class="brandWord">HRZ</span>
  </a>
  <span id="destTitle" class="destTitle" hidden>Charts</span>
  <button id="paletteBtn" class="paletteBtn" type="button" aria-haspopup="dialog" disabled>
    <span class="u-grow">Search or jump to…</span>
    <kbd class="paletteKey">⌘K</kbd>
  </button>
  <span class="spacer"></span>
  <span id="connDotTop" class="connDot" hidden title="Connection"></span>
  <button id="aiBtn" class="hrz-btn hrz-btn--ai hrz-btn--icon" type="button"
          aria-label="Open AI analyst" title="AI analyst" disabled>
    <span data-icon="sparkles"></span>
  </button>
  <span id="session">not signed in</span>
  <button id="themeToggle" class="hrz-btn hrz-btn--icon" type="button" aria-label="Switch theme">◐</button>
  <button id="signinToggle" class="hrz-btn hrz-btn--secondary hrz-btn--sm" type="button">Sign in</button>
</header>
```

`#paletteBtn` and `#aiBtn` ship `disabled`: their behaviour arrives in Task 9
(the AI drawer) and Task 10 (the palette), and a button that looks live but
does nothing is worse than one that says it is not ready. The `<kbd>` hint is replaced with `⌘K` / `Ctrl+K` by Shell once it can
see the platform.

- [ ] **Step 2: Insert the framed destinations**

Replace `<main hidden>` (`index.html:1937`) with the frame, the rail and the
destination view's opening tags:

```html
<div id="frame" hidden>
  <nav id="rail" aria-label="Destinations">
    <button type="button" class="railItem" data-dest="dashboard" aria-label="Dashboard" title="Dashboard" disabled>
      <span data-icon="activity" data-icon-size="20"></span>
    </button>
    <button type="button" class="railItem" data-dest="charts" aria-label="Charts" title="Charts" disabled>
      <span data-icon="chart" data-icon-size="20"></span>
    </button>
    <button type="button" class="railItem" data-dest="studio" aria-label="Studio" title="Studio" disabled>
      <span data-icon="sparkles" data-icon-size="20"></span>
    </button>
    <button type="button" class="railItem" data-dest="community" aria-label="Community" title="Community" disabled>
      <span data-icon="globe" data-icon-size="20"></span>
    </button>
    <button type="button" class="railItem" data-dest="bots" aria-label="Bots" title="Bots" disabled>
      <span data-icon="zap" data-icon-size="20"></span>
    </button>
    <button type="button" class="railItem" data-dest="account" aria-label="Account" title="Account" disabled>
      <span data-icon="user" data-icon-size="20"></span>
    </button>
    <span class="spacer"></span>
    <button type="button" class="railItem" id="railHelp" aria-label="Keyboard shortcuts" title="Keyboard shortcuts" disabled>
      <span data-icon="command" data-icon-size="20"></span>
    </button>
  </nav>
```

Continue directly after the rail:

```html
  <main id="mainView" hidden>
    <section class="destination" id="dest-dashboard" aria-labelledby="dest-dashboard-title" hidden>
      <h1 id="dest-dashboard-title" tabindex="-1">Dashboard</h1>
      <p class="destPlaceholder">Markets, watchlist and pulse land in M5.</p>
    </section>

    <section class="destination" id="dest-charts" aria-labelledby="dest-charts-title">
      <h1 class="sr-only" id="dest-charts-title" tabindex="-1">Charts</h1>
```

Then close the charts section immediately before `</main>` (`index.html:2912`)
and add the remaining destinations:

```html
    </section>

    <section class="destination" id="dest-studio" aria-labelledby="dest-studio-title" hidden>
      <h1 class="destHeading" id="dest-studio-title" tabindex="-1">Studio</h1>
      <div class="destHost" id="studioHost"></div>
    </section>

    <section class="destination" id="dest-community" aria-labelledby="dest-community-title" hidden>
      <h1 class="destHeading" id="dest-community-title" tabindex="-1">Community</h1>
      <div class="destHost" id="communityHost"></div>
    </section>

    <section class="destination" id="dest-bots" aria-labelledby="dest-bots-title" hidden>
      <h1 class="destHeading" id="dest-bots-title" tabindex="-1">Bots</h1>
      <div class="destHost" id="botsHost"></div>
    </section>

    <section class="destination" id="dest-account" aria-labelledby="dest-account-title" hidden>
      <h1 class="destHeading" id="dest-account-title" tabindex="-1">Account</h1>
      <div class="hrz-tabs" id="accountTabs" role="tablist" aria-label="Account sections">
        <button role="tab" type="button" class="hrz-tab" data-account="plan" aria-selected="true">Plan</button>
        <button role="tab" type="button" class="hrz-tab" data-account="brokers" aria-selected="false">Brokers</button>
        <button role="tab" type="button" class="hrz-tab" data-account="ai" aria-selected="false">AI model</button>
        <button role="tab" type="button" class="hrz-tab" data-account="profile" aria-selected="false">Profile</button>
      </div>
      <div class="destHost" id="accountHost">
        <div id="accountProfile" data-account-panel="profile" hidden>
          <p class="muted">Session, plan and appearance settings land in Spec 2.</p>
        </div>
      </div>
    </section>
  </main>
</div>
```

The existing watchlist, chart column, aside and reveal strips stay exactly where
they are; they now sit inside `#dest-charts`. Do not reorder them.

- [ ] **Step 3: Move the layout controls into the chart column**

Cut the `#split` button and the `#layoutCtl` div out of the new top bar (they
were not included above) and insert this block at the top of `#chartColumn`,
before `#globalTools`:

```html
<div id="chartToolbar" class="chartToolbar">
  <button id="split" class="hrz-btn hrz-btn--secondary hrz-btn--sm" type="button"
          title="Add another chart beside this one">
    <span data-icon="plus"></span> Panel
  </button>
  <div id="layoutCtl">
    <select id="layoutMode" class="hrz-select" aria-label="Chart layout"
            title="Split: up to four charts in rows. Grid: a scrolling wall of up to 100.">
      <option value="split">Split</option>
      <option value="grid">Grid</option>
    </select>
    <select id="chartCols" class="hrz-select" aria-label="Grid columns"
            title="How many chart columns the grid shows" hidden>
      <option value="1">1 col</option>
      <option value="2" selected>2 cols</option>
      <option value="3">3 cols</option>
      <option value="4">4 cols</option>
      <option value="5">5 cols</option>
      <option value="6">6 cols</option>
      <option value="7">7 cols</option>
      <option value="8">8 cols</option>
      <option value="9">9 cols</option>
      <option value="10">10 cols</option>
    </select>
  </div>
  <span class="spacer"></span>
  <button id="watchlistToggle" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon" type="button"
          aria-label="Toggle watchlist" title="Toggle watchlist" disabled>
    <span data-icon="panel-left"></span>
  </button>
  <button id="inspectorToggle" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon" type="button"
          aria-label="Toggle inspector" title="Toggle inspector" disabled>
    <span data-icon="panel-right"></span>
  </button>
  <button id="dockToggle" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon" type="button"
          aria-label="Toggle bottom dock" title="Bottom dock" disabled>
    <span data-icon="panel-bottom"></span>
  </button>
</div>
```

These IDs are temporary homes for controls the existing code already owns.
Task 11 turns this strip into the instrument bar; keeping `#split`, `#layoutMode`
and `#chartCols` in the DOM with their original IDs means no listener changes.

- [ ] **Step 4: Wrap the aside in an inspector host**

Insert `<div id="inspectorHost">` immediately after `#splitAside` (`index.html:2246`)
and close it immediately after `</aside>`:

```html
<div id="inspectorHost">
  <aside id="inspector">
    …
  </aside>
</div>
```

`#asideShow` stays outside the wrapper. The old tab strip stays as it is for one
more task; Task 9 replaces it.

- [ ] **Step 5: Add the dock shell**

Immediately after `#charts`'s closing `</div>` (the one that closes the wall,
before `#chartColumn` closes), insert:

```html
<div id="dock" class="dock" hidden>
  <div class="dockTabs">
    <div class="hrz-tabs" role="tablist" aria-label="Workbench">
      <button role="tab" type="button" class="hrz-tab" data-dock="studio" aria-selected="true">Studio</button>
      <button role="tab" type="button" class="hrz-tab" data-dock="alerts" aria-selected="false">Alerts</button>
      <button role="tab" type="button" class="hrz-tab" data-dock="orders" aria-selected="false">Orders</button>
      <button role="tab" type="button" class="hrz-tab" data-dock="console" aria-selected="false">Console</button>
    </div>
    <button type="button" id="dockCollapse" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon"
            aria-label="Collapse the dock" title="Collapse">
      <span data-icon="chevron-down"></span>
    </button>
  </div>
  <div class="dockBody" id="dockBody"></div>
</div>
```

- [ ] **Step 6: Replace the status bar**

Replace `#statusbar`'s contents with:

```html
<footer id="statusbar" hidden>
  <span class="sbGroup">
    <span id="sbConnection" class="sbDot" aria-hidden="true"></span>
    <span id="sbConnLabel">Offline</span>
  </span>
  <span class="sbSep">·</span>
  <span id="sbVenue">Binance</span>
  <span class="sbSep">·</span>
  <span id="sbSymbol" class="mono">—</span>
  <span id="sbTimeframe" class="mono"></span>
  <span id="sbCharts">1 chart</span>
  <span class="spacer"></span>
  <span id="sbSave" class="sbSave" title="Workspace save state">Saved</span>
  <span class="sbSep">·</span>
  <span id="econCountdown" class="econChip" hidden></span>
  <span id="sbLatency" hidden></span>
  <span id="sbPlan" class="sbPlan"></span>
  <span id="utcClock">--:--:-- UTC</span>
</footer>
```

`#sbCharts`, `#econCountdown` and `#utcClock` keep their IDs and their existing
writers. The new spans start empty; Task 9 fills them.

- [ ] **Step 7: Add the parking container and drop the old brand rule**

Before `<script src="builder.js">`:

```html
<div id="panePark" hidden></div>
<div id="overlays"></div>
```

In `shell.css`, delete the old `header { … }` block and the `h1::before`
diamond, then add:

```css
/* ---- Frame --------------------------------------------------------------- */
#topbar {
  display: flex; align-items: center; gap: var(--space-2);
  height: var(--topbar-h); flex: 0 0 var(--topbar-h);
  padding: 0 var(--space-3);
  background: var(--bg-panel); border-bottom: 1px solid var(--line);
}
.brand {
  display: inline-flex; align-items: center; gap: var(--space-2);
  color: var(--text-1); text-decoration: none;
  font: 700 var(--text-sm)/1 var(--font-ui); letter-spacing: 0.04em;
}
.brand:hover { text-decoration: none; }
.brandMark {
  width: 18px; height: 18px; flex: 0 0 auto; border-radius: 5px;
  background: linear-gradient(135deg, var(--accent), var(--ai));
  clip-path: polygon(50% 0, 100% 50%, 50% 100%, 0 50%);
}
.destTitle { font: 500 var(--text-xs)/1 var(--font-ui); color: var(--text-2); }
.paletteBtn {
  display: flex; align-items: center; gap: var(--space-2);
  width: min(420px, 34vw); height: 28px; padding: 0 var(--space-2);
  margin: 0 auto; border: 1px solid var(--line-strong); border-radius: var(--radius-1);
  background: var(--bg-canvas); color: var(--text-3);
  font: var(--text-xs)/1 var(--font-ui); text-align: left;
}
.paletteBtn:hover { border-color: var(--control-line); color: var(--text-2); }
.paletteBtn[disabled] { opacity: 0.6; cursor: default; }
.paletteKey {
  padding: 1px 5px; border: 1px solid var(--line-strong); border-radius: 3px;
  font: var(--text-2xs)/1 var(--font-mono); color: var(--text-3);
}
#session { font: var(--text-2xs)/1 var(--font-mono); color: var(--text-3); }
.connDot { width: 8px; height: 8px; border-radius: 50%; background: var(--up); }
.connDot[data-state="down"] { background: var(--down); }
.connDot[data-state="wait"] { background: var(--warn); }

#frame { flex: 1 1 auto; display: flex; min-height: 0; }
#frame[hidden] { display: none; }
#rail {
  width: var(--rail-w); flex: 0 0 var(--rail-w);
  display: flex; flex-direction: column; align-items: center; gap: 2px;
  padding: var(--space-2) 0;
  background: var(--bg-panel); border-right: 1px solid var(--line);
}
.railItem {
  position: relative; display: flex; align-items: center; justify-content: center;
  width: var(--rail-w); height: 44px; padding: 0;
  border: 0; border-radius: 0; background: transparent; color: var(--text-2);
}
.railItem:hover { background: var(--bg-raised); color: var(--text-1); }
.railItem[disabled] { opacity: 0.45; cursor: default; }
.railItem[aria-current="page"] { background: var(--bg-raised); color: var(--accent); }
.railItem[aria-current="page"]::before {
  content: ""; position: absolute; left: 0; top: 8px; bottom: 8px; width: 2px;
  border-radius: 0 2px 2px 0; background: var(--accent);
}

#mainView { flex: 1 1 auto; display: flex; min-width: 0; min-height: 0; }
#mainView[hidden] { display: none; }
.destination { flex: 1 1 auto; display: flex; flex-direction: column; min-width: 0; min-height: 0; }
.destination[hidden] { display: none; }
.destHeading { margin: 0; padding: var(--space-6) var(--space-6) 0; font-size: var(--text-lg); }
.destHost { flex: 1 1 auto; min-width: 0; min-height: 0; padding: 0 var(--space-6) var(--space-6); }
.destPlaceholder { padding: var(--space-4) 0; color: var(--text-3); font-size: var(--text-xs); }
#dest-dashboard, #dest-studio, #dest-community, #dest-bots, #dest-account { overflow: auto; }
#dest-charts { flex-direction: row; overflow: hidden; }
#panePark { display: none; }

#statusbar {
  display: flex; align-items: center; gap: var(--space-2);
  height: var(--status-h); flex: 0 0 var(--status-h);
  padding: 0 var(--space-3);
  background: var(--bg-panel); border-top: 1px solid var(--line);
  font: var(--text-2xs)/1 var(--font-mono); color: var(--text-3);
}
#statusbar[hidden] { display: none; }
.sbGroup { display: inline-flex; align-items: center; gap: var(--space-1); }
.sbDot { width: 6px; height: 6px; border-radius: 50%; background: var(--up); }
.sbDot[data-state="down"] { background: var(--down); }
.sbDot[data-state="wait"] { background: var(--warn); }
.sbSep { color: var(--line-strong); }
.sbSave[data-state="saving"] { color: var(--warn); }
.sbSave[data-state="failed"] { color: var(--down); }
.sbPlan { color: var(--text-2); }
.chartToolbar {
  display: flex; align-items: center; gap: var(--space-2);
  height: 36px; flex: 0 0 36px; padding: 0 var(--space-2);
  border-bottom: 1px solid var(--line); background: var(--bg-panel);
}
.chartToolbar .spacer { flex: 1 1 auto; }
.chartToolbar select { width: auto; }
```

The existing `main { flex: 1 }` rule becomes `#mainView`; delete the old rule to
avoid two layout owners.

- [ ] **Step 8: Lay out the Charts destination**

In `charts.css`, replace the old `main > aside` sizing with the wrapper:

```css
#dest-charts { min-height: 0; }
#dest-charts > #sideWatch { flex: 0 0 var(--watch-w); }
#dest-charts #chartColumn { flex: 1 1 auto; }
#inspectorHost { display: flex; flex: 0 0 var(--inspector-w); width: var(--inspector-w); min-height: 0; }
#inspectorHost[hidden] { display: none; }
#inspectorHost > aside { width: 100%; flex: 1 1 auto; }
#inspectorHost:has(> aside.hideAside) { display: none; }
.chartToolbar { flex: 0 0 36px; }
#dock {
  display: flex; flex-direction: column; flex: 0 0 auto;
  height: var(--dock-h); min-height: 0;
  border-top: 1px solid var(--line); background: var(--bg-panel);
}
#dock[hidden] { display: none; }
.dockTabs { display: flex; align-items: center; height: 28px; flex: 0 0 28px; padding-right: var(--space-1); }
.dockTabs .hrz-tabs { flex: 1 1 auto; height: 28px; border-bottom: 0; }
.dockTabs .hrz-tab { height: 28px; }
.dockBody { flex: 1 1 auto; min-height: 0; overflow: auto; }
```

The `:has` bridge exists only until Task 9 moves the collapse class to
`#inspectorHost`; it is deleted there.

- [ ] **Step 9: Keep the page functional**

In `app.js`:

1. `applyAuthGate` — replace the `main` lookup and toggle with the frame:

```js
function applyAuthGate() {
  const signedIn = Boolean(token());
  const home = document.getElementById("home");
  const frame = document.getElementById("frame");
  const statusbar = document.getElementById("statusbar");
  if (home) home.hidden = signedIn;
  if (statusbar) statusbar.hidden = !signedIn;
  if (frame) {
    const wasHidden = frame.hidden;
    frame.hidden = !signedIn;
    if (wasHidden && signedIn) {
      for (const pane of panes) pane.redraw();
    }
  }
  // The chat hero greets the account by name, and the composer chip names the
  // model that will answer. Both are account-level, so both repaint here.
  paintAiHero();
  paintModelChip();
}
```

2. `wireSplitters` — the aside is no longer a direct child of `main`:

```js
  wireWidth(el("splitAside"), "--aside-w", 280, 860, -1,
    () => currentWidth(document.querySelector("#dest-charts aside"), "--aside-w", 460));
```

3. `main()` — call `paintIcons(document)` after the wiring block that creates
   toolbars, next to `globalTools.build()`.

Nothing else changes: every pane, tab, splitter and status writer keeps its ID.

- [ ] **Step 10: Verify**

```bash
node --check frontend/app/app.js
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
cargo test -p api-gateway --test packaging
```

Expected: all PASS. The harness never looked at `main` or `header` by element,
so the new frame is invisible to it.

Open the gateway at 1440×900 signed in. The top bar is 44px, the rail 48px, the
status bar 24px; the chart wall is exactly where it was. Both themes. Then resize
to 900px wide: the existing narrow rules still apply, because Task 17 has not
removed them yet.

- [ ] **Step 11: Commit**

```bash
git add frontend/app/index.html frontend/app/styles/shell.css \
  frontend/app/styles/charts.css frontend/app/app.js
git commit -m "feat: the workstation frame -- top bar, rail and status bar"
```

---

### Task 9: The router, the pane host, and `views/shell.js`

**Files:**
- Create: `frontend/app/views/shell.js`
- Modify: `frontend/app/index.html` (inspector markup, Studio tabs, Account profile,
  dock resize grip, load `views/shell.js`)
- Modify: `frontend/app/styles/inspector.css` (inspector head/body)
- Modify: `frontend/app/styles/shell.css` (destination tabs, profile card, drawer body)
- Modify: `frontend/app/styles/charts.css` (dock body; delete the `:has` bridge)
- Modify: `frontend/app/app.js` (`selectPane`, `runPaneLoaders`, `HRZApp`, snapshot,
  restore, splitters, save state, events, `main()`)
- Modify: `tools/shell_check.mjs` (load the UI files, pin a desktop viewport and
  the `#/charts` route)

**Interfaces:**
- Consumes: `HRZUI.drawer`/`toast`/`icon` (Task 7), `Theme.preference()`/`set()`
  (Task 2), `#frame`, `#rail`, `#mainView`, the six `#dest-*` sections,
  `#inspectorHost`, `#dock`, `#statusbar` (Task 8), and the `HRZApp` facade
  this task adds to `app.js`.
- Produces: `window.Shell` with
  `init()`, `start()`, `go(destination, opts)`, `restore(saved)`,
  `snapshot()`, `selectLegacyPane(name)`, `legacyTab()`,
  `setSaveState(state, text)`, `setPlan(name)`, `refreshStatus()`,
  `openAi()`, `openInspector(tab)`, `setInspectorOpen(open)`,
  `setWatchlistCollapsed(collapsed)`, `openDock(tab)`, `closeDock()`,
  `selectDockTab(tab)`, and `state()`.
- Produces: `window.HRZApp` with
  `signedIn()`, `email()`, `planName()`, `runPaneLoaders(name)`,
  `resizePanes()`, `activePaneInfo()`, `noteChange()`.

**Why the facade:** `app.js` owns every instrument and `views/shell.js` owns the
page. Without a stated seam the two files would read each other's globals and
the next task touching either would break the other silently. `HRZApp` is the
seam: six answers, no state handing, and the dependency runs one way
(`shell.js` → `HRZApp`).

- [ ] **Step 1: Replace the aside's tab strip with the inspector**

Replace the whole `.tabs` block inside `<aside>` (`index.html:2248-2268`,
from `<div class="tabs" role="tablist">` through its closing `</div>`) with:

```html
    <div class="inspectorHead">
      <div class="hrz-tabs" id="inspectorTabs" role="tablist" aria-label="Inspector">
        <button role="tab" type="button" class="hrz-tab" data-inspector="ai" aria-selected="true">AI</button>
        <button role="tab" type="button" class="hrz-tab" data-inspector="indicator" aria-selected="false">Indicator</button>
        <button role="tab" type="button" class="hrz-tab" data-inspector="book" aria-selected="false">Book</button>
        <button role="tab" type="button" class="hrz-tab" data-inspector="trade" aria-selected="false">Trade</button>
        <button role="tab" type="button" class="hrz-tab" data-inspector="details" aria-selected="false">Details</button>
      </div>
      <button type="button" id="asideHide" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon"
              aria-label="Hide the inspector" title="Hide inspector">
        <span data-icon="chevron-right"></span>
      </button>
    </div>
    <div class="inspectorBody" id="inspectorPaneHost">
      <div class="inspectorPanel" id="inspectorIndicator" hidden>
        <h2 class="inspectorPanel__title">Indicator</h2>
        <p class="empty" id="indicatorProjectionEmpty">No indicator is attached to the active chart.</p>
        <div id="indicatorProjection" hidden></div>
      </div>
    </div>
```

Every existing `pane-*` element stays exactly where it is in the markup. Shell
moves the four inspector panes into `#inspectorPaneHost` on init; keeping them
in place until then means no listener is ever rebound.

- [ ] **Step 2: Add the Studio tabs, the Account profile and the dock grip**

In `#dest-studio` (`index.html:2688-2691`), insert the tab strip between the
heading and `#studioHost`:

```html
      <div class="hrz-tabs destTabs" id="studioTabs" role="tablist" aria-label="Studio tools">
        <button role="tab" type="button" class="hrz-tab" data-studio="indicator" aria-selected="true">Builder</button>
        <button role="tab" type="button" class="hrz-tab" data-studio="scan" aria-selected="false">Scanner</button>
        <button role="tab" type="button" class="hrz-tab" data-studio="strategy" aria-selected="false">Strategies</button>
        <button role="tab" type="button" class="hrz-tab" data-studio="skills" aria-selected="false">Skills</button>
      </div>
```

Replace the `#accountProfile` placeholder inside `#accountHost` with:

```html
        <div id="accountProfile" data-account-panel="profile" hidden>
          <div class="profileCard">
            <h2 class="profileCard__title">Profile</h2>
            <dl class="profileCard__list">
              <div><dt>Email</dt><dd id="profileEmail">—</dd></div>
              <div><dt>Plan</dt><dd id="profilePlan">—</dd></div>
            </dl>
            <label class="profileCard__row" for="profileTheme">
              <span class="muted">Appearance</span>
              <select id="profileTheme" class="hrz-select">
                <option value="system">System</option>
                <option value="dark">Dark</option>
                <option value="light">Light</option>
              </select>
            </label>
            <button id="profileSignout" class="hrz-btn hrz-btn--secondary hrz-btn--sm" type="button">
              Sign out
            </button>
            <p class="muted">More account settings arrive in Spec 2.</p>
          </div>
        </div>
```

Add the resize grip as the first child of `#dock`, before `.dockTabs`:

```html
  <div class="dockResize" id="dockResize" role="separator" aria-orientation="horizontal"
       aria-label="Resize the dock" tabindex="0"></div>
```

- [ ] **Step 3: Load the shell module**

In `index.html`, between the `components.js` script and `builder.js`:

```html
<script src="/assets/views/shell.js"></script>
```

Order is `theme.js` (head) · `icons.js` · `components.js` · `views/shell.js` ·
`builder.js` · `app.js`. `shell.js` only defines `window.Shell`; it must run
before `app.js` so `main()` can initialise it.

- [ ] **Step 4: Write `views/shell.js`**

`frontend/app/views/shell.js`:

```js
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
    const claim = {
      open: ["Live", "live"],
      paused: ["Paused", "wait"],
      connecting: ["Connecting", "wait"],
      idle: ["Connecting", "wait"],
      nofeed: ["No feed", "wait"],
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
    for (const id of ["aiBtn", "watchlistToggle", "inspectorToggle", "dockToggle"]) {
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
    return route;
  }

  function hashFor(destination, opts = {}) {
    if (destination === "account") {
      return `#/account/${opts.section || state.accountSection || "plan"}`;
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
    if (load) runDestinationLoaders();
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
    setDestination(route.destination, { section: route.section });
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
      setDestination(route.destination, { section: route.section });
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
    syncChrome();
    if (!signedIn || !started) return;
    if (!state.destination) {
      const route = parse(location.hash);
      setDestination(route ? route.destination : "dashboard", { section: route && route.section });
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
    state: () => ({ ...state }),
  };
})();
```

Note on `inspectorWidth`: spec §13.2's example lists only `watchlistWidth`, but
§7.5 says all widths and heights persist. The server treats the snapshot as
opaque and `inspectorWidth` is optional, so the field costs nothing and keeps
the inspector drag from resetting on every refresh. Missing values fall back to
the 360px default.

- [ ] **Step 5: Add the styles**

In `frontend/app/styles/inspector.css`:

```css
/* ---- Inspector ----------------------------------------------------------- */
#inspector { display: flex; flex-direction: column; min-height: 0; }
#inspector > .pane { display: none; } /* moved into #inspectorPaneHost on init */
.inspectorHead {
  display: flex; align-items: center; gap: var(--space-1);
  flex: 0 0 auto; padding: 0 var(--space-2);
  border-bottom: 1px solid var(--line);
}
.inspectorHead .hrz-tabs {
  flex: 1 1 auto; min-width: 0; overflow-x: auto;
  border-bottom: 0; height: 36px;
}
.inspectorHead .hrz-tab { height: 36px; }
.inspectorBody { flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; }
.inspectorBody > .pane { flex: 1 1 auto; min-height: 0; overflow: auto; }
.inspectorPanel {
  flex: 1 1 auto; min-height: 0; overflow: auto;
  padding: var(--space-3);
  font-size: var(--text-xs); color: var(--text-2);
}
.inspectorPanel__title {
  margin: 0 0 var(--space-2);
  font: 600 var(--text-xs)/1 var(--font-ui);
  letter-spacing: 0.06em; text-transform: uppercase; color: var(--text-3);
}
```

In `frontend/app/styles/shell.css`, after the destination block:

```css
.destTabs { margin: 0 var(--space-6) var(--space-3); flex: 0 0 auto; }
.destHost { display: flex; flex-direction: column; }
.destHost > .pane { flex: 1 1 auto; min-height: 0; overflow: auto; }
#accountProfile { max-width: 640px; }
.profileCard {
  display: flex; flex-direction: column; gap: var(--space-3);
  padding: var(--space-4);
  background: var(--bg-panel); border: 1px solid var(--line);
  border-radius: var(--radius-2);
}
.profileCard__title { margin: 0; font: 600 var(--text-sm)/1 var(--font-ui); }
.profileCard__list { display: grid; gap: var(--space-2); margin: 0; }
.profileCard__list > div { display: grid; grid-template-columns: 96px 1fr; gap: var(--space-2); }
.profileCard__list dt { color: var(--text-3); font-size: var(--text-xs); }
.profileCard__list dd { margin: 0; font-family: var(--font-mono); font-size: var(--text-xs); }
.profileCard__row { display: flex; align-items: center; gap: var(--space-2); }
.aiDrawerBody { height: 100%; display: flex; flex-direction: column; }
.aiDrawerBody > .pane { flex: 1 1 auto; min-height: 0; overflow: auto; }
```

In `frontend/app/styles/charts.css`, replace the `#inspectorHost:has(…` bridge
with the dock body rules:

```css
#dockBody { display: flex; flex-direction: column; overflow: hidden; }
#dockBody > .pane { flex: 1 1 auto; min-height: 0; overflow: auto; }
.dockResize {
  flex: 0 0 5px; cursor: ns-resize;
  background: transparent;
}
.dockResize:hover, .dockResize:focus-visible { background: var(--accent-line); }
.dockPlaceholder { padding: var(--space-3); color: var(--text-3); font-size: var(--text-xs); }
```

- [ ] **Step 6: Integrate `app.js`**

1. Replace `selectPane` (`app.js:13359-13391`) with the loader split and the
   delegation:

```js
/// Read a pane's data when it is opened. Panels that can be changed from
/// elsewhere (the API, another tab) read on open rather than cache at boot, so
/// a value edited somewhere else is never shown stale.
function runPaneLoaders(name) {
  if (name === "bots") refreshVenues();
  if (name === "indicator") loadWorkspaces();
  if (name === "brokers") { refreshBrokers(); loadBrokerCatalog(); }
  if (name === "aimodel") refreshAiModel();
  if (name === "billing") loadBilling();
  if (name === "community") loadCommunity();
  if (name === "skills") loadSkills();
  if (name === "strategy") loadLibrary();
  if (name === "watchlist") {
    renderWatchlist();
    loadWatchlistUniverse();
    startWatchlistTicker();
  } else {
    stopWatchlistTicker();
  }
}

/// The old name every caller still uses. It names a panel, not a destination;
/// the shell owns the translation (`Shell.selectLegacyPane`).
function selectPane(name) {
  activeTabName = name;
  scheduleWorkspaceSave();
  if (window.Shell) Shell.selectLegacyPane(name);
}
```

2. `applyAuthGate` — dispatch the change after the paint calls:

```js
  paintAiHero();
  paintModelChip();
  document.dispatchEvent(new CustomEvent("hrz:auth", { detail: { signedIn } }));
```

3. `setActive` — tell the shell the active instrument changed, before the save
   is scheduled:

```js
  refreshGlobalTools();
  syncIndicatorChip();
  document.dispatchEvent(new CustomEvent("hrz:pane", { detail: { pane } }));
  scheduleWorkspaceSave();
```

4. `setActive` and `restoreWorkspace` are the two places the active pane moves.
   At the end of `restoreWorkspace`, before `workspaceReady = true`, add:

```js
  document.dispatchEvent(new CustomEvent("hrz:pane", { detail: { pane: activePane } }));
```

5. `workspaceSnapshot` — merge the shell's keys and derive `activeTab` from the
   inspector tab, so an older reader still sees a legacy name:

```js
function workspaceSnapshot() {
  const shell = window.Shell ? Shell.snapshot() : {};
  return {
    version: 1,
    ...shell,
    layout: { mode: layoutMode, columns: gridColumns },
    activePane: Math.max(0, panes.indexOf(activePane)),
    activeTab: window.Shell ? Shell.legacyTab() : activeTabName,
    indicatorWorkspaceId: wsActiveId,
    panes: panes.map((pane) => pane.snapshot()),
  };
}
```

6. `restoreWorkspace` — restore the shell before the pane work, and keep the
   legacy `selectPane` call (Shell swallows that echo):

```js
async function restoreWorkspace(saved, entries) {
  if (!saved || !Array.isArray(saved.panes) || !saved.panes.length) {
    return false;
  }
  if (window.Shell) Shell.restore(saved);
  applyWorkspaceLayout(saved);
  // … unchanged from here; the final `selectPane(saved.activeTab)` stays …
```

7. Save state — three edits:

```js
function scheduleWorkspaceSave() {
  if (!workspaceReady || !token()) return;
  if (workspaceSaveTimer) return;
  if (window.Shell) Shell.setSaveState("saving", "Unsaved");
  workspaceSaveTimer = setTimeout(() => {
    workspaceSaveTimer = 0;
    saveWorkspaceState();
  }, 800);
}

async function saveWorkspaceState() {
  if (!workspaceReady || !token()) return;
  const snapshot = workspaceSnapshot();
  const encoded = JSON.stringify(snapshot);
  if (encoded === workspaceSavedJSON) return;
  if (window.Shell) Shell.setSaveState("saving", "Saving…");
  try {
    await api("/workspace", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ state: snapshot }),
    });
    workspaceSavedJSON = encoded;
    if (window.Shell) Shell.setSaveState("saved", "Saved");
  } catch {
    // A lost save is not worth a toast mid-drag; the next change retries.
    if (window.Shell) Shell.setSaveState("failed", "Save failed");
  }
}
```

8. `wireSplitters` — widths live in the snapshot now, and the inspector uses
   `--inspector-w` with the spec's 300–480 range. Delete the localStorage
   restore block and the `persist` function; in `wireWidth`'s `stop`, replace
   `persist();` with `noteWorkspaceChange();`. Then:

```js
  wireWidth(el("splitWatch"), "--watch-w", 200, 360, +1,
    () => currentWidth(el("sideWatch"), "--watch-w", 260));
  wireWidth(el("splitAside"), "--inspector-w", 300, 480, -1,
    () => currentWidth(document.querySelector("#dest-charts aside"), "--inspector-w", 360));
```

9. `refreshBillingPane` — mirror the plan into the status bar:

```js
function refreshBillingPane() {
  paintBillingModeNote();
  paintBillingCurrent();
  paintBillingPlans();
  const plan = billingState && billingState.plan ? String(billingState.plan) : "";
  if (window.Shell) {
    Shell.setPlan(plan ? plan.charAt(0).toUpperCase() + plan.slice(1) : "");
  }
}
```

10. `main()` — three removals and two additions:

- First line: `if (window.Shell) Shell.init();` before `paintSession();`.
- Delete the `document.querySelectorAll(".tabs button")` block (`13404-13406`):
  its buttons no longer exist, and Shell owns tab clicks.
- Delete both `selectPane("chat");` statements (`13425` and `13777`): the shell
  decides the opening destination, and a boot-time call would override the
  dashboard default.
- Delete the whole rail/aside collapse block (`13720-13775`, from
  `const rail = document.getElementById("sideWatch");` through the
  `railShow.addEventListener` handler). Shell wires `#railHide`, `#railShow`,
  `#asideHide` and `#asideShow`, and restores collapse state from the snapshot
  instead of `localStorage`.
- At the end of `main()`, after `connectBook();`, add
  `if (window.Shell) Shell.start();`. In the `loadEngine` catch, add the same
  call immediately before `return;` so an engine failure still leaves a working
  frame.

11. Facade — replace the dead `origSelectPane` line (`15227`) with `HRZApp`,
   immediately before `main();`:

```js
/// The shell's window into the app. Six answers, no state handing: everything
/// else the shell needs is in the DOM, and everything here is a question about
/// the instruments the app owns.
window.HRZApp = {
  signedIn: () => Boolean(token()),
  email() {
    const value = token();
    if (!value) return "";
    try {
      const payload = JSON.parse(atob(value.split(".")[1] || ""));
      return payload.sub || payload.email || "";
    } catch {
      return "";
    }
  },
  planName() {
    const raw = billingState && billingState.plan ? String(billingState.plan) : "";
    return raw ? raw.charAt(0).toUpperCase() + raw.slice(1) : "";
  },
  runPaneLoaders,
  resizePanes: () => { for (const pane of panes) pane.redraw(); },
  activePaneInfo() {
    if (!activePane) return null;
    const badge = activePane.root.querySelector(".feedStatus");
    return {
      symbol: activePane.symbol ? activePane.symbol() : "",
      timeframe: activePane.timeframe ? activePane.timeframe() : "",
      feed: badge ? badge.dataset.state || "" : "",
    };
  },
  noteChange: () => noteWorkspaceChange(),
};

main();
```

- [ ] **Step 7: Teach the harness to boot the real UI**

`tools/shell_check.mjs` currently injects only `app.js`. Add the UI files and
pin the viewport and route before injection. Replace:

```js
const script = document.createElement("script");
script.textContent = shell;
document.head.appendChild(script);
```

with:

```js
// The shell is three files plus the app. They are evaluated in the same order
// index.html loads them, because `views/shell.js` calls `HRZUI.drawer` and
// `app.js` calls `Shell.init` -- a missing file is a boot failure, not a
// skipped feature.
for (const file of [
  "frontend/app/ui/theme.js",
  "frontend/app/ui/icons.js",
  "frontend/app/ui/components.js",
  "frontend/app/views/shell.js",
]) {
  const ui = document.createElement("script");
  ui.textContent = await readFile(join(root, file), "utf8");
  document.head.appendChild(ui);
}

// jsdom reports every media query as false. The shell's defaults (watchlist
// and inspector open) hang off min-width queries, so pin a 1440px viewport.
window.matchMedia = (query) => {
  const min = /min-width:\s*(\d+)px/.exec(query);
  return {
    matches: min ? 1440 >= Number(min[1]) : false,
    media: query,
    onchange: null,
    addEventListener() {}, removeEventListener() {},
    addListener() {}, removeListener() {},
    dispatchEvent() { return false; },
  };
};

// These checks are about the chart wall and the panels beside it. A fresh
// account has no snapshot, so without a route the shell opens on Dashboard
// and every chart pane is behind a hidden section.
window.history.replaceState(null, "", "#/charts");

const script = document.createElement("script");
script.textContent = shell;
document.head.appendChild(script);
```

The rest of the harness is unchanged: `window.selectPane("scan")` now routes to
Studio, and `pane-scan.hidden === false` is still the right assertion.

- [ ] **Step 8: Run it**

```bash
node --check frontend/app/views/shell.js
node --check frontend/app/app.js
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
node tools/component_check.mjs
cargo test -p api-gateway --test packaging
```

Expected: every check passes. If a page error mentions `HRZUI is not defined`,
the UI files are evaluated after `app.js`; if `Shell.init` is undefined, the
script tag in `index.html` is missing.

Open the gateway at 1440×900 signed in:

1. The URL becomes `#/dashboard`; the rail shows Dashboard as current.
2. Click Charts: the wall, watchlist and inspector all appear; the inspector
   opens on AI with the conversation where it was.
3. Click Book, then Details: the same nodes move, not copies — a message sent
   in AI is still in the transcript after cycling tabs.
4. AI button from Studio: the drawer opens with the same conversation; Esc
   returns it to the inspector.
5. Studio: Builder, Scanner, Strategies and Skills all mount full width; the
   builder in the dock is the same node.
6. Toggle the watchlist, inspector and dock; drag each width; refresh. All four
   states return.
7. Sign out: the home page, no frame. Sign in: the saved destination returns.

- [ ] **Step 9: Commit**

```bash
git add frontend/app/views/shell.js frontend/app/index.html \
  frontend/app/styles frontend/app/app.js tools/shell_check.mjs
git commit -m "feat: a hash router, six destinations, and one host for every pane"
```

---

### Task 10: The command palette and the shortcut sheet

**Files:**
- Modify: `frontend/app/index.html` (the two dialogs, the palette script tag)
- Create: `frontend/app/views/palette.js`
- Modify: `frontend/app/views/shell.js` (enable the two triggers, `#/studio/<id>`,
  `Shell.destinations`, a drawer that closes on sign-out)
- Modify: `frontend/app/styles/shell.css` (palette, shortcut sheet)
- Modify: `frontend/app/app.js` (`HRZApp`: seven more answers)
- Modify: `tools/shell_check.mjs` (load the file, list the workspace API, drive
  the palette)

**Interfaces:**
- Consumes: `Shell.go`/`state`/`destinations` (this task), `HRZApp`,
  `HRZUI.toast`, `Theme.current()`/`set()`, `#paletteBtn`, `#railHelp`.
- Produces: `window.HRZPalette` with `open()`, `shortcuts()` and `close()`;
  the routes `#/studio/<id>`; `HRZApp.newPane()`, `newWorkspace()`,
  `signOut()`, `workspaces()`, `openWorkspace(id)`, `searchSymbols(q)`,
  `chartSymbol(symbol)`.

**Why a second view file:** the palette is not the frame. It can be opened
from any destination, it is the only place in the product where typing and
arrow keys mean something other than chart gestures, and it owns its own
async lifecycle (a debounced search whose answers may arrive after it closed).
Folding that into `shell.js` would make the router file the place where
keyboard conflicts are resolved, which is exactly the tangle the facade was
built to avoid.

- [ ] **Step 1: Add the two dialogs**

In `frontend/app/index.html`, directly after `<div id="overlays"></div>` and
before `<script src="builder.js">`:

```html
<!--
  The command palette. A `dialog` element, not a div, because the platform
  already knows how to make one modal: focus is trapped, the rest of the page
  is inert to the pointer, and Escape is a real event rather than a key handler
  competing with the chart's own Escape. The listbox is the results column; the
  field is the combobox that owns it.
-->
<dialog id="palette" class="palette" aria-labelledby="paletteTitle">
  <h2 id="paletteTitle" class="sr-only">Search or jump to</h2>
  <div class="paletteField">
    <span class="paletteField__icon" data-icon="search" aria-hidden="true"></span>
    <input id="paletteInput" class="paletteInput" type="text" role="combobox"
           aria-expanded="true" aria-controls="paletteList" aria-autocomplete="list"
           aria-describedby="paletteFoot" autocomplete="off" spellcheck="false"
           placeholder="Search symbols, chats and actions…" />
    <button type="button" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon"
            data-close aria-label="Close search">×</button>
  </div>
  <div id="paletteList" class="paletteList" role="listbox" aria-label="Results"></div>
  <p id="paletteEmpty" class="paletteEmpty" hidden></p>
  <p id="paletteStatus" class="sr-only" role="status" aria-live="polite"></p>
  <p id="paletteFoot" class="paletteFoot">
    <span><kbd>↑</kbd><kbd>↓</kbd> to move</span>
    <span><kbd>↵</kbd> to open</span>
    <span><kbd>esc</kbd> to close</span>
  </p>
</dialog>

<dialog id="shortcuts" class="shortcuts" aria-labelledby="shortcutsTitle">
  <div class="shortcutsHead">
    <h2 id="shortcutsTitle">Keyboard shortcuts</h2>
    <button type="button" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon"
            data-close aria-label="Close shortcuts">×</button>
  </div>
  <div class="shortcutsGrid">
    <section class="shortcutGroup" aria-labelledby="shortcutNav">
      <h3 id="shortcutNav">Navigation</h3>
      <dl>
        <div><dt>Search or jump</dt><dd><kbd data-mod>⌘</kbd><kbd>K</kbd></dd></div>
        <div><dt>Shortcut sheet</dt><dd><kbd data-mod>⌘</kbd><kbd>/</kbd></dd></div>
        <div><dt>Dashboard to Account</dt><dd><kbd>Alt</kbd><kbd>1</kbd>–<kbd>6</kbd></dd></div>
        <div><dt>Previous / next destination</dt><dd><kbd>Alt</kbd><kbd>←</kbd><kbd>→</kbd></dd></div>
        <div><dt>Close the top overlay</dt><dd><kbd>Esc</kbd></dd></div>
      </dl>
    </section>
    <section class="shortcutGroup" aria-labelledby="shortcutChart">
      <h3 id="shortcutChart">Chart</h3>
      <dl>
        <div><dt>Pan one bar</dt><dd><kbd>←</kbd><kbd>→</kbd></dd></div>
        <div><dt>Zoom</dt><dd><kbd>+</kbd><kbd>−</kbd></dd></div>
        <div><dt>Fit the series</dt><dd><kbd>Alt</kbd><kbd>R</kbd></dd></div>
        <div><dt>Delete the selected drawing</dt><dd><kbd>Delete</kbd></dd></div>
        <div><dt>Undo / redo</dt><dd><kbd data-mod>⌘</kbd><kbd>Z</kbd> <span class="muted">/</span> <kbd data-mod>⌘</kbd><kbd>⇧</kbd><kbd>Z</kbd></dd></div>
        <div><dt>Cancel the drawing tool</dt><dd><kbd>Esc</kbd></dd></div>
      </dl>
    </section>
  </div>
  <p class="shortcutsFoot">Shortcuts are ignored while you are typing in a field.</p>
</dialog>
```

Both live outside `#frame`: the sheet is readable before sign-in, and a dialog
inside a hidden section cannot be shown modally.

- [ ] **Step 2: Load the palette**

In `frontend/app/index.html`, add the script between the shell and the builder:

```html
<script src="/assets/views/shell.js"></script>
<script src="/assets/views/palette.js"></script>
<script src="builder.js"></script>
<script src="app.js"></script>
```

The palette binds its listeners on evaluation and reaches `HRZApp` only inside
those listeners, so it can load before `app.js` defines the facade.

- [ ] **Step 3: Write `frontend/app/views/palette.js`**

```js
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
    // result is off screen.
    if (state.destination === "charts") {
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
    const ids = destinations().map((destination) => destination.id);
    if (!ids.length || !shell()) return;
    const current = (shell().state() || {}).destination || "dashboard";
    const index = Math.max(0, ids.indexOf(current));
    shell().go(ids[(index + step + ids.length) % ids.length]);
  }

  function onKeydown(event) {
    if (event.defaultPrevented || event.isComposing) return;
    // Any open dialog owns the keyboard. The palette's own handlers stop
    // propagation at the dialog; this guard covers the gap before they do.
    if (document.querySelector("dialog[open]")) return;

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
    const help = document.getElementById("railHelp");
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
```

- [ ] **Step 4: Enable the triggers and the deep link in `shell.js`**

1. Enable the two controls. In `syncChrome`, replace the id list:

```js
    for (const id of ["aiBtn", "watchlistToggle", "inspectorToggle", "dockToggle"]) {
```

with:

```js
    for (const id of [
      "aiBtn",
      "paletteBtn",
      "watchlistToggle",
      "inspectorToggle",
      "dockToggle",
      "railHelp",
    ]) {
```

2. Export the destination order so the palette does not keep a second copy:

```js
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
```

3. Parse the workspace id. In `parse`:

```js
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
```

4. Write it back in `hashFor`:

```js
  function hashFor(destination, opts = {}) {
    if (destination === "account") {
      return `#/account/${opts.section || state.accountSection || "plan"}`;
    }
    if (destination === "studio" && opts.workspaceId) {
      return `#/studio/${encodeURIComponent(opts.workspaceId)}`;
    }
    return `#/${destination}`;
  }
```

5. Open the workspace. In `setDestination`, after the studio-pane block:

```js
    if (destination === "studio" && opts.workspaceId) {
      // A chat lives in the builder. A link to one is a link to that tab.
      state.studioPane = "indicator";
      load = true;
    }
```

and replace the tail of the function:

```js
    showDestination();
    if (load) runDestinationLoaders();
    note();
  }
```

with:

```js
    showDestination();
    if (load) runDestinationLoaders();
    if (destination === "studio" && opts.workspaceId && app().openWorkspace) {
      app().openWorkspace(String(opts.workspaceId));
    }
    note();
  }
```

6. Pass the id through all three route entries. In `onHash`:

```js
    setDestination(route.destination, {
      section: route.section,
      workspaceId: route.workspaceId,
    });
```

In `start`:

```js
      setDestination(route.destination, {
        section: route.section,
        workspaceId: route.workspaceId,
      });
```

In `onAuth`:

```js
      setDestination(route ? route.destination : "dashboard", {
        section: route && route.section,
        workspaceId: route && route.workspaceId,
      });
```

7. Close the AI drawer when the session ends. At the top of `onAuth`:

```js
  function onAuth(signedIn) {
    // A drawer belongs to the signed-in frame. Left open, it would hang over
    // the home page owned by a session that is gone.
    if (!signedIn && drawer) drawer.close();
    syncChrome();
    if (!signedIn || !started) return;
```

- [ ] **Step 5: Extend the app facade**

In `app.js`, add these members to the `HRZApp` object immediately after
`noteChange`:

```js
  // --- the palette's questions (Task 10) -----------------------------------

  newPane: () => addPane(),

  /// The builder creates a chat from its own name field, so this puts the
  /// caret where the user has to type rather than inventing a name for them.
  newWorkspace() {
    const name = el("wsName");
    if (name) name.focus();
  },

  signOut: () => setToken(""),

  workspaces: () => api("/indicator-workspaces"),

  /// Deep links land here. Setting the pending id and reloading is how a
  /// restored snapshot already reopens a chat, so a route reuses that path
  /// instead of growing a second way to select a workspace.
  openWorkspace(id) {
    pendingIndicatorWorkspaceId = id;
    return loadWorkspaces();
  },

  searchSymbols(query) {
    return api(`/symbols/search?q=${encodeURIComponent(query)}&limit=25`).then(
      (response) => (response && response.results) || []
    );
  },

  /// Chart a symbol, reusing the pane that already shows it. On a wall of a
  /// hundred the user means "show me this", not "add another copy".
  chartSymbol(symbol) {
    if (!symbol) return;
    const existing = panes.find((pane) => pane.symbol && pane.symbol() === symbol);
    if (existing) {
      setActive(existing);
      return;
    }
    chartFromWatchlist(symbol);
  },
```

- [ ] **Step 6: Add the styles**

In `frontend/app/styles/shell.css`, append:

```css
/* ---- Command palette ------------------------------------------------------ */
.palette,
.shortcuts {
  padding: 0;
  border: 1px solid var(--line-strong);
  border-radius: var(--radius-l);
  background: var(--bg-overlay);
  color: var(--text-1);
  box-shadow: var(--shadow-2);
}
.palette { width: min(560px, calc(100vw - 32px)); margin: 12vh auto auto; }
.shortcuts { width: min(720px, calc(100vw - 32px)); margin: 12vh auto auto; }
.palette::backdrop,
.shortcuts::backdrop { background: var(--scrim); }

.paletteField {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  height: 52px;
  padding: 0 var(--space-3);
  border-bottom: 1px solid var(--line);
}
.paletteField__icon { display: inline-flex; color: var(--text-3); }
.paletteInput {
  flex: 1;
  min-width: 0;
  height: 100%;
  border: 0;
  background: transparent;
  color: var(--text-1);
  font: 400 var(--text-base)/1 var(--font-ui);
  outline: none;
}
.paletteInput::placeholder { color: var(--text-3); }

.paletteList {
  max-height: min(56vh, 420px);
  overflow-y: auto;
  padding: var(--space-1);
}
.paletteGroup + .paletteGroup {
  margin-top: var(--space-1);
  padding-top: var(--space-1);
  border-top: 1px solid var(--line);
}
.paletteGroup__title {
  padding: 6px 10px 4px;
  font: 600 var(--text-2xs)/1 var(--font-ui);
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--text-3);
}
.paletteOpt {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  min-height: 34px;
  padding: 0 10px;
  border-radius: var(--radius-1);
  cursor: default;
}
.paletteOpt[aria-selected="true"] {
  background: var(--accent-soft);
  box-shadow: inset 2px 0 var(--accent);
}
.paletteOpt__label {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-sm);
}
.paletteOpt__hint {
  color: var(--text-3);
  font: var(--text-2xs)/1 var(--font-mono);
  white-space: nowrap;
}
.paletteEmpty {
  margin: 0;
  padding: var(--space-4) var(--space-3);
  color: var(--text-3);
  font-size: var(--text-sm);
}
.paletteFoot {
  display: flex;
  gap: var(--space-3);
  margin: 0;
  padding: var(--space-2) var(--space-3);
  border-top: 1px solid var(--line);
  color: var(--text-3);
  font-size: var(--text-2xs);
}
.palette kbd,
.shortcuts kbd {
  display: inline-block;
  min-width: 16px;
  padding: 1px 4px;
  border: 1px solid var(--line-strong);
  border-radius: 3px;
  background: var(--bg-raised);
  color: var(--text-2);
  font: var(--text-2xs)/14px var(--font-mono);
  text-align: center;
}

/* ---- Shortcut sheet ------------------------------------------------------- */
.shortcutsHead {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--space-3) var(--space-4);
  border-bottom: 1px solid var(--line);
}
.shortcutsHead h2 { margin: 0; font: 600 var(--text-md)/24px var(--font-ui); }
.shortcutsGrid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: var(--space-4);
  padding: var(--space-4);
}
.shortcutGroup h3 {
  margin: 0 0 var(--space-2);
  font: 600 var(--text-2xs)/1 var(--font-ui);
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--text-3);
}
.shortcutGroup dl { display: grid; gap: var(--space-2); margin: 0; }
.shortcutGroup dl > div { display: flex; align-items: center; gap: var(--space-3); }
.shortcutGroup dt { flex: 1; font-size: var(--text-sm); color: var(--text-2); }
.shortcutGroup dd { display: flex; align-items: center; gap: 4px; margin: 0; }
.shortcutsFoot {
  margin: 0;
  padding: 0 var(--space-4) var(--space-3);
  color: var(--text-3);
  font-size: var(--text-2xs);
}

@media (max-width: 720px) {
  .palette, .shortcuts { margin-top: 8vh; }
  .shortcutsGrid { grid-template-columns: 1fr; }
}
```

- [ ] **Step 7: Teach the harness**

In `tools/shell_check.mjs`:

1. Add the file to the boot list, after `views/shell.js`:

```js
  "frontend/app/views/shell.js",
  "frontend/app/views/palette.js",
```

2. Give the workspace API an answer. Before the catch-all in `window.fetch`
   (the comment beginning `// Anything else the shell asks for`):

```js
  if (url.startsWith("/indicator-workspaces")) {
    return json(200, [
      { id: "ws-liquidity", name: "Liquidity sweep", symbol: "BTCUSDT", timeframe: "15m" },
    ]);
  }
```

3. Insert this block immediately before the `// --- nothing threw` banner:

```js
// --- the command palette -----------------------------------------------------
//
// The palette is the shell's keyboard door: the only way to reach a symbol or
// a saved chat without walking the frame. It is driven here the way a keyboard
// drives it (a document-level event, a typed query, Enter), because a click
// would pass even if the key handler were never bound.

console.log("\nthe command palette");

document.dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "k",
  metaKey: true,
  bubbles: true,
  cancelable: true,
}));

const palette = document.getElementById("palette");
check(
  "the palette opens on its shortcut",
  Boolean(palette && palette.hasAttribute("open")),
  palette ? `open=${palette.hasAttribute("open")}` : "no #palette"
);

const paletteTitles = () =>
  [...document.querySelectorAll("#paletteList .paletteGroup__title")].map((node) => node.textContent);
const openingTitles = paletteTitles();
check(
  "and opens as a launcher, not a search",
  openingTitles.join(",") === "Navigate,Actions",
  openingTitles.join(", ") || "(none)"
);
check(
  "with the first destination selected",
  (document.querySelector('#paletteList [role="option"][aria-selected="true"]')?.textContent || "")
    .includes("Dashboard"),
  document.querySelector('#paletteList [role="option"][aria-selected="true"]')?.textContent || "(none)"
);
check(
  "the count is announced",
  (document.getElementById("paletteStatus").textContent || "").endsWith("commands"),
  document.getElementById("paletteStatus").textContent
);

const paletteInput = document.getElementById("paletteInput");
paletteInput.value = "liquidity";
paletteInput.dispatchEvent(new window.Event("input", { bubbles: true }));
await settle();

const filteredTitles = paletteTitles();
check(
  "typing filters and offers the user's saved chats",
  filteredTitles.includes("Chats"),
  filteredTitles.join(", ") || "(none)"
);

const chatOption = [...document.querySelectorAll('#paletteList [role="option"]')]
  .find((node) => node.textContent.includes("Liquidity sweep"));
check(
  "with the workspace the API listed",
  Boolean(chatOption),
  chatOption ? chatOption.textContent : "(none)"
);
if (chatOption) {
  chatOption.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  await settle();
  check(
    "and selecting one deep-links into Studio",
    location.hash === "#/studio/ws-liquidity",
    location.hash
  );
}

document.dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "/",
  metaKey: true,
  bubbles: true,
  cancelable: true,
}));
const sheet = document.getElementById("shortcuts");
check(
  "the shortcut sheet opens on its shortcut",
  Boolean(sheet && sheet.hasAttribute("open")),
  sheet ? `open=${sheet.hasAttribute("open")}` : "no #shortcuts"
);
const sheetClose = sheet && sheet.querySelector("[data-close]");
if (sheetClose) sheetClose.click();
check(
  "and closes from its own button",
  Boolean(sheet) && !sheet.hasAttribute("open"),
  sheet ? `open=${sheet.hasAttribute("open")}` : "no #shortcuts"
);

document.dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "1",
  code: "Digit1",
  altKey: true,
  bubbles: true,
  cancelable: true,
}));
check("Alt+1 goes to Dashboard", location.hash === "#/dashboard", location.hash);

document.dispatchEvent(new window.KeyboardEvent("keydown", {
  key: "2",
  code: "Digit2",
  altKey: true,
  bubbles: true,
  cancelable: true,
}));
check("and Alt+2 comes back to Charts", location.hash === "#/charts", location.hash);
```

- [ ] **Step 8: Run it**

```bash
node --check frontend/app/views/palette.js
node --check frontend/app/views/shell.js
node --check frontend/app/app.js
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
cargo test -p api-gateway --test packaging
```

Expected: all pass. If the palette opens but the list is empty, `wire()` did
not find `#paletteList` -- check the dialog was placed outside `#frame`.

Open the gateway at 1440×900 signed in and check by hand:

1. `⌘K` opens with Dashboard selected; `↓` then `↵` lands on Charts.
2. Type `btc`: a Symbols section appears; `↵` charts the first match on the
   active pane, not a new one.
3. Type part of a saved chat's name: the Chats section appears; `↵` lands on
   `#/studio/<id>` with that chat open in the Builder.
4. `⌘/` and `?` open the sheet; `Esc` closes it and returns focus to where it
   was. Typing `?` in the builder's name field inserts a question mark.
5. `Alt+1`…`Alt+6` jump; `Alt+←`/`Alt+→` cycle. With a drawing tool active,
   `Alt+←` navigates instead of panning the chart.
6. With VoiceOver on, the result count is read after each keystroke, and the
   highlighted option is announced as the selection moves.
7. Signing out from the palette closes it and any open AI drawer.

- [ ] **Step 9: Commit**

```bash
git add frontend/app/views/palette.js frontend/app/views/shell.js \
  frontend/app/index.html frontend/app/styles/shell.css \
  frontend/app/app.js tools/shell_check.mjs
git commit -m "feat: a command palette and a shortcut sheet for the whole shell"
```

---

## M4 — The charts workspace

Charts is the destination the platform exists for, and M4 is where it stops
being "the old page with a new frame" and becomes the workspace the spec
describes: one 40px instrument bar that projects the active chart, a watchlist
rail and an inspector that are always about that chart, a bottom dock that can
share the wall's height, and a chart theme that follows the app's theme.

The rule every task in M4 keeps is the spec's §7.3 sentence: **the bar is a
projection, not a second source of truth.** Each pane already owns its controls
-- symbol, timeframe, bar count, chart type, zones, Fit, reload -- hidden in
`.chartBar` and hosted by the right-click menu. M4 does not move them out and it
does not clone them: it adds named commands inside the pane and reads the
state back through one facade call. There is one implementation of "set the
symbol" and the bar, the menu and a restored snapshot all call it.

---

### Task 11: An instrument bar that projects the active chart

**Files:**
- Create: `frontend/app/views/instrument_bar.js`
- Modify: `frontend/app/index.html` (replace `#chartToolbar`, add five panels, load the script)
- Modify: `frontend/app/app.js` (`apply*` commands, `notifyPane`, the pane API,
  `attachWorkspaceRevision`, `ensureCoverage`, `activePaneInfo`, `HRZApp`, `main()`)
- Modify: `frontend/app/views/shell.js` (`aria-pressed` on the two panel toggles)
- Modify: `frontend/app/styles/tokens.css` (`--bar-h`)
- Modify: `frontend/app/styles/charts.css` (bar, panels; delete the 36px toolbar rule)
- Modify: `frontend/app/styles/shell.css` (delete the `.chartToolbar` block)
- Modify: `tools/shell_check.mjs` (boot the file, mock `/symbols/search`, drive the bar)

**Interfaces:**
- Consumes: `HRZApp.activePaneInfo()`, `HRZApp.searchSymbols()`,
  `HRZApp.workspaces()`, `HRZApp.chartSymbol()` (Task 10); `Shell.go()`,
  `Shell.state()` (Task 9); `HRZUI.icon`/`toast` (Task 7); `HRZPalette.shortcuts()`
  (Task 10); `#chartColumn`, `#charts`, `#globalTools`, `#split`, `#layoutMode`,
  `#chartCols`, `#watchlistToggle`, `#inspectorToggle`, `#dockToggle` (Task 8).
- Produces: `window.InstrumentBar` with `init()`, `sync()`, `measure()`, `close()`.
- Produces: pane API additions `mode()`, `bars()`, `zones()`, `maximized()`,
  `frames()`, `modes()`, `setSymbol()`, `setTimeframe()`, `setBars()`,
  `setMode()`, `setZones()`, `fit()`, `reload()`, `toggleMaximize()`.
- Produces: `HRZApp.setActiveSymbol()`, `setActiveTimeframe()`, `setActiveBars()`,
  `setActiveMode()`, `setActiveZones()`, `fitActivePane()`, `reloadActivePane()`,
  `toggleMaximize()`, `detachActiveIndicator()`, `setLayerVisible()`,
  `removeLayer()`, `attachWorkspace()`, `saveScreenshot()`.
- Produces: `hrz:pane` fired by every series path, not only by activation.

**Why the commands live in the pane:** the alternative is a bar that calls the
hidden select's `dispatchEvent(new Event("change"))`. It works until the first
listener reads `event.target` or the bar wants to set a value the select cannot
hold, and then the state and the bar disagree with no way to tell which is
right. A named `applyTimeframe(value)` is one call that sets the control, resets
the right things and announces the change; the listener and the bar both call
it, so they cannot diverge.

- [ ] **Step 1: Replace the toolbar with the instrument bar**

Replace the whole `#chartToolbar` block from Task 8 (everything from
`<div id="chartToolbar" class="chartToolbar">` through its closing `</div>`,
immediately before `#globalTools`) with:

```html
<div id="instrumentBar" class="instrumentBar" role="toolbar" aria-label="Active chart controls" data-state="empty" data-width="wide">
  <div class="barGroup" data-bar-group="instrument">
    <button id="barSymbol" class="barSymbol" type="button" aria-haspopup="true"
            aria-expanded="false" aria-controls="symbolPop"
            title="Search the venue for an instrument">
      <span class="barSymbolName" id="barSymbolName">—</span>
      <span data-icon="chevron-down"></span>
    </button>
    <div class="hrz-seg hrz-seg--sm" id="barFrames" role="group" aria-label="Timeframe"></div>
    <button id="barFrameMore" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon" type="button"
            aria-haspopup="true" aria-expanded="false" aria-controls="framesPop"
            aria-label="All timeframes" title="All timeframes">
      <span data-icon="chevron-down"></span>
    </button>
    <select id="barBars" class="hrz-select" aria-label="How many bars to load" title="How many bars to load">
      <option value="200">200</option>
      <option value="500">500</option>
      <option value="1000">1000</option>
    </select>
  </div>
  <div class="barGroup" data-bar-group="view">
    <div class="hrz-seg hrz-seg--sm" id="barModes" role="group" aria-label="Chart type"></div>
    <button id="barZones" class="hrz-btn hrz-btn--ghost hrz-btn--sm" type="button" aria-pressed="false"
            title="Draw the supply and demand zones the engine detects">Zones</button>
    <button id="barFit" class="hrz-btn hrz-btn--ghost hrz-btn--sm" type="button"
            title="Show the whole series again (all the bars, and a price axis fitted to them)">Fit</button>
    <button id="barReload" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon" type="button"
            aria-label="Reload this chart" title="Reload this chart">
      <span data-icon="refresh"></span>
    </button>
  </div>
  <div class="barGroup" data-bar-group="indicators">
    <span class="barChip" id="barIndicator" hidden>
      <span class="barChipDot" aria-hidden="true"></span>
      <span class="barChipName" id="barIndicatorName"></span>
      <button id="barIndicatorDetach" class="barChipBtn" type="button"
              aria-label="Detach this indicator" title="Detach this indicator">
        <span data-icon="close"></span>
      </button>
    </span>
    <button id="barAttach" class="hrz-btn hrz-btn--secondary hrz-btn--sm" type="button"
            aria-haspopup="true" aria-expanded="false" aria-controls="attachPop">Attach</button>
    <button id="barLayers" class="hrz-btn hrz-btn--ghost hrz-btn--sm" type="button"
            aria-haspopup="true" aria-expanded="false" aria-controls="layersPop" hidden>Layers</button>
  </div>
  <div class="barGroup" data-bar-group="panes">
    <button id="barLayout" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon" type="button"
            aria-haspopup="true" aria-expanded="false" aria-controls="layoutPop"
            aria-label="Chart layout" title="Split or grid layout">
      <span data-icon="grid"></span>
    </button>
    <button id="split" class="hrz-btn hrz-btn--secondary hrz-btn--sm" type="button"
            title="Add another chart beside this one">
      <span data-icon="plus"></span> Pane
    </button>
    <button id="barMaximize" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon" type="button"
            aria-pressed="false" aria-label="Expand this chart" title="Expand this chart">
      <span data-icon="maximize"></span>
    </button>
  </div>
  <span class="barSpacer"></span>
  <div class="barGroup" data-bar-group="chrome">
    <button id="watchlistToggle" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon" type="button"
            aria-label="Toggle watchlist" title="Toggle watchlist" disabled>
      <span data-icon="panel-left"></span>
    </button>
    <button id="inspectorToggle" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon" type="button"
            aria-label="Toggle inspector" title="Toggle inspector" disabled>
      <span data-icon="panel-right"></span>
    </button>
    <button id="dockToggle" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon" type="button"
            aria-label="Toggle bottom dock" title="Bottom dock" disabled>
      <span data-icon="panel-bottom"></span>
    </button>
    <button id="chartMoreBtn" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon" type="button"
            aria-haspopup="true" aria-expanded="false" aria-controls="chartMore"
            aria-label="More chart actions" title="More">
      <span data-icon="more-horizontal"></span>
    </button>
  </div>
</div>
```

`#split`, `#layoutMode` and `#chartCols` keep their IDs. `#split` stays a visible
button in the Panes group; the two layout selects move into `#layoutPop` in
Step 2. Every listener `app.js` bound to them stays bound, because the nodes are
moved, never replaced.

- [ ] **Step 2: Add the bar's panels**

These are permanent, fixed-position panels -- not `HRZUI.popover`. A popover
removes its content on close, and three of these panels *are* the app's live
controls; a close would take `#split` and the layout selects off the page and
their listeners with them. They also must live outside `#chartColumn`: the
column is a flex child with `min-width: 0`, and a fixed panel inside a subtree
that later gains `contain` would be clipped to it.

Insert immediately before `<script src="/assets/ui/icons.js"></script>` at the
end of `<body>` (after `#statusbar`):

```html
<div class="barPop barPop--symbol" id="symbolPop" role="dialog" aria-label="Find an instrument" hidden>
  <input type="search" id="symbolPopInput" class="hrz-input"
         placeholder="Search BTC, ETH, SOL…" aria-label="Search instruments" autocomplete="off" />
  <div class="barPopList" id="symbolPopList"></div>
  <p class="barPopNote" id="symbolPopNote" role="status"></p>
</div>

<div class="barPop" id="framesPop" role="dialog" aria-label="All timeframes" hidden>
  <p class="barPopTitle">Timeframe</p>
  <div class="barPopList" id="framesPopList"></div>
</div>

<div class="barPop" id="attachPop" role="dialog" aria-label="Attach an indicator" hidden>
  <p class="barPopTitle">Your indicators</p>
  <div class="barPopList" id="attachPopList"></div>
  <p class="barPopNote" id="attachPopNote" role="status"></p>
  <div class="barPopRow barPopRow--end">
    <button id="attachStudio" class="hrz-btn hrz-btn--ghost hrz-btn--sm" type="button">Open Studio</button>
    <button id="attachCommunity" class="hrz-btn hrz-btn--ghost hrz-btn--sm" type="button">Community library</button>
  </div>
</div>

<div class="barPop" id="layersPop" role="dialog" aria-label="Script layers" hidden>
  <p class="barPopTitle">Script layers</p>
  <div class="barPopList" id="layersPopList"></div>
</div>

<div class="barPop" id="layoutPop" role="dialog" aria-label="Chart layout" hidden>
  <p class="barPopTitle">Layout</p>
  <div class="barPopField">
    <div id="layoutCtl">
      <select id="layoutMode" class="hrz-select" aria-label="Chart layout"
              title="Split: up to four charts in rows. Grid: a scrolling wall of up to 100.">
        <option value="split">Split</option>
        <option value="grid">Grid</option>
      </select>
      <select id="chartCols" class="hrz-select" aria-label="Grid columns"
              title="How many chart columns the grid shows" hidden>
        <option value="1">1 col</option>
        <option value="2" selected>2 cols</option>
        <option value="3">3 cols</option>
        <option value="4">4 cols</option>
        <option value="5">5 cols</option>
        <option value="6">6 cols</option>
        <option value="7">7 cols</option>
        <option value="8">8 cols</option>
        <option value="9">9 cols</option>
        <option value="10">10 cols</option>
      </select>
    </div>
  </div>
  <p class="barPopNote">Split shows up to four charts in rows. Grid is a scrolling wall of up to 100.</p>
</div>

<div class="barPop" id="chartMore" role="dialog" aria-label="More chart actions" hidden>
  <div id="moreSection" hidden>
    <p class="barPopTitle">More controls</p>
    <div class="barPopStack" id="moreGroups"></div>
  </div>
  <div class="barPopList">
    <button id="moreShot" class="barPopItem" type="button">
      <span class="barPopIcon" data-icon="camera"></span>
      <span class="barPopMain">Save chart image</span>
    </button>
    <button id="moreKeys" class="barPopItem" type="button">
      <span class="barPopIcon" data-icon="command"></span>
      <span class="barPopMain">Keyboard shortcuts</span>
    </button>
  </div>
</div>
```

The two `<select>` elements are cut from Task 8's toolbar and pasted here with
their options unchanged. `syncLayoutControls` already shows `#chartCols` only in
grid mode, so the panel needs no extra logic.

- [ ] **Step 3: Load the bar**

In `index.html`, after the palette and before the builder:

```html
<script src="/assets/views/shell.js"></script>
<script src="/assets/views/palette.js"></script>
<script src="/assets/views/instrument_bar.js"></script>
<script src="builder.js"></script>
<script src="app.js"></script>
```

`instrument_bar.js` only defines `window.InstrumentBar` and binds document
listeners; it reaches `HRZApp` inside those listeners, so it can load before
`app.js` defines the facade.

- [ ] **Step 4: Write `frontend/app/views/instrument_bar.js`**

```js
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
    if (panel) panel.hidden = true;
    if (state.trigger) state.trigger.setAttribute("aria-expanded", "false");
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
      const group = document.querySelector(`#instrumentBar [data-bar-group="${name}"]`);
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
    // Zero means "not laid out yet" -- a hidden frame at boot, or jsdom, which
    // lays nothing out. Collapsing on that number would move the groups into
    // More on a wide screen and never move them back.
    const width = column.clientWidth || 0;
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
```

- [ ] **Step 5: Give the pane named commands**

In `frontend/app/app.js`, three edits inside `createChartPane`.

1. Immediately before `function wire()` (`6439`), insert the commands:

```js
  /* ---- The series commands -------------------------------------------------
   *
   * Every path that changes this pane's series goes through one of these: the
   * pane's own controls (bound in `wire` below), the instrument bar (through
   * `paneApi`), and a restored snapshot. That is what makes the bar a
   * projection -- it calls the same function the hidden select calls, so there
   * is nothing to keep in step and no way for the two to disagree.
   */

  function applyReload() {
    resetViewport();
    refresh().then(connectLive);
    notifyPane();
  }

  function applyBars(value) {
    el("limit").value = String(value);
    // A smaller limit drops bars the old array still holds, so the candles go
    // and are refetched at the new count.
    resetSeries();
    refresh().then(connectLive);
    notifyPane();
  }

  function applyZones(on) {
    el("zones").setAttribute("aria-pressed", on ? "true" : "false");
    // A redraw, not a refetch: the zones are detected from the candles the
    // engine already has, so there is nothing new to ask the backend for.
    render();
    notifyPane();
  }

  function applyMode(value) {
    el("mode").value = value;
    // A mode change throws the zoom away. The footprint fetches its own window
    // and the engine re-fits, which it must: a viewport resolved against candle
    // prices makes the ladder's levels vanish behind the old axis.
    resetViewport();
    refresh();
    paintTitle();
    notifyPane();
  }

  function applyTimeframe(value) {
    el("timeframe").value = value;
    // These change the series itself, so the window means nothing afterwards --
    // a bar index into the old series is not a bar in the new one.
    resetSeries();
    refresh().then(connectLive);
    paintTitle();
    notifyPane();
  }

  function applySymbol(value) {
    el("symbol").value = value;
    // A different instrument has different timeframes, so the list is rebuilt
    // before the fetch that reads the chosen one.
    fillTimeframes(value);
    // Everything about the old series goes before the fetch: candles, window
    // and indicator.
    resetSeries();
    // The concept explorer follows the instrument.
    enabledConcepts = new Set();
    conceptEvents = [];
    conceptCatalog = null;
    refreshConceptCatalog();
    refresh().then(connectLive);
    paintTitle();
    notifyPane();
    if (hooks.onSymbolChange) hooks.onSymbolChange(paneApi);
  }

  function applyFit() {
    followLive = true;
    // Following the edge again means the ladder should live again too.
    startFootprintPoll();
    applyGesture({ kind: "fit" });
  }
```

2. Replace the six handlers in `wire()` (`6442-6530`) so each calls its
   command. The block from `el("load").addEventListener` through the
   `el("fit").addEventListener` handler becomes:

```js
    el("load").addEventListener("click", applyReload);
    el("limit").addEventListener("change", () => applyBars(el("limit").value));
    el("zones").addEventListener("click", () => applyZones(!zonesOn()));
    // The forecast cone (docs/35). A redraw, not a refetch, for the same
    // reason as Zones: the simulation runs on the candles the engine
    // already has.
    el("forecast").addEventListener("click", (event) => {
      const button = event.currentTarget;
      const on = button.getAttribute("aria-pressed") !== "true";
      button.setAttribute("aria-pressed", on ? "true" : "false");
      el("volScale").hidden = !on;
      render();
    });
    // The what-if knob (docs/35): the engine re-simulates with the scaled
    // returns -- the same seed, so the cone stretches rather than reshuffles.
    el("volScale").addEventListener("input", () => {
      el("volScale").title = `What-if: scale the window's volatility — ×${volScaleValue()}`;
      scheduleRender();
    });
    el("mode").addEventListener("change", () => applyMode(el("mode").value));
    el("timeframe").addEventListener("change", () => applyTimeframe(el("timeframe").value));
    el("symbol").addEventListener("change", () => applySymbol(el("symbol").value));
    el("fit").addEventListener("click", applyFit);
```

Leave the `logScale`, `pctScale`, `.tools`, drawing and canvas listeners that
follow untouched.

3. Extend `paneApi` (`6675`). After `timeframe: () => el("timeframe").value,`
   add the getters:

```js
    mode: () => el("mode").value,
    bars: () => Number(el("limit").value),
    zones: () => zonesOn(),
    maximized: () => root.classList.contains("exp"),
    /// The ladder this pane actually has, for the instrument bar. Read from
    /// the select rather than from `coverage`: the bar may not offer a rung
    /// the pane cannot chart, and the select is what a request reads.
    frames: () => [...el("timeframe").options].map((option) => ({ value: option.value, label: option.textContent })),
    modes: () => [...el("mode").options].map((option) => ({ value: option.value, label: option.textContent })),
```

and after `symbol`/`timeframe` (before `attachIndicator`) add the commands:

```js
    /// The series commands the instrument bar calls. Each is the function this
    /// pane's own control is bound to, so the bar and the hidden controls
    /// cannot disagree about what "set the symbol" means.
    setSymbol: (value) => applySymbol(value),
    setTimeframe: (value) => applyTimeframe(value),
    setBars: (value) => applyBars(value),
    setMode: (value) => applyMode(value),
    setZones: (on) => applyZones(Boolean(on)),
    fit: () => applyFit(),
    reload: () => applyReload(),
    toggleMaximize: () => {
      setExpanded(!root.classList.contains("exp"));
      notifyPane();
    },
```

4. Announce attachment and restore changes. Add `notifyPane();` as the last
   statement of `paneApi.applyState` (`6774`) and of `paneApi.fillSeries`
   (`6935`), and at the end of `syncIndicatorChip` (`7917`):

```js
  // The instrument bar is a projection: it re-reads the active chart whenever
  // the chart changes under it. One event from every path that can change a
  // series or an overlay -- the pane's own controls, the bar's commands, a
  // restored snapshot -- so no listener keeps a list of call sites in step.
  notifyPane();
```

- [ ] **Step 6: Add `notifyPane`, and the two extracted helpers**

1. Immediately before `function setActive(pane)` (`7711`):

```js
/// Announce that the active chart's own state moved -- its series, its
/// overlay, its attachment.
///
/// One event for every path that can change it, so the listeners (the
/// instrument bar, the status bar) never keep a list of call sites in step.
/// The detail carries the pane for a listener that wants it; everything else
/// re-reads through the facade, which is the only source of truth.
function notifyPane() {
  document.dispatchEvent(new CustomEvent("hrz:pane", { detail: { pane: activePane } }));
}
```

2. Immediately before `async function selectWorkspace(id)` (`14043`), extract
   the attach-from-a-revision block:

```js
/// Attach a workspace's active revision to a chart, if it has one.
///
/// Extracted from `selectWorkspace`, which needs exactly this after loading a
/// chat: the instrument bar's Attach menu asks the same question without
/// opening the chat. The fork between "a pine-lite revision attaches as code"
/// and "a document revision attaches as a preview" lives here, once.
/// Answers a sentence describing what happened, or "" when there was nothing
/// to attach.
async function attachWorkspaceRevision(ws, target) {
  if (!ws || !target || !ws.active_revision_id) return "";
  const rev = await api(`/indicator-workspaces/${ws.id}/revisions/${ws.active_revision_id}`);
  const preview = rev.preview;
  if (rev.validation && rev.validation.engine === "pine-lite-v1" && rev.source) {
    target.attachScript({
      source: rev.source,
      inputs: {},
      name: (preview && preview.name) || ws.name,
      inputsSpec: Array.isArray(rev.validation.inputs) ? rev.validation.inputs : [],
    });
    syncIndicatorChip();
    return `Attached script "${(preview && preview.name) || ws.name}" to ${target.symbol()} ${target.timeframe()} — it re-runs on every candle`;
  }
  const live = preview && Array.isArray(preview.concepts) && preview.concepts.length > 0;
  if (preview && (live || !ws.symbol || ws.symbol === target.symbol())) {
    target.attachIndicator(preview);
    syncIndicatorChip();
    const liveNote = live
      ? " — it keeps detecting on every new candle of any chart it is attached to"
      : "";
    return `Attached "${preview.name || ws.name}" to ${target.symbol()} ${target.timeframe()}${liveNote}`;
  }
  return "";
}
```

Then replace the inline block in `selectWorkspace` (from
`if (ws.active_revision_id && activePane) {` through its closing `}`) with:

```js
  if (ws.active_revision_id && activePane) {
    try {
      const message = await attachWorkspaceRevision(ws, activePane);
      if (message) pageToast(message);
    } catch (e) {
      pageToast(`Failed to attach indicator: ${e.message}`);
    }
  }
```

3. Immediately before `function chartFromWatchlist(symbol)` (`12382`), extract
   the instrument-list growth:

```js
/// Make sure the page's instrument list contains `symbol`.
///
/// The list is built from what the platform has charted, so a symbol found by
/// search may not be in it yet. It is appended with a full, zero-coverage
/// timeframe ladder -- `/candles` fetches history on demand, so coverage is
/// not a permission -- and every pane's selects are rebuilt, because the list
/// is shared by the whole page. Each pane keeps the instrument *and the
/// timeframe* it was showing: a symbol arriving elsewhere must not re-frame a
/// chart the user is reading.
/// Returns true when the list grew.
function ensureCoverage(symbol) {
  if (!symbol || coverage.some((entry) => entry.symbol === symbol)) return false;
  coverage = [
    ...coverage,
    {
      symbol,
      timeframes: ["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "8h", "12h", "1d", "3d", "1w"].map((t) => ({
        timeframe: t,
        candles: 0,
        first: 0,
        last: 0,
      })),
    },
  ];
  for (const pane of panes) {
    pane.fillSeries(coverage, pane.symbol() || undefined, pane.timeframe() || undefined);
  }
  return true;
}
```

and replace `chartFromWatchlist`'s body with:

```js
function chartFromWatchlist(symbol) {
  if (!activePane) return;
  // 15m is the deliberate starting rung for an instrument with no history:
  // the ladder is zero-coverage everywhere, so "the deepest series" would be
  // whichever entry happened to come first.
  const added = ensureCoverage(symbol);
  activePane.fillSeries(coverage, symbol, added ? "15m" : undefined);
  setActive(activePane);
  activePane.refresh().then(() => activePane.connectLive());
}
```

- [ ] **Step 7: Answer the bar's questions through the facade**

In `app.js`, two edits to `window.HRZApp` (Task 9 created it; Task 10 added the
palette's members).

1. Replace the whole `activePaneInfo()` method with:

```js
  activePaneInfo() {
    if (!activePane) return null;
    const badge = activePane.root.querySelector(".feedStatus");
    const attached = activePane.attachedIndicator();
    return {
      symbol: activePane.symbol(),
      timeframe: activePane.timeframe(),
      feed: badge ? badge.dataset.state || "" : "",
      mode: activePane.mode(),
      bars: activePane.bars(),
      zones: activePane.zones(),
      maximized: activePane.maximized(),
      frames: activePane.frames(),
      modes: activePane.modes(),
      indicator: attached
        ? {
            name: attached.name || "generated indicator",
            live: Array.isArray(attached.concepts) && attached.concepts.length > 0,
          }
        : null,
      layers: activePane.attachedScripts().map((layer, index) => ({
        index,
        name: layer.name || "script",
        visible: layer.visible !== false,
      })),
    };
  },
```

2. Add these members immediately after `chartSymbol` (Task 10's last member):

```js
  // --- the instrument bar's commands (Task 11) ------------------------------

  /// Move the active chart to a symbol the search found. The instrument list is
  /// built from what the platform has charted, so a venue symbol may not be in
  /// it yet; `ensureCoverage` adds it -- and rebuilds every pane's selects,
  /// keeping each pane's timeframe -- before the pane's `setSymbol` runs.
  /// Without that the select would silently keep its old option.
  setActiveSymbol(symbol) {
    if (!activePane || !symbol) return;
    const wanted = String(symbol).toUpperCase();
    ensureCoverage(wanted);
    activePane.setSymbol(wanted);
  },

  setActiveTimeframe: (frame) => { if (activePane) activePane.setTimeframe(frame); },
  setActiveBars: (bars) => { if (activePane) activePane.setBars(bars); },
  setActiveMode: (mode) => { if (activePane) activePane.setMode(mode); },
  setActiveZones: (on) => { if (activePane) activePane.setZones(on); },
  fitActivePane: () => { if (activePane) activePane.fit(); },
  reloadActivePane: () => { if (activePane) activePane.reload(); },
  toggleMaximize: () => { if (activePane) activePane.toggleMaximize(); },
  detachActiveIndicator: () => detachIndicator(),
  setLayerVisible: (index, visible) => { if (activePane) activePane.setScriptVisible(index, visible); },
  removeLayer: (index) => { if (activePane) activePane.removeScriptAt(index); },

  /// Attach a saved indicator to the active chart without opening Studio.
  /// The workspace is read rather than taken from the cached list, because
  /// that list is only loaded when Studio opens and the bar can be used on a
  /// page where it never has been. The attach itself is the same helper
  /// `selectWorkspace` uses, so both paths attach identically.
  async attachWorkspace(id) {
    if (!activePane) return "Open a chart first.";
    try {
      const ws = await api(`/indicator-workspaces/${id}`);
      const message = await attachWorkspaceRevision(ws, activePane);
      return message || `"${ws.name}" has nothing to attach to this chart.`;
    } catch (error) {
      return `Could not attach: ${error.message}`;
    }
  },

  /// Save the active chart as a PNG the user can keep. The capture path is the
  /// same one the AI chat attaches, so the file is the chart the model sees:
  /// no crosshair, no hover, the plot at device resolution.
  async saveScreenshot() {
    if (!activePane) return "Open a chart first.";
    const shot = captureChart(activePane.canvas, activePane.timeframe());
    if (!shot) return "The chart is not ready to save yet.";
    const link = document.createElement("a");
    link.href = `data:${shot.media_type};base64,${shot.data}`;
    link.download = `${activePane.symbol()}-${activePane.timeframe()}.png`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    return `Saved ${activePane.symbol()} ${activePane.timeframe()}.`;
  },
```

- [ ] **Step 8: Start the bar, and keep the two panel toggles honest**

1. In `main()` (`13401`), immediately after `if (window.Shell) Shell.init();`:

```js
  if (window.InstrumentBar) InstrumentBar.init();
```

2. In `views/shell.js`, `syncWatchlist` and `syncInspector` gain the pressed
   state the bar's copies read. Append to `syncWatchlist`, after the reveal
   line:

```js
    const toggle = el("watchlistToggle");
    if (toggle) toggle.setAttribute("aria-pressed", String(state.watchlistCollapsed));
```

and to `syncInspector`, after the `aiBtn` block:

```js
    const toggle = el("inspectorToggle");
    if (toggle) toggle.setAttribute("aria-pressed", String(open));
```

`syncDock` already sets `#dockToggle`'s pressed state.

- [ ] **Step 9: Add the styles**

1. `tokens.css` — add the bar's height to the layout block, after
   `--topbar-h: 44px;`:

```css
  --bar-h: 40px;
```

2. `shell.css` — delete the whole `.chartToolbar` rule block from Task 8.

3. `charts.css` — delete the line `#chartColumn > .chartToolbar { ... }` /
   `.chartToolbar { flex: 0 0 36px; }` Task 8 added, then append:

```css
/* ---- The instrument bar --------------------------------------------------- */
/* 40px, not the old 36: the bar carries 28px segmented controls and a symbol
   button with its own padding, and a strip that clips its own buttons reads as
   broken. It belongs to the *active* chart; the panes' own controls stay in
   their hidden bars exactly where they were. */
.instrumentBar {
  display: flex; align-items: center; gap: var(--space-2);
  flex: 0 0 var(--bar-h); height: var(--bar-h);
  padding: 0 var(--space-2);
  border-bottom: 1px solid var(--line);
  background: var(--bg-panel);
  min-width: 0;
}
.barGroup { display: flex; align-items: center; gap: var(--space-1); min-width: 0; }
.barGroup + .barGroup {
  padding-left: var(--space-2); margin-left: var(--space-1);
  border-left: 1px solid var(--line);
}
.barSpacer { flex: 1 1 auto; min-width: var(--space-2); }
.barMarker { display: none; }
/* No active chart: the series controls have nothing to act on. The panel
   toggles stay live -- they are about the page, not the instrument. */
.instrumentBar[data-state="empty"] .barGroup:not([data-bar-group="chrome"]) { visibility: hidden; }

/* The symbol button is the bar's anchor: the strip always states which
   instrument the controls act on. Mono, because a symbol is an identifier. */
.barSymbol {
  display: inline-flex; align-items: center; gap: var(--space-1);
  height: 28px; padding: 0 var(--space-2);
  border: 1px solid transparent; border-radius: var(--radius-1);
  background: transparent; color: var(--text-1);
  font: 600 var(--text-sm)/1 var(--font-mono);
  white-space: nowrap;
}
.barSymbol:hover { background: var(--bg-raised); }
.barSymbol[aria-expanded="true"] { background: var(--bg-raised); border-color: var(--line-strong); }
.barSymbol svg { width: 12px; height: 12px; color: var(--text-3); }
.barSymbolName { overflow: hidden; text-overflow: ellipsis; }
.instrumentBar .hrz-seg { height: 28px; }
.instrumentBar .hrz-seg__item { padding: 0 6px; font-size: 11px; }
.instrumentBar select.hrz-select { width: auto; height: 28px; }
#barBars { width: 64px; }

/* The attached-indicator chip: a dot, a name and a ×. The script layers are a
   count ("+2") rather than more chips -- the bar is a strip, not a list, and
   their own panel is one press away. */
.barChip {
  display: inline-flex; align-items: center; gap: var(--space-1);
  max-width: 220px; height: 26px; padding: 0 var(--space-1) 0 var(--space-2);
  border: 1px solid var(--accent-line); border-radius: 999px;
  background: var(--accent-soft); color: var(--text-1);
  font-size: var(--text-xs); white-space: nowrap;
}
.barChip[hidden] { display: none; }
.barChipDot { width: 6px; height: 6px; border-radius: 50%; background: var(--accent); flex: 0 0 auto; }
.barChipName { overflow: hidden; text-overflow: ellipsis; }
.barChipBtn {
  display: inline-flex; align-items: center; justify-content: center;
  width: 18px; height: 18px; padding: 0;
  border: 0; border-radius: 50%; background: transparent; color: var(--text-2);
}
.barChipBtn:hover { background: var(--bg-overlay); color: var(--down); }
.barChipBtn svg { width: 12px; height: 12px; }

/* Compact: the same controls, tightened -- only the groups that do not fit
   have moved into More, so this buys back the padding they were using. */
.instrumentBar[data-width="compact"] { gap: var(--space-1); }
.instrumentBar[data-width="compact"] .barGroup { gap: 2px; }
.instrumentBar[data-width="compact"] .barGroup + .barGroup { padding-left: var(--space-1); }
.instrumentBar[data-width="tight"] .barSymbol { max-width: 96px; }
.instrumentBar[data-width="tight"] .barSymbolName { max-width: 60px; }

/* ---- The bar's panels -----------------------------------------------------
   Permanent fixed panels, shown and hidden, rather than `HRZUI.popover`:
   three of them HOLD the app's live controls (#split, #layoutMode, #chartCols)
   and a popover that removes its content on close would take them, and their
   listeners, off the page. Fixed and outside #chartColumn, so a later
   `contain` on the column cannot clip them. */
.barPop {
  position: fixed; left: var(--pop-left, 0); top: var(--pop-top, 0);
  z-index: 60; width: max-content; min-width: 240px; max-width: 320px;
  max-height: min(420px, 70vh); overflow: auto;
  padding: var(--space-2);
  border: 1px solid var(--line-strong); border-radius: var(--radius-2);
  background: var(--bg-overlay); box-shadow: var(--shadow-2);
}
.barPop[hidden] { display: none; }
.barPop--symbol { width: 320px; }
.barPopTitle {
  margin: var(--space-1) var(--space-1) var(--space-2);
  font-size: 11px; font-weight: 600; letter-spacing: 0.06em;
  text-transform: uppercase; color: var(--text-3);
}
.barPopList { display: flex; flex-direction: column; gap: 1px; }
.barPopStack { display: flex; flex-direction: column; gap: var(--space-2); align-items: flex-start; }
.barPopStack .barGroup { flex-wrap: wrap; }
.barPopStack .barGroup + .barGroup {
  border-left: 0; border-top: 1px solid var(--line);
  padding-left: 0; margin-left: 0;
  padding-top: var(--space-2); margin-top: var(--space-2);
}
.barPopItem {
  display: flex; align-items: center; gap: var(--space-2);
  width: 100%; min-width: 0; padding: var(--space-2);
  border: 0; border-radius: var(--radius-1);
  background: transparent; color: var(--text-1); text-align: left;
  font-size: var(--text-sm);
}
.barPopItem:hover, .barPopItem:focus-visible { background: var(--bg-raised); }
.barPopItem[aria-pressed="true"] { color: var(--accent); }
.barPopMain { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.barPopMeta { flex: 0 0 auto; font: var(--text-2xs)/1 var(--font-mono); color: var(--text-3); }
.barPopIcon { display: inline-flex; color: var(--text-2); }
.barPopIcon svg { width: 14px; height: 14px; }
.barPopRow { display: flex; align-items: center; gap: var(--space-1); }
.barPopRow--end { justify-content: flex-end; padding-top: var(--space-2); }
.barPopNote { margin: var(--space-2) var(--space-1) 0; color: var(--text-3); font-size: var(--text-xs); }
.barPopNote:empty { display: none; }
.barPopField { display: flex; flex-direction: column; gap: var(--space-1); margin-bottom: var(--space-2); }
#layoutCtl { display: flex; gap: var(--space-1); }
#layoutCtl select { width: auto; flex: 1 1 auto; }
```

Every colour above is a token. The hardcoded-colour guard from Task 3 must
still return only `tokens.css`.

- [ ] **Step 10: Teach the harness**

In `tools/shell_check.mjs`:

1. Add the file to the boot list, after `views/palette.js`:

```js
  "frontend/app/views/palette.js",
  "frontend/app/views/instrument_bar.js",
```

2. Answer the symbol search. Insert **before** the `if (url.startsWith("/symbols"))`
   branch (`625`) -- that branch matches `/symbols/search` too, and a search
   answered with the whole instrument list would never exercise the bar's own
   query handling:

```js
  // The instrument bar's search. Ahead of `/symbols` on purpose: `startsWith`
  // does not know about path segments, and `/symbols/search` starts with
  // `/symbols` -- so a mock below this one would swallow every search.
  if (url.startsWith("/symbols/search")) {
    const query = (queryOf(url).get("q") ?? "").toUpperCase();
    const all = backend.symbols.map((entry) => ({
      symbol: entry.symbol,
      base: entry.symbol.replace(/USDT$/, ""),
      quote: "USDT",
      trading: true,
    }));
    return json(200, {
      query,
      results: all.filter((entry) => entry.symbol.includes(query)),
      indexed: all.length,
      fetched_at: FIXTURE_NOW_MS,
      stale: false,
    });
  }
```

3. Drive the bar. Insert after the "timeframe options read as a ladder rather
   than in the server's order" check and before
   `console.log("\nthe layout at a narrow width")`:

```js
// --- the instrument bar ------------------------------------------------------
//
// The bar is a projection of the active pane, not a second set of controls. The
// checks drive it the way a user does -- press a frame, pick a symbol -- and
// then read the *pane's* own selects back, because "the bar says 1h" and "the
// chart is on 1h" are different claims and only the second one matters.

console.log("\nthe instrument bar");

const barNode = () => document.getElementById("instrumentBar");
check(
  "the instrument bar sits at the top of the chart column",
  barNode() && barNode().parentElement.id === "chartColumn" &&
    barNode().nextElementSibling && barNode().nextElementSibling.id === "globalTools",
  barNode() ? barNode().parentElement.id : "missing"
);
check(
  "it names the active chart's symbol",
  document.getElementById("barSymbolName").textContent === "BTCUSDT",
  document.getElementById("barSymbolName").textContent
);
check(
  "and offers the ladder the pane actually has",
  [...document.querySelectorAll("#barFrames [data-frame]")].map((b) => b.dataset.frame).join(",") ===
    "1m,5m,15m,1h,4h",
  [...document.querySelectorAll("#barFrames [data-frame]")].map((b) => b.dataset.frame).join(",")
);
check(
  "with the pane's own frame pressed",
  document.querySelector('#barFrames [data-frame="5m"]').getAttribute("aria-pressed") === "true",
  document.querySelector('#barFrames [data-frame="5m"]').getAttribute("aria-pressed")
);
check(
  "and the chart type segmented from the pane's own select",
  [...document.querySelectorAll("#barModes [data-mode]")].map((b) => b.dataset.mode).join(",") ===
    "candles,heikin_ashi,bars,line,area,footprint",
  [...document.querySelectorAll("#barModes [data-mode]")].map((b) => b.dataset.mode).join(",")
);

document.querySelector('#barFrames [data-frame="1h"]').click();
await settle();
check(
  "pressing a frame moves the pane's own timeframe select",
  selectIn(PANE, "timeframe") === "1h",
  selectIn(PANE, "timeframe")
);
check(
  "and the bar follows the pane back",
  document.querySelector('#barFrames [data-frame="1h"]').getAttribute("aria-pressed") === "true",
  document.querySelector('#barFrames [data-frame="1h"]').getAttribute("aria-pressed")
);

document.querySelector('#barModes [data-mode="bars"]').click();
await settle();
check(
  "pressing a chart type moves the pane's own select",
  selectIn(PANE, "mode") === "bars",
  selectIn(PANE, "mode")
);

document.getElementById("barZones").click();
await settle();
check(
  "the bar's Zones toggle drives the pane's own toggle",
  paneNode().querySelector(".zones").getAttribute("aria-pressed") === "true",
  paneNode().querySelector(".zones").getAttribute("aria-pressed")
);
document.getElementById("barZones").click();
await settle();

document.getElementById("barSymbol").click();
const searchInput = document.getElementById("symbolPopInput");
check(
  "the symbol panel opens with the caret in its field",
  document.getElementById("symbolPop").hidden === false && document.activeElement === searchInput,
  `hidden=${document.getElementById("symbolPop").hidden} focused=${document.activeElement && document.activeElement.id}`
);
searchInput.value = "eth";
searchInput.dispatchEvent(new window.Event("input", { bubbles: true }));
await waitFor(
  () => document.querySelector('#symbolPopList [data-symbol="ETHUSDT"]'),
  "the symbol search results"
);
check(
  "the search lists the matching instrument",
  document.querySelectorAll("#symbolPopList [data-symbol]").length === 1,
  `${document.querySelectorAll("#symbolPopList [data-symbol]").length} results`
);
document.querySelector('#symbolPopList [data-symbol="ETHUSDT"]').click();
await settle();
check(
  "picking it moves the active chart",
  selectIn(PANE, "symbol") === "ETHUSDT",
  selectIn(PANE, "symbol")
);
check("and the panel closes behind it", document.getElementById("symbolPop").hidden === true);

// The strip narrows by moving groups into More, never by cloning them: `#split`
// and the layout selects are the app's nodes and carry its listeners, so a copy
// would be a control that looks live and does nothing.
const columnWidth = (width) => {
  Object.defineProperty(document.getElementById("chartColumn"), "clientWidth", {
    value: width,
    configurable: true,
  });
  window.InstrumentBar.measure();
};

columnWidth(800);
check(
  "at 800px the indicator and pane groups move into More",
  document.getElementById("moreGroups").querySelector('[data-bar-group="indicators"]') &&
    document.getElementById("moreGroups").querySelector('[data-bar-group="panes"]') &&
    document.querySelector('#instrumentBar [data-bar-group="view"]') &&
    document.querySelector('#instrumentBar [data-bar-group="instrument"]'),
  [...document.querySelectorAll("#instrumentBar > .barGroup")].map((g) => g.dataset.barGroup).join(",")
);
check(
  "and the moved group is the same node, not a copy",
  document.getElementById("moreGroups").querySelector("#split") === document.getElementById("split")
);

columnWidth(700);
check(
  "at 700px the view group joins them, and the symbol stays",
  document.getElementById("moreGroups").querySelector('[data-bar-group="view"]') &&
    document.querySelector('#instrumentBar [data-bar-group="instrument"]') &&
    document.querySelector('#instrumentBar [data-bar-group="chrome"]'),
  [...document.querySelectorAll("#instrumentBar > .barGroup")].map((g) => g.dataset.barGroup).join(",")
);

columnWidth(1400);
check(
  "widening again puts every group back in its own place",
  [...document.querySelectorAll("#instrumentBar > .barGroup")].map((g) => g.dataset.barGroup).join(",") ===
    "instrument,view,indicators,panes,chrome",
  [...document.querySelectorAll("#instrumentBar > .barGroup")].map((g) => g.dataset.barGroup).join(",")
);
check("and More stops offering moved controls", document.getElementById("moreSection").hidden === true);

// Back to the state the rest of the file expects: the checks after this one
// assume the first chart on 5m, candles, zones off.
document.querySelector('#barFrames [data-frame="5m"]').click();
await settle();
document.querySelector('#barModes [data-mode="candles"]').click();
await settle();
check(
  "and the bar restored the chart the later checks assume",
  selectIn(PANE, "timeframe") === "5m" && selectIn(PANE, "mode") === "candles",
  `${selectIn(PANE, "timeframe")} ${selectIn(PANE, "mode")}`
);
```

Note the symbol check leaves the active pane on ETHUSDT. The chart is refetched
and the fixture answers for any symbol, so later checks that read the pane's own
symbol must not assume BTCUSDT. Search the tail of the file for `BTCUSDT`
assertions after this point; if any exist, end this block by selecting the
symbol back through the same panel:

```js
document.getElementById("barSymbol").click();
searchInput.value = "btc";
searchInput.dispatchEvent(new window.Event("input", { bubbles: true }));
await waitFor(
  () => document.querySelector('#symbolPopList [data-symbol="BTCUSDT"]'),
  "the symbol search results"
);
document.querySelector('#symbolPopList [data-symbol="BTCUSDT"]').click();
await settle();
```

- [ ] **Step 11: Run it**

```bash
node --check frontend/app/views/instrument_bar.js
node --check frontend/app/views/shell.js
node --check frontend/app/app.js
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
cargo test -p api-gateway --test packaging
```

Expected: all pass. If the bar stays `data-state="empty"`, `hrz:pane` never
fired -- check `setActive` dispatches it and that the first pane is created
through `addPane`/`restoreWorkspace`. If the frames list is empty, the pane's
`timeframe` select had no options when the bar read it.

Open the gateway at 1440×900 signed in, on Charts, and check by hand:

1. The bar reads `BTCUSDT`, `5m` pressed, 500 bars, Candles pressed.
2. Press `1h`: the pane's title, the bar and the right-click menu's timeframe
   all say `1h`. Press `1h` in the menu: the bar follows.
3. Open the symbol button, type `eth`, press `↵`: the active chart moves to
   ETHUSDT. A second pane keeps its own symbol *and* timeframe.
4. Zones on from the bar: the pane's hidden Zones button reads pressed, and the
   overlay draws. Off again, both agree.
5. Drag the watchlist splitter until the chart column is under 900px: Attach,
   Layers and the Panes group move into More and stay usable there. Under
   720px the View group joins them. Widen: every group returns in order.
6. Attach → a workspace with an active revision: the chip appears in the bar and
   in the pane head, and × on either removes it from both.
7. More → Save chart image downloads `<SYMBOL>-<TF>.png`; Keyboard shortcuts
   opens the sheet; `Esc` closes it and the bar is still focused.
8. `Tab` reaches every bar control in order; `Space` presses a frame; `Esc` on
   an open panel closes it and returns focus to its button.
9. Switch to light: borders, the chip and the panels stay legible.
10. With VoiceOver, the strip announces "Active chart controls", each toggle
    says pressed/not pressed, and the symbol button announces its expanded
    state.

- [ ] **Step 12: Commit**

```bash
git add frontend/app/views/instrument_bar.js frontend/app/index.html \
  frontend/app/app.js frontend/app/views/shell.js \
  frontend/app/styles/tokens.css frontend/app/styles/charts.css \
  frontend/app/styles/shell.css tools/shell_check.mjs
git commit -m "feat: an instrument bar that projects the active chart"
```

---

### Task 12: A watchlist and an inspector that follow the active chart

**Files:**
- Modify: `frontend/app/index.html` (watchlist markup, Library toggle,
  Indicator projection, Details facts)
- Modify: `frontend/app/app.js` (`renderWatchlist` rows, the Library toggle,
  `activePaneInfo` counts)
- Modify: `frontend/app/views/shell.js` (the two projections, the Indicator
  panel's visibility, the Charts watchlist loader)
- Modify: `frontend/app/styles/charts.css` (the rail)
- Modify: `frontend/app/styles/inspector.css` (the projection and the facts)
- Modify: `tools/shell_check.mjs` (a JWT-shaped token, ticker and workspace
  mocks, the checks)

**Interfaces:**
- Consumes: `HRZApp.activePaneInfo()`, `attachWorkspace(id)` (Task 11),
  `detachActiveIndicator()`, `email()`, `planName()`, `Shell.go`/`openDock`
  (Task 9), `addPane`, `ensureCoverage`, `setActive`, `activeSymbol`,
  `chartFromWatchlist` (Task 11).
- Produces: `.watchRow[data-dir]`, `.watchRowMain`, `#libToggle`/`#libBody`,
  the Indicator tab's `#indicatorName`/`indicatorMode`/`indicatorConcepts`/
  `indicatorZones`/`indicatorMarkers`/`indicatorNote`/`indicatorDetach`/
  `indicatorStudio`/`indicatorAlerts`, the Details tab's `#dfChart`/`dfBars`/
  `dfFeed`/`dfAccount`/`dfPlan`, and `refreshPane()` in `shell.js`.

**Why these two together:** both are projections of the active chart, and both
are read from one event. The rail's tint is the market's direction; the
inspector's two tabs are the chart's indicator and its session. Neither owns
the state it shows -- the pane does -- so a detach from a chip, from the bar or
from the inspector reaches all three through `hrz:pane`. Splitting them would
mean two more listeners that can drift out of step with the same question.

- [ ] **Step 1: Restyle the watchlist markup**

Replace the watchlist pane's contents and the Library card (`index.html`, from
`<div class="pane" id="pane-watchlist" hidden>` through the Library card's
closing `</div>`) with:

```html
    <div class="pane" id="pane-watchlist" hidden>
      <div class="row wlSearch">
        <span class="wlSearch__icon" data-icon="search" aria-hidden="true"></span>
        <input id="wlSearch" class="hrz-input" aria-label="Search every venue symbol"
               placeholder="Search every symbol (e.g. SOL, PEPE)" />
        <button id="wlClear" class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon"
                type="button" hidden title="Back to the full list" aria-label="Clear the search">×</button>
      </div>
      <div id="wlTags" class="wlTags" hidden></div>
      <div class="row wlHead">
        <span id="wlCount" class="muted"></span>
        <span id="wlSource" class="muted"></span>
      </div>
      <div id="wlRows"></div>
      <p class="empty" id="wlEmpty" hidden></p>
    </div>
    <!--
      The Library: everything the chat agents built, one click to use. Its
      header is a button, so it collapses without the list disappearing from
      the rail -- the count stays visible, which is the one thing worth
      knowing without expanding it.
    -->
    <div class="pane libCard" id="libCard">
      <button type="button" class="libToggle" id="libToggle"
              aria-expanded="true" aria-controls="libBody">
        <span class="libToggle__title">Library</span>
        <span id="libCount" class="muted"></span>
        <span data-icon="chevron-down" aria-hidden="true"></span>
      </button>
      <div id="libBody">
        <div class="libFilters" id="libFilters">
          <button type="button" data-filter="all" class="on">All</button>
          <button type="button" data-filter="indicator">Indicators</button>
          <button type="button" data-filter="skill">Skills</button>
          <button type="button" data-filter="strategy">Strategies</button>
        </div>
        <div id="libRows"></div>
        <p class="empty" id="libEmpty" hidden>Nothing built yet — ask the chat to create an indicator, a skill, or a strategy.</p>
      </div>
    </div>
```

`#wlSearch`, `#wlClear`, `#wlTags`, `#wlCount`, `#wlSource`, `#wlRows`,
`#wlEmpty`, `#libFilters`, `#libRows`, `#libEmpty` and `#libCount` keep their
ids: `wireWatchlist`, `renderWatchlist`, `renderLibrary` and the filter
delegate are untouched by the restructure.

- [ ] **Step 2: Wire the Library toggle**

In `wireWatchlist()` (`app.js:12422`), after the `wlClear` listener:

```js
  // The Library starts open: a library nobody has opened is a library nobody
  // knows is there. Collapsed, the header stays -- its count is the one thing
  // worth seeing without expanding it.
  const libToggle = el("libToggle");
  if (libToggle) {
    libToggle.addEventListener("click", () => {
      const body = el("libBody");
      const opening = body.hidden;
      body.hidden = !opening;
      libToggle.setAttribute("aria-expanded", String(opening));
      libToggle.classList.toggle("is-collapsed", !opening);
    });
  }
```

`#libBody` ships without `hidden`, which is why the markup's `aria-expanded`
starts at `"true"` and `opening` is read from the body rather than stored.

- [ ] **Step 3: Rebuild the rows**

Replace the row-building loop at the end of `renderWatchlist` (`app.js:12332`,
from `for (const row of rows) {` to the function's closing brace) with:

```js
  for (const row of rows) {
    const ticker = watchlistTickers.get(row.symbol);
    const up = ticker ? ticker.price_change_percent >= 0 : null;
    const line = document.createElement("div");
    line.className = "watchRow";
    // The 3px edge tint is the direction, so the list reads as a column of
    // arrows before a single number is read.
    if (up !== null) line.dataset.dir = up ? "up" : "down";

    const star = document.createElement("button");
    star.type = "button";
    star.className = "star";
    star.setAttribute("aria-pressed", String(favoriteSet.has(row.symbol)));
    star.textContent = favoriteSet.has(row.symbol) ? "★" : "☆";
    star.title = favoriteSet.has(row.symbol) ? "remove from favorites" : "add to favorites";
    // Not a click on the row: charting and favoriting are different intents
    // about the same row, so one must not trigger the other.
    star.onclick = (event) => {
      event.stopPropagation();
      toggleFavorite(row.symbol);
      renderWatchlist();
    };

    // The row's body is a button of its own rather than the row being one:
    // the star is a second control beside it, and a button inside a button is
    // not reachable by a keyboard or announced by a screen reader.
    const main = document.createElement("button");
    main.type = "button";
    main.className = "watchRowMain";
    main.title = "Chart this symbol. Alt-click opens it in a new pane.";

    const sym = document.createElement("span");
    sym.className = "wlSym";
    sym.textContent = row.symbol;
    if (row.trading === false) sym.title = "the venue is not currently accepting orders for this symbol";

    const price = document.createElement("span");
    price.className = "wlPrice";
    price.textContent = ticker ? fmtPrice(ticker.last_price) : "—";

    const change = document.createElement("span");
    if (ticker) {
      change.className = `wlChange ${up ? "up" : "down"}`;
      change.textContent = `${up ? "+" : ""}${ticker.price_change_percent.toFixed(2)}%`;
    } else {
      change.className = "wlChange";
    }

    main.append(sym, price, change);
    const spoken = ticker
      ? `${row.symbol}, ${fmtPrice(ticker.last_price)}, ${up ? "up" : "down"} ${Math.abs(ticker.price_change_percent).toFixed(2)} percent`
      : row.symbol;
    main.setAttribute("aria-label", `${spoken}. Chart in the active pane; Alt-click opens a new pane.`);
    main.onclick = (event) => {
      // Alt-click is "beside", not "instead": the chart the user is reading
      // stays where it is and the row's symbol arrives next to it. At the pane
      // cap `addPane` answers null, and charting in place is still the command
      // the row means.
      if (event.altKey) {
        const pane = addPane();
        if (pane) {
          const added = ensureCoverage(row.symbol);
          pane.fillSeries(coverage, row.symbol, added ? "15m" : undefined);
          setActive(pane);
          pane.refresh().then(() => pane.connectLive());
          return;
        }
      }
      chartFromWatchlist(row.symbol);
    };

    if (row.symbol === activeSymbol()) {
      line.classList.add("active");
      main.setAttribute("aria-current", "true");
    }
    line.append(star, main);
    rowsEl.appendChild(line);
  }
```

`undefined` as the third argument (rather than the pane's current frame) is
deliberate: a new pane cloned from the active chart carries that chart's
timeframe, which the row's symbol may not list. `fillSeries` then picks the
richest series for the row's own symbol, the same rule a fresh pane opens on.

- [ ] **Step 4: Put the counts on the facade**

In `app.js`, extend the `indicator` block of `window.HRZApp`'s
`activePaneInfo()` -- the method Task 11 wrote -- so the Indicator tab can read
the counts without reaching into the pane:

```js
      indicator: attached
        ? {
            name: attached.name || "generated indicator",
            live: Array.isArray(attached.concepts) && attached.concepts.length > 0,
            // Counts, not the arrays: the tab prints three numbers and the
            // facade is not the place to hand out a live preview object.
            concepts: Array.isArray(attached.concepts) ? attached.concepts.length : 0,
            zones: Array.isArray(attached.zones) ? attached.zones.length : 0,
            markers: Array.isArray(attached.markers) ? attached.markers.length : 0,
          }
        : null,
```

- [ ] **Step 5: Give the Indicator tab something to show**

Replace the `#inspectorIndicator` block Task 9 added with:

```html
      <div class="inspectorPanel" id="inspectorIndicator" hidden>
        <h2 class="inspectorPanel__title">Indicator</h2>
        <p class="empty" id="indicatorProjectionEmpty">
          No indicator is on this chart. Attach one from the instrument bar's
          Attach menu, or build one in Studio.
        </p>
        <div id="indicatorProjection" hidden>
          <p class="indicatorName" id="indicatorName">—</p>
          <dl class="inspectorFacts">
            <div><dt>Mode</dt><dd id="indicatorMode">—</dd></div>
            <div><dt>Concepts</dt><dd id="indicatorConcepts">—</dd></div>
            <div><dt>Zones</dt><dd id="indicatorZones">—</dd></div>
            <div><dt>Markers</dt><dd id="indicatorMarkers">—</dd></div>
          </dl>
          <p class="inspectorNote" id="indicatorNote"></p>
          <div class="inspectorActions">
            <button type="button" id="indicatorDetach" class="hrz-btn hrz-btn--secondary hrz-btn--sm">Detach</button>
            <button type="button" id="indicatorStudio" class="hrz-btn hrz-btn--ghost hrz-btn--sm">Open in Studio</button>
            <button type="button" id="indicatorAlerts" class="hrz-btn hrz-btn--ghost hrz-btn--sm">Alerts</button>
          </div>
        </div>
      </div>
```

There is no `version` row: the revision's number belongs to the workspace and is
not carried on the attached preview, and a number invented here would be a
claim about the chart that nothing can check. The name, mode and counts are
what the pane actually holds.

- [ ] **Step 6: Add the Details facts**

Immediately after `<div id="thesis">…</div>` inside `pane-thesis`
(`index.html:2384`), append:

```html
      <!-- What the chart is reading and who is reading it. The pairs the
           instrument bar and the account already show, gathered where a user
           looks when they ask "is this chart live, and whose session is it?". -->
      <section class="detailsFacts" id="detailsFacts" aria-labelledby="detailsFactsTitle">
        <h2 class="inspectorPanel__title" id="detailsFactsTitle">Session</h2>
        <dl class="detailsFacts__list">
          <div><dt>Chart</dt><dd id="dfChart">—</dd></div>
          <div><dt>Bars</dt><dd id="dfBars">—</dd></div>
          <div><dt>Feed</dt><dd id="dfFeed">—</dd></div>
          <div><dt>Account</dt><dd id="dfAccount">—</dd></div>
          <div><dt>Plan</dt><dd id="dfPlan">—</dd></div>
        </dl>
      </section>
```

The watcher (`#watchPanel`) and its recent events already sit above it from
the thesis panel; this adds the session and data half the spec's Details tab
lists, without a second fetch.

- [ ] **Step 7: Paint both tabs from one event**

Nine edits to `views/shell.js`.

1. Next to `const el = (id) => document.getElementById(id);` (`3249`), add:

```js
  const paint = (id, text) => {
    const node = el(id);
    if (node) node.textContent = text;
  };
```

2. After `DOCK_MIN_HEIGHT` (`3230`), add the one table both the status bar and
   the Details tab read:

```js
  /// How each feed state reads in words and as a dot tone. One table, so the
  /// same state cannot be called two things on one screen.
  const FEED_CLAIMS = {
    open: ["Live", "live"],
    paused: ["Paused", "wait"],
    connecting: ["Connecting", "wait"],
    idle: ["Connecting", "wait"],
    nofeed: ["No feed", "wait"],
    offline: ["Offline", "down"],
  };
```

3. Replace the inline map in `refreshStatus` (`3409-3416`) with:

```js
    const claim = FEED_CLAIMS[(info && info.feed) || "offline"] || ["Offline", "down"];
```

4. Insert after `refreshStatus` closes (`3423`):

```js
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
```

5. In `showDestination`, change the `refreshStatus();` call after
   `showPanes();` (`3397`) to `refreshPane();`.

6. In `showPanes` (`3300`), before the `managed()` loop's closing brace, add
   the panel that is not a `.pane`:

```js
    // The Indicator tab has no second copy of anything to move: it is drawn
    // from the active chart on every event. The loop above only walks `.pane`s,
    // so its visibility is set here -- without this the tab opens onto an
    // empty inspector body.
    const projection = el("inspectorIndicator");
    if (projection) {
      projection.hidden = !(
        state.destination === "charts" &&
        state.inspectorOpen &&
        state.inspectorTab === "indicator"
      );
    }
```

7. In `runDestinationLoaders` (`3532`), add the charts branch, so the rail
   prices itself exactly while it is on screen -- the shell knows where the
   user is, and the ticker should not keep running behind Studio:

```js
    if (state.destination === "charts") load("watchlist");
    else if (state.destination === "studio") load(state.studioPane);
```

8. In `wire()`, replace `document.addEventListener("hrz:pane", refreshStatus);`
   (`3889`) with:

```js
    document.addEventListener("hrz:pane", refreshPane);
```

9. In `wire()`, after the dock grip block, wire the tab's three buttons:

```js
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
    // chart, not a list owned by this layer. Task 13 fills the tab; until
    // then it opens the dock's placeholder.
    const alerts = el("indicatorAlerts");
    if (alerts) alerts.addEventListener("click", () => openDock("alerts"));
```

- [ ] **Step 8: Style the rail and the two tabs**

In `charts.css`, replace the watchlist rules (every rule whose selector starts
with `#sideWatch`, `.wl`, `.watchRow`, `.star`, `.libCard` or `.libFilters`)
with:

```css
/* ---- The watchlist rail --------------------------------------------------- */
#sideWatch {
  display: flex; flex-direction: column; min-height: 0;
  width: var(--watch-w, 300px); flex: 0 0 var(--watch-w, 300px);
  position: relative;
  background: var(--bg-panel); border-right: 1px solid var(--line);
}
#sideWatch .pane {
  flex: 1 1 0; min-height: 0;
  display: flex; flex-direction: column; overflow: hidden;
  padding: 0;
}
#wlRows { flex: 1 1 auto; min-height: 0; overflow-y: auto; }

.wlSearch {
  flex: 0 0 auto; position: relative;
  display: flex; align-items: center; gap: var(--space-1);
  padding: var(--space-2);
  /* Clear of the rail's own hide control, which floats at the top-right. */
  padding-right: calc(var(--space-2) + 20px);
  border-bottom: 1px solid var(--line);
}
.wlSearch__icon {
  position: absolute; left: calc(var(--space-2) + 6px);
  display: inline-flex; color: var(--text-3); pointer-events: none;
}
.wlSearch__icon svg { width: 13px; height: 13px; }
.wlSearch .hrz-input {
  flex: 1 1 auto; min-width: 0;
  height: 26px; padding-left: 24px;
  font-size: var(--text-xs);
}
.wlSearch .hrz-btn { flex: 0 0 auto; }

.wlTags { display: flex; flex-wrap: wrap; gap: var(--space-1); padding: var(--space-1) var(--space-2); }
.wlTag {
  padding: 2px var(--space-2);
  border: 1px solid var(--line-strong); border-radius: 999px;
  background: transparent; color: var(--text-2);
  font: var(--text-2xs)/1.4 var(--font-ui);
  cursor: pointer;
}
.wlTag:hover { color: var(--text-1); border-color: var(--accent-line); }

.wlHead {
  flex: 0 0 auto;
  display: flex; align-items: center; justify-content: space-between; gap: var(--space-2);
  padding: var(--space-1) var(--space-2);
  border-bottom: 1px solid var(--line);
  color: var(--text-3); font-size: var(--text-2xs);
}

.watchRow { display: flex; align-items: stretch; border-left: 3px solid transparent; }
.watchRow[data-dir="up"] { border-left-color: var(--up); }
.watchRow[data-dir="down"] { border-left-color: var(--down); }
.watchRow:hover { background: var(--bg-raised); }
.watchRow.active { background: var(--accent-soft); }
.watchRowMain {
  flex: 1 1 auto; min-width: 0; height: 28px;
  display: grid; grid-template-columns: minmax(0, 1fr) auto 60px;
  align-items: center; gap: var(--space-2);
  padding: 0 var(--space-2) 0 var(--space-1);
  border: 0; background: transparent; color: var(--text-1);
  font: var(--text-xs)/1 var(--font-mono); text-align: left;
  cursor: pointer;
}
.watchRowMain:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
.watchRow .star {
  flex: 0 0 auto; width: 20px;
  border: 0; background: transparent; color: var(--text-3);
  font-size: var(--text-sm); cursor: pointer;
}
.watchRow .star[aria-pressed="true"] { color: var(--warn); }
.wlSym { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-1); }
.wlPrice { color: var(--text-2); text-align: right; }
.wlChange { text-align: right; }
.wlChange.up { color: var(--up); }
.wlChange.down { color: var(--down); }

/* ---- The Library at the rail's foot --------------------------------------- */
#sideWatch .libCard { flex: 0 0 auto; border-top: 1px solid var(--line); }
.libToggle {
  width: 100%; height: 30px;
  display: flex; align-items: center; gap: var(--space-2);
  padding: 0 var(--space-2);
  border: 0; background: transparent; color: var(--text-1);
  font: 600 var(--text-xs)/1 var(--font-ui); text-align: left;
  cursor: pointer;
}
.libToggle:hover { background: var(--bg-raised); }
.libToggle__title { flex: 1 1 auto; }
.libToggle [data-icon] { display: inline-flex; color: var(--text-3); transition: transform 120ms ease; }
.libToggle.is-collapsed [data-icon] { transform: rotate(-90deg); }
#libBody { display: flex; flex-direction: column; min-height: 0; max-height: 240px; overflow: auto; }
.libFilters { display: flex; flex-wrap: wrap; gap: var(--space-1); padding: var(--space-1) var(--space-2); }
.libFilters button {
  padding: 2px var(--space-2);
  border: 1px solid var(--line-strong); border-radius: 999px;
  background: transparent; color: var(--text-2);
  font: var(--text-2xs)/1.4 var(--font-ui);
  cursor: pointer;
}
.libFilters button:hover { color: var(--text-1); border-color: var(--accent-line); }
.libFilters button.on { background: var(--accent-soft); border-color: var(--accent-line); color: var(--accent); }
```

Two rules matter and are easy to lose: `#libBody` needs `max-height` or a long
library eats the market list, and `#indicatorProjection` must not set `display`
without guarding `[hidden]` -- an ID selector beats the UA's `[hidden]` rule,
so the next block uses `:not([hidden])`.

In `inspector.css`, append:

```css
/* ---- The Indicator projection -------------------------------------------- */
#indicatorProjection:not([hidden]) { display: flex; flex-direction: column; gap: var(--space-3); }
.indicatorName { margin: 0; color: var(--text-1); font: 600 var(--text-sm)/1.3 var(--font-ui); }
.inspectorFacts { display: grid; gap: var(--space-1); margin: 0; }
.inspectorFacts > div { display: grid; grid-template-columns: 84px 1fr; gap: var(--space-2); align-items: baseline; }
.inspectorFacts dt { color: var(--text-3); font-size: var(--text-xs); }
.inspectorFacts dd { margin: 0; font-family: var(--font-mono); font-size: var(--text-xs); color: var(--text-2); }
.inspectorNote { margin: 0; color: var(--text-3); font-size: var(--text-xs); line-height: 1.5; }
.inspectorActions { display: flex; flex-wrap: wrap; gap: var(--space-2); }

/* ---- The Details tab's session facts ------------------------------------- */
.detailsFacts { margin-top: var(--space-4); padding-top: var(--space-3); border-top: 1px solid var(--line); }
.detailsFacts__list { display: grid; gap: var(--space-1); margin: var(--space-2) 0 0; }
.detailsFacts__list > div { display: grid; grid-template-columns: 84px 1fr; gap: var(--space-2); align-items: baseline; }
.detailsFacts__list dt { color: var(--text-3); font-size: var(--text-xs); }
.detailsFacts__list dd { margin: 0; font-family: var(--font-mono); font-size: var(--text-xs); color: var(--text-2); }
```

Every colour is a token. The hardcoded-colour guard from Task 3 must still
return only `tokens.css`.

- [ ] **Step 9: Teach the harness**

1. Give the session a readable email. Replace
   `window.localStorage.setItem(tokenKey, "a-token-for-the-harness");` (`824`):

```js
// JWT-shaped, not opaque: `HRZApp.email()` reads the payload's `sub`, and an
// opaque token makes every email read empty -- which is how the Details tab's
// account row would sit at "—" and nobody would notice. Nothing checks the
// signature, in the page or in the mocks.
const payload = Buffer.from(JSON.stringify({ sub: "harness@example.test" })).toString("base64");
window.localStorage.setItem(tokenKey, `header.${payload}.signature`);
```

2. Answer the rail's ticker. Insert after the `/candles` branch (`641`):

```js
  // One rising and one falling, so the rail's edge tint is exercised in both
  // directions by a single render.
  if (url.startsWith("/tickers")) {
    const asked = (queryOf(url).get("symbols") ?? "BTCUSDT,ETHUSDT").split(",").filter(Boolean);
    return json(200, {
      tickers: asked.map((symbol, index) => ({
        symbol,
        last_price: basePriceFor(symbol) + 10,
        price_change_percent: index % 2 === 0 ? 1.5 : -2.25,
      })),
    });
  }
```

3. Answer the workspace the Indicator checks attach. Beside the `/tickers`
   mock -- the revision path first, because it is the longer prefix:

```js
  if (url === "/indicator-workspaces/ws-1/revisions/rev-1") {
    return json(200, {
      preview: {
        name: "Liquidity Sweep",
        concepts: [{ name: "liquidity sweep" }],
        zones: [{ id: "z1" }, { id: "z2" }],
        markers: [{ id: "m1" }],
      },
    });
  }
  if (url === "/indicator-workspaces/ws-1") {
    return json(200, {
      id: "ws-1",
      name: "Liquidity Sweep",
      symbol: "BTCUSDT",
      active_revision_id: "rev-1",
    });
  }
```

4. Drive both surfaces. Insert after Task 11's final check
   (`check("and the bar restored the chart the later checks assume", …)`):

```js
// --- the watchlist and the inspector -----------------------------------------
//
// Both are projections of the active chart. The checks drive the real controls
// and read the pane back, for the same reason the bar checks do.

console.log("\nthe watchlist rail");

const watchRows = () => [...document.querySelectorAll("#wlRows .watchRow")];
check(
  "the rail lists the venue, priced",
  watchRows().length === 2 &&
    document.getElementById("wlCount").textContent.includes("whole venue"),
  `${watchRows().length} rows · ${document.getElementById("wlCount").textContent}`
);
check(
  "a rising row is tinted up and a falling row down",
  watchRows()[0].dataset.dir === "up" && watchRows()[1].dataset.dir === "down",
  watchRows().map((row) => row.dataset.dir).join(",")
);
check(
  "and the source line says how old the prices are",
  /^prices \d+s ago$/.test(document.getElementById("wlSource").textContent),
  document.getElementById("wlSource").textContent
);

check(
  "the Library starts open",
  document.getElementById("libToggle").getAttribute("aria-expanded") === "true" &&
    document.getElementById("libBody").hidden === false,
  `expanded=${document.getElementById("libToggle").getAttribute("aria-expanded")}`
);
document.getElementById("libToggle").click();
check(
  "collapsing it keeps the header and the count",
  document.getElementById("libBody").hidden === true &&
    document.getElementById("libToggle").getAttribute("aria-expanded") === "false" &&
    document.getElementById("libToggle").textContent.includes("Library"),
  `hidden=${document.getElementById("libBody").hidden}`
);
document.getElementById("libToggle").click();

// Alt-click is "beside, not instead": the chart being read stays put.
const panesBefore = paneNodes().length;
const ethRow = watchRows().find(
  (row) => row.querySelector(".wlSym").textContent === "ETHUSDT"
);
ethRow.querySelector(".watchRowMain").dispatchEvent(
  new window.MouseEvent("click", { bubbles: true, altKey: true })
);
await waitFor(() => paneNodes().length === panesBefore + 1, "the pane Alt-click opens");
check(
  "Alt-click on a row opens that symbol in a new pane",
  selectIn(paneNodes().length - 1, "symbol") === "ETHUSDT",
  selectIn(paneNodes().length - 1, "symbol")
);
check(
  "and the new pane is the active one",
  paneNodes()[paneNodes().length - 1].classList.contains("active")
);
paneNodes()[paneNodes().length - 1].querySelector(".close").click();
await settle();
check(
  "closing it returns to the first chart",
  paneNodes().length === panesBefore && paneNodes()[0].classList.contains("active"),
  `${paneNodes().length} panes`
);

console.log("\nthe inspector projections");

document.querySelector('#inspectorTabs [data-inspector="indicator"]').click();
await settle();
check(
  "the Indicator tab opens and says nothing is attached",
  document.getElementById("inspectorIndicator").hidden === false &&
    document.getElementById("indicatorProjectionEmpty").hidden === false &&
    document.getElementById("indicatorProjection").hidden === true,
  `panel=${document.getElementById("inspectorIndicator").hidden} empty=${document.getElementById("indicatorProjectionEmpty").hidden}`
);

const attached = await window.HRZApp.attachWorkspace("ws-1");
await settle();
check(
  "attaching projects the indicator into the tab",
  document.getElementById("indicatorProjection").hidden === false &&
    document.getElementById("indicatorName").textContent === "Liquidity Sweep" &&
    document.getElementById("indicatorConcepts").textContent === "1" &&
    document.getElementById("indicatorZones").textContent === "2" &&
    document.getElementById("indicatorMarkers").textContent === "1" &&
    document.getElementById("barIndicator").hidden === false,
  `${attached} · ${document.getElementById("indicatorName").textContent} / ${document.getElementById("indicatorZones").textContent} zones`
);

document.getElementById("indicatorDetach").click();
await settle();
check(
  "Detach removes it from the pane, the bar and the tab together",
  document.getElementById("indicatorProjection").hidden === true &&
    document.getElementById("indicatorProjectionEmpty").hidden === false &&
    document.getElementById("barIndicator").hidden === true &&
    paneNode().querySelector(".indicatorChip").hidden === true,
  `bar hidden=${document.getElementById("barIndicator").hidden} projection hidden=${document.getElementById("indicatorProjection").hidden}`
);

document.querySelector('#inspectorTabs [data-inspector="details"]').click();
await settle();
check(
  "the Details tab states the chart, its bars and its account",
  document.getElementById("dfChart").textContent === "BTCUSDT · 5m" &&
    document.getElementById("dfBars").textContent === "500 bars" &&
    document.getElementById("dfAccount").textContent === "harness@example.test",
  `${document.getElementById("dfChart").textContent} · ${document.getElementById("dfBars").textContent} · ${document.getElementById("dfAccount").textContent}`
);
```

The Indicator empty-state check is also the check for the visibility bug Task
9's `showPanes` had: the panel is not a `.pane`, so without Step 7.6 the tab
opens on an empty inspector body and every later assertion here fails at the
first line of the check.

- [ ] **Step 10: Run it**

```bash
node --check frontend/app/app.js
node --check frontend/app/views/shell.js
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
cargo test -p api-gateway --test packaging
```

Expected: all pass. If the rail shows `—` prices and no tint, `/tickers` is
still answered by the empty fallback -- the mock must sit above it in the same
`fetch`. If the Indicator tab is blank, `refreshPane` never ran or the
`#inspectorIndicator` visibility rule is missing.

Open the gateway at 1440×900 signed in, on Charts, and check by hand:

1. The rail opens on the whole venue, rows are 28px, and each row's left edge
   is green or red for the last 24 hours.
2. Type `sol`: the list narrows, the count says how many of the indexed
   symbols matched, and × returns to the venue.
3. Click a row: the active chart moves. Alt-click another row: a second chart
   appears on that symbol and becomes the active one, and the first chart has
   not moved.
4. Collapse the Library: the header and its count remain; expand it again and
   the filters still work.
5. Inspector → Indicator with nothing attached: the empty state names both
   ways to attach. Attach from the bar's Attach menu: the tab fills in, and
   detaching from any of the three chips (pane head, bar, tab) clears all
   three.
6. Switch the chart to 1h: the Indicator note follows to BTCUSDT 1h.
7. Inspector → Details: chart, bars, feed, account and plan read correctly;
   the watcher and the thesis above them are untouched.
8. Switch to Studio and back: the rail's prices are fresh on return (the
   shell started the ticker), and the inspector is on the tab it was left on.
9. Light theme: the tints, the filter chips and the Library header stay
   legible.
10. With VoiceOver, a row announces its symbol, price, direction and the
    Alt-click hint, and the Library announces expanded/collapsed.

- [ ] **Step 11: Commit**

```bash
git add frontend/app/index.html frontend/app/app.js \
  frontend/app/views/shell.js frontend/app/styles/charts.css \
  frontend/app/styles/inspector.css tools/shell_check.mjs
git commit -m "feat: a watchlist and an inspector that follow the active chart"
```

---

### Task 13: A dock that keeps alerts, orders and the console one click away

**Files:**
- Create: `frontend/app/ui/log.js`
- Modify: `frontend/app/index.html` (the dock's three panels, the log script)
- Modify: `frontend/app/views/shell.js` (persistent tab bar, panel switching,
  console paint, unread dot, loaders)
- Modify: `frontend/app/app.js` (`HRZLog` writers, the dock's alerts and orders,
  `runPaneLoaders` branches)
- Modify: `frontend/app/styles/charts.css` (the dock's panels and rows)
- Modify: `tools/shell_check.mjs` (monitor, event and portfolio mocks; the dock
  checks)

**Interfaces:**
- Consumes: Task 9's `#dock`/`#dockBody`/`DOCK_TABS`/`selectDockTab`/`openDock`/
  `closeDock`/`loadDock`/`syncDock`/`runDestinationLoaders`, Task 12's
  `refreshPane`, `pageToast`, `api`, the watch state (`watchMonitors`,
  `loadWatchMonitors`, `loadWatchEvents`, `renderWatchEvents`), the practice
  ledger (`TT_POS_KEY`, `TT_ORD_KEY`, `ttLoad`, `ttSave`, `ttState.marks`,
  `ttRefreshMarks`), `Shell.go`.
- Produces: `window.HRZLog` (`push`/`info`/`warn`/`error`/`entries`/`clear`),
  the `hrz:log` event, `#dockAlerts`/`#dockOrders`/`#dockConsole` with their
  row hosts (`#dockMonitorRows`, `#dockEventRows`, `#dockPosRows`,
  `#dockOrdRows`, `#consoleRows`), `runPaneLoaders("alerts"|"orders")`,
  `ttClosePosition`, `ttCancelOrder`, and the `.dockPanel`/`.dockRow`/
  `.consoleRow` classes.

**Why these three together:** they are one surface with one lifecycle. Alerts
and Orders are projections of state the app owns -- the server's monitors and
the local practice ledger -- and the Console is the record of what the app did.
They share the tab bar's presence, the open/close state and the remembered
height, so splitting them would leave the same dock half-invented in three
places. The one decision that differs is where each paints from: the shell
paints the Console (it is the shell's own log), and `app.js` paints the other
two (only it can reach the monitors and the ledger).

- [ ] **Step 1: Create the log**

Create `frontend/app/ui/log.js`:

```js
/// The client event log.
///
/// An in-memory ring of the last 200 lines, written by the paths that already
/// know something happened -- a refused request, a market channel that dropped,
/// a fill, a lost save -- and read by the dock's Console tab. Nothing is
/// persisted: a log that survives a reload is a stale log offered as evidence,
/// and the one question a console answers ("what just happened") is about this
/// session.
///
/// `hrz:log` is the only channel out. The dock listens; nothing keeps a
/// callback list it can forget to drain. `clear()` dispatches the same event
/// with a null entry, so a view repaints and a badge does not.
(() => {
  const LIMIT = 200;
  const buffer = [];
  let nextId = 1;

  function push(level, message) {
    const text = String(message == null ? "" : message).trim();
    if (!text) return null;
    const entry = { id: nextId++, at: Date.now(), level, message: text };
    buffer.push(entry);
    // Oldest first out: the newest lines are the ones a user opens the console
    // for, and a ring that dropped the newest would be the wrong way round.
    if (buffer.length > LIMIT) buffer.splice(0, buffer.length - LIMIT);
    document.dispatchEvent(new CustomEvent("hrz:log", { detail: { entry } }));
    return entry;
  }

  window.HRZLog = {
    push,
    info: (message) => push("info", message),
    warn: (message) => push("warn", message),
    error: (message) => push("error", message),
    /// A copy: the dock renders from it, and a holder of the array could
    /// otherwise mutate history the console is showing.
    entries: () => buffer.slice(),
    clear() {
      buffer.length = 0;
      document.dispatchEvent(new CustomEvent("hrz:log", { detail: { entry: null } }));
    },
  };
})();
```

- [ ] **Step 2: Write to it from the paths that know something happened**

Four edits to `app.js`. Each site already knows the fact; the log is a record,
not a second place where anything is decided.

1. `api()` -- log the refusal before it is thrown (`app.js:244`, immediately
   before `throw thrown;`):

```js
    if (window.HRZLog) {
      HRZLog.error(`${options.method || "GET"} ${path} — ${message}`);
    }
```

2. `pageToast()` -- a toast is the app telling the user something, which is
   exactly what a console records. After `if (!message) return;` (`7893`):

```js
  // One message, logged once: `api` has just written the same refusal with its
  // method and path, and repeating the toast's wording an instant later would
  // make every error look like two.
  const last = window.HRZLog ? HRZLog.entries().slice(-1)[0] : null;
  if (window.HRZLog && !(last && last.message.endsWith(message))) HRZLog.info(message);
```

3. `notifyMarket()` (`7608`) -- the one place every channel lifecycle event
   passes, and the only one that knows the channel's key. At the top of the
   function, before the listener loop:

```js
  if (window.HRZLog) {
    // `data` is excluded on purpose: one line per candle frame would fill the
    // ring before the first reconnect. The events worth keeping are the edges.
    if (event.type === "open") HRZLog.info(`market ${channel.key} connected`);
    else if (event.type === "offline") HRZLog.warn(`market ${channel.key} closed`);
    else if (event.type === "notice") HRZLog.warn(`market ${channel.key}: ${event.message}`);
    else if (event.type === "lagged") HRZLog.warn(`market ${channel.key} lagged; resyncing`);
  }
```

4. `saveWorkspaceState()`'s `catch` -- Task 9 left the failure silent on
   purpose. It stays silent to the user, because a lost save mid-drag is not
   worth a toast, but it is exactly what a console is for:

```js
  } catch {
    // A lost save is not worth a toast mid-drag; the next change retries.
    if (window.HRZLog) HRZLog.warn("workspace save failed — the next change retries");
    if (window.Shell) Shell.setSaveState("failed", "Save failed");
  }
```

- [ ] **Step 3: Give the dock its panels**

In `index.html`, replace Task 8's `<div class="dockBody" id="dockBody"></div>`
with:

```html
  <div class="dockBody" id="dockBody">
    <!--
      Alerts and Orders are projections, not stores. The monitors live on the
      server and the practice ledger lives in localStorage; both are painted
      here from that state, so a pause pressed in the dock and a pause pressed
      in the chart's thesis cannot disagree about what is watched.
    -->
    <div class="dockPanel" id="dockAlerts" hidden>
      <section class="dockSection">
        <div class="dockSectionHead">
          <h2>Watched scopes</h2>
          <span id="dockMonitorCount" class="muted"></span>
        </div>
        <div class="dockRows" id="dockMonitorRows"></div>
        <p class="empty" id="dockMonitorEmpty" hidden>
          Nothing watched yet — open a chart and press Watch in its thesis.
        </p>
      </section>
      <section class="dockSection">
        <div class="dockSectionHead"><h2>Recent touches</h2></div>
        <div class="dockRows" id="dockEventRows"></div>
        <p class="empty" id="dockEventEmpty" hidden>No zone touches yet.</p>
      </section>
    </div>
    <div class="dockPanel" id="dockOrders" hidden>
      <section class="dockSection">
        <div class="dockSectionHead">
          <h2>Paper positions</h2>
          <span id="dockPosCount" class="muted"></span>
          <span class="spacer"></span>
          <button id="dockOpenTicket" class="hrz-btn hrz-btn--ghost hrz-btn--sm" type="button">Open ticket</button>
        </div>
        <div class="dockRows" id="dockPosRows"></div>
        <p class="empty" id="dockPosEmpty" hidden>No open positions.</p>
      </section>
      <section class="dockSection">
        <div class="dockSectionHead">
          <h2>Resting orders</h2>
          <span id="dockOrdCount" class="muted"></span>
        </div>
        <div class="dockRows" id="dockOrdRows"></div>
        <p class="empty" id="dockOrdEmpty" hidden>No resting orders.</p>
      </section>
    </div>
    <div class="dockPanel dockConsole" id="dockConsole" hidden>
      <div class="dockSectionHead">
        <h2>Console</h2>
        <span class="spacer"></span>
        <button id="consoleClear" class="hrz-btn hrz-btn--ghost hrz-btn--sm" type="button">Clear</button>
      </div>
      <ol class="consoleRows" id="consoleRows"></ol>
      <p class="empty" id="consoleEmpty">Nothing logged yet.</p>
    </div>
  </div>
```

`#pane-indicator` is not in this markup because the shell moves it in: it is
the Studio tab's body, and there is only ever one builder.

- [ ] **Step 4: Load the log**

In `index.html`, after the components script and before the views:

```html
<script src="/assets/ui/components.js"></script>
<script src="/assets/ui/log.js"></script>
<script src="/assets/views/shell.js"></script>
```

The order matters twice: `app.js` writes to `HRZLog` from `api` and `pageToast`,
and the shell listens for `hrz:log`, so the log has to exist before either
file's first call. The packaging test requires every file under `ui/` to be
referenced, so a missing tag fails there rather than at runtime.

- [ ] **Step 5: Paint alerts and orders**

Six edits to `app.js`. Both panels are rebuilt from state on every paint; none
of them keeps a view-model of its own.

1. Above `paintWatchToggle` (`9342`), add the latest events and the dock's
   alert paint:

```js
/// The last events feed read, kept for the dock. The thesis watcher renders
/// the same list; keeping the array means the dock paints from what the
/// poller already fetched rather than asking the server a second time.
let watchEventsLatest = [];

/// The dock's Alerts tab: the server's monitors and their recent touches.
///
/// Rebuilt whole on every paint. The list is small and the server is the only
/// source, so a diff would be more state than it saves -- and a row that kept
/// its own DOM could show "Watching" after a pause landed.
function paintDockAlerts() {
  const rows = el("dockMonitorRows");
  if (!rows) return;
  rows.innerHTML = watchMonitors.map((m) => `<div class="dockRow dockRow--alert">
      <span class="mono">${escapeHtml(m.symbol)} <span class="muted">${escapeHtml(m.timeframe)}</span></span>
      <span class="${m.enabled ? "up" : "muted"}">${m.enabled ? "Watching" : "Paused"}</span>
      <button type="button" class="hrz-btn hrz-btn--ghost hrz-btn--sm"
              data-monitor-toggle="${escapeHtml(m.id)}" data-enabled="${m.enabled ? "true" : "false"}">
        ${m.enabled ? "Pause" : "Resume"}
      </button>
      <button type="button" class="hrz-btn hrz-btn--ghost hrz-btn--sm"
              data-monitor-delete="${escapeHtml(m.id)}">Delete</button>
    </div>`).join("");
  const empty = el("dockMonitorEmpty");
  if (empty) empty.hidden = watchMonitors.length > 0;
  const count = el("dockMonitorCount");
  if (count) count.textContent = watchMonitors.length ? String(watchMonitors.length) : "";

  const events = el("dockEventRows");
  if (!events) return;
  events.innerHTML = watchEventsLatest.map((ev) => {
    const when = new Date(ev.created_at).toLocaleString([], {
      month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
    });
    return `<div class="dockRow dockRow--event">
      <span class="muted">${escapeHtml(when)}</span>
      <span class="mono">${escapeHtml(ev.symbol)} ${escapeHtml(ev.timeframe)} — ${escapeHtml(ev.reason)}</span>
      <span class="${ev.status === "done" ? "up" : "warn"}">${ev.status === "done" ? "done" : "running"}</span>
    </div>`;
  }).join("");
  const eventsEmpty = el("dockEventEmpty");
  if (eventsEmpty) eventsEmpty.hidden = watchEventsLatest.length > 0;
}

/// The loader the dock calls for its Alerts tab: paint what is known, then ask.
function loadDockAlerts() {
  paintDockAlerts();
  loadWatchMonitors();
  loadWatchEvents(false);
}
```

2. Keep the dock in step with the polls. At the end of `loadWatchMonitors`
   (`9372`), after `paintWatchToggle();`, add `paintDockAlerts();`. Then change
   `renderWatchEvents` (`9416`) so it records the list before its own guard --
   the thesis element is not what the dock depends on:

```js
function renderWatchEvents(events) {
  watchEventsLatest = events;
  const out = el("watchEvents");
  if (!out) {
    paintDockAlerts();
    return;
  }
  // … the existing empty state and row map, unchanged …
```

and add `paintDockAlerts();` as the last statement inside the non-empty path
(after the `out.innerHTML = …` assignment) so both branches paint the dock.

3. The practice ledger gains two named commands, so the ticket and the dock
   call the same code. Above `ttPaint` (`10169`), after `ttOpenPosition`:

```js
/// Close one position. The ticket's Close button and the dock's act on the
/// same row through this, so the two views cannot price a close differently.
function ttClosePosition(id) {
  const positions = ttLoad(TT_POS_KEY);
  const at = positions.findIndex((p) => p.id === id);
  if (at < 0) return;
  const [p] = positions.splice(at, 1);
  ttSave(TT_POS_KEY, positions);
  const mark = ttState.marks[p.symbol];
  const pnl = Number.isFinite(mark) ? (mark - p.entry) * p.size * (p.side === "buy" ? 1 : -1) : 0;
  pageToast(`closed ${p.symbol}: ${(pnl >= 0 ? "+$" : "-$") + Math.abs(pnl).toLocaleString("en-US", { maximumFractionDigits: 2 })}`);
  ttPaint();
}

/// Cancel a resting order. Keyed by `at`, which the ticket already stamps:
/// the ledger's entries predate any id, and a timestamp is unique among orders
/// placed by one person.
function ttCancelOrder(at) {
  const orders = ttLoad(TT_ORD_KEY);
  const index = orders.findIndex((o) => o.at === Number(at));
  if (index < 0) return;
  const [o] = orders.splice(index, 1);
  ttSave(TT_ORD_KEY, orders);
  pageToast(`cancelled ${o.side} ${o.size} ${o.symbol} @ ${fmtPrice(o.limit)}`);
  ttPaint();
}

/// The dock's Orders tab: the local practice ledger, read back.
///
/// A projection, not a second ledger. The marks are whatever the ticket last
/// read; the dock asks for a refresh when it opens, and the ticket's own loop
/// keeps them current while either surface is on screen.
function paintDockOrders() {
  const posRows = el("dockPosRows");
  if (!posRows) return;
  const positions = ttLoad(TT_POS_KEY);
  const orders = ttLoad(TT_ORD_KEY);
  let open = 0;
  posRows.innerHTML = positions.map((p) => {
    const mark = ttState.marks[p.symbol];
    const pnl = Number.isFinite(mark) ? (mark - p.entry) * p.size * (p.side === "buy" ? 1 : -1) : null;
    open += pnl || 0;
    return `<div class="dockRow dockRow--order">
      <span class="mono">${escapeHtml(p.symbol)}</span>
      <span class="${p.side === "buy" ? "up" : "down"}">${p.side === "buy" ? "LONG" : "SHORT"} ${p.leverage}×</span>
      <span>${p.size}</span>
      <span>${fmtPrice(p.entry)}</span>
      <span class="${pnl === null ? "muted" : pnl >= 0 ? "up" : "down"}">${pnl === null ? "—" : (pnl >= 0 ? "+$" : "-$") + Math.abs(pnl).toFixed(2)}</span>
      <button type="button" class="hrz-btn hrz-btn--ghost hrz-btn--sm" data-close-pos="${escapeHtml(p.id)}">Close</button>
    </div>`;
  }).join("");
  const posEmpty = el("dockPosEmpty");
  if (posEmpty) posEmpty.hidden = positions.length > 0;
  const posCount = el("dockPosCount");
  if (posCount) {
    posCount.textContent = positions.length
      ? `${positions.length} · ${open >= 0 ? "+" : "-"}$${Math.abs(open).toFixed(2)}`
      : "";
  }

  const ordRows = el("dockOrdRows");
  if (!ordRows) return;
  ordRows.innerHTML = orders.map((o) => `<div class="dockRow dockRow--order">
      <span class="mono">${escapeHtml(o.symbol)}</span>
      <span class="${o.side === "buy" ? "up" : "down"}">${o.side === "buy" ? "BUY" : "SELL"} ${o.leverage}×</span>
      <span>${o.size}</span>
      <span>${fmtPrice(o.limit)}</span>
      <span class="muted">${fmtPrice(ttState.marks[o.symbol])}</span>
      <button type="button" class="hrz-btn hrz-btn--ghost hrz-btn--sm" data-cancel-order="${o.at}">Cancel</button>
    </div>`).join("");
  const ordEmpty = el("dockOrdEmpty");
  if (ordEmpty) ordEmpty.hidden = orders.length > 0;
  const ordCount = el("dockOrdCount");
  if (ordCount) ordCount.textContent = orders.length ? String(orders.length) : "";
}

/// The loader the dock calls for its Orders tab. Marks first: a table of P&L
/// that is three seconds stale is fine, but one that is empty until the next
/// tick is not.
function loadDockOrders() {
  paintDockOrders();
  ttRefreshMarks();
}
```

4. Keep the two views in step. Add `paintDockOrders();` as the last statement
   of `ttPaint` (`10199`), and replace the body of the ticket's close delegate
   (`10230-10243`) with:

```js
  el("ttPosRows").addEventListener("click", (e) => {
    const btn = e.target.closest(".tt-close");
    if (btn) ttClosePosition(btn.dataset.pos);
  });
```

Then widen the mark loop's condition (`10247`), so the dock's P&L lives while
the ticket's pane is hidden:

```js
  if (!ttTimer) {
    ttTimer = setInterval(() => {
      const dockOrders = el("dockOrders");
      if (!el("pane-trade").hidden || (dockOrders && !dockOrders.hidden)) ttRefreshMarks();
    }, 3000);
  }
```

5. The dock's own interactions. Below `wireWatchlist` (`12422`), add:

```js
/// One delegated listener per dock table: the rows are rebuilt on every poll,
/// and a listener per row would be re-attached as often as the data changes.
function wireDock() {
  const monitors = el("dockMonitorRows");
  if (monitors) {
    monitors.addEventListener("click", (event) => {
      const toggle = event.target.closest("[data-monitor-toggle]");
      if (toggle) {
        const enabled = toggle.dataset.enabled !== "true";
        api("/watch-monitors/" + encodeURIComponent(toggle.dataset.monitorToggle), {
          method: "PATCH",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ enabled }),
        }).then(loadWatchMonitors).catch((error) => pageToast(error.message));
        return;
      }
      const remove = event.target.closest("[data-monitor-delete]");
      if (remove) {
        api("/watch-monitors/" + encodeURIComponent(remove.dataset.monitorDelete), {
          method: "DELETE",
        }).then(loadWatchMonitors).catch((error) => pageToast(error.message));
      }
    });
  }
  const positions = el("dockPosRows");
  if (positions) {
    positions.addEventListener("click", (event) => {
      const close = event.target.closest("[data-close-pos]");
      if (close) ttClosePosition(close.dataset.closePos);
    });
  }
  const orders = el("dockOrdRows");
  if (orders) {
    orders.addEventListener("click", (event) => {
      const cancel = event.target.closest("[data-cancel-order]");
      if (cancel) ttCancelOrder(cancel.dataset.cancelOrder);
    });
  }
  // The dock is a summary; placing an order stays in the ticket. The button
  // goes there rather than duplicating the form in a 240px drawer.
  const ticket = el("dockOpenTicket");
  if (ticket) {
    ticket.addEventListener("click", () => {
      if (window.Shell) Shell.go("charts", { inspectorTab: "trade" });
    });
  }
}
```

6. Teach the loader its two new names. In `runPaneLoaders` (Task 9's version),
   add before the `watchlist` branch:

```js
  if (name === "alerts") loadDockAlerts();
  if (name === "orders") loadDockOrders();
```

In `main()`, after `wireWatchlist();` (`13408`), add `wireDock();`.

Neither name collides with a pane: the loader is keyed by the old pane names,
and the dock is the only caller that asks for these two.

- [ ] **Step 6: Make the tab bar persistent**

Five edits to `views/shell.js`.

1. Replace `DOCK_PLACEHOLDER` (`3225`) with the map of real panels:

```js
  /// The dock tabs whose body is a panel in `#dockBody`. Studio has no entry:
  /// its body is `pane-indicator`, moved in by `mountIndicator`.
  const DOCK_PANELS = { alerts: "dockAlerts", orders: "dockOrders", console: "dockConsole" };
```

2. In `mount()`, delete the block that creates `#dockPanel` (`3279-3286`) --
   the placeholder has no tab left.

3. Replace `syncDock` (`3359`) with:

```js
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
    for (const button of document.querySelectorAll("#dock [data-dock]")) {
      button.setAttribute("aria-selected", String(button.dataset.dock === state.dockTab));
    }
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
```

4. Replace `loadDock` (`3583`). Console gets its paint here because the log is
   the shell's; the other two ask the app:

```js
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
```

5. Above `syncDock` (`3359`), add the console's three functions and its state:

```js
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
```

6. In `wire()`, replace `if (collapse) collapse.addEventListener("click", closeDock);`
   (`3842`) with the toggle, and add the Console's two listeners after it:

```js
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
```

7. A restored session can open on a non-Studio dock tab. In
   `runDestinationLoaders` (Task 12's version), extend the Charts branch:

```js
    if (state.destination === "charts") {
      load("watchlist");
      // A restored workspace can open on Alerts, Orders or Console, and no tab
      // click happens in that case -- the panel would sit empty until the user
      // switched away and back.
      if (state.dockOpen) loadDock(state.dockTab);
    } else if (state.destination === "studio") {
      load(state.studioPane);
    }
```

Also update the comment Task 12 left on the Indicator tab's Alerts button
("Task 13 fills the tab; until then it opens the dock's placeholder"): the
placeholder is gone, and the button now opens a tab that has content.

- [ ] **Step 7: Style the dock**

In `charts.css`, append after Task 8's `.dockBody` rule -- and change that rule
to hold three panels rather than one pane:

```css
.dockBody { flex: 1 1 auto; min-height: 0; overflow: auto; }
.dockBody[hidden] { display: none; }
/* Shut, the dock is its tab bar: 28px, still on screen, still selectable. The
   Console's dot is only worth having if the bar stays when the body goes. */
#dock[data-open="false"] { height: 28px; flex-basis: 28px; }
#dock[data-open="false"] .dockBody,
#dock[data-open="false"] .dockResize { display: none; }
#dockCollapse [data-icon] { transition: transform 120ms ease; }
#dock[data-open="false"] #dockCollapse [data-icon] { transform: rotate(180deg); }
#dock .hrz-tab[data-unread="true"]::after {
  content: ""; display: inline-block; width: 6px; height: 6px;
  margin-left: 6px; border-radius: 50%; background: var(--accent);
  vertical-align: middle;
}

/* ---- The dock's panels ---------------------------------------------------- */
.dockPanel {
  display: flex; gap: var(--space-4);
  height: 100%; min-height: 0;
  padding: var(--space-2) var(--space-3);
  box-sizing: border-box;
}
.dockPanel[hidden] { display: none; }
.dockSection { display: flex; flex-direction: column; gap: var(--space-1); flex: 1 1 0; min-width: 0; min-height: 0; overflow: auto; }
.dockSectionHead { display: flex; align-items: center; gap: var(--space-2); flex: 0 0 22px; }
.dockSectionHead h2 {
  margin: 0; color: var(--text-3);
  font: 600 var(--text-2xs)/1 var(--font-ui);
  text-transform: uppercase; letter-spacing: .04em;
}
.dockRows { display: flex; flex-direction: column; gap: 1px; }
.dockRow {
  display: grid; align-items: center; gap: var(--space-2);
  min-height: 26px; padding: 0 var(--space-1);
  border-radius: var(--radius-1);
  font: var(--text-2xs)/1.3 var(--font-mono); color: var(--text-2);
}
.dockRow:nth-child(odd) { background: var(--bg-raised); }
.dockRow > span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dockRow .up { color: var(--up); }
.dockRow .down { color: var(--down); }
.dockRow .warn { color: var(--warn); }
.dockRow--alert { grid-template-columns: minmax(0, 1.4fr) 76px auto auto; }
.dockRow--event { grid-template-columns: 96px minmax(0, 1fr) 64px; }
.dockRow--order { grid-template-columns: minmax(84px, 1fr) 92px 56px 84px 92px auto; }

/* ---- The console ---------------------------------------------------------- */
.dockConsole { flex-direction: column; gap: var(--space-1); }
.consoleRows {
  flex: 1 1 auto; min-height: 0; margin: 0; padding: 0;
  overflow: auto; list-style: none;
  font: var(--text-2xs)/1.5 var(--font-mono);
}
.consoleRow { display: grid; grid-template-columns: 64px minmax(0, 1fr); gap: var(--space-2); }
.consoleTime { color: var(--text-3); }
.consoleMsg { color: var(--text-2); overflow-wrap: anywhere; }
.consoleRow--warn .consoleMsg { color: var(--warn); }
.consoleRow--error .consoleMsg { color: var(--down); }
```

Three rules carry the traps: `.dockBody[hidden]` must be stated, because a
class selector that sets `display` would otherwise beat the UA's `[hidden]`
rule (the same trap Task 12 hit on `#indicatorProjection`); the collapsed dock
sets a height *and* a `flex-basis`, because `#dock` is a flex item whose basis
is `auto`; and `.dockPanel` needs `height: 100%` to fill the body, which is why
the body must not scroll its panels out from under their own section
scrollbars.

- [ ] **Step 8: Teach the harness**

1. Boot the log. In the boot list of `shell_check.mjs`, after
   `"frontend/app/ui/components.js"`:

```js
  "frontend/app/ui/log.js",
```

2. Give the harness a watch state and a portfolio. Add to `backend`
   (`401`), after `meStatus: 200,`:

```js
  // The dock's three sources. Two monitors -- one enabled, one paused -- so a
  // check can tell "Pause" from "Resume" without first pressing Pause; one
  // event, so the touches list has a row and the dock's two sections are both
  // non-empty at once.
  monitors: [
    { id: "mon-1", symbol: "BTCUSDT", timeframe: "5m", enabled: true, created_at: "2026-10-27T09:00:00Z" },
    { id: "mon-2", symbol: "ETHUSDT", timeframe: "1h", enabled: false, created_at: "2026-10-26T09:00:00Z" },
  ],
  watchEvents: [
    {
      id: "ev-1", symbol: "BTCUSDT", timeframe: "5m",
      reason: "price touched the lower demand zone", status: "done",
      summary: "the agent re-read the chart and held", created_at: "2026-10-27T09:30:00Z",
    },
  ],
```

3. Answer the three routes. Insert before the `/capabilities` branch (`757`):

```js
  // The dock's Alerts tab. `PATCH` and `DELETE` store the change rather than
  // answering a fixed list: a stub that ignored either would let "Pause" pass
  // while the row still said Watching, which is the one thing this tab is for.
  if (url.startsWith("/watch-monitors")) {
    const id = url.split("/")[2];
    if (method === "PATCH") {
      const body = JSON.parse(options.body || "{}");
      const monitor = backend.monitors.find((m) => m.id === id);
      if (monitor) monitor.enabled = Boolean(body.enabled);
      return json(200, monitor || {});
    }
    if (method === "DELETE") {
      backend.monitors = backend.monitors.filter((m) => m.id !== id);
      return json(204, null);
    }
    return json(200, { monitors: backend.monitors });
  }
  if (url.startsWith("/watch-events")) {
    return json(200, { events: backend.watchEvents });
  }
  // The dock's Orders tab reads the local ledger; this route is the ticket's
  // server-side contract list, which the tab only links to.
  if (url.startsWith("/trade/portfolio")) {
    return json(200, []);
  }
```

4. Drive the dock. Insert after Task 12's last check, the Details check:

```js
// --- the dock ----------------------------------------------------------------
//
// The tab bar is the dock when it is shut, which is what makes the Console's
// unread dot worth having. Every check below drives a real control and reads
// the panel back; none of them calls the paint functions directly.

console.log("\nthe dock");

const dockNode = () => document.getElementById("dock");
const dockTab = (name) => document.querySelector(`#dock [data-dock="${name}"]`);

check(
  "the dock's tab bar is on screen on Charts, with its body shut",
  dockNode().hidden === false && dockNode().dataset.open === "false" &&
    document.getElementById("dockBody").hidden === true,
  `hidden=${dockNode().hidden} open=${dockNode().dataset.open} body=${document.getElementById("dockBody").hidden}`
);

window.HRZLog.warn("a warning from the harness");
check(
  "a line logged while the console is shut marks its tab",
  dockTab("console").dataset.unread === "true" &&
    dockTab("console").getAttribute("aria-label").includes("1 new"),
  dockTab("console").getAttribute("aria-label")
);

dockTab("alerts").click();
await waitFor(
  () => document.querySelectorAll("#dockMonitorRows .dockRow").length === 2,
  "the monitors load into the dock"
);
check(
  "the Alerts tab opens the dock and lists the server's monitors",
  dockNode().dataset.open === "true" &&
    document.getElementById("dockAlerts").hidden === false &&
    document.getElementById("dockMonitorCount").textContent === "2" &&
    document.querySelectorAll("#dockEventRows .dockRow").length === 1,
  `${document.querySelectorAll("#dockMonitorRows .dockRow").length} monitors · ${document.querySelectorAll("#dockEventRows .dockRow").length} events`
);

document.querySelector('#dockMonitorRows [data-monitor-toggle="mon-1"]').click();
await waitFor(
  () => document.querySelector('#dockMonitorRows [data-monitor-toggle="mon-1"]').textContent.trim() === "Resume",
  "the pause reaches the row"
);
check(
  "Pause sends the PATCH and the row follows the server's answer",
  backend.calls.some((call) => call.method === "PATCH" && call.url === "/watch-monitors/mon-1") &&
    document.querySelector('#dockMonitorRows [data-monitor-toggle="mon-1"]').dataset.enabled === "false"
);

document.querySelector('#dockMonitorRows [data-monitor-delete="mon-2"]').click();
await waitFor(
  () => document.querySelectorAll("#dockMonitorRows .dockRow").length === 1,
  "the deleted monitor leaves the list"
);
check(
  "Delete sends the DELETE and the deleted scope leaves the list",
  backend.calls.some((call) => call.method === "DELETE" && call.url === "/watch-monitors/mon-2")
);

// The ledger is localStorage, so seeding it is the same act as opening a
// position -- except that no toast fires and no mark is needed for the row.
window.localStorage.setItem("paper.positions.v1", JSON.stringify([
  { id: "p1", symbol: "BTCUSDT", side: "buy", size: 1, entry: 100, leverage: 5, liq: 82, at: 1 },
]));
window.localStorage.setItem("paper.orders.v1", JSON.stringify([
  { symbol: "ETHUSDT", side: "sell", size: 2, limit: 1000000, leverage: 10, at: 2 },
]));
dockTab("orders").click();
await settle();
check(
  "the Orders tab reads the practice ledger back",
  document.getElementById("dockOrders").hidden === false &&
    document.querySelectorAll("#dockPosRows .dockRow").length === 1 &&
    document.querySelectorAll("#dockOrdRows .dockRow").length === 1 &&
    document.getElementById("dockPosCount").textContent.includes("1"),
  `${document.querySelectorAll("#dockPosRows .dockRow").length} positions · ${document.querySelectorAll("#dockOrdRows .dockRow").length} orders`
);

document.querySelector("#dockPosRows [data-close-pos]").click();
await settle();
check(
  "Close on a dock row closes the same position the ticket owns",
  document.querySelectorAll("#dockPosRows .dockRow").length === 0 &&
    document.getElementById("dockPosEmpty").hidden === false &&
    JSON.parse(window.localStorage.getItem("paper.positions.v1") || "[]").length === 0
);

document.querySelector("#dockOrdRows [data-cancel-order]").click();
await settle();
check(
  "Cancel removes the resting order",
  document.querySelectorAll("#dockOrdRows .dockRow").length === 0 &&
    JSON.parse(window.localStorage.getItem("paper.orders.v1") || "[]").length === 0
);

dockTab("console").click();
await waitFor(
  () => document.querySelectorAll("#consoleRows .consoleRow").length >= 2,
  "the console prints what was logged"
);
check(
  "the Console prints the session's log, newest last, and clears the unread mark",
  dockTab("console").dataset.unread === "false" &&
    document.getElementById("dockConsole").hidden === false &&
    document.getElementById("consoleRows").lastElementChild.textContent.includes("a warning from the harness"),
  `${document.querySelectorAll("#consoleRows .consoleRow").length} lines`
);

document.getElementById("consoleClear").click();
await settle();
check(
  "Clear empties the console and the empty state takes over",
  window.HRZLog.entries().length === 0 &&
    document.querySelectorAll("#consoleRows .consoleRow").length === 0 &&
    document.getElementById("consoleEmpty").hidden === false
);

for (let line = 0; line < 205; line += 1) window.HRZLog.info(`line ${line}`);
check(
  "the ring keeps the newest 200 lines and drops the oldest",
  window.HRZLog.entries().length === 200 &&
    window.HRZLog.entries()[199].message === "line 204"
);

document.getElementById("dockCollapse").click();
await settle();
check(
  "the chevron shuts the body and leaves the tab bar",
  dockNode().dataset.open === "false" &&
    document.getElementById("dockBody").hidden === true &&
    dockNode().hidden === false
);

window.HRZLog.warn("logged while the dock is shut");
check(
  "a line logged after the dock shuts marks the Console tab",
  dockTab("console").dataset.unread === "true"
);

if (window.Shell) Shell.go("dashboard");
await settle();
check(
  "leaving Charts takes the dock off screen",
  dockNode().hidden === true
);

if (window.Shell) Shell.go("charts");
await settle();
check(
  "returning restores the tab bar, still shut",
  dockNode().hidden === false && dockNode().dataset.open === "false"
);
```

The 205-line loop looks like noise in a harness, and it is the only way to
check a ring buffer: a 200-entry limit is invisible until line 201 arrives.

- [ ] **Step 9: Run it**

```bash
node --check frontend/app/ui/log.js
node --check frontend/app/app.js
node --check frontend/app/views/shell.js
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
cargo test -p api-gateway --test packaging
```

Expected: all pass. If the dock checks fail at the first line, `#dock` is still
`hidden` by Task 8's markup default and `syncDock` was not called on the Charts
route. If the monitor rows never arrive, `/watch-monitors` is falling through to
the generic `{}` fallback (the branch must sit above it). If the unread check
fails, `noteLog` is not listening -- `ui/log.js` must be in the boot list.

Open the gateway at 1440×900 signed in, on Charts, and check by hand:

1. The dock's tab bar sits above the status bar with the body shut. The chart
   keeps the height the shut body frees, and no line of the chart is clipped.
2. Click Studio: the builder opens at the remembered height. Double-click the
   tab bar: the height returns to 240. Drag the grip: the chart resizes with it.
3. Click Alerts: the monitors the thesis shows are the same list; Pause flips a
   row to Paused and the thesis's Watch button agrees. Delete removes it from
   both. Recent touches shows the last firing with its reason.
4. Open a paper position from the Trade tab, then look at Orders: it is there
   with a live P&L, and Close in the dock closes it and empties the ticket's
   table too. Place a resting limit order and Cancel it from the dock.
5. Open the DevTools console's Actions menu and attach an indicator while the
   Console tab is open: attaching does not log noise, but a refused request
   appears in red with its method and path.
6. Shut the dock. Cause a log line (reload the chart: the market channel logs
   a reconnect). A dot appears on Console. Open it: the dot clears, and the
   reconnect line is at the bottom.
7. Switch to Dashboard: the dock is gone, not collapsed. Return to Charts: the
   tab bar is back with the body still shut.
8. Resize the dock to its minimum: the tab bar and one row of content remain
   visible; nothing overlaps the status bar.
9. Light theme: rows, the console's warn/error colours and the dot stay legible
   against the panel.
10. With VoiceOver, the tab bar announces four tabs with the selected one
    marked, the chevron announces Expand/Collapse, and the Console announces
    how many new messages the dot stands for.

- [ ] **Step 10: Commit**

```bash
git add frontend/app/ui/log.js frontend/app/index.html \
  frontend/app/app.js frontend/app/views/shell.js \
  frontend/app/styles/charts.css tools/shell_check.mjs
git commit -m "feat: a bottom dock for alerts, orders and the console"
```

---

### Task 14: A chart that wears the theme, and a rail that names its chart

The chart is the only surface in the product that paints its own colours: a
canvas reads `fillStyle = "#2ebd85"`, not `var(--up)`. That is why it stayed on
its dark TV palette when the shell went light, and why it had no colour-blind
option at all. This task makes the palette a *derived* value -- one function of
the theme and two preferences -- and moves the drawing toolbar from a
full-width strip above the charts to a 36px rail down the left of the wall, so
the tools cost a sliver of width instead of a row of height.

**Files:**
- Modify: `tools/fetch_icons.mjs` (eight new names), regenerate `frontend/app/ui/icons.js`
- Modify: `frontend/app/app.js`
- Modify: `frontend/app/index.html`
- Modify: `frontend/app/views/instrument_bar.js`
- Modify: `frontend/app/styles/charts.css`
- Modify: `frontend/app/styles/shell.css` (the shortcut sheet's third column)
- Modify: `tools/shell_check.mjs`

**Interfaces:**
- Produces: `chartPalette(theme, settings) -> palette` and `chartSettings()` in
  `app.js`; storage keys `hrz.chart` (`{cvd, grid}`) and `hrz.coach.charts`.
- Produces: `pane.setPalette(palette)` on the pane API, and
  `HRZApp.chartPalette()`, `HRZApp.chartSettings()`,
  `HRZApp.setChartSetting(name, on)`, `HRZApp.openChartSettings()`.
- Produces: `#chartStage`, `#chartCoach` (+ `#coachKeys`, `#coachAgree`),
  `#chartSettings` (+ `#csGrid`, `#csReset`, `[data-cvd]`), `#moreCvd`,
  `#moreChartSettings`.
- Consumes: `hrz:theme` (Task 2), `#chartMore` and `InstrumentBar` (Task 11),
  `HRZPalette.shortcuts()` (Task 10), `HRZUI.icon` (Task 4), the dock's
  position under the wall (Task 13).

- [ ] **Step 1: Teach the sprite five rail glyphs and three group marks**

The rail is icon-only, and the sprite has no magnet, no cursor, no undo arrow,
no group marks. Add these to `MAP` in `tools/fetch_icons.mjs`, keeping the
object alphabetical by key:

```js
  magnet: "magnet",
  "mouse-pointer-2": "mouse-pointer-2",
  "redo-2": "redo-2",
  ruler: "ruler",
  shapes: "shapes",
  square: "square",
  "trending-up": "trending-up",
  "undo-2": "undo-2",
```

Then regenerate and check:

```bash
node tools/fetch_icons.mjs
node --check frontend/app/ui/icons.js
```

Expected: `wrote ui/icons.js with 69 icons` (61 from Task 4 plus these eight).
The generator fails loudly if a name is missing from the pinned release, which
is the point of pinning it.

- [ ] **Step 2: Give the rail and the wall a shared row, and teach the page its new chrome**

In `index.html`, three markup changes and one deletion.

**a.** Wrap `#globalTools` and `#charts` in `#chartStage`. Insert
`<div id="chartStage">` immediately before `#globalTools` (`index.html:2015`,
after Task 11's `#instrumentBar`) and its closing `</div>` immediately after
`#charts`'s closing `</div>` -- the line before `#dock`, which Task 8 inserted
there. `#globalTools` loses its `hidden` attribute: the rail is part of the
layout, and a strip that appears only once the engine has loaded shifts the
whole wall sideways a second after the page looks ready.

```html
  <div id="chartColumn">
    <div id="instrumentBar" …></div>
    <!-- The rail and the wall are one row: the rail is a column of icons down
         the left, the wall takes everything that is left. They share a wrapper
         because the dock below spans *both* -- a dock inside a row would be a
         third column. -->
    <div id="chartStage">
      <div id="globalTools" class="globalTools" role="toolbar"
           aria-orientation="vertical" aria-label="Drawing tools for the active chart"></div>
      <div id="charts">
        …
      </div>
      <!-- The gesture coach (step 2c) sits here, over the wall and clear of
           the dock, because the wall's own overflow rules must not clip it. -->
    </div>
    <div id="dock" …></div>
  </div>
```

**b.** Delete the pane's gesture hint -- the markup and its CSS. In the pane
head, remove:

```html
        <span class="chartHint">wheel zooms · shift+wheel price · drag pans · right-click menu</span>
```

with the comment above it. The hint was one line of prose in every title bar,
at 10px, repeating what the coach (step 2c) says once and what the shortcut
sheet says on demand. Two of those three is enough; a permanent third is
furniture. Leave the rest of `.paneHead` alone.

**c.** Add the coach mark and the settings dialog. The coach goes inside
`#chartStage` after `#charts`; the dialog goes next to `#shortcuts`, **outside**
`#frame` -- Task 10's own note applies: a dialog inside a hidden destination
cannot be shown modally, and `#dest-charts` is hidden on every other route.

```html
      <div id="chartCoach" class="chartCoach" role="note" aria-label="Chart gestures" hidden>
        <p class="coachTitle">This chart answers to the mouse</p>
        <p class="coachBody">
          Scroll to zoom the time axis, <span class="coachKey">shift</span> + scroll
          to stretch price, drag to pan, right-click for this chart's own menu.
        </p>
        <div class="coachFoot">
          <button id="coachKeys" class="hrz-btn hrz-btn--ghost hrz-btn--sm" type="button">See all gestures</button>
          <button id="coachAgree" class="hrz-btn hrz-btn--secondary hrz-btn--sm" type="button">Got it</button>
        </div>
      </div>
```

```html
<!--
  Chart appearance. A modal dialog rather than a panel: it changes the chart
  the user is looking at, and the answer is visible behind it -- there is
  nothing to navigate to and nothing to lose. Two settings, because they are
  the two that change what the plot *is* rather than what it holds.
-->
<dialog id="chartSettings" class="chartSettings" aria-labelledby="chartSettingsTitle">
  <form method="dialog" class="csForm">
    <header class="csHead">
      <h2 id="chartSettingsTitle">Chart settings</h2>
      <button class="hrz-btn hrz-btn--ghost hrz-btn--sm hrz-btn--icon"
              value="close" aria-label="Close chart settings">×</button>
    </header>
    <div class="csRow">
      <span id="csCvdLabel">Candle colours</span>
      <span class="hrz-seg hrz-seg--sm" role="group" aria-labelledby="csCvdLabel">
        <button type="button" class="hrz-seg__item" data-cvd="false" aria-pressed="true">Green / red</button>
        <button type="button" class="hrz-seg__item" data-cvd="true" aria-pressed="false">Blue / orange</button>
      </span>
    </div>
    <label class="csRow">
      <span>Grid lines</span>
      <input type="checkbox" id="csGrid" />
    </label>
    <footer class="csFoot">
      <button type="button" id="csReset" class="hrz-btn hrz-btn--ghost hrz-btn--sm">Reset to defaults</button>
      <button class="hrz-btn hrz-btn--primary hrz-btn--sm" value="done">Done</button>
    </footer>
  </form>
</dialog>
```

**d.** Add the two new items to the **top** of `#chartMore`'s `barPopList`,
before `#moreShot`. The colour-blind row stays put when pressed -- it is a
toggle you press while looking at the chart behind the panel, not a command
that ends the visit.

```html
    <button id="moreCvd" class="barPopItem" type="button" aria-pressed="false">
      <span class="barPopIcon" data-icon="eye"></span>
      <span class="barPopMain">Colour-blind candles</span>
    </button>
    <button id="moreChartSettings" class="barPopItem" type="button">
      <span class="barPopIcon" data-icon="sliders"></span>
      <span class="barPopMain">Chart settings</span>
    </button>
```

**e.** Add a Gestures column to the shortcut sheet (`#shortcuts`), after the
Chart group. These are the gestures the coach names once, written down where
they can be looked up:

```html
    <section class="shortcutGroup" aria-labelledby="shortcutGestures">
      <h3 id="shortcutGestures">Gestures</h3>
      <dl>
        <div><dt>Zoom the time axis</dt><dd>Scroll</dd></div>
        <div><dt>Stretch the price axis</dt><dd><kbd>⇧</kbd> + scroll</dd></div>
        <div><dt>Pan</dt><dd>Drag</dd></div>
        <div><dt>This chart's menu</dt><dd>Right-click</dd></div>
        <div><dt>Place or move a drawing</dt><dd>Click, then drag an anchor</dd></div>
      </dl>
    </section>
```

- [ ] **Step 3: Make the palette a function of the theme**

Everything here is module level. Insert it immediately before the comment
`/// One chart, and everything that belongs to it.` (`app.js:548`).

Cut the whole `const COLORS = { … };` literal out of `createChartPane` (the
block beginning after the `// Drawing` divider, `app.js:712-783`) and use it as
the body of `CHART_PALETTE_DARK` below, unchanged except for one added entry:
after `sell: "#f6465d",` add

```js
    // Sub-panes draw on a slightly darker field than the price plot. It is a
    // palette entry rather than a literal in `drawSubPanes` because the light
    // theme needs the opposite wash.
    subPane: "rgba(13, 17, 26, 0.65)",
```

Then write:

```js
// ---------------------------------------------------------------------------
// Chart appearance
//
// A canvas cannot read a CSS custom property -- `var(--up)` is not a colour to
// `fillStyle` -- so the plot's palette is a literal. It is a literal *derived*
// from the theme, though: one function, two preferences, one answer per pane,
// so a white shell never holds a black chart and a trader who cannot separate
// red from green never has to squint at one.
// ---------------------------------------------------------------------------
const CHART_SETTINGS_KEY = "hrz.chart";

/// The chart's display choices, from localStorage with the shipped defaults.
///
/// Deliberately *not* in the workspace snapshot: a colour-vision choice is
/// about the reader, not the layout, and restoring a colleague's shared
/// workspace must not repaint the candles green.
function chartSettings() {
  let saved = null;
  try { saved = JSON.parse(localStorage.getItem(CHART_SETTINGS_KEY) || "null"); } catch { saved = null; }
  return {
    cvd: Boolean(saved && saved.cvd),
    grid: !saved || saved.grid !== false,
  };
}

const CHART_PALETTE_DARK = {
  …the literal, with `subPane` added…
};

/// The same roles at the depth a white plot needs: the TV hue is too pale to
/// read on #ffffff (a 2px candle at #2ebd85 on white is a pencil line), so the
/// light column keeps every hue's *meaning* and darkens it to the token value
/// the chrome already uses for the same concept.
const CHART_PALETTE_LIGHT = {
  up: "#067a4d",
  down: "#d92d3f",
  wick: "#8b95a5",
  profile: "#e6ecf3",
  value: "#2563eb",
  grid: "#e7ecf2",
  text: "#5f6b7a",
  vwap: "#b45309",
  poc: "#1f2937",
  vah: "#6b7686",
  val: "#6b7686",
  entry: "#2563eb",
  stop: "#d92d3f",
  target: "#067a4d",
  level: "#6b7686",
  other: "#6b7686",
  trendline: "#2563eb",
  hline: "#b45309",
  rect: "#7c3aed",
  fib: "#0f7b6c",
  fib_extension: "#0f766e",
  vline: "#1f2937",
  ray: "#b45309",
  extended: "#6b7686",
  measure: "#2563eb",
  channel: "#0f7b6c",
  angle: "#1f2937",
  arc: "#b45309",
  circle: "#7c3aed",
  triangle: "#b45309",
  position_long: "#067a4d",
  position_short: "#d92d3f",
  dateprice_range: "#2563eb",
  footBuy: "#7c3aed",
  footSell: "#2563eb",
  footValue: "#2563eb",
  demand: "#067a4d",
  supply: "#d92d3f",
  buy: "#067a4d",
  sell: "#d92d3f",
  subPane: "rgba(226, 232, 240, 0.55)",
};

/// The colour-blind pair, in both themes.
///
/// Blue and orange survive every common form of colour-vision difference --
/// they differ in *luminance* as well as hue, which green and red do not.
/// Only the pair that carries "up or down" changes: entry, stop and target are
/// a trade's three prices and keep their own colours, because a stop is not a
/// candle and confusing the two is worse than re-learning a shade.
const CHART_CVD = {
  up: "#2962ff",
  down: "#ff6d00",
  demand: "#2962ff",
  supply: "#ff6d00",
  buy: "#2962ff",
  sell: "#ff6d00",
  position_long: "#2962ff",
  position_short: "#ff6d00",
  footBuy: "#2962ff",
  footSell: "#ff6d00",
};

/// One palette per (theme, settings). The two preferences are applied here
/// rather than at each drawing call, so "grid off" is one transparent entry
/// instead of a condition in twenty painters.
function chartPalette(
  theme = document.documentElement.dataset.theme || "dark",
  settings = chartSettings()
) {
  const palette = { ...(theme === "light" ? CHART_PALETTE_LIGHT : CHART_PALETTE_DARK) };
  if (settings.cvd) Object.assign(palette, CHART_CVD);
  if (!settings.grid) palette.grid = "rgba(0, 0, 0, 0)";
  return palette;
}
```

Then, in its old place inside `createChartPane`:

```js
  // The palette is a `let`: a theme switch or a colour-blind choice replaces it
  // on a live pane -- see `setPalette` on the pane API.
  let COLORS = chartPalette();
```

And in `drawSubPanes`, replace the literal wash `"rgba(13, 17, 26, 0.65)"` with
`COLORS.subPane`. The footprint's black ground (`app.js:819`) stays black in
both themes on purpose: its cells *are* brightness, and a light backdrop
flattens the gradient that carries the information.

- [ ] **Step 4: Let a live pane swap its palette**

In the pane API object, beside `redraw()`:

```js
    /// Swap this pane's palette and repaint. Called by the page for every pane
    /// on a theme switch or a candle-colour choice.
    setPalette(next) {
      COLORS = next;
      // The static layers are pre-rendered *in* these colours, so a palette
      // change throws the buffer away rather than merely re-blitting it -- the
      // grid would otherwise keep the previous theme's colour forever.
      staticLayerKey = null;
      if (scene) scheduleRender();
    },
```

- [ ] **Step 5: Wire the theme, the settings and the coach**

Insert after `refreshGlobalTools()` (`app.js:8019`):

```js
// ---------------------------------------------------------------------------
// Chart appearance, wired
// ---------------------------------------------------------------------------

/// Paint every pane from the current theme and settings. One place, so a theme
/// switch and a settings change cannot disagree about which pane got what.
function applyChartPalette() {
  const settings = chartSettings();
  const theme = document.documentElement.dataset.theme || "dark";
  for (const pane of panes) pane.setPalette(chartPalette(theme, settings));
  paintChartSettings(settings);
}

/// Paint the two surfaces that show the settings -- the dialog and the More
/// panel's row. Both read the store; neither holds a copy of it.
function paintChartSettings(settings = chartSettings()) {
  for (const button of document.querySelectorAll("#chartSettings [data-cvd]")) {
    button.setAttribute("aria-pressed", String((button.dataset.cvd === "true") === settings.cvd));
  }
  const grid = document.getElementById("csGrid");
  if (grid) grid.checked = settings.grid;
  const more = document.getElementById("moreCvd");
  if (more) more.setAttribute("aria-pressed", String(settings.cvd));
}

function setChartSetting(name, on) {
  const settings = chartSettings();
  settings[name] = !!on;
  try {
    localStorage.setItem(CHART_SETTINGS_KEY, JSON.stringify(settings));
  } catch { /* private mode: the choice lasts this session */ }
  applyChartPalette();
}

function openChartSettings() {
  const dialog = document.getElementById("chartSettings");
  if (!dialog) return;
  paintChartSettings();
  // jsdom implements `<dialog>`'s property but not `showModal`; a page that
  // cannot show it modally still shows it.
  if (typeof dialog.showModal === "function") {
    try { dialog.showModal(); return; } catch { /* fall through */ }
  }
  dialog.open = true;
}

function wireChartSettings() {
  const dialog = document.getElementById("chartSettings");
  if (!dialog) return;
  dialog.addEventListener("click", (event) => {
    const choice = event.target.closest("[data-cvd]");
    if (choice) setChartSetting("cvd", choice.dataset.cvd === "true");
  });
  const grid = document.getElementById("csGrid");
  if (grid) grid.addEventListener("change", () => setChartSetting("grid", grid.checked));
  const reset = document.getElementById("csReset");
  if (reset) {
    reset.addEventListener("click", () => {
      // Reset is "forget the choices", not "write the defaults": the store then
      // reads as a browser that never chose, which is the state that keeps
      // working if the defaults ever change.
      try { localStorage.removeItem(CHART_SETTINGS_KEY); } catch { /* nothing cached */ }
      applyChartPalette();
    });
  }
  paintChartSettings();
}

function wireChartTheme() {
  applyChartPalette();
  window.addEventListener("hrz:theme", () => applyChartPalette());
}

// The gesture coach. Shown once per browser, on the chart itself, because the
// gestures are the part of the product no menu can teach: a wheel that zooms
// has to be tried to be believed, and a right-click that opens a menu is the
// one place a browser habit guesses wrong.
const COACH_KEY = "hrz.coach.charts";

function maybeShowCoach() {
  const node = document.getElementById("chartCoach");
  if (!node || !activePane) return;
  let seen = false;
  try { seen = localStorage.getItem(COACH_KEY) === "1"; } catch { seen = true; }
  node.hidden = seen;
}

function dismissCoach() {
  const node = document.getElementById("chartCoach");
  if (node) node.hidden = true;
  try { localStorage.setItem(COACH_KEY, "1"); } catch { /* it may show again; nothing breaks */ }
}

function wireCoach() {
  const keys = document.getElementById("coachKeys");
  if (keys) {
    keys.addEventListener("click", () => {
      dismissCoach();
      if (window.HRZPalette) HRZPalette.shortcuts();
    });
  }
  const agree = document.getElementById("coachAgree");
  if (agree) agree.addEventListener("click", dismissCoach);
  // A pane can arrive after boot -- a restored workspace, a new chart, a
  // sign-in -- so the first activation is another chance to show it.
  document.addEventListener("hrz:pane", maybeShowCoach);
  document.addEventListener("hrz:auth", maybeShowCoach);
}
```

- [ ] **Step 6: Make the rail icon-only**

Three edits in `app.js`.

**a.** Immediately before `const globalTools = {` (`app.js:7750`), add the group
marks and the icon helper:

```js
/// One icon per tool group, for the rail. A group the engine added later falls
/// back to the generic chart mark, so a new group is an odd icon rather than a
/// blank square.
const TOOL_GROUP_ICONS = {
  lines: "trending-up",
  shapes: "shapes",
  measurement: "ruler",
  positions: "square",
};

/// An icon for the rail, with a lettered fallback: a sprite that failed to load
/// should cost a glyph, never eleven blank buttons.
function railIcon(name) {
  if (window.HRZUI && HRZUI.icon) return HRZUI.icon(name, { size: 16 });
  const span = document.createElement("span");
  span.className = "railGlyph";
  span.setAttribute("aria-hidden", "true");
  span.textContent = name.slice(0, 1).toUpperCase();
  return span;
}
```

**b.** In `globalTools.build()`, swap every label for its icon and put the name
where a 28px square can keep it -- `aria-label` and `title`:

```js
    const which = document.createElement("span");
    which.className = "globalToolsWhich";
    which.id = "globalToolsWhich";
    frag.appendChild(which);

    // Icon-only: the rail is 36px wide and a label would not fit. The name
    // lives in the tooltip and the accessible label instead.
    const cursor = document.createElement("button");
    cursor.dataset.tool = "cursor";
    cursor.appendChild(railIcon("mouse-pointer-2"));
    cursor.setAttribute("aria-label", "Cursor");
    cursor.title = "Select a drawing, move its anchors, or pan the chart";
    frag.appendChild(cursor);
```

In the group loop, replace the trigger's text with its group mark and keep
`dataset.groupLabel` (step 6c relies on it):

```js
      const trigger = document.createElement("button");
      trigger.className = "toolGroupTrigger";
      trigger.appendChild(railIcon(TOOL_GROUP_ICONS[group.name] || "chart"));
      trigger.title = group.label;
      trigger.setAttribute("aria-label", group.label);
      trigger.dataset.group = group.name;
      trigger.dataset.groupLabel = group.label;
      trigger.setAttribute("aria-haspopup", "true");
      trigger.setAttribute("aria-expanded", "false");
```

The flyout's own buttons keep their text -- a flyout is a labelled list, and
an icon there would be a puzzle.

Before the page-level controls, add the divider, and add an icon to each control:

```js
    // A hairline between "what the next click draws" and "how the chart
    // behaves": one column, two kinds of control.
    const sep = document.createElement("span");
    sep.className = "railSep";
    sep.setAttribute("aria-hidden", "true");
    frag.appendChild(sep);

    const controls = [
      ["button[data-magnet]", "Magnet", "Snap new drawings to nearby open, high, low, close and value-area prices", "magnet"],
      ["button[data-ai-layer]", "AI", "Show objects the AI agent drew on the active chart, with the reason it gave for each", "sparkles"],
      ["button[data-profile-anchor]", "Profile", "Anchor the volume profile to the plot's left edge instead of the right", "panel-left"],
      ["button[data-undo]", "Undo", "Undo the last drawing change on the active chart (Ctrl+Z)", "undo-2"],
      ["button[data-redo]", "Redo", "Redo an undone drawing change on the active chart (Ctrl+Y)", "redo-2"],
    ];
    for (const [attr, label, title, iconName] of controls) {
      const button = document.createElement("button");
      button.setAttribute(attr.match(/data-[a-z-]+/)[0], "");
      button.appendChild(railIcon(iconName));
      button.setAttribute("aria-label", label);
      button.title = title;
      frag.appendChild(button);
    }
```

**c.** Replace `refreshGlobalTools()` wholesale (`app.js:7979-8019`). The rail is
now part of the layout, so it never leaves; an empty binding disables it.

```js
/// Re-derive every rail button's state from the active pane. Derived, never
/// stored -- the same rule the pane toolbars follow for `aria-pressed`.
function refreshGlobalTools() {
  const node = globalTools.node;
  if (!node) return;
  // The rail is a layout element: hiding it would pull 36px out from under the
  // wall every time the last pane closed. No pane means no binding, which means
  // every button is off, not that the rail is gone.
  node.hidden = false;
  node.dataset.empty = activePane ? "false" : "true";
  for (const button of node.querySelectorAll("button")) button.disabled = !activePane;
  const which = document.getElementById("globalToolsWhich");
  if (!activePane) {
    if (which) which.textContent = "No chart";
    return;
  }
  const name = `${activePane.symbol() || "—"} · ${activePane.timeframe() || "—"}`;
  if (which) which.textContent = name;
  // Sighted users read the binding from the rail's tooltip; the label is the
  // same sentence for a screen reader. Neither is painted in the rail.
  node.title = name;
  node.setAttribute("aria-label", `Drawing tools for ${name}`);

  const tool = activePane.currentTool();
  for (const button of node.querySelectorAll("button[data-tool]")) {
    button.setAttribute("aria-pressed", String(button.dataset.tool === tool));
  }
  // Group triggers relabel themselves with the armed tool, as the pane
  // toolbars' `refreshGroupTriggers` does: the trigger carries its group's
  // name, and the armed tool belongs to at most one group. The armed tool goes
  // into the *name*, not the button's text -- a 28px square has no room for
  // "Lines · trendline", and the flyout's own row already shows it pressed.
  for (const trigger of node.querySelectorAll(".toolGroupTrigger")) {
    const base = trigger.dataset.groupLabel;
    if (!base) continue;
    const armed = toolRegistry.some(
      (entry) => entry.group === trigger.dataset.group && entry.kind === tool
    );
    const label = armed ? `${base} · ${tool}` : base;
    trigger.setAttribute("aria-label", label);
    trigger.title = label;
  }
  const magnet = node.querySelector("[data-magnet]");
  if (magnet) magnet.setAttribute("aria-pressed", String(activePane.magnetOn()));
  const ai = node.querySelector("[data-ai-layer]");
  if (ai) ai.setAttribute("aria-pressed", String(activePane.aiLayerOn()));
  const profile = node.querySelector("[data-profile-anchor]");
  if (profile) profile.setAttribute("aria-pressed", String(activePane.profileLeft()));
  const undo = node.querySelector("[data-undo]");
  if (undo) undo.disabled = !activePane.canUndo();
  const redo = node.querySelector("[data-redo]");
  if (redo) redo.disabled = !activePane.canRedo();
}
```

`globalTools.setHidden` stays as it is: nothing calls it, and the rail's
visibility is now the destination's business.

- [ ] **Step 7: Hand the chart's settings to the shell's facade**

In the `window.HRZApp` object (Task 9), after `saveScreenshot`:

```js
  chartPalette: () => chartPalette(),
  chartSettings: () => chartSettings(),
  setChartSetting,
  openChartSettings,
```

`chartPalette` is exposed for the harness and for Spec 2's chart-embedding
screens -- the same answer the panes are painted from, never a second opinion.

- [ ] **Step 8: Call it from `main()`**

At the end of `main()`, around Task 9's `if (window.Shell) Shell.start();`:

```js
  // After the panes exist and before the shell routes: a theme or a stored
  // preference is a property of the chart, so it is applied to the charts that
  // are already on the page.
  wireChartTheme();
  wireChartSettings();
  wireCoach();
  if (window.Shell) Shell.start();
  // After `start()`, because the coach needs a chart to be about -- and on a
  // restored workspace that chart exists only once the shell has routed.
  maybeShowCoach();
```

- [ ] **Step 9: Wire the More panel's two new rows**

In `frontend/app/views/instrument_bar.js`, after the `moreKeys` listener:

```js
    byId("moreCvd").addEventListener("click", () => {
      const on = byId("moreCvd").getAttribute("aria-pressed") !== "true";
      if (app().setChartSetting) app().setChartSetting("cvd", on);
    });
    byId("moreChartSettings").addEventListener("click", () => {
      close();
      if (app().openChartSettings) app().openChartSettings();
    });
```

The colour-blind row deliberately does not `close()` the panel: you flip it,
look at the chart behind the panel, and flip it back if it is worse.

- [ ] **Step 10: Style it**

In `frontend/app/styles/charts.css`, replace the whole `.globalTools` block
(the one Task 3 moved out of `index.html`) with:

```css
/* ---- The chart rail ------------------------------------------------------- */
/*
  The drawing toolbar as a **vertical** strip: 36px of icons down the left edge
  of the wall, each group a flyout to the right. As a horizontal strip it cost
  a row of height for a row of text buttons on every screen; as a rail it costs
  36px of width and the tenth tool is as reachable as the first.

  The rail is a layout element, not an overlay: it never hides, and an empty
  binding disables it (`data-empty`). Hiding it would shift the wall sideways
  every time the last pane closed.
*/
.globalTools {
  flex: 0 0 36px;
  width: 36px;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 2px;
  padding: 6px 0;
  border-right: 1px solid var(--line);
  background: var(--bg-panel);
  /* Deliberately no `overflow-y`: the flyouts are positioned children and a
     scroll box would clip them. Eleven 28px controls are 334px; shorter walls
     lose the last of them before they lose a tool that works. */
}
.globalTools[hidden] { display: none; }
.globalTools button {
  width: 28px;
  height: 28px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  padding: 0;
  border: 1px solid transparent;
  border-radius: var(--radius-2);
  background: transparent;
  color: var(--text-3);
  cursor: pointer;
}
.globalTools button:hover { color: var(--text-1); background: var(--bg-raised); }
.globalTools button[aria-pressed="true"] {
  color: var(--accent);
  background: var(--accent-soft);
  border-color: var(--accent-line);
}
.globalTools button:disabled { opacity: 0.35; cursor: default; }
.globalTools button:disabled:hover { color: var(--text-3); background: transparent; }
/* The binding, for a screen reader and the harness. The visible copy is the
   rail's own title, which costs no width. */
.globalTools .globalToolsWhich {
  position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px;
  overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; border: 0;
}
.railSep { width: 20px; height: 1px; margin: 4px 0; background: var(--line); }
.railGlyph { font: 600 11px/1 var(--font-mono); }
.globalTools .toolGroup { position: relative; display: block; }
.globalTools .toolFlyout {
  position: absolute;
  top: 0;
  left: calc(100% + 6px);
  z-index: 20;
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 120px;
  padding: 4px;
  background: var(--bg-overlay);
  border: 1px solid var(--line-strong);
  border-radius: var(--radius-2);
  box-shadow: var(--shadow-2);
}
.globalTools .toolFlyout[hidden] { display: none; }
/* The flyout is a labelled list: restore text sizing and left alignment after
   the rail's icon-only button rule. */
.globalTools .toolFlyout button {
  width: auto;
  height: auto;
  min-height: 26px;
  justify-content: flex-start;
  padding: 3px 8px;
  text-align: left;
  white-space: nowrap;
  color: var(--text-2);
  border-radius: var(--radius-1);
}
.globalTools .toolFlyout button:hover { color: var(--text-1); background: var(--bg-raised); }
.globalTools .toolFlyout button[aria-pressed="true"] { color: var(--text-1); background: var(--accent-soft); }
```

Add, beside it:

```css
/* The rail and the wall are one row; the dock below spans both. */
#chartStage { flex: 1; display: flex; min-width: 0; min-height: 0; position: relative; }
```

Then replace the existing `.paneHead` and `.paneTitle` rules with:

```css
/* 32px of chrome: the title, the state, and nothing that needs a second line. */
.paneHead {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 10px;
  background: var(--bg-panel);
  border-bottom: 1px solid var(--line);
  font-size: var(--text-2xs);
  /* Containing block for the layer-settings popover (docs/25). */
  position: relative;
}
.paneTitle {
  color: var(--text-2);
  font-family: var(--font-mono);
  font-weight: 600;
  letter-spacing: 0.02em;
  font-size: var(--text-2xs);
  /* A narrow pane must truncate its name, never wrap: a two-line title bar
     would shove every chart's plot down mid-grid. */
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 0 1 auto; min-width: 0;
}
/* The active chart is the one the rail, the bar, the watchlist and the aside
   all act on. A dot says so; the old 2px inset line read as a row selection. */
.chartPane.active .paneTitle { color: var(--text-1); }
.chartPane.active .paneHead::before {
  content: "";
  flex: 0 0 auto;
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--accent);
}

/* ---- The gesture coach ---------------------------------------------------- */
.chartCoach {
  position: absolute;
  left: calc(36px + var(--space-4));
  bottom: var(--space-4);
  z-index: 40;
  width: min(320px, calc(100% - 72px));
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  padding: var(--space-3);
  background: var(--bg-overlay);
  border: 1px solid var(--accent-line);
  border-radius: var(--radius-3);
  box-shadow: var(--shadow-2);
}
.chartCoach[hidden] { display: none; }
.chartCoach .coachTitle { margin: 0; font: 600 var(--text-sm)/1.3 var(--font-ui); color: var(--text-1); }
.chartCoach .coachBody { margin: 0; font-size: var(--text-xs); line-height: 1.55; color: var(--text-2); }
.chartCoach .coachKey { font-family: var(--font-mono); color: var(--text-1); }
.chartCoach .coachFoot { display: flex; justify-content: flex-end; gap: var(--space-2); }

/* ---- Chart settings ------------------------------------------------------- */
.chartSettings {
  width: min(380px, calc(100vw - 32px));
  padding: 0;
  border: 1px solid var(--line-strong);
  border-radius: var(--radius-3);
  background: var(--bg-panel);
  color: var(--text-1);
  box-shadow: var(--shadow-2);
}
.chartSettings::backdrop { background: rgba(4, 7, 12, 0.55); }
.chartSettings .csForm { display: flex; flex-direction: column; gap: var(--space-4); padding: var(--space-4); }
.chartSettings .csHead { display: flex; align-items: center; justify-content: space-between; }
.chartSettings .csHead h2 { margin: 0; font: 600 var(--text-md)/24px var(--font-ui); }
.chartSettings .csRow {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  font-size: var(--text-sm);
  color: var(--text-2);
}
.chartSettings input[type="checkbox"] { width: 16px; height: 16px; accent-color: var(--accent); }
.chartSettings .csFoot { display: flex; align-items: center; justify-content: space-between; gap: var(--space-2); }
```

Delete the `box-shadow` rule `.chartPane.active .paneHead` outright -- the dot
replaces it.

Delete the `.chartHint` rules that Task 3 moved into `charts.css` (originally
`index.html:594-620`) and the `.chartHint { display: none; }` rule with its
comment inside the 1100px media block -- the element is gone in step 2b, and a
rule for a ghost is the kind of thing that outlives the reason for it.

In `frontend/app/styles/shell.css`, the sheet is now three columns:

```css
.shortcutsGrid { grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); }
```

and delete `.shortcutsGrid { grid-template-columns: 1fr; }` from the 720px
media block: `auto-fit` already collapses to one column on a phone, and the
override would freeze the wide sheet at one column if the dialog is ever
widened again.

- [ ] **Step 11: Teach the harness**

In `tools/shell_check.mjs`:

1. Task 11's layout check asserts the bar's next sibling is `#globalTools`.
   That sibling is now the stage:

```js
check(
  "the instrument bar sits at the top of the chart column",
  barNode() && barNode().parentElement.id === "chartColumn" &&
    barNode().nextElementSibling && barNode().nextElementSibling.id === "chartStage",
  barNode() ? `${barNode().parentElement.id} / ${barNode().nextElementSibling?.id}` : "missing"
);
```

2. Append after Task 13's last check (the returning-dock one):

```js
// --- the chart's own chrome --------------------------------------------------
//
// The rail, the theme bridge and the two settings surfaces. The palette checks
// read `HRZApp.chartPalette()` -- the same function the panes are painted from
// -- rather than a canvas pixel, because the harness's 2D context records calls
// and not colours.

console.log("\nthe chart rail, theme and settings");

const paletteNow = () => (window.HRZApp && HRZApp.chartPalette ? HRZApp.chartPalette() : {});
const railNode = () => document.getElementById("globalTools");

check(
  "the rail and the wall share a row inside the chart column",
  railNode().parentElement.id === "chartStage" &&
    document.getElementById("charts").parentElement.id === "chartStage" &&
    document.getElementById("chartStage").parentElement.id === "chartColumn",
  `${railNode().parentElement.id} / ${document.getElementById("chartStage").parentElement.id}`
);
check(
  "the rail is one button per tool group, each named but none lettered",
  document.querySelectorAll("#globalTools .toolGroupTrigger").length === 4 &&
    [...document.querySelectorAll("#globalTools .toolGroupTrigger")].every(
      (b) => b.getAttribute("aria-label") && b.textContent.trim() === ""
    ) &&
    document.querySelector("#globalTools button[data-tool='cursor']").textContent.trim() === "" &&
    document.querySelector("#globalTools button[data-tool='cursor']").getAttribute("aria-label") === "Cursor",
  [...document.querySelectorAll("#globalTools .toolGroupTrigger")].map((b) => b.getAttribute("aria-label")).join(",")
);
check(
  "and the rail names the chart it will draw on",
  document.getElementById("globalToolsWhich").textContent.includes("BTCUSDT") &&
    railNode().title === document.getElementById("globalToolsWhich").textContent &&
    railNode().getAttribute("aria-label").includes("BTCUSDT"),
  railNode().title
);
check(
  "a group mark still opens its flyout, beside the rail rather than below it",
  (() => {
    const trigger = document.querySelector("#globalTools .toolGroupTrigger");
    const flyout = trigger.parentElement.querySelector(".toolFlyout");
    trigger.click();
    const opened = flyout.hidden === false;
    trigger.click();
    return opened && flyout.hidden === true;
  })()
);

const darkUp = paletteNow().up;
window.Theme.set("light");
await settle();
check(
  "the chart palette follows the theme instead of staying dark on a white page",
  document.documentElement.dataset.theme === "light" &&
    paletteNow().up === "#067a4d" &&
    paletteNow().text === "#5f6b7a" &&
    paletteNow().grid === "#e7ecf2",
  `${document.documentElement.dataset.theme} · ${paletteNow().up} · ${paletteNow().grid}`
);
check("and the dark palette is the one the chart shipped with", darkUp === "#2ebd85", darkUp);

document.getElementById("moreChartSettings").click();
await settle();
check(
  "Chart settings opens as a dialog from the More panel",
  document.getElementById("chartSettings").open === true,
  `open=${document.getElementById("chartSettings").open}`
);

document.querySelector('#chartSettings [data-cvd="true"]').click();
await settle();
check(
  "Blue / orange swaps the direction colours, and both surfaces know",
  paletteNow().up === "#2962ff" &&
    paletteNow().down === "#ff6d00" &&
    paletteNow().entry === "#2563eb" &&
    JSON.parse(window.localStorage.getItem("hrz.chart")).cvd === true &&
    document.querySelector('#chartSettings [data-cvd="true"]').getAttribute("aria-pressed") === "true" &&
    document.getElementById("moreCvd").getAttribute("aria-pressed") === "true",
  `${paletteNow().up} / ${paletteNow().down} · entry ${paletteNow().entry}`
);

const gridToggle = document.getElementById("csGrid");
gridToggle.checked = false;
gridToggle.dispatchEvent(new window.Event("change"));
await settle();
check(
  "unchecking Grid lines takes the grid out of the palette",
  paletteNow().grid === "rgba(0, 0, 0, 0)" &&
    JSON.parse(window.localStorage.getItem("hrz.chart")).grid === false,
  paletteNow().grid
);

document.getElementById("csReset").click();
await settle();
check(
  "Reset forgets both choices and both come back",
  paletteNow().grid !== "rgba(0, 0, 0, 0)" &&
    paletteNow().up === "#067a4d" &&
    window.localStorage.getItem("hrz.chart") === null &&
    document.getElementById("csGrid").checked === true
);
document.getElementById("chartSettings").open = false;
window.Theme.set("dark");
await settle();
check("going back to dark restores the dark palette", paletteNow().up === "#2ebd85", paletteNow().up);

check(
  "the gesture coach is on screen until it is dismissed",
  document.getElementById("chartCoach").hidden === false
);
document.getElementById("coachAgree").click();
await settle();
check(
  "Got it hides the coach and remembers the choice",
  document.getElementById("chartCoach").hidden === true &&
    window.localStorage.getItem("hrz.coach.charts") === "1"
);
if (window.Shell) Shell.go("dashboard");
await settle();
if (window.Shell) Shell.go("charts");
await settle();
check(
  "and it does not come back on the next visit",
  document.getElementById("chartCoach").hidden === true
);

check(
  "the shortcut sheet documents the gestures the coach names",
  document.querySelectorAll("#shortcuts .shortcutGroup").length === 3 &&
    document.getElementById("shortcutGestures") !== null,
  `${document.querySelectorAll("#shortcuts .shortcutGroup").length} groups`
);
```

The `entry` assertion in the colour-blind check is the one that matters most:
`entry` is a *trade price*, not a direction, so it must **not** move. A
palette swap that recolours it would be a chart that lies about a stop.

- [ ] **Step 12: Run it**

```bash
node --check frontend/app/app.js
node --check frontend/app/views/instrument_bar.js
node --check frontend/app/ui/icons.js
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
cargo test -p api-gateway --test packaging
```

Expected: all pass. If the rail checks fail on "named but none lettered", the
icons are not reaching the buttons -- `railIcon` fell back to `.railGlyph`,
which means `ui/icons.js` is not in the boot list or the generator was not
re-run. If the palette check fails with dark values under a light theme, the
`hrz:theme` listener is registered on `document` instead of `window`; the
event is dispatched on `window` (Task 2).

Open the gateway at 1440×900 signed in, on Charts, and check by hand:

1. The rail runs down the left edge of the wall: cursor, four group marks, a
   hairline, magnet, AI, profile, undo, redo. Hovering any of them names it;
   hovering the rail itself names the chart (`BTCUSDT · 5m`).
2. Press the Lines mark: its flyout opens to the **right** of the rail and a
   flyout button arms the tool. The mark takes the accent and its tooltip reads
   `Lines · trendline`.
3. The wall is visibly taller than before -- the strip that used to sit above
   it is gone -- and no chart is narrower than it was minus 36px.
4. Switch to light: candles, wicks, grid, axes, the volume profile, sub-pane
   washes and drawing colours all change with the chrome. Nothing stays at TV
   dark. Footprint mode still draws on black.
5. More → Colour-blind candles: candles and footprint aggression become
   blue/orange in place, the row stays pressed, and entry/stop/target lines keep
   their colours. Reload: the choice survives.
6. More → Chart settings: the dialog opens over the chart, shows the same two
   states, unchecking Grid removes the lines behind it, Reset brings both back,
   Done closes.
7. The coach shows on first sign-in, bottom-left. "See all gestures" opens the
   shortcut sheet with a new Gestures column; "Got it" hides the coach. Reload:
   it does not return.
8. Activate the second pane: the accent dot moves to it and the rail's tooltip
   names the second chart's symbol.
9. Resize to a phone width: the rail stays 36px, the coach is still readable,
   and the settings dialog fits the viewport.
10. With VoiceOver the rail is announced as a vertical toolbar named for the
    chart; each group mark announces its group and its armed tool.

- [ ] **Step 13: Commit**

```bash
git add tools/fetch_icons.mjs frontend/app/ui/icons.js \
  frontend/app/app.js frontend/app/index.html \
  frontend/app/views/instrument_bar.js \
  frontend/app/styles/charts.css frontend/app/styles/shell.css \
  tools/shell_check.mjs
git commit -m "feat: a theme-aware chart, a chart rail and chart settings"
```

---

### Task 15: A dashboard with a pulse, a market list and a heatmap

Dashboard is the signed-in landing page, and it is the only destination that
answers "what is happening" instead of "what am I working on". Its four pulse
cards answer four different questions from four different endpoints, the
markets table reads the venue's whole list, the watchlist is the *same* list the
Charts rail shows, and the heatmap compresses sixty pairs into one glance. This
task writes the markup and the stylesheet; Task 16 makes them breathe.

The markup ships empty on purpose. Every container below is a target Task 16
fills, and every ID below is named in Task 16's interface block -- so the two
tasks can be reviewed against each other without reading each other's code.
Until Task 16 lands, the page shows its loading and empty states, which is
exactly what a visitor with a slow `/tickers` sees anyway.

**Files:**
- Modify: `frontend/app/index.html` (the `#dest-dashboard` placeholder)
- Modify: `frontend/app/styles/dashboard.css` (created empty by Task 3)
- Modify: `frontend/app/styles/shell.css` (delete the placeholder rule)
- Modify: `tools/shell_check.mjs` (four structure checks)

**Interfaces:**
- Produces: `#dest-dashboard > .dashInner` with `.dashHead` (`#dashDate`,
  `#dashMarket`, `#dashRefresh`, `#dashNewChart`), `#pulseRow` with four
  `.pulseCard[data-pulse]` (`#pulseMarket`, `#pulseWatch`, `#pulseAuto`,
  `#pulsePlan`), `.dashGrid` holding `#marketTabs` (`[data-market]`),
  `#marketFilter`, `#marketRows`, `#marketEmpty`, `#marketMore`,
  `#marketCount`, the `#dashWatch` panel (`#dashWatchAdd`,
  `#dashWatchSearch`, `#dashWatchInput`, `#dashWatchResults`,
  `#dashWatchRows`, `#dashWatchEmpty`, `#dashWatchAge`), and the heat card
  (`#heatHint`, `#heatTableToggle`, `#heatGrid`, `#heatTableWrap`,
  `#heatRows`, `#heatEmpty`).
- Produces: the row and tile classes Task 16 builds -- `.marketSym`,
  `.marketQuote`, `.marketMark`, `.dashWatchRow`, `.dashWatchRow__sym`,
  `.dashWatchRow__last`, `.dashWatchRow__pct`, `.dashWatchRow__star`,
  `.dashWatch__result`, `.heatTile` (`[data-bucket]`, `[data-size]`),
  `.heatTile__sym`, `.heatTile__pct`, `.pulseValue`, `.pulseValueRow`,
  `.pulseNote`, `.pulseSpark`, `.pulseSplit`, `.pulseMeter`.
- Consumes: `.hrz-panel`, `.hrz-tabs`, `.hrz-table`, `.hrz-input`,
  `.hrz-btn`, `.hrz-price`, `.empty` (Task 6), the tokens (Task 1), and
  `#dest-dashboard`'s `overflow: auto` and `aria-labelledby` (Task 8).

- [ ] **Step 1: Replace the placeholder with the page**

In `index.html`, replace the whole `#dest-dashboard` section (Task 8's three
lines, `index.html:2664-2667`) with the block below. The heading keeps its id
because the router focuses it, and the section keeps `hidden` because the
router owns which destination is on screen.

```html
    <section class="destination" id="dest-dashboard" aria-labelledby="dest-dashboard-title" hidden>
      <div class="dashInner">
        <header class="dashHead">
          <div class="dashHead__text">
            <h1 class="dashTitle" id="dest-dashboard-title" tabindex="-1">Dashboard</h1>
            <p class="dashSub">
              <span id="dashDate"></span>
              <span class="dashSub__sep" aria-hidden="true">&middot;</span>
              <span id="dashMarket">&mdash;</span>
            </p>
          </div>
          <div class="dashHead__actions">
            <button type="button" class="hrz-btn hrz-btn--ghost hrz-btn--icon" id="dashRefresh"
                    aria-label="Refresh the dashboard" title="Refresh">
              <span data-icon="refresh" aria-hidden="true"></span>
            </button>
            <button type="button" class="hrz-btn hrz-btn--primary hrz-btn--sm" id="dashNewChart">
              <span data-icon="plus" aria-hidden="true"></span> New chart
            </button>
          </div>
        </header>

        <!--
          Four questions, four cards, four sources. Each body is painted whole
          from one fetch, so a card that fails can say so in place without the
          row losing its shape -- which is why the error lives inside the card
          and not in a toast at the corner of the screen.
        -->
        <div class="pulseRow" id="pulseRow">
          <article class="pulseCard" data-pulse="market" aria-labelledby="pulseMarketTitle">
            <h2 class="pulseCard__title" id="pulseMarketTitle">Market</h2>
            <div class="pulseCard__body" id="pulseMarket"><p class="pulseNote">Loading&hellip;</p></div>
          </article>
          <article class="pulseCard" data-pulse="watchlist" aria-labelledby="pulseWatchTitle">
            <h2 class="pulseCard__title" id="pulseWatchTitle">Watchlist</h2>
            <div class="pulseCard__body" id="pulseWatch"><p class="pulseNote">Loading&hellip;</p></div>
          </article>
          <article class="pulseCard" data-pulse="automation" aria-labelledby="pulseAutoTitle">
            <h2 class="pulseCard__title" id="pulseAutoTitle">Automation</h2>
            <div class="pulseCard__body" id="pulseAuto"><p class="pulseNote">Loading&hellip;</p></div>
          </article>
          <article class="pulseCard" data-pulse="plan" aria-labelledby="pulsePlanTitle">
            <h2 class="pulseCard__title" id="pulsePlanTitle">Plan</h2>
            <div class="pulseCard__body" id="pulsePlan"><p class="pulseNote">Loading&hellip;</p></div>
          </article>
        </div>

        <div class="dashGrid">
          <section class="hrz-panel marketsCard" aria-labelledby="marketsTitle">
            <header class="hrz-panel__head">
              <h2 class="hrz-panel__title" id="marketsTitle">Markets</h2>
              <div class="hrz-panel__actions">
                <input id="marketFilter" class="hrz-input marketFilter" type="search"
                       placeholder="Filter" aria-label="Filter markets by symbol" />
              </div>
            </header>
            <div class="hrz-tabs marketTabs" id="marketTabs" role="tablist" aria-label="Market lists">
              <button role="tab" type="button" class="hrz-tab" data-market="all" aria-selected="true">All</button>
              <button role="tab" type="button" class="hrz-tab" data-market="majors" aria-selected="false">Majors</button>
              <button role="tab" type="button" class="hrz-tab" data-market="alts" aria-selected="false">Alts</button>
              <button role="tab" type="button" class="hrz-tab" data-market="watchlist" aria-selected="false">Watchlist</button>
            </div>
            <div class="hrz-panel__body marketsBody" id="marketBody">
              <table class="hrz-table hrz-table--compact marketTable">
                <caption class="sr-only">Markets, sortable and filtered by the list above</caption>
                <thead>
                  <tr>
                    <th data-sort="symbol" aria-sort="none" scope="col"><button type="button" class="hrz-table__sort">Symbol</button></th>
                    <th data-sort="last" aria-sort="none" scope="col" class="hrz-table__num"><button type="button" class="hrz-table__sort">Last</button></th>
                    <th data-sort="change" aria-sort="none" scope="col" class="hrz-table__num"><button type="button" class="hrz-table__sort">24h %</button></th>
                    <th data-sort="volume" aria-sort="descending" scope="col" class="hrz-table__num"><button type="button" class="hrz-table__sort">24h Volume</button></th>
                  </tr>
                </thead>
                <tbody id="marketRows"></tbody>
              </table>
              <p class="empty" id="marketEmpty" hidden></p>
              <div class="marketFoot">
                <span id="marketCount" class="muted"></span>
                <button type="button" class="hrz-btn hrz-btn--secondary hrz-btn--sm" id="marketMore" hidden>Show 50 more</button>
              </div>
            </div>
          </section>

          <aside class="hrz-panel dashWatch" id="dashWatch" aria-labelledby="dashWatchTitle">
            <header class="hrz-panel__head">
              <h2 class="hrz-panel__title" id="dashWatchTitle">Watchlist</h2>
              <div class="hrz-panel__actions">
                <button type="button" class="hrz-btn hrz-btn--ghost hrz-btn--sm" id="dashWatchAdd"
                        aria-expanded="false" aria-controls="dashWatchSearch">+ Add</button>
              </div>
            </header>
            <div class="dashWatch__search" id="dashWatchSearch" hidden>
              <input id="dashWatchInput" class="hrz-input" type="search"
                     placeholder="Add a symbol" aria-label="Search the venue for a symbol to add" />
              <div class="dashWatch__results" id="dashWatchResults"></div>
            </div>
            <div id="dashWatchRows"></div>
            <p class="empty" id="dashWatchEmpty" hidden>
              Star a symbol to pin it here.
            </p>
            <div class="dashWatch__foot">
              <span id="dashWatchAge" class="muted"></span>
              <a class="dashLink" href="#/charts">Open Charts</a>
            </div>
          </aside>

          <section class="hrz-panel heatCard" aria-labelledby="heatTitle">
            <header class="hrz-panel__head">
              <h2 class="hrz-panel__title" id="heatTitle">Heatmap</h2>
              <div class="hrz-panel__actions">
                <span class="muted heatHint" id="heatHint">Top 60 by 24h volume</span>
                <button type="button" class="hrz-btn hrz-btn--ghost hrz-btn--sm" id="heatTableToggle"
                        aria-pressed="false">Show as table</button>
              </div>
            </header>
            <div class="heatGrid" id="heatGrid" role="group" aria-label="Market heatmap"></div>
            <div class="hrz-panel__body" id="heatTableWrap" hidden>
              <table class="hrz-table hrz-table--compact">
                <caption class="sr-only">The heatmap's data as a table</caption>
                <thead>
                  <tr>
                    <th scope="col">Symbol</th>
                    <th scope="col" class="hrz-table__num">24h %</th>
                    <th scope="col" class="hrz-table__num">24h Volume</th>
                  </tr>
                </thead>
                <tbody id="heatRows"></tbody>
              </table>
            </div>
            <p class="empty" id="heatEmpty" hidden>No market data yet.</p>
          </section>
        </div>
      </div>
    </section>
```

Two details are load-bearing. The heat card carries its table view **inside**
the same panel rather than swapping cards, because "Show as table" is a view of
the same data and a second card would be a second source. And `dashWatch` is
an `<aside>`: it is a rail of the page, not a third section, and its role in
the grid is what makes the reading order (markets, then your list, then the
map) match the visual order.

- [ ] **Step 2: Write `dashboard.css`**

Task 3 created the file empty. Its content:

```css
/* ===========================================================================
   The dashboard: the signed-in landing page.

   Four pulse cards, the markets table, the watchlist and the heatmap. Every
   colour is a token -- Task 3's gate greps this file -- and the seven heat
   buckets are mixed from the two direction tokens into the panel colour, so
   the ramp is the same seven steps in both themes instead of two hand-picked
   ones that drift.

   The breakpoints live here, not in responsive.css: each rule below belongs to
   a class this file defines, and a page's own grid is the one thing the frame's
   media queries must not move. responsive.css keeps the frame.
   ========================================================================= */

.dashInner {
  width: 100%; max-width: 1440px; margin: 0 auto;
  padding: var(--space-6);
  display: flex; flex-direction: column; gap: var(--space-4);
}

/* ---- The page header ------------------------------------------------------ */
.dashHead { display: flex; align-items: flex-end; gap: var(--space-3); flex-wrap: wrap; }
.dashHead__text { flex: 1 1 280px; min-width: 0; }
.dashTitle { margin: 0; color: var(--text-1); font: 600 var(--text-lg)/1.2 var(--font-ui); }
.dashSub {
  display: flex; align-items: baseline; gap: var(--space-2); flex-wrap: wrap;
  margin: var(--space-1) 0 0; color: var(--text-3);
  font: var(--text-xs)/1.4 var(--font-ui);
}
.dashSub__sep { color: var(--line-strong); }
#dashMarket { color: var(--text-2); font-family: var(--font-mono); }
#dashMarket .hrz-price { margin-left: var(--space-1); }
.dashHead__actions { display: flex; align-items: center; gap: var(--space-2); }

/* ---- Pulse ---------------------------------------------------------------- */
.pulseRow { display: grid; gap: var(--space-4); grid-template-columns: repeat(4, minmax(0, 1fr)); }
.pulseCard {
  display: flex; flex-direction: column; gap: var(--space-2);
  min-height: 112px; padding: var(--space-3) var(--space-4);
  background: var(--bg-panel); border: 1px solid var(--line);
  border-radius: var(--radius-3);
}
.pulseCard__title {
  margin: 0; color: var(--text-3);
  font: 500 var(--text-2xs)/1 var(--font-ui);
  text-transform: uppercase; letter-spacing: 0.08em;
}
.pulseCard__body { display: flex; flex-direction: column; gap: var(--space-1); flex: 1 1 auto; min-height: 0; }
.pulseValueRow { display: flex; align-items: baseline; gap: var(--space-2); flex-wrap: wrap; }
.pulseValue {
  color: var(--text-1);
  font: 600 var(--text-xl)/1.1 var(--font-mono);
  font-variant-numeric: tabular-nums;
}
.pulseValue--sm { font-size: var(--text-sm); line-height: 1.35; }
.pulseNote { margin: 0; color: var(--text-3); font-size: var(--text-xs); line-height: 1.5; }
.pulseNote--error { color: var(--down); }
.pulseNote .hrz-btn { margin-left: var(--space-1); }
.pulseMeter + .pulseMeter { margin-top: var(--space-2); }
/* An SVG line, not `HRZUI.sparkline`: a canvas cannot read `var(--up)`, and a
   pulse card has to follow the theme like everything else on the page. */
.pulseSpark { display: block; width: 100%; height: 26px; margin-top: auto; }
.pulseSpark polyline {
  fill: none; stroke-width: 1.5;
  stroke-linecap: round; stroke-linejoin: round;
}
.pulseSpark[data-trend="up"] polyline { stroke: var(--up); }
.pulseSpark[data-trend="down"] polyline { stroke: var(--down); }
.pulseSplit { display: flex; align-items: center; gap: var(--space-2); font: var(--text-2xs)/1 var(--font-mono); }
.pulseSplit__up { color: var(--up); }
.pulseSplit__down { color: var(--down); }

/* ---- The three panels ----------------------------------------------------- */
.dashGrid {
  display: grid; gap: var(--space-4); align-items: start;
  grid-template-columns: repeat(12, minmax(0, 1fr));
}
.marketsCard { grid-column: span 8; }
.dashWatch { grid-column: span 4; }
.heatCard { grid-column: 1 / -1; min-height: 320px; }

/* ---- Markets -------------------------------------------------------------- */
.marketFilter { width: 180px; height: 24px; }
.marketTabs { padding: 0 var(--space-2); }
.marketsBody { display: flex; flex-direction: column; min-height: 0; padding: 0; }
.marketTable tbody tr[data-symbol] { cursor: pointer; }
.marketSym {
  display: inline-flex; align-items: center; gap: var(--space-2);
  padding: 0; border: 0; background: transparent; color: var(--text-1);
  font: 600 var(--text-xs)/1 var(--font-mono); cursor: pointer;
}
.marketSym:hover { color: var(--accent); }
/* The mark is the direction, so a column of rows reads as green or red before
   a single number is read. */
.marketMark { width: 6px; height: 6px; flex: 0 0 auto; border-radius: 1px; background: var(--line-strong); }
.marketMark[data-dir="up"] { background: var(--up); }
.marketMark[data-dir="down"] { background: var(--down); }
.marketQuote { color: var(--text-3); font-weight: 400; }
.marketFoot {
  display: flex; align-items: center; gap: var(--space-2);
  height: 36px; padding: 0 var(--space-3);
  border-top: 1px solid var(--line);
}
.marketFoot .hrz-btn { margin-left: auto; }

/* ---- The watchlist panel -------------------------------------------------- */
.dashWatch { display: flex; flex-direction: column; min-width: 0; }
.dashLink {
  color: var(--text-2); font: 500 var(--text-2xs)/1 var(--font-ui);
  text-decoration: none;
}
.dashLink:hover { color: var(--accent); text-decoration: none; }
.dashWatch__search { padding: var(--space-2) var(--space-3); border-bottom: 1px solid var(--line); }
.dashWatch__search[hidden] { display: none; }
.dashWatch__results { max-height: 180px; overflow-y: auto; margin-top: var(--space-1); }
.dashWatch__result {
  display: flex; align-items: center; gap: var(--space-2);
  width: 100%; height: 26px; padding: 0 var(--space-1);
  border: 0; background: transparent; color: var(--text-1);
  font: var(--text-xs)/1 var(--font-mono); text-align: left; cursor: pointer;
}
.dashWatch__result:hover { background: var(--bg-raised); }
.dashWatch__result .muted { margin-left: auto; font: var(--text-2xs)/1 var(--font-ui); }
#dashWatchRows { display: flex; flex-direction: column; min-height: 0; max-height: 352px; overflow-y: auto; }
.dashWatchRow {
  display: grid; grid-template-columns: minmax(0, 1fr) auto 60px 22px;
  align-items: center; gap: var(--space-2);
  height: 30px; padding: 0 var(--space-3);
  border-bottom: 1px solid var(--line);
}
.dashWatchRow:hover { background: var(--bg-raised); }
.dashWatchRow__sym {
  padding: 0; border: 0; background: transparent; color: var(--text-1);
  font: 600 var(--text-xs)/1 var(--font-mono); text-align: left; cursor: pointer;
}
.dashWatchRow__sym:hover { color: var(--accent); }
.dashWatchRow__last {
  color: var(--text-1); text-align: right;
  font: var(--text-2xs)/1 var(--font-mono);
  font-variant-numeric: tabular-nums;
}
.dashWatchRow__pct { text-align: right; font: var(--text-2xs)/1 var(--font-mono); }
.dashWatchRow__star {
  width: 22px; height: 22px; padding: 0;
  border: 0; background: transparent; color: var(--warn);
  font-size: var(--text-xs); line-height: 1; cursor: pointer;
}
.dashWatchRow__star[aria-pressed="false"] { color: var(--text-3); }
.dashWatch__foot {
  display: flex; align-items: center; gap: var(--space-2);
  height: 32px; padding: 0 var(--space-3);
  border-top: 1px solid var(--line);
}
.dashWatch__foot .dashLink { margin-left: auto; }

/* ---- The heatmap ---------------------------------------------------------- */
.heatHint { margin-right: var(--space-1); font-size: var(--text-2xs); }
#heatGrid { padding: var(--space-3); }
#heatGrid[hidden] { display: none; }
#heatTableWrap { padding: 0; }
#heatTableWrap[hidden] { display: none; }
.heatGrid {
  display: grid; gap: var(--space-1);
  grid-template-columns: repeat(auto-fill, minmax(104px, 1fr));
  grid-auto-rows: 40px; grid-auto-flow: dense;
}
/* Size spans rows and never columns: a tile wider than the grid makes an
   implicit column, and an implicit column is a horizontal scrollbar on a
   phone. Rows are free. */
.heatTile {
  display: flex; flex-direction: column; justify-content: center; gap: 2px;
  min-width: 0; padding: var(--space-1) var(--space-2);
  border: 0; border-radius: var(--radius-1);
  background: var(--bg-raised); color: var(--text-1);
  text-align: left; cursor: pointer;
}
.heatTile:hover { outline: 1px solid var(--control-line); }
.heatTile[data-size="2"] { grid-row: span 2; }
.heatTile[data-size="3"] { grid-row: span 3; }
.heatTile[data-size="4"] { grid-row: span 4; }
/* Seven steps, four up, flat, two down: `-3` is "down hard", not "third". The
   percentages are the share of the direction token in the panel colour. */
.heatTile[data-bucket="3"] { background: color-mix(in oklab, var(--up) 46%, var(--bg-panel)); }
.heatTile[data-bucket="2"] { background: color-mix(in oklab, var(--up) 30%, var(--bg-panel)); }
.heatTile[data-bucket="1"] { background: color-mix(in oklab, var(--up) 14%, var(--bg-panel)); }
.heatTile[data-bucket="-1"] { background: color-mix(in oklab, var(--down) 14%, var(--bg-panel)); }
.heatTile[data-bucket="-2"] { background: color-mix(in oklab, var(--down) 30%, var(--bg-panel)); }
.heatTile[data-bucket="-3"] { background: color-mix(in oklab, var(--down) 46%, var(--bg-panel)); }
.heatTile__sym { overflow: hidden; text-overflow: ellipsis; font: 600 var(--text-2xs)/1.2 var(--font-mono); }
.heatTile__pct { font: var(--text-2xs)/1.2 var(--font-mono); font-variant-numeric: tabular-nums; opacity: 0.85; }

/* ---- Breakpoints ---------------------------------------------------------- */
@media (max-width: 1279px) {
  .pulseRow { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .marketsCard { grid-column: span 7; }
  .dashWatch { grid-column: span 5; }
}
@media (max-width: 1023px) {
  .marketsCard, .dashWatch { grid-column: 1 / -1; }
  #dashWatchRows { max-height: 240px; }
}
@media (max-width: 767px) {
  .pulseRow { grid-template-columns: minmax(0, 1fr); }
  .dashHead__actions { width: 100%; }
  .dashHead__actions .hrz-btn--primary { flex: 1 1 auto; justify-content: center; }
  .marketsCard .hrz-panel__head { height: auto; flex-wrap: wrap; padding: var(--space-2) var(--space-3); }
  .marketsCard .hrz-panel__actions { width: 100%; margin-left: 0; }
  .marketFilter { width: 100%; }
  /* Rows become cards: symbol over price over delta, and the volume column
     goes -- it is the one number a phone user cannot act on. */
  .marketTable thead { display: none; }
  .marketTable, .marketTable tbody { display: block; }
  .marketTable tr {
    display: grid; grid-template-columns: minmax(0, 1fr) auto;
    gap: 2px var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--line);
  }
  .marketTable td { display: block; height: auto; padding: 0; border: 0; }
  .marketTable td:nth-child(1) { grid-column: 1; grid-row: 1; }
  .marketTable td:nth-child(2) { grid-column: 2; grid-row: 1; text-align: right; }
  .marketTable td:nth-child(3) { grid-column: 2; grid-row: 2; text-align: right; }
  .marketTable td:nth-child(4) { display: none; }
  .heatGrid { grid-template-columns: repeat(auto-fill, minmax(88px, 1fr)); grid-auto-rows: 36px; }
  .heatTile[data-size="4"] { grid-row: span 3; }
}
```

Two rules exist because their absence is invisible until it is expensive.
`#heatGrid[hidden]`, `#heatTableWrap[hidden]` and `.dashWatch__search[hidden]`
are explicit: an id selector beats the UA's `[hidden]` rule, and a heatmap that
stays on screen behind "Show as table" is a bug nobody looks for. And the
mobile `tr` grid assumes the four cells stay in the order the markup sets -- if
Task 16 ever adds a column, the phone card needs its `nth-child` rules moved
with it.

- [ ] **Step 3: Delete the placeholder rule it replaced**

In `shell.css`, remove:

```css
.destPlaceholder { padding: var(--space-4) 0; color: var(--text-3); font-size: var(--text-xs); }
```

Nothing else in the file uses the class (Task 8 added it for this one section),
and the other destinations use `.destHeading` and `.destHost`, which stay.

- [ ] **Step 4: Teach the harness the structure**

Append after Task 14's last check (the gesture-group one). The checks below are
the ones that stay true once Task 16 fills the page: the skeleton Task 16
paints into, the labels a screen reader reads, and the placeholder that must
not still be there.

```js
// --- the dashboard's structure ------------------------------------------------
//
// Task 16 fills these panels with data. These checks are the ones that survive
// both halves of the task: the structure a reader navigates, and the fact that
// the placeholder it replaced is gone.

console.log("\nthe dashboard");

if (window.Shell) Shell.go("dashboard");
await settle();

check(
  "Dashboard is a page of its own, not a placeholder",
  document.querySelector("#dest-dashboard .dashInner") !== null &&
    document.getElementById("dest-dashboard").hidden === false &&
    document.getElementById("dest-dashboard").textContent.includes("Markets") &&
    !document.getElementById("dest-dashboard").textContent.includes("land in M5"),
  document.getElementById("dest-dashboard").textContent.slice(0, 60).trim()
);
check(
  "four pulse cards, each named for the question it answers",
  [...document.querySelectorAll("#pulseRow .pulseCard")].map((card) => card.querySelector("h2").textContent).join(",") ===
    "Market,Watchlist,Automation,Plan",
  [...document.querySelectorAll("#pulseRow .pulseCard h2")].map((head) => head.textContent).join(",")
);
check(
  "the markets table offers four lists and four sortable columns",
  document.querySelectorAll("#marketTabs [role='tab']").length === 4 &&
    document.querySelectorAll(".marketTable thead th[data-sort]").length === 4 &&
    document.querySelector(".marketTable th[data-sort='volume']").getAttribute("aria-sort") === "descending" &&
    document.getElementById("marketRows") !== null &&
    document.getElementById("marketMore").hidden === true,
  `${document.querySelectorAll("#marketTabs [role='tab']").length} tabs · ${document.querySelectorAll(".marketTable thead th[data-sort]").length} columns`
);
check(
  "the watchlist adds if asked and the heatmap has both views",
  document.getElementById("dashWatchAdd").getAttribute("aria-expanded") === "false" &&
    document.getElementById("dashWatchSearch").hidden === true &&
    document.querySelector("#dest-dashboard a[href='#/charts']") !== null &&
    document.getElementById("heatTableToggle").getAttribute("aria-pressed") === "false" &&
    document.getElementById("heatTableWrap").hidden === true &&
    document.getElementById("heatGrid").hidden === false,
  `add=${document.getElementById("dashWatchAdd").getAttribute("aria-expanded")} table=${document.getElementById("heatTableWrap").hidden}`
);
```

The checks end on Dashboard rather than returning to Charts: Task 16's own
block opens by going there, and a round trip that exists only to restore the
previous destination is a round trip the next task deletes.

- [ ] **Step 5: Run it**

```bash
grep -rnE '#[0-9a-fA-F]{3,8}|rgba?\(' frontend/app/styles/ | grep -v tokens.css
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
cargo test -p api-gateway --test packaging
```

Expected: the grep prints nothing (the heat ramp is `color-mix`, not three-
digit hex), the harness passes with four new lines, and packaging passes
because `dashboard.css` was already linked and referenced in Task 3.

Open the gateway at 1440×900 signed in. The landing page is the dashboard:

1. Header: "Dashboard", today's date, a market line holding a dash until Task
   16 lands, a refresh button and "New chart".
2. Four empty cards of equal height, 112px, in one row. Each says
   "Loading…" -- Task 16 replaces the text with a value.
3. Below: a wide Markets card (tabs, table header, an empty body, a footer),
   a narrower Watchlist card with "+ Add" and "Open Charts", and a full-width
   Heatmap card at least 320px tall.
4. Narrow the window to 1100: the pulse row is 2×2 and the markets card keeps
   7 columns to the watchlist's 5. At 900: both are full width.
5. At 375: one pulse card per row, the markets header stacks its filter, and
   the table has no header row. Nothing scrolls sideways.
6. Light theme: the cards, the table zebra-less rows and the empty state are
   legible; no card is a dark slab on a white page.

- [ ] **Step 6: Commit**

```bash
git add frontend/app/index.html frontend/app/styles/dashboard.css \
  frontend/app/styles/shell.css tools/shell_check.mjs
git commit -m "feat: a dashboard of pulse, markets, watchlist and heatmap"
```

---

### Task 16: A dashboard that breathes on one clock

Task 15 drew the page; this task gives it data. The design constraint that
decides most of the code is Spec 6.4: *the panel never starts a second ticker.*
Four surfaces need prices -- the markets table, the heatmap, the watchlist
panel and the market pulse -- and they are four views of one venue list, so
**one** five-second request repaints all four. The watchlist's own clock (the
Charts rail's ticker) is stopped while the dashboard is up, which is why the
shell gains a single hook: `HRZApp.setDestinationVisible(destination)`.

Everything else follows the same rule the other destinations already follow: a
panel reads on open, paints from the last answer when it is reopened, and stops
its clock on leave. The bots and the sparkline ride a slower half of the same
clock -- every other tick -- because a bot count that is three seconds old and
a one-hour close are not questions that need five-second freshness.

**Files:**
- Modify: `frontend/app/app.js` (the dashboard section, `refreshBillingPane`,
  `main()`, the `HRZApp` facade)
- Modify: `frontend/app/views/shell.js` (`runDestinationLoaders`,
  `showDestination`)
- Modify: `tools/shell_check.mjs` (an interval recorder, the market fixture,
  `/tickers?limit=`, `/bots`, billing mocks, the checks)

**Interfaces:**
- Consumes: every id and class from Task 15; `api`, `el`, `pageToast`,
  `fmtPrice`, `watchlistFavorites`, `toggleFavorite`, `watchlistTickers`,
  `renderWatchlist`, `billingState`, `loadBilling`, `activePane` (app.js);
  `HRZApp.chartSymbol`, `HRZApp.newPane`, `HRZApp.searchSymbols` (Tasks 9-11);
  `HRZUI.format`, `HRZUI.meter` (Task 7); `Shell.go` (Task 9).
- Produces: `setDestinationVisible(destination)` and the `HRZApp` member of the
  same name; `DASH_TICK_MS` (5000), `DASH_PAGE` (50), `HEAT_LIMIT` (60),
  `MAJORS`, the storage key `hrz.heat.table`.
- Produces: `runPaneLoaders()` with no argument now means "no pane is on
  screen", which stops the watchlist ticker; the dashboard branch of
  `runDestinationLoaders` calls it.

- [ ] **Step 1: Give the page a clock, and give the shell a way to stop it**

In `app.js`, immediately before `async function main() {`, start a new section:

```js
// ---------------------------------------------------------------------------
// The dashboard
//
// Four questions, four sources, and one clock. The markets batch is the clock:
// every five seconds it re-prices the table, the heatmap, the watchlist panel
// and the market pulse, because all four are views of one venue list. That is
// also why the panel never starts a ticker of its own (Spec 6.4) -- on this
// destination the markets tick *is* the watchlist ticker, and the Charts rail's
// clock is stopped while the dashboard is up.
// ---------------------------------------------------------------------------

/// Five seconds, because the venue caches `/tickers` for five: asking faster
/// buys the same answer, asking slower makes the table read as stale.
const DASH_TICK_MS = 5000;
/// The table's page size, and the step "Show 50 more" adds.
const DASH_PAGE = 50;
/// The heatmap's tile count.
const HEAT_LIMIT = 60;
const HEAT_KEY = "hrz.heat.table";
/// The ten liquid pairs the Majors tab means. A constant and not a rule: the
/// venue has no "major" flag, and a computed one ("today's top ten by volume")
/// would move the tab's contents between polls.
const MAJORS = [
  "BTCUSDT", "ETHUSDT", "SOLUSDT", "BNBUSDT", "XRPUSDT",
  "ADAUSDT", "DOGEUSDT", "AVAXUSDT", "LINKUSDT", "DOTUSDT",
];

let dashTimer = 0;
let dashTicks = 0;
let dashUniverse = [];        // `/tickers?limit=1000`
let dashUniverseAt = 0;
let dashUniverseFailure = "";
let dashSpark = null;         // the last 24 hourly closes of BTC
let dashBots = null;          // `{ bots, events, at }`, or null before the first answer
let dashBotsFailure = "";
let dashTab = "all";
let dashSort = { key: "volume", dir: "desc" };
let dashFilter = "";
let dashShown = DASH_PAGE;

/// A node with a class and text, so a paint function reads as the shape it
/// draws rather than a wall of `createElement` calls.
const dashEl = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

const baseOf = (symbol) => symbol.replace(/USDT$/, "");
const quoteOf = (symbol) => (symbol.endsWith("USDT") ? "USDT" : "");

/// The shell's one hook into this destination.
///
/// Idempotent on purpose: the shell calls it on every `showDestination`,
/// including the ones a panel toggle causes, and a clock that restarted on
/// every call would poll each time a tab moved. Coming back repaints from the
/// cache first, so a returning reader sees their table before the batch lands.
function setDestinationVisible(destination) {
  if (destination === "dashboard") {
    if (!dashTimer) {
      dashTimer = setInterval(dashTick, DASH_TICK_MS);
      dashTicks = 0;
      applyHeatView();
      dashTick();
    }
    if (!billingState) loadBilling();
    return;
  }
  if (dashTimer) {
    clearInterval(dashTimer);
    dashTimer = 0;
  }
}

/// One batch, four surfaces.
///
/// The favorites outside the venue's most active thousand are asked for by
/// name in the same tick. That is a second *request*, not a second timer: it
/// rides the same five seconds and stops with them.
function dashTick() {
  dashTicks += 1;
  paintDashDate();
  api("/tickers?limit=1000")
    .then((response) => {
      dashUniverse = Array.isArray(response.tickers) ? response.tickers : [];
      dashUniverseAt = Date.now();
      dashUniverseFailure = "";
      for (const ticker of dashUniverse) watchlistTickers.set(ticker.symbol, ticker);
      const priced = new Set(dashUniverse.map((ticker) => ticker.symbol));
      const missing = watchlistFavorites().filter((symbol) => !priced.has(symbol));
      if (!missing.length) return undefined;
      return api(`/tickers?symbols=${encodeURIComponent(missing.join(","))}`)
        .then((answer) => {
          for (const ticker of answer.tickers || []) watchlistTickers.set(ticker.symbol, ticker);
        })
        .catch(() => undefined);
    })
    .catch((error) => {
      dashUniverseFailure = error.message;
    })
    .then(() => {
      paintDashMarkets();
      paintDashWatch();
      paintDashMarketPulse();
      paintDashWatchPulse();
      paintDashHeat();
    });
  // The slower half of the same clock: a bot count and a one-hour close are
  // not five-second questions, and `/bots` walks the database.
  if (dashTicks === 1 || dashTicks % 2 === 0) {
    loadDashBots();
    loadDashSpark();
  }
}

function paintDashDate() {
  const node = el("dashDate");
  if (!node) return;
  node.textContent = new Date().toLocaleDateString("en-GB", {
    weekday: "long", day: "numeric", month: "long",
  });
}

/// A card's own error, with its own retry. The row keeps its shape: a failed
/// card says so in place rather than blanking a quarter of the page.
function dashInlineError(kind, message) {
  const line = dashEl("p", "pulseNote pulseNote--error");
  line.appendChild(document.createTextNode(`${message} `));
  const retry = dashEl("button", "hrz-btn hrz-btn--ghost hrz-btn--sm", "Retry");
  retry.type = "button";
  retry.dataset.retry = kind;
  line.appendChild(retry);
  return line;
}

function percentNode(value) {
  const number = Number(value);
  const dir = number > 0 ? "up" : number < 0 ? "down" : "flat";
  return dashEl("span", `hrz-price hrz-price--${dir}`, HRZUI.format.percent(number));
}

function ageText(value) {
  const at = Date.parse(value || "");
  if (!Number.isFinite(at)) return "—";
  const seconds = Math.max(0, Math.round((Date.now() - at) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)}h ago`;
  return `${Math.round(seconds / 86400)}d ago`;
}

/// Show a symbol on the chart wall, from any of the dashboard's four lists.
///
/// `chartSymbol` already means "reuse the pane showing it, otherwise the active
/// pane", so the dashboard does not add a second opinion about where a chart
/// goes -- it only makes sure the destination is the one that has panes.
function openInCharts(symbol) {
  if (!symbol) return;
  if (window.Shell) Shell.go("charts");
  if (window.HRZApp && HRZApp.chartSymbol) HRZApp.chartSymbol(symbol);
}
```

- [ ] **Step 2: The header and the four pulses**

Continue the same section:

```js
/* ---- The page header ------------------------------------------------------ */

function paintDashMarketLine() {
  const host = el("dashMarket");
  if (!host) return;
  host.replaceChildren();
  const ticker = dashUniverse.find((row) => row.symbol === "BTCUSDT") || dashUniverse[0];
  if (!ticker) {
    host.textContent = "—";
    return;
  }
  host.appendChild(dashEl("span", "", ticker.symbol));
  host.appendChild(document.createTextNode(" "));
  host.appendChild(dashEl("span", "num", fmtPrice(Number(ticker.last_price))));
  host.appendChild(document.createTextNode(" "));
  host.appendChild(percentNode(ticker.price_change_percent));
}

/* ---- Market --------------------------------------------------------------- */

function paintDashMarketPulse() {
  const host = el("pulseMarket");
  if (!host) return;
  host.replaceChildren();
  if (dashUniverseFailure && !dashUniverse.length) {
    host.appendChild(dashInlineError("market", dashUniverseFailure));
    return;
  }
  const ticker = dashUniverse.find((row) => row.symbol === "BTCUSDT") || dashUniverse[0];
  if (!ticker) {
    host.appendChild(dashEl("p", "pulseNote", "No market data yet."));
    return;
  }
  const row = dashEl("div", "pulseValueRow");
  row.appendChild(dashEl("span", "pulseValue", fmtPrice(Number(ticker.last_price))));
  row.appendChild(percentNode(ticker.price_change_percent));
  host.appendChild(row);
  host.appendChild(dashEl("p", "pulseNote",
    `${ticker.symbol} · 24h volume ${HRZUI.format.volume(ticker.quote_volume)}`));
  if (dashSpark && dashSpark.length > 1) {
    host.appendChild(sparkLine(dashSpark, Number(ticker.price_change_percent) < 0 ? "down" : "up"));
  }
}

/// The pulse line is SVG, not `HRZUI.sparkline`: a canvas cannot read
/// `var(--up)`, and this card has to follow the theme like the chrome around it.
function sparkLine(values, trend) {
  const width = 100;
  const height = 26;
  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = max - min || 1;
  const points = values
    .map((value, index) => {
      const x = (index / (values.length - 1)) * width;
      const y = height - 2 - ((value - min) / span) * (height - 4);
      return `${x.toFixed(2)},${y.toFixed(2)}`;
    })
    .join(" ");
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "pulseSpark");
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
  svg.setAttribute("preserveAspectRatio", "none");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label",
    `Last ${values.length} hours: low ${fmtPrice(min)}, high ${fmtPrice(max)}, now ${fmtPrice(values[values.length - 1])}`);
  svg.dataset.trend = trend;
  const line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
  line.setAttribute("points", points);
  svg.appendChild(line);
  return svg;
}

async function loadDashSpark() {
  try {
    const answer = await api("/candles?symbol=BTCUSDT&timeframe=1h&limit=24");
    const closes = (answer.candles || [])
      .map((candle) => Number(candle.close))
      .filter((value) => Number.isFinite(value))
      .slice(-24);
    if (closes.length > 1) {
      dashSpark = closes;
      paintDashMarketPulse();
    }
  } catch {
    // The card keeps its price and drops the line: a sparkline nobody can load
    // is not worth an error message in the space a price goes.
  }
}

/* ---- Watchlist ------------------------------------------------------------ */

function paintDashWatchPulse() {
  const host = el("pulseWatch");
  if (!host) return;
  host.replaceChildren();
  const favorites = watchlistFavorites();
  if (!favorites.length) {
    host.appendChild(dashEl("p", "pulseNote", "No favorites yet. Star a symbol, or use + Add beside the list."));
    return;
  }
  const priced = favorites.map((symbol) => watchlistTickers.get(symbol)).filter(Boolean);
  const up = priced.filter((ticker) => Number(ticker.price_change_percent) >= 0).length;
  const row = dashEl("div", "pulseValueRow");
  row.appendChild(dashEl("span", "pulseValue", String(favorites.length)));
  row.appendChild(dashEl("span", "pulseNote", favorites.length === 1 ? "symbol" : "symbols"));
  host.appendChild(row);
  const split = dashEl("div", "pulseSplit");
  split.appendChild(dashEl("span", "pulseSplit__up", `${up} up`));
  split.appendChild(dashEl("span", "muted", "·"));
  split.appendChild(dashEl("span", "pulseSplit__down", `${priced.length - up} down`));
  host.appendChild(split);
  const best = priced
    .slice()
    .sort((a, b) => Number(b.price_change_percent) - Number(a.price_change_percent))[0];
  if (best) {
    const line = dashEl("p", "pulseNote");
    line.appendChild(document.createTextNode("top "));
    line.appendChild(dashEl("span", "num", best.symbol));
    line.appendChild(document.createTextNode(" "));
    line.appendChild(percentNode(best.price_change_percent));
    host.appendChild(line);
  }
}

/* ---- Automation ----------------------------------------------------------- */

async function loadDashBots() {
  try {
    const [bots, events] = await Promise.all([
      api("/bots"),
      api("/watch-events?limit=5").catch(() => ({ events: [] })),
    ]);
    dashBots = {
      bots: Array.isArray(bots) ? bots : [],
      events: (events && events.events) || [],
      at: Date.now(),
    };
    dashBotsFailure = "";
  } catch (error) {
    dashBotsFailure = error.message;
  }
  paintDashAutomationPulse();
}

function paintDashAutomationPulse() {
  const host = el("pulseAuto");
  if (!host) return;
  host.replaceChildren();
  if (!dashBots) {
    host.appendChild(
      dashBotsFailure
        ? dashInlineError("automation", dashBotsFailure)
        : dashEl("p", "pulseNote", "Loading…")
    );
    return;
  }
  // "killed" is the risk engine's word; "stopped" is the reader's.
  const counts = { running: 0, paused: 0, killed: 0 };
  for (const bot of dashBots.bots) {
    if (bot.status in counts) counts[bot.status] += 1;
  }
  const row = dashEl("div", "pulseValueRow");
  row.appendChild(dashEl("span", "pulseValue pulseValue--sm",
    `${counts.running} running · ${counts.paused} paused · ${counts.killed} stopped`));
  host.appendChild(row);
  const latest = dashBots.events[0];
  host.appendChild(dashEl("p", "pulseNote",
    latest ? `last signal ${ageText(latest.created_at)}` : "no signals yet"));
}

/* ---- Plan ----------------------------------------------------------------- */

function planMeter(label, used, limit) {
  const meter = dashEl("div", "pulseMeter");
  const max = Number(limit);
  if (max < 0) {
    meter.appendChild(dashEl("span", "pulseNote", `${label}: unlimited`));
    return meter;
  }
  const value = Number(used) || 0;
  HRZUI.meter(meter, {
    value,
    max: max || 1,
    label,
    tone: max > 0 && value / max >= 0.8 ? "warn" : "",
  });
  return meter;
}

/// Painted from `billingState` and repainted from `refreshBillingPane`, which
/// every billing write already calls -- so a subscription started on Account is
/// the card's new answer without a second fetch or a second listener.
function paintDashPlan() {
  const host = el("pulsePlan");
  if (!host) return;
  host.replaceChildren();
  if (!billingState) {
    host.appendChild(
      billingLoad
        ? dashEl("p", "pulseNote", "Loading…")
        : dashInlineError("plan", "Plan unavailable")
    );
    return;
  }
  const usage = billingState.usage || {};
  host.appendChild(dashEl("p", "pulseValue pulseValue--sm",
    billingState.plan_name || billingState.plan || "No plan"));
  const credits = usage.ai_credits || { used: 0, limit: 0 };
  host.appendChild(planMeter("AI credits", credits.used, credits.limit));
  const backtests = usage.backtests || { used: 0, limit: 0 };
  host.appendChild(planMeter("Backtests", backtests.used, backtests.limit));
  const resets = usage.resets_at ? new Date(usage.resets_at) : null;
  if (resets && !Number.isNaN(resets.getTime())) {
    host.appendChild(dashEl("p", "pulseNote",
      `resets ${resets.toLocaleDateString("en-GB", { day: "numeric", month: "short" })}`));
  }
}
```

`billingLoad` is read, not written: `loadBilling` already nulls it in its
`finally`, so "no state and a load in flight" and "no state and no load" are
different sentences on the card, and neither lies.

- [ ] **Step 3: The markets table**

Continue the section:

```js
/* ---- Markets -------------------------------------------------------------- */

/// What the active tab shows, filtered and sorted. Kept separate from the paint
/// so the sort, the tab and the filter all agree about what "total" means --
/// the count in the footer is the number of rows *after* filtering, which is
/// the only number a reader who typed in the box cares about.
function dashMarketRows() {
  let rows;
  if (dashTab === "watchlist") {
    rows = watchlistFavorites().map((symbol) => watchlistTickers.get(symbol) || { symbol });
  } else {
    rows = dashUniverse.slice();
    if (dashTab === "majors") rows = rows.filter((ticker) => MAJORS.includes(ticker.symbol));
    if (dashTab === "alts") rows = rows.filter((ticker) => !MAJORS.includes(ticker.symbol));
  }
  if (dashFilter) rows = rows.filter((ticker) => ticker.symbol.includes(dashFilter));
  const key = dashSort.key;
  const read = (ticker) =>
    key === "symbol" ? ticker.symbol
      : key === "last" ? Number(ticker.last_price)
        : key === "change" ? Number(ticker.price_change_percent)
          : Number(ticker.quote_volume);
  return rows.sort((a, b) => {
    const left = read(a);
    const right = read(b);
    if (typeof left === "string" || typeof right === "string") {
      return dashSort.dir === "asc"
        ? String(left).localeCompare(String(right))
        : String(right).localeCompare(String(left));
    }
    // A row with no quote sinks whichever way the column sorts: it is not the
    // "highest", it is unknown.
    const leftOk = Number.isFinite(left);
    const rightOk = Number.isFinite(right);
    if (!leftOk || !rightOk) return leftOk === rightOk ? 0 : leftOk ? -1 : 1;
    return dashSort.dir === "asc" ? left - right : right - left;
  });
}

function marketRow(ticker, activeSymbol) {
  const row = document.createElement("tr");
  row.dataset.symbol = ticker.symbol;
  if (ticker.symbol === activeSymbol) row.className = "hrz-table__row--selected";

  const first = document.createElement("td");
  const open = dashEl("button", "marketSym");
  open.type = "button";
  open.dataset.open = ticker.symbol;
  const mark = dashEl("span", "marketMark");
  const change = Number(ticker.price_change_percent);
  if (Number.isFinite(change) && change !== 0) mark.dataset.dir = change > 0 ? "up" : "down";
  open.appendChild(mark);
  open.appendChild(dashEl("span", "", baseOf(ticker.symbol)));
  const quote = quoteOf(ticker.symbol);
  if (quote) open.appendChild(dashEl("span", "marketQuote", quote));
  first.appendChild(open);
  row.appendChild(first);

  row.appendChild(dashEl("td", "hrz-table__num", fmtPrice(Number(ticker.last_price))));

  const delta = dashEl("td", "hrz-table__num");
  delta.appendChild(percentNode(change));
  row.appendChild(delta);

  row.appendChild(dashEl("td", "hrz-table__num", HRZUI.format.volume(ticker.quote_volume)));
  return row;
}

function paintDashMarkets() {
  paintDashMarketLine();
  const body = el("marketRows");
  if (!body) return;
  const rows = dashMarketRows();
  const active = activePane ? activePane.symbol() : "";
  body.replaceChildren();
  for (const ticker of rows.slice(0, dashShown)) body.appendChild(marketRow(ticker, active));

  const total = rows.length;
  el("marketEmpty").hidden = total > 0;
  if (!total) {
    el("marketEmpty").textContent = dashFilter
      ? `Nothing matches "${dashFilter}".`
      : dashTab === "watchlist"
        ? "No favorites yet. Star a symbol in Charts, or use + Add on the watchlist panel."
        : "The venue returned no instruments.";
  }
  el("marketCount").textContent = total > dashShown
    ? `${dashShown} of ${total}`
    : `${total} instrument${total === 1 ? "" : "s"}`;
  el("marketMore").hidden = total <= dashShown;
  // The header states the sort, because a table that reorders itself without
  // saying so is a table the reader has to re-read to trust.
  for (const head of document.querySelectorAll(".marketTable thead th[data-sort]")) {
    head.setAttribute(
      "aria-sort",
      head.dataset.sort === dashSort.key
        ? (dashSort.dir === "desc" ? "descending" : "ascending")
        : "none"
    );
  }
}
```

- [ ] **Step 4: The watchlist panel**

It is the same list the Charts rail shows -- one `localStorage` key, one ticker
map -- in a second presentation. Removing a star here updates the rail too,
which is why the star calls `renderWatchlist()` and not only its own paint.

```js
/* ---- The watchlist panel -------------------------------------------------- */

function paintDashWatch() {
  const host = el("dashWatchRows");
  if (!host) return;
  const favorites = watchlistFavorites();
  host.replaceChildren();
  el("dashWatchEmpty").hidden = favorites.length > 0;
  for (const symbol of favorites) {
    const ticker = watchlistTickers.get(symbol) || {};
    const change = Number(ticker.price_change_percent);
    const row = dashEl("div", "dashWatchRow");

    const open = dashEl("button", "dashWatchRow__sym", baseOf(symbol));
    open.type = "button";
    open.dataset.open = symbol;
    open.setAttribute("aria-label", `Open ${symbol} in Charts`);
    row.appendChild(open);

    row.appendChild(dashEl("span", "dashWatchRow__last",
      ticker.last_price === undefined ? "—" : fmtPrice(Number(ticker.last_price))));

    const pct = dashEl("span", "dashWatchRow__pct");
    pct.appendChild(percentNode(change));
    row.appendChild(pct);

    const star = dashEl("button", "dashWatchRow__star", "★");
    star.type = "button";
    star.dataset.star = symbol;
    star.setAttribute("aria-pressed", "true");
    star.setAttribute("aria-label", `Remove ${symbol} from the watchlist`);
    row.appendChild(star);

    host.appendChild(row);
  }
  el("dashWatchAge").textContent = dashUniverseAt
    ? `prices ${Math.max(0, Math.round((Date.now() - dashUniverseAt) / 1000))}s ago`
    : "";
}

/// The panel's own search, because the palette's symbol items *chart* a symbol
/// and this one *pins* it. Reusing the palette would mean a symbol search with
/// two different meanings behind one control.
async function dashWatchSearch(query) {
  const host = el("dashWatchResults");
  if (!host) return;
  host.replaceChildren();
  if (!query) return;
  try {
    const results = await HRZApp.searchSymbols(query);
    if (el("dashWatchInput").value.trim() !== query) return;
    const starred = new Set(watchlistFavorites());
    for (const result of results.slice(0, 12)) {
      const button = dashEl("button", "dashWatch__result");
      button.type = "button";
      button.dataset.symbol = result.symbol;
      button.appendChild(dashEl("span", "", result.base || baseOf(result.symbol)));
      button.appendChild(dashEl("span", "muted", starred.has(result.symbol) ? "★" : "☆"));
      host.appendChild(button);
    }
    if (!host.children.length) host.appendChild(dashEl("p", "pulseNote", "Nothing matches."));
  } catch (error) {
    host.appendChild(dashEl("p", "pulseNote pulseNote--error", error.message));
  }
}
```

- [ ] **Step 5: The heatmap**

Three functions decide everything the reader sees: which sixty rows, which of
seven colours, and which of four heights. The bucket is a *band*, not a rank,
so two pairs up 5% are the same colour; the size is a *rank*, so the mosaic
says "this is where the volume is" even on a flat day.

```js
/* ---- The heatmap ---------------------------------------------------------- */

function dashHeatRows() {
  return dashUniverse
    .filter((ticker) => Number.isFinite(Number(ticker.quote_volume)))
    .slice()
    .sort((a, b) => Number(b.quote_volume) - Number(a.quote_volume))
    .slice(0, HEAT_LIMIT);
}

/// Seven bands: down hard, down, down a little, flat, up a little, up, up hard.
/// Four percent is the outer edge because it is roughly where a daily move
/// stops being noise on the venues this page lists.
function heatBucket(change) {
  const value = Number(change);
  if (!Number.isFinite(value) || Math.abs(value) < 0.05) return "0";
  if (value >= 4) return "3";
  if (value >= 2) return "2";
  if (value > 0) return "1";
  if (value <= -4) return "-3";
  if (value <= -2) return "-2";
  return "-1";
}

/// Quartiles of the sliced list: the top quarter spans four rows, the bottom
/// quarter one. Ranked, not measured, so the mosaic always uses its whole range
/// even when every pair traded the same amount.
function heatSize(index, total) {
  if (!total) return 1;
  return 4 - Math.floor((index / total) * 4);
}

function paintDashHeat() {
  const grid = el("heatGrid");
  if (!grid) return;
  const rows = dashHeatRows();
  const table = el("heatRows");
  grid.replaceChildren();
  if (table) table.replaceChildren();
  el("heatEmpty").hidden = rows.length > 0;
  el("heatHint").textContent = rows.length
    ? `Top ${rows.length} by 24h volume`
    : "Top 60 by 24h volume";
  rows.forEach((ticker, index) => {
    const base = baseOf(ticker.symbol);
    const change = Number(ticker.price_change_percent);
    const tile = dashEl("button", "heatTile");
    tile.type = "button";
    tile.dataset.symbol = ticker.symbol;
    tile.dataset.bucket = heatBucket(change);
    tile.dataset.size = String(heatSize(index, rows.length));
    const direction = change > 0 ? "up" : change < 0 ? "down" : "flat";
    // The label is the tile's whole content to a screen reader, so it carries
    // the direction as a word: "SOL, up 8.1 percent".
    tile.setAttribute("aria-label",
      `${base}, ${direction} ${Math.abs(Number.isFinite(change) ? change : 0).toFixed(1)} percent`);
    tile.appendChild(dashEl("span", "heatTile__sym", base));
    tile.appendChild(dashEl("span", "heatTile__pct", HRZUI.format.percent(change)));
    grid.appendChild(tile);

    if (table) {
      const row = document.createElement("tr");
      row.appendChild(dashEl("td", "", ticker.symbol));
      const pct = dashEl("td", "hrz-table__num");
      pct.appendChild(percentNode(change));
      row.appendChild(pct);
      row.appendChild(dashEl("td", "hrz-table__num", HRZUI.format.volume(ticker.quote_volume)));
      table.appendChild(row);
    }
  });
}

/// The view is a device preference, so it is remembered locally: a user who
/// asked for a table once should not have to ask again every morning. Written
/// and read through one function so the toggle and the restored state cannot
/// disagree about which is on.
function setHeatView(asTable) {
  el("heatGrid").hidden = asTable;
  el("heatTableWrap").hidden = !asTable;
  const toggle = el("heatTableToggle");
  toggle.setAttribute("aria-pressed", String(asTable));
  toggle.textContent = asTable ? "Show as map" : "Show as table";
  try {
    localStorage.setItem(HEAT_KEY, String(asTable));
  } catch {
    // The view still swaps; only the memory of it is lost.
  }
}

function applyHeatView() {
  let asTable = false;
  try {
    asTable = localStorage.getItem(HEAT_KEY) === "true";
  } catch {
    asTable = false;
  }
  setHeatView(asTable);
}
```

- [ ] **Step 6: Wire it, and hand the shell the hook**

1. Add to the dashboard section, after `applyHeatView`:

```js
/* ---- Wiring --------------------------------------------------------------- */

function wireDashboard() {
  const body = el("marketRows");
  if (body) {
    body.addEventListener("click", (event) => {
      const target = event.target.closest("[data-open]") || event.target.closest("tr[data-symbol]");
      if (target) openInCharts(target.dataset.open || target.dataset.symbol);
    });
  }

  const tabs = el("marketTabs");
  if (tabs) {
    tabs.addEventListener("click", (event) => {
      const tab = event.target.closest("[data-market]");
      if (!tab) return;
      dashTab = tab.dataset.market;
      dashShown = DASH_PAGE;
      for (const button of tabs.querySelectorAll("[data-market]")) {
        button.setAttribute("aria-selected", String(button === tab));
      }
      paintDashMarkets();
    });
  }

  for (const head of document.querySelectorAll(".marketTable thead th[data-sort]")) {
    const button = head.querySelector(".hrz-table__sort");
    if (!button) continue;
    button.addEventListener("click", () => {
      // A column that is already sorting flips; a new column starts on its
      // useful direction (A-Z for names, biggest first for numbers).
      if (dashSort.key === head.dataset.sort) {
        dashSort = { key: dashSort.key, dir: dashSort.dir === "desc" ? "asc" : "desc" };
      } else {
        dashSort = { key: head.dataset.sort, dir: head.dataset.sort === "symbol" ? "asc" : "desc" };
      }
      dashShown = DASH_PAGE;
      paintDashMarkets();
    });
  }

  const filter = el("marketFilter");
  if (filter) {
    filter.addEventListener("input", () => {
      dashFilter = filter.value.trim().toUpperCase();
      dashShown = DASH_PAGE;
      paintDashMarkets();
    });
  }

  const more = el("marketMore");
  if (more) {
    more.addEventListener("click", () => {
      dashShown += DASH_PAGE;
      paintDashMarkets();
    });
  }

  const refresh = el("dashRefresh");
  if (refresh) {
    refresh.addEventListener("click", () => {
      dashTick();
      pageToast("Dashboard refreshed");
    });
  }

  const newChart = el("dashNewChart");
  if (newChart) {
    newChart.addEventListener("click", () => {
      if (window.Shell) Shell.go("charts");
      if (window.HRZApp && HRZApp.newPane) HRZApp.newPane();
    });
  }

  // One listener for four retries: the card names the thing to retry, so the
  // row does not grow a handler per card.
  const pulses = el("pulseRow");
  if (pulses) {
    pulses.addEventListener("click", (event) => {
      const retry = event.target.closest("[data-retry]");
      if (!retry) return;
      if (retry.dataset.retry === "plan") {
        loadBilling();
      } else if (retry.dataset.retry === "automation") {
        dashBotsFailure = "";
        loadDashBots();
      } else {
        dashTick();
      }
    });
  }

  const watchRows = el("dashWatchRows");
  if (watchRows) {
    watchRows.addEventListener("click", (event) => {
      const star = event.target.closest("[data-star]");
      if (star) {
        toggleFavorite(star.dataset.star);
        paintDashWatch();
        paintDashWatchPulse();
        paintDashMarkets();
        // The rail is the same list: removing here must remove it there.
        renderWatchlist();
        return;
      }
      const open = event.target.closest("[data-open]");
      if (open) openInCharts(open.dataset.open);
    });
  }

  const add = el("dashWatchAdd");
  if (add) {
    const box = el("dashWatchSearch");
    const input = el("dashWatchInput");
    add.addEventListener("click", () => {
      const opening = box.hidden;
      box.hidden = !opening;
      add.setAttribute("aria-expanded", String(opening));
      add.textContent = opening ? "Done" : "+ Add";
      if (opening) {
        input.value = "";
        el("dashWatchResults").replaceChildren();
        input.focus();
      }
    });
    input.addEventListener("input", () => dashWatchSearch(input.value.trim()));
    el("dashWatchResults").addEventListener("click", (event) => {
      const result = event.target.closest("[data-symbol]");
      if (!result) return;
      toggleFavorite(result.dataset.symbol);
      paintDashWatch();
      paintDashWatchPulse();
      paintDashMarkets();
      renderWatchlist();
      // Reflect the star in the list still on screen: the query has not
      // changed, so re-asking the server would be a request for an answer
      // this page already has.
      const starred = new Set(watchlistFavorites());
      for (const row of el("dashWatchResults").children) {
        const mark = row.querySelector(".muted");
        if (mark && row.dataset.symbol) {
          mark.textContent = starred.has(row.dataset.symbol) ? "★" : "☆";
        }
      }
    });
  }

  const heat = el("heatGrid");
  if (heat) {
    heat.addEventListener("click", (event) => {
      const tile = event.target.closest("[data-symbol]");
      if (tile) openInCharts(tile.dataset.symbol);
    });
  }
  const table = el("heatTableToggle");
  if (table) {
    table.addEventListener("click", () => {
      setHeatView(table.getAttribute("aria-pressed") !== "true");
    });
  }
}
```

2. In `main()`, after Task 13's `wireDock();`, add:

```js
  wireDashboard();
```

3. In the `window.HRZApp` object, after Task 14's `openChartSettings,`:

```js
  setDestinationVisible,
```

4. Repaint the plan card on every billing write. In `refreshBillingPane`
   (`app.js:13252`), add the last line:

```js
function refreshBillingPane() {
  paintBillingModeNote();
  paintBillingCurrent();
  paintBillingPlans();
  // The dashboard's Plan card is a second view of the same state, and this is
  // the one function every subscription write already calls -- so the card
  // cannot disagree with the Account page.
  paintDashPlan();
}
```

`paintDashPlan` returns at its guard when the dashboard is not in the
document's flow, so this costs one `getElementById` on Account.

- [ ] **Step 7: Let the shell stop the clock, and stop the rail's**

Two edits in `views/shell.js`.

1. `runDestinationLoaders` (Task 9's version, extended by Task 12) gains a
   dashboard branch. Called with no name, `runPaneLoaders` falls through every
   `if` and stops the watchlist ticker -- which is exactly what leaving Charts
   for the dashboard should do:

```js
  function runDestinationLoaders() {
    const load = app().runPaneLoaders;
    if (!load) return;
    if (state.destination === "studio") load(state.studioPane);
    else if (state.destination === "community") load("community");
    else if (state.destination === "bots") load("bots");
    else if (state.destination === "account") {
      const legacy = LEGACY_FROM_ACCOUNT[state.accountSection];
      if (legacy) load(legacy);
    } else if (state.destination === "dashboard") {
      // No pane is on screen -- and the loader's own `else` is what stops the
      // watchlist ticker, so the dashboard's page clock is the only one left
      // running. `runPaneLoaders()` with no name means exactly that.
      load();
    }
  }
```

2. In `showDestination`, after the `resizePanes` line:

```js
    if (state.destination === "charts" && app().resizePanes) app().resizePanes();
    // The dashboard keeps a clock, and it is told when it is on screen rather
    // than asking the DOM. Called last, so the destination is fully mounted
    // before the first paint lands on it.
    if (app().setDestinationVisible) app().setDestinationVisible(state.destination);
  }
```

- [ ] **Step 8: Teach the harness**

In `tools/shell_check.mjs`:

1. Record the page's clocks. Right after the token is written
   (`window.localStorage.setItem(tokenKey, …)`), and before any script is
   injected, add:

```js
// --- the page's clocks -------------------------------------------------------
//
// The dashboard's tick is a five-second interval, and jsdom will not run one
// inside a test. Recording what the page registered -- by the callback's name,
// which is a function declaration's name -- is how "the dashboard's clock runs
// there and nowhere else" is checked without waiting five seconds.
const intervals = new Map();
const realSetInterval = window.setInterval;
const realClearInterval = window.clearInterval;
window.setInterval = (callback, ms, ...rest) => {
  const id = realSetInterval(callback, ms, ...rest);
  intervals.set(id, { ms, name: callback && callback.name });
  return id;
};
window.clearInterval = (id) => {
  intervals.delete(id);
  realClearInterval(id);
};
/// How many live clocks were registered by a function with this name.
const clocksNamed = (name) => [...intervals.values()].filter((clock) => clock.name === name).length;
```

2. Add the venue's most active list to the fixture. Inside `backend`, after
   `calls: []`:

```js
  // The dashboard's one batch, `GET /tickers?limit=1000`: ten majors and sixty
  // alts, volumes descending (which is the order the route really returns),
  // changes chosen to land in every heat bucket. BTC and ETH keep the
  // directions Task 12's rail checks expect (BTC up, ETH down) so whichever
  // of the two batches last answered, the rail's tint is the same.
  markets: (() => {
    const majors = [
      ["BTCUSDT", 67412.5, 2.5],
      ["ETHUSDT", 2481.1, -2.25],
      ["SOLUSDT", 148.2, 8.1],
      ["BNBUSDT", 592.4, 0.4],
      ["XRPUSDT", 0.6123, -0.9],
      ["ADAUSDT", 0.4412, -2.6],
      ["DOGEUSDT", 0.1284, -4.4],
      ["AVAXUSDT", 34.2, 0],
      ["LINKUSDT", 17.8, 1.2],
      ["DOTUSDT", 7.05, -1.4],
    ];
    const rows = majors.map(([symbol, last, change], index) => ({
      symbol,
      last_price: last,
      price_change_percent: change,
      high_price: last * 1.05,
      low_price: last * 0.95,
      quote_volume: 2_000_000_000 - index * 100_000_000,
    }));
    for (let i = 0; i < 60; i += 1) {
      rows.push({
        symbol: `ALT${String(i).padStart(2, "0")}USDT`,
        last_price: 500 + i * 10,
        price_change_percent: i % 2 === 0 ? 1.4 : -1.6,
        high_price: 600 + i * 10,
        low_price: 400 + i * 10,
        quote_volume: 900_000_000 - i * 10_000_000,
      });
    }
    return rows;
  })(),
```

3. Answer `?limit=` from that list. Inside the `/tickers` branch Task 12 added,
   its first lines become:

```js
  if (url.startsWith("/tickers")) {
    const query = queryOf(url);
    // The venue's most active list: what the markets table, the heatmap and
    // the watchlist panel read on the dashboard.
    if (query.get("limit")) return json(200, { tickers: backend.markets });
    const asked = (query.get("symbols") ?? "BTCUSDT,ETHUSDT").split(",").filter(Boolean);
```

4. Answer `/bots`, `/billing/plans` and `/billing/subscription`. Insert beside
   the `/watch-events` branch:

```js
  // The dashboard's Automation pulse. Two running, one paused, one killed --
  // one of each status the counts distinguish.
  if (url === "/bots") {
    return json(200, [
      { id: "bot-1", status: "running", mode: "paper", venue: "binance" },
      { id: "bot-2", status: "running", mode: "paper", venue: "binance" },
      { id: "bot-3", status: "paused", mode: "paper", venue: "binance" },
      { id: "bot-4", status: "killed", mode: "paper", venue: "binance" },
    ]);
  }
  // The dashboard's Plan pulse. `loadBilling` reads the catalog first, so a
  // missing one would leave the card saying "unavailable" while everything it
  // needs was configured.
  if (url === "/billing/plans") {
    return json(200, { billing_available: true, mode: "internal", trial_days: 14, plans: [] });
  }
  if (url === "/billing/subscription") {
    return json(200, {
      plan: "scout",
      plan_name: "Scout",
      limits: { max_panes: 4, max_ai_credits: 100 },
      features: {},
      subscription: null,
      usage: {
        period_start: "2026-10-01T00:00:00Z",
        resets_at: "2026-11-01T00:00:00Z",
        ai_credits: { used: 40, limit: 100, remaining: 60 },
        backtests: { used: 3, limit: -1, remaining: -1 },
      },
      community_indicators_available: true,
    });
  }
```

5. Drive it. Append after Task 15's four structure checks:

```js
// --- the dashboard's data ----------------------------------------------------
//
// One clock, four surfaces. Every check drives a control or reads a pane back;
// none of them calls a paint function directly.

if (window.Shell) Shell.go("dashboard");
await waitFor(
  () => document.querySelectorAll("#marketRows tr[data-symbol]").length > 0,
  "the markets table to fill"
);

const marketRows = () => [...document.querySelectorAll("#marketRows tr[data-symbol]")];
const marketSymbols = () => marketRows().map((row) => row.dataset.symbol);

check(
  "the page leads with its date and a market line",
  document.getElementById("dashDate").textContent.length > 0 &&
    document.getElementById("dashMarket").textContent.includes("BTCUSDT") &&
    document.getElementById("dashMarket").textContent.includes("67,412.50"),
  document.getElementById("dashMarket").textContent
);
check(
  "the market pulse reads BTC and draws the 24-hour line",
  document.getElementById("pulseMarket").textContent.includes("67,412.50") &&
    document.querySelectorAll("#pulseMarket .hrz-price--up").length === 1 &&
    document.querySelector("#pulseMarket svg.pulseSpark polyline") !== null,
  document.getElementById("pulseMarket").textContent.trim()
);
check(
  "with no favorites the watchlist pulse and panel both say so",
  document.getElementById("pulseWatch").textContent.includes("No favorites") &&
    document.getElementById("dashWatchEmpty").hidden === false &&
    document.querySelectorAll("#dashWatchRows .dashWatchRow").length === 0
);
check(
  "the automation pulse counts every status and ages the last signal",
  document.getElementById("pulseAuto").textContent.includes("2 running · 1 paused · 1 stopped") &&
    /last signal .+ ago/.test(document.getElementById("pulseAuto").textContent),
  document.getElementById("pulseAuto").textContent.trim()
);
check(
  "the plan pulse names the plan and meters the credits",
  document.getElementById("pulsePlan").textContent.includes("Scout") &&
    document.getElementById("pulsePlan").querySelector('[role="progressbar"]').getAttribute("aria-valuenow") === "40" &&
    document.getElementById("pulsePlan").textContent.includes("Backtests: unlimited"),
  document.getElementById("pulsePlan").textContent.trim()
);

check(
  "the table shows the first fifty of seventy, sorted by volume",
  marketRows().length === 50 &&
    document.getElementById("marketCount").textContent === "50 of 70" &&
    document.getElementById("marketMore").hidden === false &&
    marketSymbols()[0] === "BTCUSDT",
  `${marketRows().length} rows · ${document.getElementById("marketCount").textContent}`
);
document.getElementById("marketMore").click();
await settle();
check(
  "Show 50 more appends the rest and goes away",
  marketRows().length === 70 && document.getElementById("marketMore").hidden === true,
  `${marketRows().length} rows`
);

document.querySelector("#marketTabs [data-market='majors']").click();
await settle();
check(
  "Majors is the ten liquid pairs",
  marketRows().length === 10 && marketSymbols().includes("DOGEUSDT") && !marketSymbols().includes("ALT00USDT"),
  `${marketRows().length} rows`
);
document.querySelector("#marketTabs [data-market='alts']").click();
await settle();
check(
  "Alts is everything else, paged at fifty",
  marketRows().length === 50 &&
    document.getElementById("marketCount").textContent === "50 of 60" &&
    !marketSymbols().includes("BTCUSDT"),
  `${marketRows().length} rows · ${document.getElementById("marketCount").textContent}`
);
document.querySelector("#marketTabs [data-market='all']").click();
await settle();

const lastHead = document.querySelector(".marketTable th[data-sort='last']");
lastHead.querySelector(".hrz-table__sort").click();
await settle();
check(
  "sorting by last starts biggest-first and says so",
  lastHead.getAttribute("aria-sort") === "descending" && marketSymbols()[0] === "BTCUSDT",
  `${lastHead.getAttribute("aria-sort")} · ${marketSymbols()[0]}`
);
lastHead.querySelector(".hrz-table__sort").click();
await settle();
check(
  "a second press flips the column",
  lastHead.getAttribute("aria-sort") === "ascending" && marketSymbols()[0] === "DOGEUSDT",
  `${lastHead.getAttribute("aria-sort")} · ${marketSymbols()[0]}`
);
document.querySelector(".marketTable th[data-sort='volume'] .hrz-table__sort").click();
await settle();

const filter = document.getElementById("marketFilter");
filter.value = "btc";
filter.dispatchEvent(new window.Event("input"));
await settle();
check(
  "the filter narrows before the page does",
  marketRows().length === 1 && marketSymbols()[0] === "BTCUSDT" &&
    document.getElementById("marketCount").textContent === "1 instrument",
  `${marketRows().length} rows · ${document.getElementById("marketCount").textContent}`
);
filter.value = "";
filter.dispatchEvent(new window.Event("input"));
await settle();

// --- the watchlist panel -----------------------------------------------------
//
// The panel writes the same key the Charts rail reads, so the check that
// matters is the one that leaves through the panel and reads the rail.

document.getElementById("dashWatchAdd").click();
check(
  "the add control opens a search and says so",
  document.getElementById("dashWatchSearch").hidden === false &&
    document.getElementById("dashWatchAdd").getAttribute("aria-expanded") === "true"
);
const addInput = document.getElementById("dashWatchInput");
addInput.value = "ETH";
addInput.dispatchEvent(new window.Event("input"));
await waitFor(
  () => document.querySelectorAll("#dashWatchResults [data-symbol]").length > 0,
  "the symbol search to answer"
);
document.querySelector("#dashWatchResults [data-symbol='ETHUSDT']").click();
await settle();
check(
  "adding a symbol pins it, prices it and marks it in the results",
  document.querySelectorAll("#dashWatchRows .dashWatchRow").length === 1 &&
    document.getElementById("dashWatchRows").textContent.includes("ETH") &&
    document.getElementById("dashWatchEmpty").hidden === true &&
    JSON.parse(window.localStorage.getItem("watchlist.favorites")).includes("ETHUSDT") &&
    document.querySelector("#dashWatchResults [data-symbol='ETHUSDT'] .muted").textContent === "★",
  document.getElementById("dashWatchRows").textContent.trim()
);
document.getElementById("dashWatchAdd").click();
check(
  "and Done puts the search away",
  document.getElementById("dashWatchSearch").hidden === true &&
    document.getElementById("dashWatchAdd").getAttribute("aria-expanded") === "false"
);
document.querySelector("#dashWatchRows [data-star]").click();
await settle();
check(
  "the row's star removes it from both lists",
  document.querySelectorAll("#dashWatchRows .dashWatchRow").length === 0 &&
    document.getElementById("dashWatchEmpty").hidden === false &&
    JSON.parse(window.localStorage.getItem("watchlist.favorites")).length === 0 &&
    !document.querySelector("#wlRows .watchRow[data-symbol='ETHUSDT']"),
  document.getElementById("dashWatchRows").textContent.trim()
);

// --- the heatmap -------------------------------------------------------------

const tiles = () => [...document.querySelectorAll("#heatGrid .heatTile")];
check(
  "the map is the top sixty, sized by volume and coloured by change",
  tiles().length === 60 &&
    tiles()[0].dataset.size === "4" &&
    tiles()[59].dataset.size === "1" &&
    document.querySelector("#heatGrid .heatTile[data-bucket='3']") !== null &&
    document.querySelector("#heatGrid .heatTile[data-bucket='-3']") !== null &&
    document.querySelector("#heatGrid .heatTile[data-bucket='0']") !== null,
  `${tiles().length} tiles · ${tiles()[0].dataset.size}/${tiles()[59].dataset.size}`
);
check(
  "a tile names itself for a screen reader",
  tiles()[0].getAttribute("aria-label") === "BTC, up 2.5 percent",
  tiles()[0].getAttribute("aria-label")
);
document.getElementById("heatTableToggle").click();
await settle();
check(
  "Show as table swaps in the same sixty rows and remembers the choice",
  document.getElementById("heatGrid").hidden === true &&
    document.getElementById("heatTableWrap").hidden === false &&
    document.querySelectorAll("#heatRows tr").length === 60 &&
    window.localStorage.getItem("hrz.heat.table") === "true",
  `${document.querySelectorAll("#heatRows tr").length} rows`
);
document.getElementById("heatTableToggle").click();
await settle();
check(
  "and back to the map",
  document.getElementById("heatGrid").hidden === false &&
    document.getElementById("heatTableWrap").hidden === true &&
    window.localStorage.getItem("hrz.heat.table") === "false"
);

// --- the clock ---------------------------------------------------------------
//
// The dashboard's tick is live here, and the Charts rail's is not: one clock,
// not two. Leaving hands the wall its own clock back.

check(
  "the dashboard's clock is the only one running",
  clocksNamed("dashTick") === 1 && clocksNamed("fetchWatchlistTickers") === 0,
  `dash=${clocksNamed("dashTick")} rail=${clocksNamed("fetchWatchlistTickers")}`
);
document.querySelector("#heatGrid .heatTile[data-symbol='SOLUSDT']").click();
await waitFor(
  () => document.getElementById("dest-charts").hidden === false,
  "the heatmap tile to open Charts"
);
await settle();
check(
  "a heat tile opens that symbol in Charts",
  selectIn(0, "symbol") === "SOLUSDT",
  selectIn(0, "symbol")
);
check(
  "and leaving stops the dashboard's clock and starts the rail's",
  clocksNamed("dashTick") === 0 && clocksNamed("fetchWatchlistTickers") === 1,
  `dash=${clocksNamed("dashTick")} rail=${clocksNamed("fetchWatchlistTickers")}`
);
```

Two assumptions in the checks are worth stating, because they are about the
fixture and not the page: the fixture's BTC is up 2.5% and its DOGE is the
cheapest of the seventy, which is what makes "biggest first" and "flipped" two
different symbols. A fixture whose extremes are the same row would pass either
way.

- [ ] **Step 9: Run it**

```bash
node --check frontend/app/app.js
node --check frontend/app/views/shell.js
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
cargo test -p api-gateway --test packaging
```

Expected: every check passes. If "the dashboard's clock is the only one
running" fails with `rail=1`, `runPaneLoaders()` is being called with a name on
the dashboard branch; if it fails with `dash=0`, `setDestinationVisible` is not
reached from `showDestination`, or the shell called it before `Shell.start()`
finished mounting.

Open the gateway at 1440×900 signed in:

1. Dashboard: the four cards show BTC's price and line, "No favorites yet",
   the bot counts, and the plan with a half-filled credits meter.
2. The table lists seventy pairs, fifty at a time, volume first; sort by
   Symbol, flip it, and filter to "sol". Open a row: Charts, on that pair.
3. In the watchlist panel, "+ Add", type `sol`, press the result, press
   "Done": SOL is pinned with a price, and the Charts rail lists it too.
   Star it out again; both lists drop it.
4. Heatmap: SOL is the brightest green, DOGE the deepest red, the flat pairs
   sit on the panel colour, and BTC's tile is the tallest. Click a tile:
   Charts, on that pair.
5. "Show as table" swaps the map for sixty rows; reload the page: it is still
   a table, and the switch says "Show as map".
6. Leave Dashboard for Studio, wait ten seconds, come back: the table repaints
   from cache instantly and then ticks.
7. Light theme: the seven buckets invert into a legible ramp on white; no tile
   text is lost against its background.
8. With VoiceOver on Dashboard: four cards announce their names, the table
   announces the sorted column, and each tile announces "SOL, up 8.1 percent".

- [ ] **Step 10: Commit**

```bash
git add frontend/app/app.js frontend/app/views/shell.js tools/shell_check.mjs
git commit -m "feat: a dashboard that reads one batch on one clock"
```

---

### Task 17: A frame that gives way from the rail to the bottom nav

Task 3 moved the old `@media (max-width: 1100px)` and `(max-width: 900px)`
blocks into `responsive.css` **unchanged**, and every task since has leaned on
them. They were written for a page that had one chart, one panel and nothing
else; the frame now has a rail, a status bar, a watchlist, an inspector, a dock
and a dashboard, and those two rules cannot describe it. This task replaces
them with three breakpoints, each answering one question:

| Width | What changes | Why |
| --- | --- | --- |
| ≤1279 | `--aside-w` 320, `--watch-w` 240 | The inspector and the watchlist stop both fitting beside a chart that is still worth reading. The watchlist also defaults to collapsed — Shell already reads `(min-width: 1280px)` at init. |
| ≤1023 | Panels leave the row, controls grow to 44px | 768–1023 is a tablet: the chart keeps the whole width and the watchlist, inspector and dock become overlays. A finger needs 44px, so the instrument bar and its controls grow with it. |
| ≤767 | Rail and status bar give way to the bottom nav; top bar 48; wall stacks | A phone has no room for a rail and a status bar *and* a chart. The bottom nav is the destination list; the instrument bar states the chart. |

The dashboard's own grid breakpoints stay in `dashboard.css` (Task 15): a page's
grid is its own business, and these rules never write one. The only page-level
property here is the padding, and it goes through one token so the frame edge
and the page edge cannot disagree.

**Files:**
- Modify: `frontend/app/index.html` (a search icon in `#paletteBtn`, the
  `#phoneNav` markup)
- Modify: `frontend/app/styles/tokens.css` (`--page-pad`)
- Modify: `frontend/app/styles/shell.css` (destination padding reads the token)
- Modify: `frontend/app/styles/dashboard.css` (the page reads the token)
- Rewrite: `frontend/app/styles/responsive.css`
- Modify: `frontend/app/views/shell.js` (both navigations, `syncChrome`)
- Modify: `tools/shell_check.mjs` (the narrow-width section, phone-nav checks)

**Interfaces:**
- Consumes: Task 8's frame and `#topbar`/`#rail`/`#statusbar`; Task 9's `Shell`
  and its `matchMedia` defaults; Task 11's `#instrumentBar` and `--bar-h`;
  Task 13's `#dock`; Task 15's dashboard breakpoints.
- Produces: `#phoneNav` (six `[data-dest]` buttons, hidden until signed in),
  `--page-pad`, and the sole owners of the 1279/1023/767 frame rules.
- Produces: `destButtons()` in `views/shell.js`; every rail selector now names
  both navigations, so `aria-current` and `disabled` cannot disagree.

- [ ] **Step 1: Add the second navigation**

In `frontend/app/index.html`, the palette button becomes a search icon and a
label; the label and the key hint can hide on a phone, the icon cannot:

```html
  <button id="paletteBtn" class="paletteBtn" type="button" aria-haspopup="dialog" disabled>
    <span data-icon="search" data-icon-size="14"></span>
    <span class="u-grow">Search or jump to…</span>
    <kbd class="paletteKey">⌘K</kbd>
  </button>
```

Then insert the bottom nav between the frame's closing `</div>` (the one that
closes `#frame`, immediately after `</main>`) and `<footer id="statusbar"
hidden>`:

```html
<nav id="phoneNav" class="phoneNav" aria-label="Destinations" hidden>
  <button type="button" class="phoneNav__btn" data-dest="dashboard" aria-label="Dashboard" disabled>
    <span data-icon="activity" data-icon-size="20"></span>
    <span class="phoneNav__label">Dashboard</span>
  </button>
  <button type="button" class="phoneNav__btn" data-dest="charts" aria-label="Charts" disabled>
    <span data-icon="chart" data-icon-size="20"></span>
    <span class="phoneNav__label">Charts</span>
  </button>
  <button type="button" class="phoneNav__btn" data-dest="studio" aria-label="Studio" disabled>
    <span data-icon="sparkles" data-icon-size="20"></span>
    <span class="phoneNav__label">Studio</span>
  </button>
  <button type="button" class="phoneNav__btn" data-dest="community" aria-label="Community" disabled>
    <span data-icon="globe" data-icon-size="20"></span>
    <span class="phoneNav__label">Community</span>
  </button>
  <button type="button" class="phoneNav__btn" data-dest="bots" aria-label="Bots" disabled>
    <span data-icon="zap" data-icon-size="20"></span>
    <span class="phoneNav__label">Bots</span>
  </button>
  <button type="button" class="phoneNav__btn" data-dest="account" aria-label="Account" disabled>
    <span data-icon="user" data-icon-size="20"></span>
    <span class="phoneNav__label">Account</span>
  </button>
</nav>
```

It ships `hidden` and `disabled` like the rail: a signed-out phone must not see
a destination list into a frame that is not there. `Shell.syncChrome` owns both
attributes.

- [ ] **Step 2: One padding token for the frame and every page**

In `tokens.css`, after `--space-8: 40px;`, add:

```css
  --page-pad: var(--space-6);
```

Then three edits read it instead of a literal:

```css
/* shell.css -- Tasks 8's destination padding */
.destHeading { margin: 0; padding: var(--page-pad) var(--page-pad) 0; font-size: var(--text-lg); }
.destHost { flex: 1 1 auto; min-width: 0; min-height: 0; padding: 0 var(--page-pad) var(--page-pad); }
```

```css
/* dashboard.css -- Task 15's page padding */
.dashInner {
  width: 100%; max-width: 1440px; margin: 0 auto;
  padding: var(--page-pad);
  display: flex; flex-direction: column; gap: var(--space-4);
}
```

The token is what makes 24/16/12 (Spec §9) one declaration per breakpoint
rather than a hunt through three files.

- [ ] **Step 3: Replace `responsive.css`**

The file currently holds the two old blocks, moved unchanged in Task 3. Replace
its whole content with:

```css
/* ===========================================================================
   The frame from the desk down to the phone.

   Loaded last, because every rule here is an override: at a given width it
   must beat the base layout rather than tie with it. The Dashboard's own
   breakpoints stay in dashboard.css -- a page's grid is its own business --
   and these rules never write a dashboard property except the page padding,
   which is a frame concern.

     1279  the inspector and the watchlist stop both fitting beside a chart
     1023  those panels become overlays over the chart, and every control a
           finger lands on grows to 44px
      767  the rail and status bar give way to the bottom nav, the top bar
           grows to 48, and the wall stacks one chart per screen

   Nothing here hides a control that has no other route: the watchlist,
   inspector and dock all keep the instrument bar's toggles. The pane chip bar
   and touch gestures are Spec 3's; below 768 the wall is a vertical column of
   full-height charts until then.
   ========================================================================= */

/* Not part of the desktop frame; the 767 block turns it on. */
.phoneNav { display: none; }

/* ---- 1279: narrower columns ---------------------------------------------- */
@media (max-width: 1279px) {
  /* Through the custom properties, not a hard width: the splitters write
     these inline on :root, and a hard width in a media query would outrank a
     dragged one and silently undo the drag below 1280. */
  :root { --aside-w: 320px; --watch-w: 240px; }
}

/* ---- 1023: the panels leave the row -------------------------------------- */
@media (max-width: 1023px) {
  :root { --page-pad: var(--space-4); --bar-h: 52px; }

  /* A finger is not a mouse. The bar grows to hold 44px controls rather than
     clipping them, and the tabs grow with it. */
  .instrumentBar .hrz-btn,
  .instrumentBar .barSymbol,
  .instrumentBar select.hrz-select,
  .instrumentBar .hrz-seg { height: 44px; min-height: 44px; }
  .instrumentBar .hrz-seg__item { height: 44px; min-height: 44px; padding: 0 var(--space-2); }
  .instrumentBar .barChip { height: 34px; }
  #topbar .hrz-btn { min-height: 44px; }
  #topbar .hrz-btn--icon { min-width: 44px; }
  .hrz-tabs { height: auto; min-height: 44px; }
  .hrz-tab { min-height: 44px; }
  .hrz-input, .hrz-select { min-height: 44px; }
  .hrz-panel__head { height: auto; min-height: 44px; }
  .dockTabs { height: 48px; flex-basis: 48px; }
  .dockTabs .hrz-tabs, .dockTabs .hrz-tab { height: 44px; }
  /* The compact table is a mouse table; at a tablet its rows are targets. */
  .hrz-table--compact td, .hrz-table--compact th { height: 44px; }

  /* The watchlist leaves the row and becomes a drawer over the chart. It is
     still the same element, so the ticker, the favourites and every row
     handler are unchanged -- only where the browser puts the box. */
  #sideWatch {
    position: fixed; z-index: 45;
    top: var(--topbar-h); bottom: var(--status-h); left: var(--rail-w);
    width: min(320px, 80vw);
    border-right: 1px solid var(--line-strong);
    background: var(--bg-panel); box-shadow: var(--shadow-2);
  }
  #splitWatch, #railShow, #asideShow { display: none; }

  /* The inspector is the same idea on the other edge. #inspectorHost is the
     element the shell hides, so `hidden` still wins over this block. */
  #inspectorHost {
    position: fixed; z-index: 45;
    top: var(--topbar-h); right: 0; bottom: var(--status-h);
    width: min(var(--inspector-w), 86vw); flex-basis: auto;
    border-left: 1px solid var(--line-strong);
    background: var(--bg-panel); box-shadow: var(--shadow-2);
  }
  #inspectorHost[hidden] { display: none; }

  /* The dock is a sheet over the foot of the chart, not a third column that
     leaves the candles nothing. */
  #dock {
    position: fixed; z-index: 44;
    left: var(--rail-w); right: 0; bottom: var(--status-h);
    height: min(50vh, 360px);
    border-left: 1px solid var(--line-strong);
    box-shadow: var(--shadow-2);
  }
  #dock[hidden] { display: none; }
}

/* ---- 767: the phone frame ------------------------------------------------ */
@media (max-width: 767px) {
  :root {
    --page-pad: var(--space-3);
    --topbar-h: 48px;
    --phone-nav-h: 56px;
  }

  /* No rail, no status: the bottom nav is the destination list and the
     instrument bar states the chart. */
  #rail { display: none; }
  #statusbar { display: none; }

  #topbar { gap: var(--space-1); padding: 0 var(--space-2); }
  .brandWord, #session, #connDotTop { display: none; }
  #paletteBtn {
    width: 44px; height: 44px; min-width: 44px; padding: 0; margin: 0;
    justify-content: center;
  }
  #paletteBtn .u-grow, #paletteBtn .paletteKey { display: none; }
  #paletteBtn [data-icon] { display: inline-flex; }

  .phoneNav {
    display: flex; flex: 0 0 auto;
    height: calc(var(--phone-nav-h) + env(safe-area-inset-bottom));
    padding-bottom: env(safe-area-inset-bottom);
    background: var(--bg-panel); border-top: 1px solid var(--line);
  }
  .phoneNav[hidden] { display: none; }
  .phoneNav__btn {
    flex: 1 1 0; min-width: 0; min-height: var(--phone-nav-h);
    display: flex; flex-direction: column; align-items: center; justify-content: center;
    gap: 2px; padding: 0;
    border: 0; background: transparent; color: var(--text-3);
    font: var(--text-2xs)/1 var(--font-ui);
  }
  .phoneNav__btn[disabled] { opacity: 0.45; }
  .phoneNav__btn[aria-current="page"] { color: var(--accent); }
  .phoneNav__label {
    max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }

  /* The panels are sheets above the nav now. 1023 positioned them for a
     tablet; only the edges and the bottom change. */
  #sideWatch, #inspectorHost, #dock {
    top: auto; left: 0; right: 0;
    bottom: calc(var(--phone-nav-h) + env(safe-area-inset-bottom));
    width: auto;
    border-left: 0; border-right: 0; border-top: 1px solid var(--line-strong);
    border-radius: var(--radius-3) var(--radius-3) 0 0;
  }
  #sideWatch { height: min(60vh, 460px); max-height: 60vh; }
  #inspectorHost { height: min(70vh, 600px); max-height: 70vh; }
  #dock { height: min(50vh, 360px); }

  /* The wall is a column of screens, not two 170px charts. It scrolls; each
     pane keeps a height a candle can be read at. */
  #dest-charts { flex-direction: column; overflow: hidden; }
  #charts { overflow: auto; }
  .chartRow { display: block; flex: 0 0 auto; }
  .chartRow .chartPane {
    height: calc(100vh - var(--topbar-h) - var(--phone-nav-h) - var(--bar-h));
    height: calc(100dvh - var(--topbar-h) - var(--phone-nav-h) - var(--bar-h));
    min-height: 300px;
    border-left: 0; border-top: 1px solid var(--line);
  }
  #charts .chartRow:first-child .chartPane:first-child { border-top: 0; }
  .chartWrap { flex: 1 1 auto; height: auto; min-height: 0; }
  #charts.gridLayout {
    display: grid; grid-template-columns: 1fr;
    grid-auto-rows: minmax(300px, 70dvh);
  }
  #charts.gridLayout > .chartPane { height: auto; min-height: 300px; }

  /* Six controls do not fit; the toolbar is an overlay, so anything past the
     edge is unreachable rather than merely clipped. It scrolls, with the
     instrument pinned left and the panel toggles pinned right. */
  .tools { flex-wrap: wrap; max-width: calc(100% - var(--page-pad)); }
  .chartHint { display: none; }
  .instrumentBar {
    overflow-x: auto; overflow-y: hidden;
    scrollbar-width: none;
    gap: var(--space-1);
  }
  .instrumentBar::-webkit-scrollbar { display: none; }
  .instrumentBar > .barGroup { flex: 0 0 auto; }
  .instrumentBar .barSpacer { display: none; }
  .instrumentBar [data-bar-group="instrument"] {
    position: sticky; left: 0; z-index: 2;
    padding-right: var(--space-2); background: var(--bg-panel);
  }
  .instrumentBar [data-bar-group="chrome"] {
    position: sticky; right: 0; z-index: 2;
    padding-left: var(--space-2); background: var(--bg-panel);
  }
  .barPop { max-width: calc(100vw - 2 * var(--space-2)); }

  /* A dialog is a sheet on a phone. The palette and the shortcut sheet are
     the two that open from the top bar. */
  .hrz-dialog {
    position: fixed; inset: auto 0 0 0;
    width: 100%; max-width: 100%; max-height: 85vh;
    margin: 0;
    border-radius: var(--radius-3) var(--radius-3) 0 0;
  }

  /* Home is rebuilt in Spec 4; until then it keeps the phone shape it had. */
  #signin { flex-wrap: wrap; }
  #signin input { flex: 1 1 140px; min-width: 0; }
  .homeInner { padding: var(--space-4) var(--space-3); }
  .marketGrid { grid-template-columns: 1fr; }
  .homeHero h1 { font-size: 26px; }
}

/* 320px is the narrowest width the acceptance matrix names. Six labels at
   11px do not fit under four icons, so the icons carry the nav with their
   accessible names; the labels come back at 360. */
@media (max-width: 359px) {
  .phoneNav__label { display: none; }
}
```

Two notes on what is deliberately absent. There is no `html, body { height:
auto }` any more: the phone frame is a fixed-height column -- top bar, frame,
bottom nav -- so `#dest-dashboard` and `#charts` scroll inside it rather than
the page growing. And there is no scrim behind the overlays: each panel keeps
its own close control (`#railHide`, `#asideHide`, `#dockCollapse`) and the
instrument bar's toggles close it too, which is the same route a keyboard uses.
Spec 3 adds the scrim, the pane chip bar and the touch gestures.

- [ ] **Step 4: Teach the shell that there are two navigations**

In `frontend/app/views/shell.js`, next to `const el = (id) =>
document.getElementById(id);`, add:

```js
  /// The rail and the phone's bottom nav are the same navigation. Every
  /// selector that used to name the rail names both, so a destination cannot
  /// be current in one place and not the other.
  const destButtons = () =>
    document.querySelectorAll("#rail [data-dest], #phoneNav [data-dest]");
```

Then replace the rail-only loop in each of the three functions:

```js
// wire()
for (const button of destButtons()) {
  button.addEventListener("click", () => go(button.dataset.dest));
}
```

```js
// showDestination()
for (const button of destButtons()) {
  if (button.dataset.dest === state.destination) button.setAttribute("aria-current", "page");
  else button.removeAttribute("aria-current");
}
```

```js
// syncChrome()
function syncChrome() {
  const signedIn = isSignedIn();
  for (const button of destButtons()) button.disabled = !signedIn;
  // The bottom nav is outside #frame, so the frame's own hide does not reach
  // it: a signed-out phone must not list destinations into a frame that is
  // not there. CSS shows it only below 768; this attribute is the other half.
  const phone = el("phoneNav");
  if (phone) phone.hidden = !signedIn;
  for (const id of ["aiBtn", "watchlistToggle", "inspectorToggle", "dockToggle"]) {
    const button = el(id);
    if (button) button.disabled = !signedIn;
  }
  refreshStatus();
  paintProfile();
}
```

No new state and no resize listener: the CSS decides which navigation is on
screen; the shell only keeps both in the same state.

- [ ] **Step 5: Replace the narrow-width checks**

In `tools/shell_check.mjs`, replace the whole section from
`// --- the layout at a narrow width` through the last check
("and the stacking breakpoint comes second, so it wins at the widths that
match both"). The new section reads `responsive.css` directly, because the
rules no longer live in one inline block, and it drives the phone's navigation
because that half is a real second router.

```js
// --- the frame at every width -------------------------------------------------
//
// jsdom lays nothing out, so the breakpoint checks read responsive.css the way
// the old 1100/900 checks read the inline block. They are not "a media query
// exists": each one asserts the declaration that makes the width work. The DOM
// half is the phone's navigation, which is a second route to the same
// destinations and must not be a second router.

console.log("\nthe frame at every width");

const responsiveText = await readFile(
  join(root, "frontend/app/styles/responsive.css"),
  "utf8"
);
/// The declarations of one rule inside a block, or "" if it is not there.
///
/// The selector is escaped before it goes into a pattern, because selectors
/// are full of `.` and `#` and an unescaped `.` matches any character.
const ruleIn = (block, selector) => {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const found = new RegExp(`(^|[\\s,])${escaped}\\s*\\{([^}]*)\\}`).exec(block);
  return found ? found[2] : "";
};
const responsiveBlock = (width) => {
  const at = responsiveText.indexOf(`@media (max-width: ${width}px)`);
  if (at < 0) return "";
  const next = responsiveText.indexOf("@media", at + 1);
  return responsiveText.slice(at, next < 0 ? responsiveText.length : next);
};

check(
  "the old 1100 and 900 breakpoints are gone",
  !styleText.includes("@media (max-width: 1100px)") &&
    !styleText.includes("@media (max-width: 900px)"),
  `1100=${styleText.indexOf("@media (max-width: 1100px)")} 900=${styleText.indexOf("@media (max-width: 900px)")}`
);
check(
  "three breakpoints, widest first",
  responsiveText.indexOf("@media (max-width: 1279px)") >= 0 &&
    responsiveText.indexOf("@media (max-width: 1279px)") <
      responsiveText.indexOf("@media (max-width: 1023px)") &&
    responsiveText.indexOf("@media (max-width: 1023px)") <
      responsiveText.indexOf("@media (max-width: 767px)"),
  `${responsiveText.indexOf("@media (max-width: 1279px)")} < ${responsiveText.indexOf("@media (max-width: 1023px)")} < ${responsiveText.indexOf("@media (max-width: 767px)")}`
);
check(
  "at 1279 the inspector narrows through the property the splitter writes",
  /--aside-w:\s*320px/.test(ruleIn(responsiveBlock(1279), ":root")),
  ruleIn(responsiveBlock(1279), ":root").trim() || "(no :root rule)"
);
check(
  "at 1023 the panels leave the row and the controls grow to a finger",
  /position:\s*fixed/.test(ruleIn(responsiveBlock(1023), "#sideWatch")) &&
    /position:\s*fixed/.test(ruleIn(responsiveBlock(1023), "#inspectorHost")) &&
    /position:\s*fixed/.test(ruleIn(responsiveBlock(1023), "#dock")) &&
    /min-height:\s*44px/.test(ruleIn(responsiveBlock(1023), ".hrz-tab")) &&
    /--bar-h:\s*52px/.test(ruleIn(responsiveBlock(1023), ":root")),
  ruleIn(responsiveBlock(1023), "#sideWatch").trim().slice(0, 60) || "(no drawer)"
);
check(
  "at 767 the rail and status bar give way to the bottom nav",
  /display:\s*none/.test(ruleIn(responsiveBlock(767), "#rail")) &&
    /display:\s*none/.test(ruleIn(responsiveBlock(767), "#statusbar")) &&
    /display:\s*flex/.test(ruleIn(responsiveBlock(767), ".phoneNav")) &&
    /--topbar-h:\s*48px/.test(ruleIn(responsiveBlock(767), ":root")),
  ruleIn(responsiveBlock(767), ":root").trim() || "(no :root rule)"
);
check(
  "and the wall stacks one chart per screen",
  /display:\s*block/.test(ruleIn(responsiveBlock(767), ".chartRow")) &&
    /100dvh/.test(ruleIn(responsiveBlock(767), ".chartRow .chartPane")),
  ruleIn(responsiveBlock(767), ".chartRow .chartPane").trim().slice(0, 60) ||
    "(no .chartPane rule)"
);
check(
  "page padding follows one token, 24 then 16 then 12",
  /--page-pad:\s*var\(--space-6\)/.test(styleText) &&
    /--page-pad:\s*var\(--space-4\)/.test(responsiveBlock(1023)) &&
    /--page-pad:\s*var\(--space-3\)/.test(responsiveBlock(767)),
  `24=${/--page-pad:\s*var\(--space-6\)/.test(styleText)} 16=${/--page-pad:\s*var\(--space-4\)/.test(responsiveBlock(1023))} 12=${/--page-pad:\s*var\(--space-3\)/.test(responsiveBlock(767))}`
);

check(
  "the bottom nav is the rail on a phone",
  document.querySelectorAll("#phoneNav [data-dest]").length === 6 &&
    document.getElementById("phoneNav").hidden === false,
  `${document.querySelectorAll("#phoneNav [data-dest]").length} destinations · hidden=${document.getElementById("phoneNav").hidden}`
);
document.querySelector("#phoneNav [data-dest='dashboard']").click();
await settle();
check(
  "and its buttons are the router, not a second one",
  document.getElementById("dest-dashboard").hidden === false &&
    document.querySelector("#phoneNav [data-dest='dashboard']").getAttribute("aria-current") === "page" &&
    document.querySelector("#rail [data-dest='dashboard']").getAttribute("aria-current") === "page",
  `phone=${document.querySelector("#phoneNav [data-dest='dashboard']").getAttribute("aria-current")} rail=${document.querySelector("#rail [data-dest='dashboard']").getAttribute("aria-current")}`
);
document.querySelector("#phoneNav [data-dest='charts']").click();
await settle();
```

The section ends on Charts, where the previous section ended, so Task 18's
checks open where they expect to.

- [ ] **Step 6: Run it**

```bash
node --check frontend/app/views/shell.js
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
cargo test -p api-gateway --test packaging
```

Expected: every check passes; packaging passes because `responsive.css` was
already linked in Task 3 and no new file was added.

Open the gateway signed in:

1. **1440×900**: the rail and status bar are exactly as before; the padding is
   24. Nothing in the chart area moved.
2. **1100×800**: the inspector is 320 and still open beside the chart; the
   watchlist is collapsed. Press its toggle: it slides over the chart from the
   left, at 320px, and the chart does not reflow. Press `#railHide` and it is
   gone. The dock opens as a sheet over the bottom of the chart, not as a third
   column.
3. **900×800**: the same drawer/sheet arrangement, with the status bar still
   24px. The instrument bar's buttons are 44px tall and comfortably tappable.
4. **390×844**: no rail and no status bar; the top bar is 48px; the bottom nav
   lists six destinations and marks the current one in accent. Dashboard: one
   pulse card per row, the markets table as cards. Charts: one full-height
   chart per screen, scrolling vertically; the instrument bar scrolls sideways
   with the symbol pinned left and the panel toggles pinned right. Watchlist,
   inspector and dock open as sheets above the nav.
5. **320×568**: the nav labels give way to icons (their accessible names stay);
   nothing scrolls the page sideways.
6. **Keyboard**: Tab reaches the bottom nav after the chart; Enter changes
   destination; `aria-current` moves with it. The palette dialog is a sheet and
   Esc still closes it.
7. **Light theme**: the nav's current item, the sheets' edges and the drawer
   shadows are legible on white.

- [ ] **Step 7: Commit**

```bash
git add frontend/app/index.html frontend/app/styles/tokens.css \
  frontend/app/styles/shell.css frontend/app/styles/dashboard.css \
  frontend/app/styles/responsive.css frontend/app/views/shell.js \
  tools/shell_check.mjs
git commit -m "feat: a frame that gives way from the rail to the bottom nav"
```

---

### Task 18: The bar the spec sets — accessibility, idle cost, and a finish line

Tasks 8–17 built a frame that looks right and routes right. This task holds it
to Spec §10 and stops it paying for a tab nobody is looking at, then writes the
Definition of Done the rest of the program inherits.

Three things are deliberately *not* here, and each is said out loud below
rather than left implied:

- **No axe-core run in this repository.** `axe-core` is not a dependency and
  this plan does not put one in the deployment image. The audit is Lighthouse's
  Accessibility panel (which runs axe-core) by hand; the harness holds the
  parts of §10 that are a property of the DOM rather than of a renderer.
- **No panned viewport in the chart's accessible name.** The visible range is
  the engine's; JavaScript never holds it. The canvas names the loaded window
  and its bar count instead of a range it would have to guess.
- **No scrim, chip bar or touch gestures.** They are Spec 3's, as Task 17 said.

**Files:**
- Modify: `frontend/app/index.html` (skip link, live region, landmark name,
  the dock tablist's id, two `role="status"` labels, `#mainView` focusable)
- Modify: `frontend/app/ui/components.js` (`UI.tabKeyboard`; the sparkline token
  bug)
- Modify: `frontend/app/views/shell.js` (`syncTabStrip`, the two announcements)
- Modify: `frontend/app/app.js` (`paintChartSummary`, `#marketTabs` keyboard,
  the two idle guards)
- Modify: `tools/shell_check.mjs` (style recording, the accessibility contract
  and idle-cost checks)

**Interfaces:**
- Consumes: Task 2's `.skipLink` and reduced-motion rules; Task 8's frame and
  status-bar spans; Task 9's `Shell.refreshStatus` and `setSaveState`; Task 11's
  `notifyPane`; Task 13's dock; Task 15's `#marketTabs`; Task 16's
  `setDestinationVisible`.
- Produces: `HRZUI.tabKeyboard(root)`, `paintChartSummary(pane)`,
  `announceSeries(info)`, `syncTabStrip(root, attribute, selected)`,
  `announcedSeries` state, and `#chartAnnounce`.

- [ ] **Step 1: Give the page a way in, a name for each landmark, and a voice**

In `frontend/app/index.html`, make the skip link and the live region the first
two things in `<body>` — immediately after the `<body>` tag, above everything
Task 8 left there, so `document.body.firstElementChild` is the link:

```html
<body>

<a class="skipLink" href="#mainView">Skip to content</a>
<p id="chartAnnounce" class="sr-only" role="status" aria-live="polite"></p>
```

`.skipLink` already exists in `base.css` (Task 2) — it is a real link that is
visually out of the way until it takes focus, which is the whole point of it.

Then five attributes, each one a landmark or a live region the spec names:

```html
<!-- a destination is focusable, so the skip link has somewhere to land -->
  <main id="mainView" hidden tabindex="-1">
```

```html
<!-- the aside has been a landmark since Task 8, but an unnamed one -->
  <aside id="inspector" aria-label="Inspector">
```

```html
<!-- the dock's strip needs a name to be addressable and a single Tab stop -->
    <div class="hrz-tabs" id="dockTablist" role="tablist" aria-label="Workbench">
```

```html
<!-- connection and save state change rarely and matter; both announce once -->
    <span id="sbConnLabel" role="status">Offline</span>
```

```html
  <span id="sbSave" class="sbSave" role="status" title="Workspace save state">Saved</span>
```

`#chartAnnounce` is the region §10 asks for: it speaks symbol and timeframe
changes and nothing else. It starts empty on purpose — boot is not a change the
user made.

The link lands in `#mainView`, which lives inside the frame the shell unhides on
sign-in. On the signed-out page the frame is still `hidden`, so the link is
inert there; Spec 4 rebuilds the public pages and points the same link at their
content.

- [ ] **Step 2: One Tab stop for the strips the shell owns, and a sparkline that paints**

In `frontend/app/ui/components.js`, insert after `UI.tabs`'s closing `};`
(before `/* ---- Table`) :

```js
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
```

Then fix the sparkline. In `UI.sparkline`, replace the line that reads

```js
    const stroke = trend === "down" ? "var(--down)" : "var(--up)";
```

with:

```js
    const token = trend === "down" ? "--down" : "--up";
    // A canvas paints an unresolved `var(--up)` as nothing at all -- which is
    // how this shipped broken since Task 4. Resolve the token against this
    // canvas's own tree, so a light theme gets its green and a detached canvas
    // still gets the literal.
    const resolved = window.getComputedStyle(canvas).getPropertyValue(token).trim();
    const stroke = resolved || (trend === "down" ? "#ea3943" : "#16c784");
```

`stroke` is used for both `strokeStyle` and `fillStyle` already, so nothing else
in the function changes.

- [ ] **Step 3: The shell tells the strips what is selected, and the region what is on screen**

In `frontend/app/views/shell.js`, add the strip helper immediately before
`syncInspector`:

```js
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
```

Then replace each strip's loop with a call:

```js
// syncInspector()
syncTabStrip(el("inspectorTabs"), "inspector", state.inspectorTab);
```

```js
// syncStudio()
syncTabStrip(el("studioTabs"), "studio", state.studioPane);
```

```js
// syncAccount()
syncTabStrip(el("accountTabs"), "account", state.accountSection);
```

```js
// syncDock() -- the strip gained an id in Step 1
syncTabStrip(el("dockTablist"), "dock", state.dockTab);
```

Next, the voice. Add above `refreshStatus`:

```js
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
```

Call it at the end of `refreshStatus`, and stop the connection label from
re-writing the same word:

```js
    const label = el("sbConnLabel");
    // `role="status"`: write only on a change, so a retry every five seconds is
    // not read out every five seconds.
    if (label && label.textContent !== claim[0]) label.textContent = claim[0];
```

```js
    if (dot) dot.dataset.state = claim[1];
    const top = el("connDotTop");
    if (top) top.dataset.state = claim[1];
    announceSeries(info);
  }
```

The same rule applies to the save state, which fires on every debounced
workspace change. In `setSaveState`:

```js
  function setSaveState(name, text) {
    const node = el("sbSave");
    if (!node) return;
    node.dataset.state = name || "";
    const label = text || "Saved";
    if (node.textContent !== label) node.textContent = label;
  }
```

- [ ] **Step 4: A canvas that names itself, one strip in the dashboard, and a tab that stops paying**

In `frontend/app/app.js`, four edits.

**4a. Expose the newest price.** `createChartPane` keeps `livePrice` (a closure
over `lastPrice`) but does not publish it. Add it to `paneApi` beside the two
selects it sits with:

```js
    timeframe: () => el("timeframe").value,

    /// The newest price the pane has seen, or null before the first tick. A
    /// function for the same reason `symbol` is: the accessible name is built
    /// when the summary is painted, not when the pane was.
    livePrice,
```

**4b. The chart summary.** Insert after `notifyPane` (Task 11):

```js
/// The canvas's accessible name.
///
/// `role="img"` needs a name, and the name must survive being read aloud: the
/// newest price is included because it is the number a reader came for, and it
/// is rewritten only when the series, the mode or the attachment changes --
/// the events that already repaint the bar. A pan is view-only and does not
/// rewrite it. The engine keeps its viewport to itself, so this names the
/// loaded window and its bar count rather than a range JavaScript would have
/// to invent.
function paintChartSummary(pane) {
  if (!pane || !pane.canvas) return;
  const parts = [pane.symbol() || "No symbol"];
  const timeframe = pane.timeframe();
  if (timeframe) parts.push(`${timeframe} candles`);
  const mode = pane.mode ? pane.mode() : "";
  if (mode && mode !== "candles") parts.push(mode);
  const bars = pane.bars ? pane.bars() : 0;
  if (bars) parts.push(`${bars} bars loaded`);
  const price = pane.livePrice ? pane.livePrice() : null;
  if (price != null) parts.push(`last ${HRZUI.format.price(price)}`);
  pane.canvas.setAttribute("role", "img");
  pane.canvas.setAttribute("aria-label", parts.join(", "));
}
```

Task 11's `notifyPane()` takes no argument, and every command that already calls
it keeps doing so -- with no pane it names the active one. Give it the pane that
changed where the caller knows it, so a pane the user is not on is labelled too:

```js
function notifyPane(pane) {
  paintChartSummary(pane || activePane);
  document.dispatchEvent(new CustomEvent("hrz:pane", { detail: { pane: activePane } }));
}
```

Three call sites pass one: `applyState` and `fillSeries` (both inside
`createChartPane`) end with `notifyPane(this);`, and the page-level
`syncIndicatorChip` ends with `notifyPane(activePane);`. The rest -- Task 11's
seven series commands -- stay `notifyPane();`.

**4c. The dashboard's strip.** In `wireDashboard`, replace the `#marketTabs`
block with one that takes the keyboard handle and refreshes it where the
selection moves:

```js
  const tabs = el("marketTabs");
  if (tabs) {
    // `HRZUI.tabs` cannot own this strip: its panels are the table's rows, and
    // its selection is a sort key. The keyboard half is still shared.
    const keyboard = HRZUI.tabKeyboard(tabs);
    tabs.addEventListener("click", (event) => {
      const tab = event.target.closest("[data-market]");
      if (!tab) return;
      dashTab = tab.dataset.market;
      dashShown = DASH_PAGE;
      for (const button of tabs.querySelectorAll("[data-market]")) {
        button.setAttribute("aria-selected", String(button === tab));
      }
      keyboard.refresh();
      paintDashMarkets();
    });
  }
```

**4d. Two clocks that stop when nobody is looking.** At the top of `dashTick`,
and at the top of `fetchWatchlistTickers`:

```js
  // A backgrounded tab is not reading a dashboard. The interval keeps its
  // place; the first visible tick repaints from cache and then from the venue.
  if (document.hidden) return;
```

A phone that is put in a pocket for a minute stops making twelve `/tickers`
requests and resumes within five seconds of coming back.

- [ ] **Step 5: Record what the canvas was told, and check the contract**

In `tools/shell_check.mjs`, first make the 2D stub record assignments as well
as calls:

```js
const painted = { ops: [], text: [], styles: [] };
const context = new Proxy(
  {},
  {
    get(_target, prop) {
      if (prop === "measureText") return () => ({ width: 10 });
      return (...args) => {
        painted.ops.push(String(prop));
        if (prop === "fillText") painted.text.push(String(args[0]));
        return undefined;
      };
    },
    // Assignments matter too: a canvas paints `var(--up)` as nothing, so
    // "nothing hands the context a token" is a claim about these.
    set(_target, prop, value) {
      painted.styles.push(`${String(prop)}=${String(value)}`);
      return true;
    },
  }
);
```

Then append after Task 16's last dashboard check (the one that ends "and
leaving stops the dashboard's clock and starts the rail's"). It runs there on
purpose: the checks want a Charts page with a handful of panes and an active
first pane, not the hundred-chart wall two sections later — and the block puts
back the symbol it changes, so the sections that follow start where they did.

```js
// --- the accessibility contract ----------------------------------------------
//
// The parts of Spec 10 that are a property of the DOM: the skip link, named
// landmarks, one heading per destination, one Tab stop per strip, a region that
// speaks the series and not the tick, and a canvas that names itself. Contrast,
// 200% zoom, reduced motion and a real screen reader are the manual pass in
// Step 6 -- a renderer is what they need, and jsdom is not one.

console.log("\nthe accessibility contract");

check(
  "a skip link is the first thing Tab reaches, and its target can take focus",
  document.body.firstElementChild.classList.contains("skipLink") &&
    document.body.firstElementChild.getAttribute("href") === "#mainView" &&
    document.getElementById("mainView").tabIndex === -1,
  document.body.firstElementChild.className
);
check(
  "the landmarks are named: main, destinations, inspector, status",
  document.getElementById("mainView").tagName === "MAIN" &&
    document.getElementById("rail").getAttribute("aria-label") === "Destinations" &&
    document.getElementById("inspector").getAttribute("aria-label") === "Inspector" &&
    document.getElementById("statusbar").tagName === "FOOTER"
);
// Two reads in a row say the same thing, so nothing may be written: a
// `role="status"` node announces every text change, and the rail re-reads the
// status every few seconds -- a write per read would read the connection aloud
// forever. The observer is the only way to see a write that changes nothing.
// The first read runs unobserved, so a stale label left by an earlier tick is
// written here and is not mistaken for a re-write below.
window.Shell.refreshStatus();
const a11yWrites = [];
const a11yWatch = new window.MutationObserver((records) => a11yWrites.push(...records));
for (const id of ["sbConnLabel", "sbSave"]) {
  a11yWatch.observe(document.getElementById(id), { childList: true, characterData: true, subtree: true });
}
window.Shell.refreshStatus();
window.Shell.refreshStatus();
await settle();
a11yWatch.disconnect();
check(
  "connection and save state announce, and are written only when they change",
  a11yWrites.length === 0 &&
    document.getElementById("sbConnLabel").getAttribute("role") === "status" &&
    document.getElementById("sbSave").getAttribute("role") === "status",
  `${a11yWrites.length} write(s)`
);
const a11yDestinations = [...document.querySelectorAll("#mainView .destination")];
check(
  "every destination has exactly one h1",
  a11yDestinations.length === 6 &&
    a11yDestinations.every((section) => section.querySelectorAll("h1").length === 1),
  a11yDestinations
    .map((section) => `${section.id}:${section.querySelectorAll("h1").length}`)
    .join(" ")
);
const a11yPositive = [...document.querySelectorAll("[tabindex]")].filter(
  (node) => Number(node.getAttribute("tabindex")) > 0
);
check(
  "nothing asks for a positive tabindex",
  a11yPositive.length === 0,
  a11yPositive.map((node) => node.id || node.className).join(" ")
);

const a11yStrip = document.getElementById("inspectorTabs");
const a11yStops = () => [...a11yStrip.querySelectorAll('[role="tab"]')].filter((tab) => tab.tabIndex === 0);
const a11yChosen = a11yStrip.querySelector('[aria-selected="true"]');
check(
  "a strip is one Tab stop, and it is the selected tab",
  a11yStops().length === 1 && a11yStops()[0] === a11yChosen,
  `${a11yStops().length} stop(s)`
);
const a11yTabsBefore = [...a11yStrip.querySelectorAll('[role="tab"]')];
const a11yAt = a11yTabsBefore.indexOf(a11yChosen);
a11yChosen.focus();
a11yChosen.dispatchEvent(new window.KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
await settle();
const a11yTabsAfter = [...a11yStrip.querySelectorAll('[role="tab"]')];
const a11yNext = a11yTabsAfter[(a11yAt + 1) % a11yTabsAfter.length];
check(
  "the right arrow moves selection, focus and the Tab stop together",
  a11yNext.getAttribute("aria-selected") === "true" &&
    document.activeElement === a11yNext &&
    a11yNext.tabIndex === 0 &&
    a11yChosen.tabIndex === -1,
  `${a11yNext.dataset.inspector} selected=${a11yNext.getAttribute("aria-selected")} focus=${document.activeElement === a11yNext}`
);
a11yChosen.click();
await settle();
check(
  "clicking the first tab puts all three back",
  a11yChosen.getAttribute("aria-selected") === "true" && a11yChosen.tabIndex === 0
);
check(
  "the dashboard's strip is one Tab stop too",
  [...document.querySelectorAll("#marketTabs [role='tab']")].filter((tab) => tab.tabIndex === 0).length === 1,
  `${[...document.querySelectorAll("#marketTabs [role='tab']")].filter((tab) => tab.tabIndex === 0).length} stop(s)`
);

// The canvas check comes first: it reads the series the page opened with, not
// the one the next check asks for.
const a11yCanvas = canvasOf(0);
check(
  "the chart canvas names its own series for a reader",
  a11yCanvas.getAttribute("role") === "img" &&
    (a11yCanvas.getAttribute("aria-label") || "").startsWith(selectIn(0, "symbol")),
  a11yCanvas.getAttribute("aria-label")
);

const a11yAnnounce = document.getElementById("chartAnnounce");
check(
  "the chart's live region is polite",
  a11yAnnounce.getAttribute("role") === "status" &&
    a11yAnnounce.getAttribute("aria-live") === "polite",
  `${a11yAnnounce.getAttribute("role")} ${a11yAnnounce.getAttribute("aria-live")}`
);
// Change the series through the pane's own control, the way a person does.
// The options come from that pane's select, so this works whatever series the
// heat tile left open, and `/candles` synthesises a window for any symbol. By
// this point in the file the page has been alive long enough that the region
// already holds a series, so the check is "a change speaks, a tick does not",
// not "it starts empty".
const a11ySymbol = paneNode(0).querySelector(".symbol");
const a11ySymbolWas = a11ySymbol.value;
a11ySymbol.value = [...a11ySymbol.options]
  .map((option) => option.value)
  .find((value) => value !== a11ySymbolWas);
a11ySymbol.dispatchEvent(new window.Event("change"));
await waitFor(
  () => a11yAnnounce.textContent.includes(a11ySymbol.value),
  "the live region to name the series the user asked for"
);
check(
  "and it names the series that was chosen, with no price",
  a11yAnnounce.textContent.startsWith("Chart on ") &&
    !/\d+\.\d{2}/.test(a11yAnnounce.textContent),
  a11yAnnounce.textContent
);
const a11ySpoken = a11yAnnounce.textContent;
await window.fetchWatchlistTickers();
await settle();
check(
  "a price tick never reaches the live region",
  a11yAnnounce.textContent === a11ySpoken,
  a11yAnnounce.textContent
);
// Put the pane back on the series it started on: every section after this one
// assumes the page is as Task 16 left it.
a11ySymbol.value = a11ySymbolWas;
a11ySymbol.dispatchEvent(new window.Event("change"));
await settle();

const a11ySpark = document.createElement("canvas");
document.body.appendChild(a11ySpark);
const a11yStylesBefore = painted.styles.length;
window.HRZUI.sparkline(a11ySpark, [1, 2, 1.5, 3], { trend: "down", label: "XRP" });
const a11yStyles = painted.styles.slice(a11yStylesBefore);
check(
  "a sparkline hands the canvas a colour, not a custom property",
  a11yStyles.some((entry) => entry.startsWith("strokeStyle=")) &&
    !a11yStyles.some((entry) => entry.includes("var(")),
  a11yStyles.filter((entry) => entry.includes("Style=")).join(" ")
);
a11ySpark.remove();

// --- idle cost ---------------------------------------------------------------
//
// jsdom's `document.hidden` is a getter on the prototype; an own property
// shadows it for the length of this check, which is the only way to ask "what
// does this tab do when nobody is looking" without a real browser.

Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
const a11yCalls = backend.calls.length;
await window.fetchWatchlistTickers();
window.dashTick();
await settle();
check(
  "a tab nobody is looking at does not poll the venue",
  backend.calls.length === a11yCalls,
  `${backend.calls.length - a11yCalls} call(s)`
);
// Deleting the own property hands `document.hidden` back to jsdom's prototype.
delete document.hidden;
window.Shell.go("charts");
await settle();
```

Three notes on what these can and cannot see. The canvas colour check is about
what reached the context, not about what the browser drew: jsdom resolves no
custom property, so the literal fallback is what it sees. The live-region check
drives a real `<select>` and then puts it back, so every section after it still
opens on the series it expects. And the strip checks assert the *stop* and the
arrow, not the announcement a screen reader makes — that is Step 6.

- [ ] **Step 6: The manual pass, and the Definition of Done**

Run the machine half:

```bash
node --check frontend/app/ui/components.js
node --check frontend/app/views/shell.js
node --check frontend/app/app.js
NODE_PATH="${WORKBUDDY_NODE_WORKSPACE:-$HOME/.workbuddy-node}/node_modules" node tools/shell_check.mjs
cargo test -p api-gateway --test packaging
node tools/check_builder.mjs
```

Expected: every check passes. If "a strip is one Tab stop" fails with `0
stop(s)`, `syncTabStrip` is setting `aria-selected` but `HRZUI.tabKeyboard` is
not reached — check the `sync` function still runs on this destination. If "a
skip link is the first thing Tab reaches" fails, the anchor is not
`document.body.firstElementChild`: something (a comment or a stray element) got
above it.

Then the part no jsdom can do, at 1440×900 unless stated:

1. **Keyboard only**, from a signed-out page: sign in without a pointer. Then,
   signed in and from the top of the document, Tab once → "Skip to content"
   appears; Enter jumps into the destination. Open Charts, Tab through the
   instrument bar; at the inspector reach one Tab stop, then ArrowRight through
   the tabs and watch the pane change. Attach an indicator, detach it, and
   confirm the chip and the Indicator tab agree. Open AI, send a message,
   install a community indicator, pause a bot, sign out — every step without a
   pointer, and focus never disappears behind a drawer.
2. **Screen reader** (VoiceOver on macOS, NVDA on Windows): on Dashboard, the
   four cards announce their names and values, the table announces the sorted
   column, and a tile announces "SOL, up 8.1 percent". On Charts, the canvas
   announces the symbol, timeframe, bar count and last price; changing the
   symbol speaks `Chart on …` once; letting the market tick for a minute speaks
   nothing.
3. **Reduced motion**: with "Reduce motion" on, panes appear without the 160ms
   lift, the price flash does not run, and nothing scrolls smoothly.
4. **200% zoom** at 1440×900 and **320px** wide: every action on every
   destination is reachable, the only thing that scrolls sideways is the
   instrument bar on a phone, and no text is clipped.
5. **Lighthouse → Accessibility** on all six destinations, dark and light.
   Target: no critical or serious findings. Record the score in the PR
   description; a finding that is a false positive against a canvas gets a note
   in the PR, not a suppression comment in the source.
6. **Contrast spot-check** with the DevTools colour picker on the three lowest
   pairs in §1.2 (`--text-3` on `--bg-panel`, `--text-3` on `--bg-canvas`,
   `--control-line` on `--bg-canvas`) in both themes.

**Definition of Done for Spec 1.** Every line names where it is held:

| §10 / §11 promise | Held by |
| --- | --- |
| Skip link, focusable main | harness (Step 5) + keyboard pass |
| Named landmarks, one `h1` per destination | harness (Step 5) |
| Roving tabindex, Arrow/Home/End on every strip | harness (Step 5) + keyboard pass |
| Live region: series only, never a tick | harness (Step 5) + screen reader |
| Connection and save announce once | harness (Step 5, the mutation observer) + write-on-change (Step 3) |
| Charts expose `role="img"` with a summary | harness (Step 5) + screen reader |
| No positive tabindex | harness (Step 5) |
| `:focus-visible` 2px accent, never removed | `base.css` (Task 2) + keyboard pass |
| Reduced motion | `base.css` (Task 2) kills every animation and transition; manual with the OS setting |
| 200% zoom and 320px reflow | manual (Step 6.4) |
| Contrast ledger | manual (Step 6.6) |
| axe: 0 critical/serious on six destinations × two themes | manual Lighthouse (Step 6.5) |
| Frame at 1279/1023/767, phone nav | harness (Task 17) + manual |
| Every route, panel and control still works | harness (Tasks 8–17) |
| Packaging guard: every asset linked, every linked asset present | `cargo test -p api-gateway --test packaging` |

- [ ] **Step 7: Commit**

```bash
git add frontend/app/index.html frontend/app/ui/components.js \
  frontend/app/views/shell.js frontend/app/app.js tools/shell_check.mjs
git commit -m "feat: the shell meets its accessibility and performance bar"
```

---
