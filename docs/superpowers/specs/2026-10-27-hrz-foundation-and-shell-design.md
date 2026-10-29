# HRZ Platform UI — Spec 1: Foundation & Shell

**Status:** design approved; awaiting spec review
**Program:** platform UI redesign, split into four specs
**Quality bar:** TradingView and LuxAlgo, with WCAG 2.1 AA as a hard floor

## Purpose

The workstation works, but it is still a page that grew: one header strip, a
watchlist rail, a chart wall and a tabbed aside. Controls hide in a right-click
menu, the home page and the app share a header, every panel is styled on its own,
and nothing is themed. This program gives HRZ one designed system: a real
application frame, a destination for each job, one component language, two
themes and a chart workspace that reads as an instrument rather than a demo.

This spec builds the foundation and the shell — the part everything else sits
on. It does not redesign the inside of Studio, Community, Bots or Account, and it
does not rebuild the public pages. Those are Specs 2 and 4.

## Program split

| Spec | Delivers | Depends on |
| --- | --- | --- |
| **1. Foundation & shell** (this one) | Identity, tokens, themes, typography, icons, `HRZUI` components, static asset delivery, top bar, rail, status bar, router, command palette, docked chart workspace, dashboard, mobile shell, AA foundation | — |
| 2. Destinations | Studio, Community, Bots and Account rebuilt as designed pages; shared library surfaces; account sections | Spec 1 |
| 3. Mobile parity | Touch chart gestures, per-destination phone layouts, low-end performance, full parity audit | Specs 1–2 |
| 4. Public surfaces | New home, sign-in/register, pricing, public community catalog and detail, anonymous read-only endpoints, SPA fallback | Specs 1–2 |

Spec 1 must leave the product fully usable at the end of every milestone. Studio,
Community, Bots and Account mount their existing panes inside the new frame;
their internals are replaced in Spec 2.

## What the shell must preserve

This is a refactor of a working product, not a rewrite. Every behaviour below
keeps working, and the IDs and classes the existing wiring and test harnesses
depend on stay in the markup.

- **Chart panes:** `#charts`, `.chartPane`, `.chartBar`, `.tools`, `.symbol`,
  `.timeframe`, `.limit`, `.mode`, `.close`, `.load`, `.clearDrawings`,
  `.deleteDrawing`, `.footprintStats`, `.chartMsg`, `.feedStatus`, `.chartWrap`,
  `.chartNote`, `.paneHead`, `#globalTools`.
- **Pane controls:** `#split`, `#layoutMode`, `#chartCols`, splitters
  (`#splitWatch`, `#splitAside`), `#railHide`, `#railShow`, the collapse/expand
  pair.
- **Header and auth:** `#session`, `#signinToggle`, `#home`, `#homeEmail`,
  `#homePassword`, `#homeSignIn`, `#homeRegister`, `#homeMsg`.
- **Pane IDs:** every `pane-*` section keeps its ID and its internal IDs
  (`wlRows`, `libRows`, `chatStream`, `ws*`, `community*`, `billing*`,
  `broker*`, `bot*`, `skill*`, `watch*`).
- **`selectPane(name)`** stays as a compatibility function. Internal callers
  (`selectPane("chat")`, `"indicator"`, `"billing"` …) are not rewritten; the
  function maps a legacy pane name to a destination plus an inspector tab and
  lets the router do the rest. Mapping is in §5.1.
- **Workspace snapshot:** existing `version: 1` snapshots load unchanged. New
  fields are optional additions under the same version (§13.2).
- **Chart behaviour:** shared streams per series, offscreen pause, the 100-pane
  grid, splitters, per-pane context menu, drawing tools, footprint, indicator
  chips, AI drawings, chart capture.
- **AI, community, billing and bot flows:** their panes are mounted, not
  cloned or reimplemented.

## Design principles

1. **One surface, one job.** A destination answers "where am I working"; an
   inspector answers "what is this chart"; a dock answers "what is running".
   Controls live on the surface they change.
2. **Density with air.** 13px UI on a 4px grid, 1px lines, no decorative
   shadows in the workspace. Dashboard cards may lift; panels do not.
3. **Colour has meaning.** Cyan is interactive, violet is AI-authored, green
   and red are market direction, amber is caution. Nothing else gets an accent.
4. **State is visible.** Loading, empty, stale, offline, saving and failed are
   designed states, not accidents.
5. **The chart is the product.** Chrome frames it; chrome never competes with
   it. The chart keeps the largest area at every breakpoint.
6. **Keyboard is a first-class input.** Every state-changing control is
   reachable and operable without a pointer.

## 1. Identity — "Signal"

### 1.1 The mark

The old gradient diamond is retired. The mark is a **crosshair-overshoot H**:
two 3px vertical strokes joined by a 3px crossbar that extends past both
strokes, so the negative space reads as a crosshair. It is a single SVG path
rendered in `currentColor`, 20px in the top bar and 16px elsewhere.

- Top bar: mark + the wordmark **HRZ** (Inter 600, 13px, letter-spacing
  `0.12em`, uppercase). No gradient, no glow.
- Favicon: the same mark on the theme ground, with
  `prefers-color-scheme` handling.
- The mark is decorative; the accessible name of the home link is
  "HRZ — Dashboard".

### 1.2 Colour

Two themes, one semantic vocabulary. Hues are approved; two values are
adjusted for AA and marked below. All text pairs meet 4.5:1, all control
boundaries and focus rings meet 3:1.

| Role | Dark | Light |
| --- | --- | --- |
| Canvas | `#0a0d12` | `#f6f8fa` |
| Panel | `#0f141b` | `#ffffff` |
| Raised | `#141b24` | `#f2f5f8` |
| Overlay | `#1a222d` | `#ffffff` |
| Line (decorative) | `#1b2430` | `#dfe5ec` |
| Line strong (hover/drag) | `#2a3644` | `#c6d0db` |
| Control line (inputs, 3:1) | `#5c6b7c` | `#7a8794` |
| Text 1 | `#e9edf3` | `#0d1520` |
| Text 2 | `#aeb8c6` | `#465364` |
| Text 3 | `#7c8898` | `#5f6b7a` † |
| Accent | `#22d3ee` | `#0e7490` ‡ |
| Accent ink | `#04222b` | `#ffffff` |
| Accent soft | `rgba(34,211,238,.12)` | `rgba(14,116,144,.10)` |
| Accent line | `rgba(34,211,238,.35)` | `rgba(14,116,144,.30)` |
| AI | `#a78bfa` | `#7c3aed` |
| AI ink | `#1b1030` | `#ffffff` |
| AI soft | `rgba(167,139,250,.12)` | `rgba(124,58,237,.10)` |
| Up | `#16c784` | `#067a4d` † |
| Down | `#ea3943` | `#d42a3c` § |
| Warn | `#f5a524` | `#b45309` |
| Focus | `#22d3ee` | `#0e7490` |

† Adjusted from the approved light values `#6b7889` and `#0a8f5b`, which
measure 4.2:1 and 4.1:1 on the light ground. The adjusted values measure
5.1:1 and 5.1:1. `#0a8f5b` survives only as `--up-fill`, used for chart
candles where text contrast does not apply.

‡ Light mode keeps the brand cyan `#22d3ee` for the mark and decorative
chart glows. Interactive text, borders, fills and focus use `#0e7490` so
white ink and cyan-on-white text clear AA.

§ Adjusted from the approved light value `#d92d3f`, which measures 4.5:1 on
the light ground -- under AA for text. `#d42a3c` measures 4.7:1. `#d92d3f`
survives only as `--down-fill` and `--down-soft`, for chart candles and tints
where text contrast does not apply.

**Measured (sRGB, WCAG formula):** dark text-3 5.4:1; dark text-2 9.7:1; dark
accent 10.8:1; dark ink-on-accent 9.2:1; dark up 8.8:1; dark down 4.8:1;
light text-3 5.1:1; light text-2 7.4:1; light accent 5.0:1; white-on-light-accent
5.4:1; light up 5.1:1; light down 4.7:1; light warn 4.7:1; dark control line
3.6:1; light control line 3.5:1. M1 re-verifies every pair with a contrast
checker.

Colour is never the only carrier: deltas show `+`/`−` and an arrow, up/down
candles can switch to a blue/orange colour-blind palette, and heatmap tiles
print the percentage.

### 1.3 Typography

- **UI:** Inter variable, weights 400/500/600/700, latin subset, self-hosted
  `frontend/app/fonts/inter-var.woff2`, OFL licence shipped beside it.
- **Data:** JetBrains Mono variable, 400/600, latin subset, self-hosted
  `jetbrains-mono-var.woff2`, OFL licence.
- **Fallbacks:** `system-ui, -apple-system, "Segoe UI", sans-serif` and
  `ui-monospace, "SF Mono", Menlo, Consolas, monospace`.
- **Loading:** `font-display: swap`; both files preloaded. A blocked or missing
  font changes the typeface, never the layout.
- **Scale:** 11/12/13/14/16/20/28/40 with line heights 16/16/20/20/24/28/34/46.
  Base is 13/20.
- **Rules:** tabular numerals on every number (`font-variant-numeric:
  tabular-nums`); prices, quantities, IDs, timestamps and code in mono;
  headings Inter 600 with `-0.01em`; micro-labels 10–11px uppercase with
  `0.06em`. Prose (chat, community descriptions) is capped at 68ch.

### 1.4 Spacing, geometry, elevation

- 4px base: 4, 8, 12, 16, 20, 24, 32, 40. Panel padding 12; page padding 24
  desktop, 16 tablet, 12 phone; dashboard gaps 16; control gaps 8.
- Control heights: 24 (compact), 28 (default), 32 (large/touch). Icon buttons
  are square at the same heights.
- Radii: 4 controls, 6 panels, 10 overlays and dashboard cards; pills are
  fully round.
- Elevation is communicated by lines and ground steps. Shadows are reserved
  for overlays: `0 8px 24px rgba(4,8,12,.35)` dark,
  `0 8px 24px rgba(16,24,40,.12)` light.
- The chart pane has no rounded frame and no shadow: it is an instrument,
  not a card.

### 1.5 Motion

- Durations: 120ms hover/focus/toggle, 180ms overlay/drawer/toast, 240ms
  panel collapse/expand.
- Easing: `cubic-bezier(.2,.8,.2,1)`. No bounce, no spring, no parallax.
- Motion only explains a change: an overlay fades and scales 0.98→1, a dock
  slides its height, a price flashes its background for 240ms.
- `prefers-reduced-motion: reduce` sets all durations to 0 and stops the
  connection pulse and price flash. Chart gestures still work.
- No infinite animation except the connection dot while disconnected, which
  is disabled under reduced motion.

### 1.6 Iconography

One family replaces every emoji in the shell: 24px grid, 1.5px stroke,
round caps, `currentColor`, rendered at 16px (controls), 20px (rail and
headings) and 14px (inside 24px controls).

Inventory: dashboard, chart, studio, community, bots, account, search, sun,
moon, monitor, plus, minus, close, chevron-up/down/left/right, play, pause,
stop, alert, book, trade, settings, upload, download, copy, check, external,
filter, sort, more-horizontal, more-vertical, panel-left, panel-right,
panel-bottom, refresh, trash, edit, eye, eye-off, lock, globe, user, log-out,
command, wifi, zap, layers, clock, arrow-up-right, arrow-down-right, maximize,
minimize, split, grid, sparkles (AI), image, file, link, warning, info,
check-circle, x-circle, loader.

Rules: icon-only buttons carry `aria-label` and a tooltip (300ms delay, on
hover and focus); a tooltip never contains the only copy of an action's
meaning; icons never animate on hover.

## 2. Design tokens

`frontend/app/styles/tokens.css` is the single source of truth. New code uses
semantic tokens only; no literal hex, no literal rgba, no magic spacing.

```
Grounds   --bg-canvas --bg-panel --bg-raised --bg-overlay
Lines     --line --line-strong --control-line
Text      --text-1 --text-2 --text-3
Accent    --accent --accent-ink --accent-soft --accent-line --focus
AI        --ai --ai-ink --ai-soft
Market    --up --down --warn --up-fill --down-fill
          --up-soft --down-soft --warn-soft
Radii     --radius-1 --radius-2 --radius-3
Space     --space-1..--space-8  (4,8,12,16,20,24,32,40)
Type      --font-ui --font-mono --text-2xs --text-xs --text-sm --text-base
          --text-md --text-lg --text-xl --text-2xl
Durations --dur-1 --dur-2 --dur-3 --ease
Elevation --shadow-1 --shadow-2
Layout    --topbar-h --rail-w --status-h --watch-w --inspector-w --dock-h
```

**Compatibility layer.** The existing stylesheet uses `--bg`, `--panel`,
`--panel-2`, `--text-dim`, `--muted`, `--faint`, `--accent-dim`,
`--border-subtle`, `--border-strong`, `--radius-s/m/l`, `--shadow-1/2/3`,
`--mono`. These aliases are defined in `tokens.css` and point at the new
tokens, so the 1,800 lines of existing CSS inherit the new palette before
they are migrated. `--shadow-1` and `--shadow-2` map to the two new overlay
shadows; legacy `--shadow-3` maps to `--shadow-2` (the old third shadow was
used only for dialogs). Aliases are removed only after the last consumer is
converted.

Theme application: `<html data-theme="dark|light">`, set by `ui/theme.js`
before first paint. The preference (`system|dark|light`) lives in
`localStorage["hrz.theme"]`; `system` follows `prefers-color-scheme` live.

## 3. Component layer — `HRZUI`

`frontend/app/ui/components.js` defines a global `HRZUI` on a classic script
(no modules, no bundler). The layer is **CSS-first**: markup uses classes and
data attributes; JavaScript provides only the behaviour that CSS cannot.

### 3.1 API

```
HRZUI.icon(name, { size = 16, label })        -> SVGElement
HRZUI.tooltip(target, text)                   -> void
HRZUI.menu(trigger, items)                    -> { open, close }
HRZUI.popover(trigger, content, options)      -> { open, close }
HRZUI.dialog({ title, body, actions, tone })  -> Promise<result>
HRZUI.confirm({ title, body, confirmLabel, tone }) -> Promise<boolean>
HRZUI.drawer({ side, title, content })        -> { open, close }
HRZUI.toast(message, { tone, action, timeout })
HRZUI.tabs(root)                              -> { select(name) }
HRZUI.table(root)                             -> { setRows(rows), sort(key) }
HRZUI.sparkline(canvas, values, { trend })
HRZUI.meter(el, { value, max, tone })
HRZUI.skeleton(el, { rows, kind })
HRZUI.empty(el, { icon, title, body, action })
HRZUI.errorState(el, { title, message, retry })
HRZUI.format.price / volume / percent / compact / time
HRZUI.escape(value)                           -> safe text
```

All imperative components return handles with `open`, `close` or `destroy`;
all of them restore focus to the trigger on close. All text is inserted with
`textContent` or `HRZUI.escape`; no API or user string is concatenated into
HTML.

### 3.2 Inventory and behaviour

| Component | Contract |
| --- | --- |
| Button | Variants `primary` (accent fill), `secondary` (raised + line), `ghost`, `danger` (down), `ai` (violet outline); sizes 24/28/32; icon-only square; busy state shows a 12px spinner, keeps width, sets `aria-busy` |
| Field | 12px label above; 28/32px input; optional leading icon, trailing clear/reveal; hint 11px; error text + `aria-invalid` + `aria-describedby`; never placeholder-only labels |
| Select | Native `<select>` styled with a chevron icon; 28/32px; full-width on mobile |
| Segmented | 24/28px, one container border, active segment raised; used for timeframe, asset tabs, split/grid |
| Switch | 32×18 track, 12px thumb, accent when on, 120ms; label is the accessible name |
| Slider | 4px track, 12px thumb, value readout in mono; arrow keys step |
| Tabs | 32px strip, active 2px accent underline, roving tabindex, Arrow/Home/End; `role=tablist/tab/tabpanel` |
| Panel | Header 32px with title and right-aligned actions; body padding 12; `flush` variant for tables/lists; no shadow |
| Compact table | 28px header, 30px rows (compact) or 36px; sticky header; right-aligned numerics; `aria-sort`; sortable header buttons; row hover raised, selected row accent tint + 2px left bar |
| Price pill | Mono, tabular, trend tint, optional flash on change; `aria-label` includes the full value and direction |
| Sparkline | 64×20 canvas, 1.25px line, 10% area tint, no axes; `role=img` with a min/max/last label |
| Meter | 4px track, tone fill, label + reading; `role=progressbar` with aria values |
| Badge | 18px, 10px uppercase, tones neutral/accent/up/down/warn/AI |
| Menu | 240px, 4px padding, 28px rows, shortcut hint right-aligned, separators, danger row; roving focus; Esc/outside closes; anchored with flip |
| Popover | 320px, optional header, Esc/outside closes, focus moves in and returns |
| Dialog | Native `<dialog>`, max 480px, title 16px, body 13px, footer right; destructive confirm names its object ("Delete “SMT divergence”") |
| Drawer | Right 400px on tablet, bottom sheet on phone; 90vh/90vw max; drag handle on mobile; focus trap |
| Toast | Bottom-right desktop, top-center phone; max 3; 4s; `role=status` for info, `role=alert` for errors; optional action |
| Skeleton | Rows/cards matching the final layout; shimmer disabled under reduced motion; used for data loads, never a full-page spinner |
| Empty state | 20px icon, 13px title, 12px body, one action |
| Error state | Warn icon, title, message, Retry; inline, never a toast |
| Connection banner | 28px strip under the top bar: "Reconnecting — last update 12s ago"; auto-hides on recovery |
| Tooltip | 300ms delay, above/below flip, 11px, overlay ground; keyboard focus shows it |

## 4. Shell

### 4.1 Frame

```
┌──────────────────────────────────────────────────────────────┐
│ Top bar 44                                                   │
├────┬─────────────────────────────────────────────────────────┤
│Rail│ Destination view                                      │
│48  │                                                       │
├────┴─────────────────────────────────────────────────────────┤
│ Status bar 24                                                │
└──────────────────────────────────────────────────────────────┘
```

The rail spans between the top bar and the status bar. The destination view
owns all remaining space and never scrolls the page itself; each destination
manages its own scroll regions.

### 4.2 Top bar (44px)

Left: mark + wordmark (home link), destination title on tablet and phone.
Centre: a search field styled as a button — "Search or jump to…" with the
`⌘K` hint — that opens the command palette. Right: connection dot (phone
only), **AI** button (sparkles) that opens the inspector on its AI tab from
any destination, session chip (avatar initials + plan badge), theme toggle,
account menu. Signed out, the right side is `Sign in` + theme toggle, and the
rail and status bar are absent.

The top bar never carries chart controls. `＋ Panels`, layout and columns move
to the Charts instrument bar.

### 4.3 Left rail (48px)

> **Superseded 2026-10-28 by §22.** The rail is gone; its six buttons became
> three top-bar view icons, two panel icons and an avatar. §4.1, §4.5, §4.6,
> §5, §9 and §20 also describe the rail and read as history from here.

Six destinations, in order: **Dashboard, Charts, Studio, Community, Bots,
Account**. Each is a 48×44 icon button with a 20px icon, an accessible label
and a delayed tooltip. Active state: raised ground, accent icon, 2px accent
bar on the left edge. A help button at the bottom opens the shortcut sheet.

The rail does not collapse on desktop; 48px is already the minimum. On phone
it becomes a bottom navigation bar (§9).

### 4.4 Status bar (24px)

11px mono, text-3, separated by hairline groups:

- **Left:** connection dot + `Live` / `Reconnecting` / `Offline`, venue name,
  active symbol and timeframe.
- **Centre:** save state — `Saved`, `Saving…`, `Not saved` (after three
  consecutive failures) with a tooltip explaining the retry.
- **Right:** clock (local, toggleable to UTC in the help sheet), feed latency
  (when known), plan name, build version.

On phone the status bar is hidden; the connection dot moves into the top bar
and the rest is available in the help sheet.

### 4.5 Command palette

`⌘K` / `Ctrl+K`, also the top-bar search field. One overlay, 560px, sections
in this order:

1. **Navigate** — the six destinations.
2. **Symbols** — debounced `/symbols/search?q=` (25 results), selecting one
   switches to Charts, reuses a pane already showing it or the active pane,
   and calls `fillSeries`.
3. **Chats** — the indicator workspaces, fetched on the first palette open if
   they are not loaded yet; selecting one opens `#/studio/<id>`.
4. **Actions** — new chart pane, new chat, toggle theme, keyboard shortcuts,
   sign out.

Keyboard: arrows move, Enter runs, Esc closes, focus returns to the trigger.
The palette is a combobox/listbox pair with `aria-activedescendant`; the
result count is announced. Empty input shows Navigate + Actions; no result
shows "No matches for “…”".

### 4.6 Router

Client-side hash routes. The gateway serves `index.html` at `/`; hashes need no
server change and are shareable.

| Route | Destination |
| --- | --- |
| `#/dashboard` | Dashboard (default signed in) |
| `#/charts` | Charts |
| `#/studio` · `#/studio/<workspaceId>` | Studio; selects a chat |
| `#/community` · `#/community/<slug>` | Community; opens a detail |
| `#/bots` · `#/bots/<botId>` | Bots; selects a bot |
| `#/account` | Redirects to `#/account/plan` |
| `#/account/plan` · `/brokers` · `/ai` · `/profile` | Account sections |

- Signed out: the hash is ignored and the home page is shown.
- On sign-in: use the saved destination; a new user gets Dashboard.
- Unknown route: replace with `#/dashboard`.
- Route changes update `document.title` (`Charts · HRZ`), move focus to the
  destination's `h1` (`tabindex="-1"`) and save the destination.
- Inspector/dock tab changes do **not** create history entries; they are
  snapshot state.

### 4.7 Destination lifecycle

Every destination implements:

```
enter(ctx, params)   // mount panes, start timers, focus the heading
leave()              // stop timers and tickers, unmount transient overlays
```

`leave` stops the dashboard's 5s ticker poll, the watchlist tick, any
destination-specific interval and any open menu/drawer. Charts pauses pane
streams through the existing visibility gate; returning resumes them. Nothing
keeps polling behind a destination the user is not on.

### 4.8 Pane host

Panes are **moved, never cloned**. A pane node has exactly one host at a time:

```
registerPane(id, node)
mountPane(id, host)     // moves node into host, clears hidden
unmountPane(id)         // moves node to the hidden parking container
```

All existing `el(id)` lookups and listeners keep working after a move.
Destinations declare their mounts; a pane that is not mounted is hidden and
its timers are stopped. Two copies of a pane would be two wirings and two
opinions about the same state, and are forbidden.

## 5. Destinations

| Destination | Route | Content in Spec 1 |
| --- | --- | --- |
| Dashboard | `#/dashboard` | New: pulse cards, markets table, watchlist panel, heatmap (§6) |
| Charts | `#/charts` | Watchlist, chart wall, inspector, bottom dock (§7) |
| Studio | `#/studio` | `pane-indicator` mounted full width; two-column internals in Spec 2 |
| Community | `#/community` | `pane-community` mounted full width; catalog/detail layout in Spec 2 |
| Bots | `#/bots` | `pane-bots` mounted full width; libraries and bot detail in Spec 2 |
| Account | `#/account/*` | Secondary tabs: Plan (`pane-billing`), Brokers (`pane-brokers`), AI model (`pane-aimodel`), Profile (session, plan, sign out) |

The unified analyst chat (`pane-chat`) lives in the Charts inspector's AI tab.
From any other destination the top-bar AI button opens the inspector as a
right drawer on the same AI tab, hosting the same node. The Studio builder
chat stays inside `pane-indicator`; it is a different conversation with a
different job.

Account's Profile tab is the only new content in this table: email, display
name when present, plan, member since, theme preference and sign out. Spec 2
expands it.

### 5.1 Legacy `selectPane` mapping

| Legacy name | Destination | Inspector tab |
| --- | --- | --- |
| `chat` | Charts | AI |
| `thesis` | Charts | Details |
| `book` | Charts | Book |
| `trade` | Charts | Trade |
| `indicator` | Studio | — |
| `community` | Community | — |
| `bots` | Bots | — |
| `billing` | Account / plan | — |
| `brokers` | Account / brokers | — |
| `aimodel` | Account / ai | — |
| `watchlist` | Charts | — (watchlist visible) |
| `scan`, `strategy`, `skills` | Studio | — |

`scan`, `strategy` and `skills` are internal sub-panes rather than
destinations. Their existing openers keep working; the mapping above only
ensures a direct `selectPane` call lands somewhere sensible.

## 6. Dashboard

The signed-in landing page, and the product's answer to "what is happening".
Content width max 1440px, 24px page padding, 12-column grid, 16px gap.

### 6.1 Page header

H1 "Dashboard" (20px), subtitle with the current date and the active market
summary (price + 24h delta), and a right-aligned "New chart" action and
refresh button. All values are quoted in USDT; there is no currency selector
in this release.

### 6.2 Pulse cards

Four cards, 112px tall, 10px radius, 1px line, equal width at ≥1280, 2×2 at
768–1279, one column below 768.

| Card | Content | Source |
| --- | --- | --- |
| Market | BTC last, 24h %, 24h volume, 24-bar 1h sparkline | `/tickers?symbols=BTCUSDT`, `/candles?symbol=BTCUSDT&timeframe=1h&limit=24` |
| Watchlist | symbol count, up/down split, top gainer with % | `/tickers?symbols=<favorites>` |
| Automation | running / paused / stopped bot counts, last signal age | `/bots`, `/watch-events` |
| Plan | plan name, AI credits meter, backtests meter, reset date | `/billing/subscription`, `/billing/usage` |

A card whose request fails shows its own inline error with Retry; it never
blanks the row. Cards refresh every 10s while the destination is visible.

### 6.3 Markets table

Full-width card spanning 8 columns at ≥1280, 7 at 1024–1279, 12 below 1024.

- Tabs: **All · Majors · Alts · Watchlist**. Majors is a fixed list of ten
  liquid pairs (BTC, ETH, SOL, BNB, XRP, ADA, DOGE, AVAX, LINK, DOT); Alts is
  everything else; Watchlist reads the saved favourites.
- Columns: Symbol (mark + pair), Last, 24h %, 24h Volume. Numerics mono,
  tabular, right-aligned. Header buttons sort; the default is volume
  descending.
- Data: `/tickers?limit=1000`, refreshed every 5s while visible. The table
  renders the top 50 rows for the active tab and offers "Show 50 more";
  filtering by a text field narrows before slicing.
- Row click (or Enter) opens the symbol in Charts: reuse a pane already
  showing it, otherwise the active pane; `fillSeries`.
- Selected/hover row follows the compact-table contract. On phone rows become
  cards: symbol + price + delta, no header.

### 6.4 Watchlist panel

Spans 4 columns at ≥1280, 5 at 1024–1279, full width below 1024. It is the same watchlist source as
the Charts rail — one list, two presentations. Compact rows (symbol, last,
24h %) with a star affordance to remove; a "＋ Add" opens the symbol search;
an "Open Charts" link goes to the Charts destination. The panel never starts
a second ticker: it subscribes to the watchlist ticker while Dashboard is
visible and unsubscribes on leave.

### 6.5 Heatmap

Full width, 320px tall at desktop. Tiles are the top 60 pairs by 24h volume;
tile size maps volume into four buckets, tile colour maps 24h change into
seven semantic buckets (down-strong … flat … up-strong). Each tile prints
the base symbol and the signed percentage, and is a button with an
`aria-label` ("SOL, up 4.2 percent"). Selecting a tile opens it in Charts.
A "Show as table" switch swaps the heatmap for the same data in the compact
table for users who prefer it; the choice is remembered per device.

## 7. Charts workspace

### 7.1 Layout

```
┌ top bar 44 ──────────────────────────────────────────────────────────┐
│ rail 48 │ watchlist 260 │ instrument bar 40 + wall │ inspector 360   │
│         │               │      bottom dock          │                 │
├─────────┴───────────────┴──────────────────────────┴─────────────────┤
│ status 24                                                            │
└──────────────────────────────────────────────────────────────────────┘
```

The watchlist and inspector are independent collapsible columns. The
instrument bar spans only the chart column; the dock spans only the chart
column; neither pushes under the watchlist or inspector.

Defaults: watchlist 260 (range 200–360), inspector 360 (range 300–480),
dock closed with a remembered height of 240 (range 160–45vh). All widths and
the open/collapsed states persist in the workspace snapshot.

### 7.2 Active pane

One pane is active. Clicking or focusing a pane, or any command that targets a
pane, makes it active. Active pane: 2px accent line at the top of its header,
title in text-1, accent dot. Inactive titles are text-2.

The instrument bar and the inspector always describe the **active pane**. The
agent's `open_chart` / `set_chart` commands, dashboard row clicks, watchlist
rows and palette symbol results all activate a pane, so the bar and inspector
agree with what just changed.

### 7.3 Instrument bar (40px)

A projection of the active pane's own controls, not a second source of truth.
It reads pane state and writes through the pane's command API; the per-pane
`.chartBar` nodes remain the canonical controls (and remain available from the
context menu), so the harness and existing wiring keep working.

Groups, left to right:

1. **Instrument:** symbol button (opens symbol search), timeframe segmented
   (1m, 5m, 15m, 1h, 4h) with an overflow for the rest, bar limit select.
2. **View:** chart type segmented (Candles, Bars, Line, Area, Footprint),
   zones switch, Fit button, reload.
3. **Indicators:** attached indicator chip (name + version), an "Attach" menu
   (Studio workspaces, community installs), and a layers popover.
4. **Panes:** layout popover (Split/Grid + columns), `＋ Pane`, maximize.
5. **Chrome:** watchlist toggle, inspector toggle, dock toggle, More
   (screenshot, colour-blind candles, chart settings, keyboard help).

The bar responds to the **chart column's** width, not the viewport, because
the watchlist and inspector change that width independently. A container
query on the column collapses the groups in this priority order: Indicators
and Panes first (under 900px), then View (under 720px). Symbol, timeframe and
Fit never collapse.

The pane command API is extended so both the bar and the pane controls call
one implementation: `setSymbol`, `setTimeframe`, `setBars`, `setMode`,
`setZones`, `fit`, `reload`. `fillSeries` keeps its current signature.

### 7.4 Chart wall

The existing split/grid wall, restyled:

- Split: up to four panes in rows, splitters preserved. Grid: up to 100 panes
  in a scrolling wall, columns selectable 1–10.
- `#split` (add pane), `#layoutMode`, `#chartCols` move into the instrument
  bar's layout popover; their IDs and listeners are unchanged.
- Pane chrome: 32px header with title, indicator chip, badge row, collapse,
  expand, close. The gesture hint moves out of the header into a first-run
  coach mark and the help sheet.
- `#globalTools` becomes a 36px vertical drawing rail on the left edge of the
  chart column, with flyout labels; it still drives the active pane.
- The wall uses CSS containment (`contain: layout paint`) and the existing
  offscreen observer. No new per-pane interval or rAF loop is added.
- At 768–1023 split is capped at 2 columns and grid at 2 columns. Below 768
  the wall shows one pane at a time with a pane chip bar (Spec 3 polishes the
  touch experience).

### 7.5 Watchlist rail

The existing watchlist pane plus the Library card, restyled: search field at
the top, tag filters, count and source line, compact rows (symbol, last, 24h
delta), Library collapsible at the bottom. Rows show a 3px up/down edge tint.
Row click opens the symbol in the active pane; Alt-click adds a pane.
Collapsed state: a 20px reveal strip with a chevron on pointer devices; the
instrument bar's watchlist toggle does the same thing and is the only reveal
on touch. `#railHide` and `#railShow` become these collapse controls, keeping
their existing wiring.

### 7.6 Inspector (360px)

Tabs across the top: **AI · Indicator · Book · Trade · Details**.

| Tab | Content | Node |
| --- | --- | --- |
| AI | The unified analyst chat, history, composer, model chip | `pane-chat` |
| Indicator | Active chart's attached indicator: name, version, source summary, detach, "Open in Studio", alerts for this layer | New projection of the active pane's indicator state |
| Book | Live depth ladder | `pane-book` |
| Trade | Ticket and practice ledger | `pane-trade` |
| Details | Thesis and drawings summary, watch monitor, recent alerts, session/data details | `pane-thesis` + alerts/monitor blocks |

The inspector remembers its open tab in the snapshot. Closing it
leaves a 20px reveal strip with the inspector icon on pointer devices; the
instrument bar and the top-bar AI button reopen it, and are the only reveal
on touch. The AI tab is the default on Charts.

On <1024px the inspector is a right drawer; on <768 it is a bottom sheet. The
AI button in the top bar opens it to the AI tab from any destination.

### 7.7 Bottom dock

A 28px tab bar at the bottom of the chart column, with four tabs and a
collapse chevron. Collapsed by default; selecting a tab opens the dock to its
remembered height. Double-clicking the tab bar resets the height. The dock
never covers the status bar.

| Tab | Content | Source |
| --- | --- | --- |
| Studio | The indicator builder (`pane-indicator`) mounted in the dock | Existing pane |
| Alerts | Watch monitors (enable/pause/delete) + recent watch events | `/watch-monitors`, `/watch-events` |
| Orders | Today's practice fills and open positions, compact table | `/trade/portfolio` + the local practice ledger |
| Console | Client event log: connections, reconnects, orders, bot notifications, API errors, save failures | In-memory ring buffer, 200 entries |

The Studio tab mounts the same `pane-indicator` node used by the Studio
destination; the pane host moves it, so there is only ever one builder. The
Console is a new global `HRZLog` that existing error paths write to; nothing
is persisted. A new event marks the tab with a dot when the dock is closed.

### 7.8 Chart theme bridge

The canvas palette is derived from the active theme:

```
chartPalette('dark' | 'light') -> { up, down, wick, profile, value, grid,
  text, vwap, poc, vah, val, entry, stop, target, drawings, footprint }
```

`createChartPane`'s `COLORS` object is populated from `chartPalette`, and a
`hrz:theme` listener does `Object.assign(COLORS, chartPalette(current))` then
`panes.forEach(p => p.redraw())`. The pane ground stays transparent so the
CSS theme shows through. Screenshot/PNG export uses the active palette.

The colour-blind option swaps candles to blue `#2962ff` / orange `#ff6d00`
in both themes and is stored per device (`chart.cvd`).

## 8. Command palette and shortcuts

| Shortcut | Action |
| --- | --- |
| `⌘K` / `Ctrl+K` | Command palette |
| `⌘/` | Shortcut sheet (also the rail's help button) |
| `Alt+1…6` | Dashboard, Charts, Studio, Community, Bots, Account |
| `Alt+←/→` | Previous/next destination |
| `Esc` | Close the top overlay, then clear focus |
| `? ` (not in a field) | Shortcut sheet |
| Chart keys | Existing behaviour is preserved: arrows pan, `+/-` zoom, `Alt+R` fits, `Delete` deletes a selected drawing |

The shortcut sheet lists these plus the chart gestures, grouped, with
platform-aware modifier labels. Shortcuts inside inputs are never intercepted.

## 9. Responsive model

| Range | Frame | Charts | Dashboard |
| --- | --- | --- | --- |
| ≥1280 | Rail 48, status 24 | Watchlist 260 open, inspector 360 open, dock closed | 4 pulse cards, 8/4 markets + watchlist, heatmap full |
| 1024–1279 | Rail 48, status 24 | Watchlist collapsed by default, inspector 320 open | 2×2 pulse cards, 7/5 markets + watchlist |
| 768–1023 | Rail 48, status 24 | Watchlist and inspector as drawers; dock as a collapsible sheet; split capped at 2 | 2 pulse columns, markets full width, watchlist full, heatmap 240 |
| <768 | Bottom nav 56 + safe area, top bar 48, no rail/status | One pane at a time with a chip bar; instrument bar scrolls horizontally with symbol + timeframe pinned; watchlist/inspector/dock as bottom sheets | 1 pulse column, markets as cards, watchlist card, heatmap 2 columns |

Additional rules:

- Page padding follows the breakpoint (24/16/12).
- All touch targets are at least 44×44 at <1024.
- No horizontal page scroll at 320 CSS px.
- Tables become cards below 768; heatmap tiles keep labels and percentages.
- Dialogs become bottom sheets below 768; drawers become sheets; toasts move
  to top-center.
- Desktop keeps hover affordances; touch devices do not rely on hover to
  reveal an action (the × on a library row, for example, is always visible on
  touch).

Chart touch gestures (pinch zoom, drag pan, long-press menu) are implemented
in Spec 3. Spec 1's mobile shell is usable with the existing pointer pan and
the instrument bar's zoom/fit controls.

## 10. Accessibility — WCAG 2.1 AA

**Text.** Every text token pair meets 4.5:1 against its ground; the ledger in
§1.2 is the contract. Text-3 is the lowest-contrast text and never carries
essential information alone.

**Non-text.** Control boundaries meet 3:1 (`--control-line`), focus rings meet
3:1, and state changes that use colour also change text, icon or shape.

**Keyboard.** Skip link to the destination main. Every control is reachable in
DOM order; no positive tabindex. `:focus-visible` uses a 2px accent outline
with 2px offset, never removed. Tabs use roving tabindex with arrow/Home/End.
Menus use roving focus and return focus to the trigger. Dialogs trap focus
natively via `<dialog>`; drawers trap focus manually and restore it. Escape
closes the topmost layer only.

**Landmarks and headings.** `<header>` (top bar), `<nav aria-label="Destinations">`,
`<main>` (destination), `<aside aria-label="Inspector">`, `<footer>` (status).
One `h1` per destination; panels use `h2`/`h3` in order. Icon-only buttons
have an accessible name.

**Live updates.** Toasts use `role=status`/`alert`. Connection changes are
announced once, not on every retry. Price ticks are **not** announced; a
visually hidden live region announces symbol, timeframe and snapshot changes
only. Charts expose `role="img"` with a text summary (symbol, timeframe, last
price, visible range) updated on user-driven changes.

**Charts.** Every state-changing chart action has a keyboard-reachable control
(symbol, timeframe, mode, bars, zones, indicators, drawing tools, fit, reload).
Pan/zoom gestures are view-only and have zoom/fit buttons as equivalents.
The colour-blind candle option is in the instrument bar's More menu.

**Motion.** `prefers-reduced-motion` removes transitions, pulses and price
flashes. No content flashes more than three times per second.

**Zoom and reflow.** 200% zoom and 320px width keep all content and actions
reachable without horizontal scrolling, except the chart canvas itself, which
is a zoomable instrument with controls outside it.

**Verification.** axe-core (0 critical/serious) on all six destinations in both
themes; a keyboard-only pass through sign-in, chart setup, AI chat, community
install, bot pause and sign-out; a screen-reader smoke test on Dashboard and
Charts; contrast checker on the §1.2 pairs.

## 11. Frontend architecture

### 11.1 File layout

```
frontend/app/
  index.html              shell markup only; no <style> block
  styles/
    tokens.css            variables, aliases, @font-face
    base.css              reset, typography, focus, scrollbars
    components.css        HRZUI classes and states
    shell.css             top bar, rail, status, router, palette
    charts.css            wall, panes, instrument bar, watchlist, dock
    dashboard.css         pulse, markets, watchlist panel, heatmap
    inspector.css         inspector tabs and panels
    studio.css            current Studio styles, moved and tokenised
    community.css         current community styles, moved and tokenised
    account.css           billing, brokers, AI model, profile
    public.css            home and auth styles (rebuilt in Spec 4)
    responsive.css        breakpoints, last in cascade
  ui/
    icons.js              sprite + icon()
    theme.js              theme boot + Theme API
    components.js         HRZUI
  views/
    shell.js              frame, router, pane host, palette
  fonts/
    inter-var.woff2
    jetbrains-mono-var.woff2
    OFL-Inter.txt
    OFL-JetBrainsMono.txt
  favicon.svg
  builder.js
  app.js
```

Load order in `index.html`:

```
<link rel="stylesheet" href="/assets/styles/tokens.css">
... base, components, shell, charts, dashboard, inspector,
    studio, community, account, public
<link rel="stylesheet" href="/assets/styles/responsive.css">
<link rel="preload" as="font" type="font/woff2" href="/assets/fonts/inter-var.woff2" crossorigin>
<link rel="preload" as="font" type="font/woff2" href="/assets/fonts/jetbrains-mono-var.woff2" crossorigin>
<link rel="icon" type="image/svg+xml" href="/assets/favicon.svg">
<head> <script src="/assets/ui/theme.js"></script>   (blocking, sets data-theme before paint)
<body end>
<script src="/assets/ui/icons.js"></script>
<script src="/assets/ui/components.js"></script>
<script src="builder.js"></script>                   (existing route and path)
<script src="/assets/views/shell.js"></script>
<script src="app.js"></script>                       (existing route and path)
```

`app.js` and `builder.js` keep their current routes so existing tooling,
`tools/check_builder.mjs` and the browser cache behave as before. New files
live under `/assets/`.

### 11.2 Globals and boundaries

- `HRZUI` — components, formatting, escaping.
- `Theme` — `boot()`, `current()`, `set(pref)`, `onChange(cb)`.
- `Shell` — `init()`, `start()`, `go(destination)`, `restore()`, `snapshot()`,
  `refreshStatus()`, `openInspector(tab)`, `setInspectorOpen(open)`,
  `setWatchlistCollapsed(collapsed)`, `openDock(tab)`, `closeDock()`,
  `state()`; the hash is the route.
- `HRZLog` — `write(level, message)`, `entries()`, `onEntry(cb)`.
- There is no `Dashboard` global: the dashboard's modules live in `app.js`,
  because every input they read — the ticker batch, the favourites, the bot
  list, the subscription — is state `app.js` already owns. The shell reaches
  them through one hook, `HRZApp.setDestinationVisible(destination)`, which
  starts and stops the page's clock.
- `app.js` keeps ownership of market data, panes, chat, workspaces, billing,
  community and bots. The new views call into it through a small documented
  `HRZApp` facade — session (`signedIn`, `email`, `planName`), panes
  (`chartSymbol`, `newPane`, `activePaneInfo`, `runPaneLoaders`, `resizePanes`),
  panels (`openWorkspace`, `searchSymbols`, `detachActiveIndicator`), chart
  settings (`chartPalette`, `chartSettings`, `setChartSetting`,
  `openChartSettings`) and the dashboard's `setDestinationVisible` — rather
  than reaching into its internals. The facade is a list of existing functions,
  not a second implementation.

No module bundler, no ES modules, no framework. Files are classic scripts
executed in order. A feature that needs a new file gets a `<script>` tag and
an entry in the packaging guard.

### 11.3 Markup rules

- New markup uses semantic elements, `data-*` hooks and HRZUI classes.
- No inline `style` attributes in new code; dynamic values use CSS custom
  properties (`el.style.setProperty("--w", ...)` is allowed for geometry).
- No user or API string is inserted with `innerHTML`; use `textContent` or
  `HRZUI.escape`.
- Existing IDs and classes are preserved; deprecated wrappers stay until the
  harness and all callers migrate.
- Every interactive element is a `<button>`, `<a>` or a real form control; no
  clickable `<div>`.

### 11.4 Existing code cleanups folded into the migration

- Remove the unused `origSelectPane` alias at the end of `app.js`.
- Replace the remaining emoji in shell chrome with icons from `icons.js`.
- Move the hardcoded canvas palette into `chartPalette`.
- Replace hardcoded colours in the moved CSS with tokens; `grep -nE
  '#[0-9a-fA-F]{3,8}|rgba?\('` on `styles/` must return only `tokens.css`
  (and the chart palette in `app.js`).
- Remove the obsolete 1100/900 media rules in M6 once the 1280/1024/768 rules
  cover the same behaviour; `shell_check`'s media-order assertion is updated
  with them.

## 12. Static asset delivery (backend)

The gateway currently serves exactly four files at fixed routes (`/`,
`/app.js`, `/builder.js`, `/chart_engine.wasm`). The split stylesheets and new
scripts need a route before the shell can load them. This is the only backend
change in Spec 1.

### 12.1 Route

`GET /assets/{*path}` serves `frontend/app/{path}`.

- Public (no auth), beside `/` in the router.
- Allowed extensions: `.css`, `.js`, `.woff2`, `.svg`.
- Content types: `text/css; charset=utf-8`, `text/javascript; charset=utf-8`,
  `font/woff2`, `image/svg+xml`.
- Headers: `Cache-Control: no-cache`, `X-Content-Type-Options: nosniff`. Fonts
  may switch to immutable caching when file names gain content hashes (later).
- 404 for a missing file, a disallowed extension, an empty path, a dotfile, a
  path whose decoded segments include `.` or `..`, any backslash, or a path
  longer than 200 characters. The response body is empty; the warning log
  names the resolved path, as `read_frontend` does today.
- The handler uses the same `frontend_dir()` resolution as the existing
  routes, so `FRONTEND_DIR` deployments keep working.

### 12.2 Tests

- `packaging.rs` learns `<link rel="stylesheet" href>` and `/assets/` script
  references: every referenced file exists on disk and the router exposes
  `/assets/{*path}`. Each `.css`, `.js` and `.woff2` under `styles/`, `ui/`,
  `views/` and `fonts/` is referenced (no orphans; licence texts are exempt),
  `tokens.css` is first, `responsive.css` is last, and `index.html` contains
  no `<style>` block.
- New `assets_flow.rs`: 200 + content type for one file of each extension;
  404 for `../Cargo.toml`, `%2e%2e/%2e%2e/Cargo.toml`, `.env`, `app.rs`,
  an empty path and a missing file; `nosniff` present.
- `tools/shell_check.mjs` reads the stylesheet files in load order for its
  responsive assertions instead of slicing the inline `<style>` block.

## 13. State and persistence

### 13.1 Theme

`localStorage["hrz.theme"]` = `system|dark|light`, default `system`.
`Theme.boot()` in the head sets `data-theme` before paint; `Theme.set()`
persists and dispatches `hrz:theme`. The top-bar button toggles
light/dark; Account → Appearance offers System/Light/Dark. Theme is per
device, not part of the server snapshot.

### 13.2 Snapshot extension

The server validates only `version`, `panes` and the size bound; the rest is
opaque. Spec 1 keeps `version: 1` and adds optional keys. Old clients ignore
them; new clients default missing values. No backend migration.

```json
{
  "version": 1,
  "destination": "charts",
  "inspectorTab": "ai",
  "inspectorOpen": true,
  "watchlistWidth": 260,
  "watchlistCollapsed": false,
  "dockOpen": false,
  "dockTab": "studio",
  "dockHeight": 240,
  "layout": { "mode": "split", "columns": 2 },
  "activePane": 0,
  "activeTab": "chat",
  "indicatorWorkspaceId": null,
  "panes": [ ... ]
}
```

Defaults: destination `dashboard`, inspectorTab `ai`, inspectorOpen true at
≥1024 (false below), watchlistWidth 260, collapsed false at ≥1280 (true
below), dockOpen false, dockTab `studio`, dockHeight 240.

`activeTab` remains a **legacy pane name** so an older reader still
understands the snapshot: AI writes `chat`, Details writes `thesis`, Book
writes `book`, Trade writes `trade`, Indicator writes `chat`. New readers use
`inspectorTab`; `activeTab` is derived from it on save and ignored on load.

### 13.3 v1 migration

| Snapshot `activeTab` | Destination | Inspector tab |
| --- | --- | --- |
| `chat` | Charts | AI |
| `thesis` | Charts | Details |
| `book` | Charts | Book |
| `trade` | Charts | Trade |
| `indicator` | Studio | AI |
| `community` | Community | AI |
| `bots` | Bots | AI |
| `billing` | Account / plan | AI |
| `brokers` | Account / brokers | AI |
| `aimodel` | Account / ai | AI |
| missing + panes present | Charts | AI |
| missing + no panes | Dashboard | AI |

Saves stay debounced and skip when the encoded snapshot is unchanged. Widths
and collapse states are saved through the same debounce; a drag writes once
when the pointer releases, not per frame.

## 14. Loading, empty, error, offline

- **First paint:** a static shell skeleton (top bar, rail, content placeholder)
  is in the markup, so the frame appears before scripts. Panes and panels
  keep their own skeletons.
- **Auth check:** the frame skeleton stays until the token check resolves;
  signed-out shows home. No flash of the dashboard for signed-out users.
- **Data loads:** skeletons for tables, cards and lists. No full-page spinner.
- **Empty:** each module has a specific empty state with one action. Examples:
  "No bots yet — build one from a strategy" → Bots; "This chart has no
  indicator — attach one from Studio" → Indicator tab; "No alerts yet — add a
  watch monitor from Details".
- **Errors:** inline error state with Retry inside the affected panel;
  mutations use a toast. A failed workspace save is silent for three attempts,
  then the status bar reads `Not saved` with a retry action.
- **Offline / reconnect:** a banner under the top bar with the last update
  age; charts keep the last frame and show a "stale" pill on the active pane;
  actions that need the server disable with a tooltip. Recovery removes the
  banner and logs to Console.
- **401:** the shell returns to the home auth card and preserves the current
  route, restoring it after sign-in.

## 15. Performance

- Initial shell CSS ≤ 80KB uncompressed; new JS ≤ 60KB uncompressed; fonts
  ≤ 120KB total (woff2, latin).
- First shell paint on the local gateway < 1s; destination switch < 100ms;
  theme switch completes within one frame after the palette swap.
- Dashboard polls only while visible (5s markets, 10s cards); leaving stops
  every timer.
- The 100-pane wall keeps the existing shared streams and offscreen pause. The
  shell adds no per-pane interval, ResizeObserver or rAF loop. Panes use
  `contain: layout paint`; the existing offscreen observer pauses streams.
  `content-visibility` is not applied to chart cells because it interferes
  with canvas sizing.
- No layout shift from font swap: fixed line heights and preloaded fonts;
  target CLS < 0.05.
- Menus/dialogs are built on open and discarded on close; no permanent hidden
  DOM for every row.

## 16. Delivery milestones

Each milestone ends with a working product, a commit, and manual verification.
The order is foundation first, so no milestone restyles a page that has not
received the tokens.

| # | Milestone | Deliverable | Verification |
| --- | --- | --- | --- |
| M1 | Tokens, type, icons, themes | CSS split into files; tokens + aliases; Inter/JetBrains Mono; icon sprite; theme boot and toggle; asset route; packaging/assets tests; harness updated | Both themes on every existing page; no console errors; `node --check`; packaging + assets tests; screenshot review at 1440×900 |
| M2 | Components | `HRZUI` core (buttons, fields, selects, segmented, switch, tabs, table, badges, panel, menu, dialog, drawer, toast, skeleton, empty/error, sparkline, meter); migrate shell-level controls | Keyboard pass on components; contrast ledger verified; axe 0 critical/serious |
| M3 | Frame, router, pane host | Top bar, rail, status bar, command palette, hash router, destination lifecycle, snapshot extension, legacy `selectPane` mapping; Studio/Community/Bots/Account mount existing panes | Refresh restores destination/tab/layout; v1 snapshot loads; every legacy caller lands correctly; old flows still work |
| M4 | Charts workspace | Instrument bar projection + pane command API, active pane, watchlist rail, inspector tabs, dock (Studio/Alerts/Orders/Console), chart theme bridge, colour-blind palette | All chart controls still work through both bar and pane; theme switch redraws every pane; 100-pane wall still streams and pauses offscreen; `shell_check` green |
| M5 | Dashboard | Pulse cards, markets table, watchlist panel, heatmap, deep links | Data matches `/tickers`, `/candles`, `/bots`, `/billing`; row/tile clicks open the right chart; leave stops timers |
| M6 | Responsive, AA, polish | Breakpoint rules, phone nav/sheets, obsolete media rules removed, full a11y audit, performance pass | Width matrix 390/768/1024/1280/1440; 200% zoom; reduced motion; axe on six destinations × two themes; keyboard-only end-to-end |

Dependencies: M2 needs M1's tokens; M3 needs M2's frame components; M4 needs M3's
pane host; M5 needs M1–M3; M6 needs all.

## 17. Verification and tests

**Automated**
- `node --check` on `ui/*.js`, `views/*.js`, `app.js`, `builder.js`.
- `cargo test -p api-gateway --test packaging` (updated) and the new
  `assets_flow` test.
- `cargo test -p api-gateway --lib` to prove no accidental backend change.
- `tools/shell_check.mjs` with the stylesheet loader (chart layout, pane
  controls, responsive cascade).
- Existing community, billing, bot and market tests remain untouched and
  green.
- `git diff --check` clean.

**Manual (browser, local gateway)**
- Sign in, land on Dashboard, navigate all six destinations, refresh and
  confirm restoration (destination, inspector tab, widths, dock, panes).
- Existing flows: add/close/maximize panes; split and grid; attach an
  indicator; AI chat; publish/install a community indicator; switch plans;
  pause/resume a bot; sign out and back in.
- Both themes on every destination; theme switch with 4 panes open.
- Command palette for each section; keyboard-only journey; screen-reader
  smoke test.
- Widths 390, 768, 1024, 1280, 1440; 200% zoom; reduced motion; colour-blind
  candles.
- Connection loss (stop the feed or go offline) shows the banner, stale pill
  and `Not saved`, and recovers.

**Operational constraints**
- The user tests on the local gateway at `:8080`. Do not restart it, rebuild
  it, or run `tools/guard_check.py` while they are testing; ask first.
- Frontend files are served from disk with `no-cache`, so CSS/JS changes
  appear on refresh. The asset route and any Rust change need one deliberate
  rebuild and restart, only when the user agrees.
- Do not run `cargo fmt --all`; the repository is not rustfmt-clean.

## 18. Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| The harness is coupled to inline CSS and per-pane controls | Keep every ID and class; update `shell_check` in M1 before moving styles; run it at every milestone |
| Moving panes breaks listeners or ancestry-dependent CSS | Move nodes, never clone; audit `aside .pane` and similar selectors; manual flow test each milestone |
| The instrument bar and pane controls disagree | One command API; the bar is a projection that reads and writes through it; no independent state |
| Canvas palette drifts from the CSS theme | One `chartPalette()` source; theme event redraws all panes; visual check on both themes |
| 100-pane performance regresses | No new per-pane observers/timers; containment; benchmark the wall in M4 |
| Fonts block or shift layout | Self-hosted, preloaded, `swap`, fixed line heights, system fallbacks |
| The asset route becomes a file-disclosure hole | Extension allowlist, path validation, `nosniff`, traversal tests |
| A long refactor delays value | Six shippable milestones; every commit leaves the app working; the user can test after each |
| Destinations look uneven until Spec 2 | Accepted and sequenced: shell quality first, page internals next |

## 19. Out of scope

- Redesigning Studio, Community, Bots or Account internals (Spec 2).
- Chart touch gestures and phone-specific destination layouts (Spec 3).
- The public home, pricing, public catalog/detail, anonymous endpoints and
  SPA fallback (Spec 4).
- Drag-and-drop pane rearrangement, saved named layouts, multi-monitor popout.
- Additional quote currencies, asset classes or venues.
- Internationalisation, RTL, custom accent colours, user themes.
- Server-side rendering or a frontend build step.
- Notification centre or a bell in the top bar.
- Any change to billing, community, bot, market or agent behaviour.

## 20. Definition of done

- [ ] The shell renders in both themes with no hardcoded colours outside
      tokens and the chart palette.
- [ ] All six destinations are reachable and restore after a refresh,
      including a v1 snapshot.
- [ ] Every existing flow listed in §17 still works.
- [ ] Charts are driven from the instrument bar and the inspector; the active
      pane is always the one they describe.
- [ ] Dashboard modules use real endpoints and stop polling when hidden.
- [ ] The phone shell (bottom nav, sheets, one pane) is usable and every
      target is 44px.
- [ ] axe reports 0 critical/serious on all destinations in both themes; the
      keyboard journey passes; 200% zoom and 320px reflow pass.
- [ ] Packaging, assets, shell_check, `node --check` and the existing gateway
      tests are green.
- [ ] No backend behaviour changed except `GET /assets/{*path}`.

## 21. Hand-off

Spec 1 ends with the frame, the chart workspace and the dashboard. Spec 2 then
rebuilds the four destination interiors on this foundation: Studio as a
two-column builder, Community as a catalog and detail page, Bots as a fleet
view with a bot detail, Account as a real settings area. Spec 2 consumes
`HRZUI`, the tokens, the pane host and the router defined here and does not
change them.

## 22. Amendment — the panels that float over the frame (2026-10-28)

Spec 1 shipped a left rail of six destinations. It was removed the day the
phone-first review compared it with TradingView and LuxAlgo: their subpanels
open *over* the chart, so the chart stays on screen, and none of them spends a
permanent column on three surfaces a user visits occasionally. The rail cost
48px of chart width at every width, on every screen.

### 22.1 Two kinds of route

The six ids did not become six of the same thing. They split:

| Kind | Ids | Owns |
| --- | --- | --- |
| **view** | `dashboard`, `charts`, `studio` | a `#dest-*` section, the frame, the bottom nav |
| **panel** | `community`, `bots`, `account` | one shared modal, `#workPanel`, and nothing else |

`views/shell.js` reads both out of one `NAV` table, so an id cannot be a view in
the router and a panel in the palette.

### 22.2 Grammar

Canonical: `#/<view>[/<panel>[/<section>]]`. Aliased: `#/<panel>[/<section>]`,
which is what the bar said before, still opens that panel — over Charts, the
view it used to sit beside — and is rewritten to the canonical form with
`replaceState`, so Back leaves the screen rather than one spelling of it.

Two consequences a reader should expect:

- A workspace named exactly `community`, `bots` or `account` is unreachable by
  URL; `#/studio/<id>` reads its second segment as a panel first. The builder's
  own list still opens it.
- **The hash is the only truth about a panel.** `syncPanel()` is the one place
  the modal is told what to be, so a click, a Back press, a pasted link and a
  reload all arrive through the same door.

### 22.3 The modal

`HRZUI.panel(root, { onRequestClose, onSelect })` wraps a native `<dialog>`,
which is what carries the top layer, the scrim, the focus trap, Escape and the
frozen background — the clocks behind it keep ticking, because a modal is not a
pause. It is deliberately dumb: it knows a name, a title, a width
(`narrow` 520 / `medium` 760 / `wide` 1080) and which host is on screen. It does
not know that closing should change the address; that is the owner's answer to
`onRequestClose`.

**Content does not move.** `#communityHost`, `#botsHost` and `#accountHost` are
the same nodes `mount()` fills, relocated under `<dialog id="workPanel">` in the
markup. `HOSTS`, the pane host and every builder in `app.js` are untouched.

### 22.4 Chrome

- Top bar, left: three view icons (`#topNav`). Beside them, behind a hairline:
  Community and Bots (`#panelNav`). Right end: the avatar (`#accountBtn`), which
  opens the Account panel directly — its sections are tabs inside it, and sign
  out is in Profile.
- The shortcut sheet's opener is `#shortcutsBtn`; `#railHelp` no longer exists.
- Phone: `#phoneNav` lists three views, not six. Panels open from the same
  top-bar icons, and the modal becomes a full-width sheet at every `[data-width]`.
- `--rail-w` is gone from `tokens.css`; `#sideWatch` and `#dock` are fixed to
  `left: 0` at 1023 instead of starting after a rail.

### 22.5 Verification

`tools/ui_check.mjs` asserts the primitive (17 checks: one host on screen, the
close button and Escape ask rather than close, focus returns to the opener).
`tools/shell_check.mjs` asserts the shell's use of it — a new section, "the
panels that float over the frame", plus the palette's `Alt+4` and a sign-out
that closes the modal and disables every trigger. jsdom has no `showModal`, so
that file shims `showModal`/`close` on `HTMLDialogElement.prototype`; the top
layer, scrim and focus trap are verified by hand in a browser.

### 22.6 What only a browser could tell us

Three of these were found by running the page, and none of them is visible to a
harness — jsdom has no cascade, no top layer and no focus restoration.

- **`display` belongs to `[open]`, and so does its own hiding.** The user-agent
  sheet hides a closed `<dialog>` and a `[hidden]` element with `display: none`,
  and any author `display` outranks it. `.hrz-dialog--panel { display: flex }`
  painted a shut panel on the signed-out home page, where no route had opened it
  and nothing could close it; `.panelBody { display: flex }` painted the Account
  sections under whichever panel was open. Both now write `display` only on
  `[open]` and `[hidden]`.
- **The panel's title is not on screen.** It is the dialog's accessible name and
  the place focus lands, and it duplicates the tab that is already selected — so
  the head shows the strip and the close button, and the title is clipped to a
  pixel. `aria-labelledby` still points at real text.
- **Closing hands focus to the door of the face that was showing.** One modal
  wears three faces, so the owner names the trigger on every `show()`: moving
  from Community to Bots while the panel stays open moves the return address
  with it, and a panel the *address* opened — with nothing focused — still has
  an icon to go back to.
- **Who owns the keyboard.** `⌘K` works over a panel: the palette is a dialog
  too, so it stacks above and closes back down onto it. The chart's window key
  handler does the opposite and goes silent while any dialog is open, because a
  `<dialog>` holds the pointer but lets the keydown bubble to the window — with
  no guard, an Escape pressed inside the panel cancelled a drawing on the chart
  behind it.
- **A phone's bar has 390px, not 430.** It drops `#signinToggle`, because sign
  out lives in the Account panel's Profile pane and the landing page has its own
  card; that is the room the two panel icons and the avatar needed. And a `.row`
  inside a panel wraps instead of pushing its last control past the edge, where
  a scroll would hide it rather than show it.

## 23. Amendment — the Indicators face, and the pictures on its cards (2026-10-29)

The concept library was the last surface the frame still gave a page to. It is
now the fourth face of `#workPanel`, and it is the reason the panel system was
worth building: 182 concepts across 17 families is a list worth opening over a
chart, and it is not worth a permanent column beside one.

### 23.1 A face, not a destination

`NAV` gained `{ id: "concepts", kind: "panel", label: "Indicators", width:
"wide", view: "charts" }` — so `#/charts/concepts[/<family>]`, Alt+6 counting
the table, a permanent icon in `#panelNav`, and a `/` that means the same thing
TradingView's `/` means.

`view: "charts"` is the part that is not cosmetic. The face draws a chart's
indicators, so opening it from the Dashboard routes to Charts first: an icon
whose panel would show nothing where the trader is standing is an icon that
lies. The same field makes the face a *projection* rather than a place — its
content is the active pane's instrument, timeframe and enabled set, and it
repaints when any of the three changes.

### 23.2 What the cards show

TradingView's indicator dialog and LuxAlgo's concept picker both list names and
let the trader add one to find out what it draws. Every card here answers before
being clicked:

- **A miniature of the pane's own last 96 bars**, drawn as candles with the
  concept's marks on them — the same timestamps the chart and `/indicator-series`
  both name a bar with, so the picture is a crop of the chart and not a sketch
  of one.
- **A tally in the corner of that picture**: how many times the concept has
  spoken on this chart. Twelve marks and no marks are different choices, and the
  number is read against the picture it counts. Zero is styled quiet, because
  nothing to say is an answer, not a failure.
- **A rail that counts each family `on/total`** and a coverage line that says
  `182 of 182 work on BTCUSDT · 1m`, so the size of the library and the state of
  this chart read at one glance.

Adding is TradingView-shaped: the card adds, the chip on the pane head names
what is live, and either end can dismiss it.

### 23.3 The economics of 182 previews

A card that draws needs data, and 182 cards asking at once is a request storm
over a chart that is trying to stream. So:

- The face reads `/concepts` once and `/concept-events/scan` once — that scan is
  shared by every event card, which is why the whole grid can be honest about
  this chart for the price of one call.
- `/indicator-series` is asked per concept, and only for a card that is actually
  on screen: an `IntersectionObserver` with a 160px lead, plus a synchronous
  check at render for the cards already visible before it fires.
- Four in flight (`CONCEPT_PREVIEW_LIMIT`), keyed `symbol|tf|id` in a cache, with
  a second map of in-flight promises so two cards for the same concept await one
  request. A job token drops a late answer whose card has moved on.

Measured over a full top-to-bottom sweep of the real catalog: **1 scan + 23
unique series requests for 137 revealed cards, zero duplicates**, and nothing
fetched for a card clipped out of the panel.

### 23.4 Verification

`tools/shell_check.mjs` covers the face's lazy economy in jsdom — the observer
is stubbed so a card can be declared on or off screen, and the checks assert that
nothing is requested while it is off. The gates are unchanged: shell, ui,
component and builder all pass.

### 23.5 What only a browser could tell us

Four defects, none visible to a harness, all found by running the page.

- **A card under the panel's fold is not on screen.** The first clip test used
  the window, and the panel scrolls inside its own body: every card in a
  19,000px grid had a rect inside `innerHeight`, so the observer's work was
  undone by a check that let everything through. `conceptPreviewViewport` now
  walks up to the scrolling ancestor and clips against *its* box.
- **The face can open before the chart has bars.** Every event preview drew the
  same bare baseline because the pane's window was empty and nothing said so.
  The view now carries OHLC, and `refresh()` dispatches `hrz:pane-bars` when the
  first batch lands — the previews wait for the chart, not for the next thing
  the trader happens to do.
- **One stale zone can squash a whole picture.** A BOS card drew its candles in
  a thin band at the bottom: a mark from before the crop's window stretched the
  price scale. Marks are now clamped to the crop's own range plus 35% padding.
- **A bare `aside` rule was holding the rail open.** `aside { width: var(--aside-w) }`
  — written for the inspector, where `#inspectorHost > aside` already outranks it
  — pinned the concepts rail to 360px inside a 216px grid track, painting the
  family counts over the first column of cards at every desktop width, and
  pinning the dashboard's watch card to 360px inside a 453px grid cell. Deleting
  it fixed all three. The tell was text that appeared to belong to no card.

