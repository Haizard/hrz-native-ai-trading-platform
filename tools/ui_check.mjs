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

// jsdom 30 ships no `<dialog>` behaviour: `showModal` and `close` are missing,
// so a modal component cannot be driven at all. What this file can test is the
// logic the panel is built on -- which host is showing, what Escape asks the
// owner for, and where focus lands -- so the two methods are given the one
// thing that logic reads: the `open` property and the `close` event. The top
// layer, the scrim and the focus trap are the browser's, and are checked by
// hand against the running gateway.
function dialogShim(el) {
  el.open = false;
  el.showModal = () => { el.open = true; };
  el.close = () => {
    if (!el.open) return;
    el.open = false;
    el.dispatchEvent(new window.Event("close"));
  };
  return el;
}

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

/* ---- Panel --------------------------------------------------------------- */
const panelRoot = dialogShim(document.createElement("dialog"));
panelRoot.className = "hrz-dialog hrz-dialog--panel";
panelRoot.innerHTML = [
  '<header class="hrz-dialog__head">',
  '<h2 class="hrz-dialog__title"></h2>',
  '<div class="hrz-tabs" role="tablist">',
  '<button type="button" class="hrz-tab" role="tab" data-tab="community">Community</button>',
  '<button type="button" class="hrz-tab" role="tab" data-tab="bots">Bots</button>',
  "</div>",
  '<button type="button" class="hrz-dialog__close" aria-label="Close the panel"></button>',
  "</header>",
  '<div class="hrz-dialog__body">',
  '<div data-body="community"></div>',
  '<div data-body="bots"></div>',
  "</div>",
].join("");
document.body.appendChild(panelRoot);

const asks = [];
const picks = [];
const panel = HRZUI.panel(panelRoot, {
  onRequestClose: (name) => asks.push(`close:${name}`),
  onSelect: (name) => picks.push(name),
});
check("the same dialog gets the same panel", HRZUI.panel(panelRoot) === panel);

const opener = document.createElement("button");
document.body.appendChild(opener);
opener.focus();

panel.show("community", { title: "Community", width: "wide" });
check("showing a panel opens the dialog", panelRoot.open === true);
check("showing a panel titles it",
  panelRoot.querySelector(".hrz-dialog__title").textContent === "Community");
check("showing a panel sizes it", panelRoot.dataset.width === "wide");
check("showing a panel leaves one host on screen",
  panelRoot.querySelector('[data-body="community"]').hidden === false &&
    panelRoot.querySelector('[data-body="bots"]').hidden === true);
check("showing a panel marks its head tab",
  panelRoot.querySelector('[data-tab="community"]').getAttribute("aria-selected") === "true");
check("the panel reports what it is showing",
  panel.name() === "community" && panel.isOpen() === true);

panelRoot.querySelector('[data-tab="bots"]').click();
check("a head tab asks the owner instead of moving itself",
  picks.join() === "bots" && panelRoot.querySelector('[data-body="bots"]').hidden === true);
panel.show("bots", { title: "Bots", width: "medium" });
check("the owner's answer is what moves the panel",
  panelRoot.querySelector('[data-body="bots"]').hidden === false &&
    panelRoot.querySelector('[data-body="community"]').hidden === true);

const asksBefore = asks.length;
panelRoot.querySelector(".hrz-dialog__close").click();
check("the close button asks rather than closes",
  asks.length === asksBefore + 1 && panelRoot.open === true);
const esc = new window.Event("cancel", { cancelable: true });
panelRoot.dispatchEvent(esc);
check("Escape asks rather than closes",
  asks.length === asksBefore + 2 && panelRoot.open === true);
check("Escape is prevented, so the address cannot be left behind", esc.defaultPrevented);
check("asking names the panel the user wants gone", asks.at(-1) === "close:bots");
panelRoot.dispatchEvent(
  new window.MouseEvent("click", { bubbles: true, clientX: 9999, clientY: 9999 }),
);
check("a click past the edge asks too", asks.length === asksBefore + 3);

panel.hide();
check("hiding closes the dialog", panelRoot.open === false && panel.name() === null);
check("focus returns to the button that opened it", document.activeElement === opener);

console.log(failures === 0 ? "\nPASS" : `\n${failures} FAILED`);
process.exit(failures === 0 ? 0 : 1);
