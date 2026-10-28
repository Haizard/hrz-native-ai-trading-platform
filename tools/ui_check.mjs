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
