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
  "hrz-dialog--panel", "hrz-dialog__head", "hrz-dialog__close",
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

// Only a `var()` with *no fallback* can break visibly, so only those must be
// declared. `var(--x, 0px)` is how the component layer takes runtime values
// (a menu's position, a meter's fill) that no stylesheet could name in
// advance -- requiring those to be tokens would be a false alarm.
const declared = new Set([...(tokens + css).matchAll(/--([\w-]+)\s*:/g)].map((m) => m[1]));
const unresolved = [...new Set([...css.matchAll(/var\(--([\w-]+)\s*\)/g)].map((m) => m[1]))].filter(
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
