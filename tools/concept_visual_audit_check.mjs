// The gate's own test: one row that must pass, and the rows that must not.
//
// `complete` is built by spreading the *seeded* skeleton of `smc.bos` over the fields
// Task 3 leaves empty, so the fixture passes through the same shape `seed` writes. A
// hand-written row that only looks like the real one would let the gate pass a fixture
// and still fail the library.
//
// Run: node tools/concept_visual_audit_check.mjs
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { rowProblems, channelNames, ledgerDir } from "./concept_visual_audit.mjs";

let failures = 0;
const ok = (what) => console.log(`ok   ${what}`);
const fail = (what) => {
  failures += 1;
  console.log(`FAIL ${what}`);
};

const shard = JSON.parse(await readFile(join(ledgerDir, "smc.json"), "utf8"));
const seeded = (shard.rows || shard).find((row) => row.concept_id === "smc.bos");
if (!seeded) fail("the smc shard has no smc.bos row to build the fixture from");

const channels = await channelNames();
// Rule 7 is the only rule that touches the filesystem, and the render pages are
// gitignored working artifacts -- so the good row is told its own page exists, and no
// other path is. The rejection test then uses a path that could never exist.
const ctx = {
  channels,
  hasFile: (path) => path === seeded.render,
};

const complete = {
  ...seeded,
  kind: "event",
  reference: {
    source: "LuxAlgo Smart Money Concepts",
    url: "https://www.luxalgo.com/library/smart-money-concepts/",
    quote: "The break of structure line extends from the broken swing point to the right until price returns to it.",
    checked: "2026-10-29",
  },
  draw: {
    today: "A 1.3px stub and the word 'bos' floating over the break bar.",
    canonical: "A dashed level running right from the swing it broke, stopping where price returns.",
    verdict: "differs_major",
  },
  fires: {
    our_rule: "close beyond the last swing high on the closed bar.",
    their_rule: "a wick or close beyond the swing, measured from the swing high itself.",
    verdict: "different_rule",
  },
  rule_as_data: {
    form: "predicate",
    rule: "swing(high, i) && close[i] > swing(high, i) && status[i] == confirmed",
    inputs: [{ name: "candles", channel: "candles" }],
  },
  warmup: { bars: 22, before: "nothing draws; the detector needs 22 bars to name a swing." },
  target_shape: { name: "level_run", status: "new_rendering", scale: "price" },
  detector: "analytics_core::events::detect_structure",
  parameters: { declared: seeded.registry.parameters, honoured: [] },
  preview_agrees: "differs",
};

const mustPass = (row, label) => {
  const problems = rowProblems(row, ctx);
  if (problems.length) fail(`${label} must pass, but the gate said: ${problems.map((p) => p.message).join(" | ")}`);
  else ok(`${label} passes all the rules`);
};

const mustReject = (row, rule, label) => {
  const rules = rowProblems(row, ctx).map((p) => p.rule);
  if (!rules.includes(rule)) fail(`${label} must be rejected by rule ${rule} (got: ${rules.join(", ") || "nothing"})`);
  else ok(`${label} rejected by rule ${rule}`);
};

mustPass(complete, "the complete row");

// The five the plan names, then the two halves of rule 5 and the parameter gap.
mustReject({ ...complete, reference: undefined }, 3, "a row with no reference");
mustReject(
  { ...complete, kind: "event", fires: { ...complete.fires, verdict: "continuous" } },
  4,
  "an event that claims it fires continuously",
);
mustReject(
  { ...complete, kind: "series", registry: { ...complete.registry, pane: false, overlay: false } },
  5,
  "a series the registry says has no scale",
);
mustReject(
  { ...complete, kind: "series", registry: { ...complete.registry, pane: true, overlay: false },
    target_shape: { ...complete.target_shape, scale: "price" } },
  5,
  "a pane series priced on the chart's own scale",
);
mustReject(
  { ...complete, rule_as_data: { form: "predicate", rule: "x > y", inputs: [{ name: "book", channel: "order_book" }] } },
  6,
  "a rule that reads a channel DataChannels does not name",
);
// The spec cites the bit ("TRADES_AGGRESSOR, not 'the tape'") and the registry answers
// the name ("trades_aggressor"). Both spellings are the same channel.
mustPass(
  { ...complete, rule_as_data: { form: "predicate", rule: "signed volume accumulates", inputs: [{ name: "tape", channel: "TRADES_AGGRESSOR" }] } },
  "a rule that cites the bit name instead of the registry's spelling",
);
mustReject({ ...complete, render: "reports/concept-visual-audit/nope/nope.html" }, 7, "a row whose photograph is missing");
mustReject(
  { ...complete, parameters: { declared: ["made_up"], honoured: ["made_up"] } },
  9,
  "a row that declares a parameter the registry does not",
);

// A row left exactly as the seed wrote it is the state the whole gate exists to catch.
const seededProblems = rowProblems(seeded, ctx);
if (!seededProblems.length) fail("a row nobody has judged passed the gate");
else {
  const rules = new Set(seededProblems.map((p) => p.rule));
  for (const rule of [2, 3, 6, 8, 9]) {
    if (rules.has(rule)) ok(`the untouched seeded row is caught by rule ${rule}`);
    else fail(`the untouched seeded row slipped past rule ${rule}`);
  }
}

// The enum rules must refuse an invention, not only an empty.
mustReject({ ...complete, draw: { ...complete.draw, verdict: "close enough" } }, 2, "a verdict outside the ledger's vocabulary");
mustReject({ ...complete, kind: "pattern" }, 2, "a kind outside the ledger's vocabulary");
// and a house-style reference is a legal answer only when it says why.
mustPass({ ...complete, reference: { no_convention: true, reason: "No charting platform draws a Kalman filter's state estimate as a pattern; the convention is a numeric readout." } },
  "a reference that says there is no external convention");
mustReject({ ...complete, reference: { no_convention: true, reason: "" } }, 3, "a no-convention reference with no reason");

if (failures) {
  console.log(`\n${failures} fixture(s) failed.`);
  process.exit(1);
}
console.log(`\nall fixtures behave -- the gate takes the seeded row's shape and refuses every invention off it.`);
