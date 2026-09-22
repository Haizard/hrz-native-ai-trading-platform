#!/usr/bin/env node
/*
 * Exercises the visual builder's document model outside a browser.
 *
 * Three things are checked, and they are the three ways a form-driven editor
 * can be wrong:
 *
 *   1. every emitted document is written to `target/builder-check/` so
 *      `strategy-cli validate` can run the *real* validator over it. That is
 *      the check that matters -- a hand-written YAML emitter in JavaScript is
 *      exactly the kind of thing that looks right and parses wrong.
 *   2. emit -> parse -> emit is idempotent. Opening a document in the builder
 *      and applying it without touching anything must not change it, or a user
 *      who glances at the builder has silently rewritten their strategy.
 *   3. conditions round-trip through the parser, and anything the parser does
 *      not model degrades to raw text rather than to a different condition.
 *
 * Usage: node tools/check_builder.mjs [--out <dir>]
 */
import { createRequire } from "node:module";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const B = require("../frontend/app/builder.js");

function argValue(name) {
  const at = process.argv.indexOf(name);
  return at >= 0 ? process.argv[at + 1] : null;
}

const here = dirname(fileURLToPath(import.meta.url));
const outDir = argValue("--out") || join(here, "..", "target", "builder-check");

/*
 * Mirrors `GET /strategies/schema`. Only `stops`, `timeframes` and
 * `take_profit_types` change what gets emitted; the rest exists because the
 * shell reads them for its dropdowns. This fixture drifting from the server is
 * not the risk it looks like: the emitted files go through `strategy-cli`, so a
 * wrong stop parameter fails there, not silently.
 */
const SCHEMA = {
  /// The concept language, served by `GET /strategies/schema` and derived from
  /// `analytics_core::concepts` and `strategy_dsl::expr::ConceptPart` for the
  /// same reason the rest of this object is derived rather than hand-listed: a
  /// selector added in Rust has to appear in the builder's concept editor with
  /// no JS change, and the builder must not be able to offer a concept the
  /// validator then rejects.
  concepts: {
    parts: [
      { name: "exists", type: "bool", reads: "the concept found a band at all" },
      { name: "fresh", type: "bool", reads: "the newest band has not been traded back into" },
      { name: "mitigated", type: "number", reads: "how much of the newest band price has traded back through, 0.0..=1.0" },
      { name: "top", type: "number", reads: "the newest band's dearer edge" },
      { name: "bottom", type: "number", reads: "the newest band's cheaper edge" },
    ],
    selectors: ["open", "high", "low", "close", "mid", "volume"],
    ops: ["below", "above", "below_or_equal", "above_or_equal"],
    window: [2, 8],
    max_concepts: 5,
    sides: ["buy", "sell"],
  },
  stops: [
    { kind: "below_sweep_low", params: [], implies_direction: "long" },
    { kind: "above_sweep_high", params: [], implies_direction: "short" },
    { kind: "below_swing_low", params: [], implies_direction: "long" },
    { kind: "above_swing_high", params: [], implies_direction: "short" },
    { kind: "below_recent_low", params: ["bars"], implies_direction: "long" },
    { kind: "above_recent_high", params: ["bars"], implies_direction: "short" },
    { kind: "atr", params: ["multiple", "period"], implies_direction: null },
    { kind: "fixed", params: ["price"], implies_direction: null },
  ],
  timeframes: ["1m", "5m", "15m", "1h", "4h", "1d"],
  take_profit_types: ["risk_multiple", "atr_multiple", "fixed_price"],
  document_kinds: ["indicator", "strategy", "bot"],
  directions: ["long", "short"],
  operators: ["==", "!=", ">", ">=", "<", "<="],
  fields: [{ name: "close", type: "number", position_scoped: false }],
  funcs: [{ name: "above", arity: [2, 2], returns: "bool", param_types: ["number", "number"] }],
};

// ---------------------------------------------------------------------------
// Tiny assertion harness, matching the shape of wasm_abi_check.mjs
// ---------------------------------------------------------------------------

let passed = 0;
const failures = [];

function ok(name, condition, detail) {
  if (condition) {
    passed += 1;
    console.log(`ok   ${name}`);
  } else {
    failures.push(name);
    console.log(`FAIL ${name}${detail ? ` -- ${detail}` : ""}`);
  }
}

function eq(name, actual, expected) {
  ok(name, actual === expected, `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

// ---------------------------------------------------------------------------
// Conditions: parse them apart, put them back, get the same text
// ---------------------------------------------------------------------------

// Each of these is a real condition from a shipped document or from expr.rs's
// grammar. The property that matters is that the builder's rendering is the
// inverse of its parsing -- otherwise opening a document rewrites it.
const ROUND_TRIPS = [
  "close > vwap",
  "close >= liquidity.swept_level",
  "market_structure.trend == \"bullish\"",
  "liquidity.swept == \"sell_side\"",
  "delta > threshold(5)",
  "delta > threshold(0.25)",
  "close_below(stop_price)",
  "close_above(vwap)",
  "new_low(20)",
  "new_high()",
  "crosses_above(close, vwap)",
  "not in_position",
  "absorption.detected",
  "imbalance.stacked",
  "unrealized_r < -1",
  "close > vwap and delta > 0",
  "close > vwap or close < val",
  "close > vwap and delta > 0 and volume > 10",
];

for (const text of ROUND_TRIPS) {
  // The same path a document takes: a row built from the parsed condition,
  // rendered back out.
  const rendered = B.rowText(B.rowFromConditional({ timeframe: "entry", condition: text }));
  eq(`condition round-trips: ${text}`, rendered, text);
}

// Anything the parser does not model must come back as the same text, not as
// a guess. This is the graceful-degradation guarantee: a grammar the builder
// has not caught up with shows the condition as text.
const BEYOND_THE_BUILDER = [
  "not (close > vwap and delta > 0)", // a negated group, not a negated clause
  "1", // a bare literal is not a condition
  "close > vwap and (delta > 0 or volume > 10)", // mixed connectives
  "above(delta, threshold(5))", // a call inside a call
];

for (const text of BEYOND_THE_BUILDER) {
  const row = B.rowFromConditional({ timeframe: "entry", condition: text });
  // Whether or not the parser understood it, the text must survive.
  eq(`kept exactly: ${text}`, B.rowText(row), text);
  // And the builder must say it did *not* model it, rather than pretending.
  const form = B.emptyForm(SCHEMA);
  form.groups.entryAll = [row];
  eq(`shown as text, not modelled: ${text}`, B.rawRowCount(form), 1);
}

// ---------------------------------------------------------------------------
// The reference strategy, built from the form
// ---------------------------------------------------------------------------

function referenceForm() {
  const form = B.emptyForm(SCHEMA);
  form.name = "Liquidity Sweep + Reclaim (BTCUSDT 5m)";
  form.version = "1.0";
  form.market = "BTCUSDT";
  form.kind = "strategy";
  form.timeframes = [
    { name: "trend", tf: "4h" },
    { name: "entry", tf: "5m" },
  ];
  form.direction = "long";
  form.groups.entryAll = [
    {
      timeframe: "trend",
      label: "",
      joiner: "and",
      clauses: [
        {
          form: "compare",
          not: false,
          left: { kind: "field", value: "market_structure.trend" },
          op: "==",
          right: { kind: "string", value: "bullish" },
          tunable: false,
        },
      ],
    },
    {
      timeframe: "entry",
      label: "",
      joiner: "and",
      clauses: [
        {
          form: "compare",
          not: false,
          left: { kind: "field", value: "liquidity.swept" },
          op: "==",
          right: { kind: "string", value: "sell_side" },
          tunable: false,
        },
      ],
    },
    {
      timeframe: "entry",
      label: "",
      joiner: "and",
      clauses: [
        {
          form: "compare",
          not: false,
          left: { kind: "field", value: "close" },
          op: ">",
          right: { kind: "field", value: "liquidity.swept_level" },
          tunable: false,
        },
      ],
    },
    {
      timeframe: "entry",
      label: "",
      joiner: "and",
      clauses: [
        {
          form: "compare",
          not: false,
          left: { kind: "field", value: "close" },
          op: ">",
          right: { kind: "field", value: "vwap" },
          tunable: false,
        },
      ],
    },
    {
      timeframe: "entry",
      label: "",
      joiner: "and",
      clauses: [
        {
          form: "compare",
          not: false,
          left: { kind: "field", value: "delta" },
          op: ">",
          right: { kind: "number", value: "5" },
          tunable: true,
        },
      ],
    },
  ];
  form.groups.invalidation = [
    {
      timeframe: "entry",
      label: "",
      joiner: "and",
      clauses: [{ form: "call", not: false, func: "close_below", args: [{ kind: "field", value: "stop_price" }] }],
    },
  ];
  form.risk.maxRiskPct = "1.0";
  form.risk.stop = { kind: "below_sweep_low", params: {} };
  form.risk.hasTakeProfit = true;
  form.risk.takeProfit = { type: "risk_multiple", value: "2.5" };
  form.skillRef = "liquidity-sweep-reclaim-btcusdt-5m-v1";
  return form;
}

function atrForm() {
  const form = B.emptyForm(SCHEMA);
  form.name = "ATR stop, any-of entry";
  form.version = "2.1";
  form.market = "BTCUSDT";
  form.direction = "short";
  form.timeframes = [{ name: "entry", tf: "15m" }];
  form.groups.entryAny = [
    {
      timeframe: "entry",
      label: "the tape flipped",
      joiner: "or",
      clauses: [
        { form: "call", not: false, func: "new_low", args: [{ kind: "number", value: "20" }] },
        {
          form: "compare",
          not: true,
          left: { kind: "field", value: "absorption.bullish" },
          op: "",
          right: { kind: "number", value: "0" },
          tunable: false,
        },
      ],
    },
  ];
  form.groups.invalidation = [
    {
      timeframe: "entry",
      label: "",
      joiner: "and",
      clauses: [
        {
          form: "compare",
          not: false,
          left: { kind: "field", value: "unrealized_r" },
          op: "<",
          right: { kind: "number", value: "-1" },
          tunable: false,
        },
      ],
    },
  ];
  form.risk.maxRiskPct = "0.5";
  form.risk.stop = { kind: "atr", params: { multiple: "1.5", period: "14" } };
  form.risk.hasTakeProfit = false;
  return form;
}

function indicatorForm() {
  const form = B.emptyForm(SCHEMA);
  form.name = "VWAP bands";
  form.version = "0.1";
  form.kind = "indicator";
  form.market = "BTCUSDT";
  form.timeframes = [{ name: "entry", tf: "5m" }];
  form.description = "an indicator has no trade logic";
  return form;
}

const FIXTURES = [
  { file: "liquidity-sweep.yaml", form: referenceForm() },
  { file: "atr-stop.yaml", form: atrForm() },
  { file: "indicator.yaml", form: indicatorForm() },
];

mkdirSync(outDir, { recursive: true });

for (const fixture of FIXTURES) {
  const issues = B.formIssues(fixture.form, SCHEMA);
  ok(`${fixture.file}: the form is complete`, issues.length === 0, issues.join("; "));

  const doc = B.documentFromForm(fixture.form, SCHEMA);
  const yaml = B.toYaml(doc);
  writeFileSync(join(outDir, fixture.file), yaml);

  // The idempotence property: open it back up, apply it untouched, and the
  // document must be byte-identical.
  const reopened = B.formFromDocument(doc, SCHEMA);
  const again = B.toYaml(B.documentFromForm(reopened, SCHEMA));
  eq(`${fixture.file}: emit -> parse -> emit is identical`, again, yaml);

  // Nothing was silently downgraded to raw text on the way through.
  eq(`${fixture.file}: every condition was modelled`, B.rawRowCount(reopened), 0);
}

// ---------------------------------------------------------------------------
// Emitting details that would otherwise only fail in a browser
// ---------------------------------------------------------------------------

const yaml = B.toYaml(B.documentFromForm(referenceForm(), SCHEMA));

ok("a numeric-looking version is quoted so it stays a string", yaml.includes('version: "1.0"'), yaml);
ok("a bare identifier is not quoted", yaml.includes("kind: strategy"), yaml);
ok("timeframe values are quoted, or '5m' would read as a string anyway but '1h' as a sexagesimal",
  yaml.includes('entry: "5m"'), yaml);
ok("a condition with a quoted string stays plain", yaml.includes('condition: market_structure.trend == "bullish"'), yaml);
ok("threshold() is written where the user asked for a tunable number", yaml.includes("delta > threshold(5)"), yaml);
ok("the stop rule is a bare string when it takes no parameters", yaml.includes("stop: below_sweep_low"), yaml);
ok("the builder marks what it produced", /\n {2}created_by: visual_builder\n/.test(yaml), yaml);

// A market name that would start a YAML mapping must be quoted.
const tricky = referenceForm();
tricky.market = "BTC:USDT";
ok("a market containing ': ' is quoted", B.toYaml(B.documentFromForm(tricky, SCHEMA)).includes('market: "BTC:USDT"'));

// An empty group is omitted, which is what the schema's own serializer does.
const noExit = referenceForm();
ok("an empty exit block is omitted", !B.toYaml(B.documentFromForm(noExit, SCHEMA)).includes("exit:"));

// An indicator carries no trade logic at all.
const indicatorYaml = B.toYaml(B.documentFromForm(indicatorForm(), SCHEMA));
// "entry:" also appears as a timeframe *name*, so these match at the top level.
ok("an indicator has no entry block", !/\nentry:/.test(indicatorYaml), indicatorYaml);
ok("an indicator has no risk block", !/\nrisk:/.test(indicatorYaml), indicatorYaml);

// ---------------------------------------------------------------------------
// Concepts: a measurement a client defines, declared as data.
// ---------------------------------------------------------------------------

function conceptForm() {
  const form = B.emptyForm(SCHEMA);
  form.name = "Fair value gap / reclaim";
  form.version = "1.0";
  form.market = "BTCUSDT";
  form.kind = "strategy";
  form.timeframes = [{ name: "entry", tf: "5m" }];
  form.direction = "long";
  form.groups.entryAll = [
    {
      timeframe: "entry",
      label: "",
      joiner: "and",
      clauses: [{ form: "compare", not: false, left: { kind: "field", value: "close" }, op: ">", right: { kind: "field", value: "vwap" }, tunable: false }],
    },
  ];
  form.groups.invalidation = [
    {
      timeframe: "entry",
      label: "",
      joiner: "and",
      clauses: [{ form: "call", not: false, func: "close_below", args: [{ kind: "field", value: "stop_price" }] }],
    },
  ];
  form.risk.maxRiskPct = "1.0";
  form.risk.stop = { kind: "below_sweep_low", params: {} };
  form.risk.hasTakeProfit = true;
  form.risk.takeProfit = { type: "risk_multiple", value: "2.0" };
  form.concepts = {
    max: B.conceptMaxFromSchema(SCHEMA),
    items: [
      {
        name: "fvg",
        label: "fvg",
        side: "buy",
        window: "3",
        lower: { selector: "high", index: "0" },
        upper: { selector: "low", index: "2" },
        require: [
          { left: { selector: "high", index: "0" }, op: "below", right: { selector: "low", index: "2" } },
        ],
        min_band_ratio: "0.2",
      },
    ],
  };
  return form;
}

function conceptWithRawSelector() {
  const form = B.emptyForm(SCHEMA);
  form.name = "Concept with a selector shape the builder does not model";
  form.version = "0.1";
  form.market = "BTCUSDT";
  form.kind = "indicator";
  form.timeframes = [{ name: "entry", tf: "15m" }];
  form.concepts = {
    max: B.conceptMaxFromSchema(SCHEMA),
    items: [
      {
        // A future vocabulary might add a labeled or free-form edge. Until it
        // does, the builder keeps this as raw text and the validator is the
        // word on whether it is accepted.
        raw: "{high: 0}",
      },
    ],
  };
  return form;
}

const CONCEPT_FIXTURES = [
  { file: "fvg-concept.yaml", form: conceptForm() },
  { file: "raw-concept.yaml", form: conceptWithRawSelector() },
];

for (const fixture of CONCEPT_FIXTURES) {
  const issues = B.documentIssues(fixture.form, SCHEMA);
  ok(`${fixture.file}: the form is complete`, issues.length === 0, issues.join("; "));

  const doc = B.documentFromConceptForm(fixture.form, SCHEMA);
  const yaml = B.toYaml(doc);
  writeFileSync(join(outDir, fixture.file), yaml);

  // The idempotence property, now including concepts: open it back up, apply
  // it untouched, and the document must be byte-identical.
  const reopened = B.formFromDocument(doc, SCHEMA);
  const again = B.toYaml(B.documentFromConceptForm(reopened, SCHEMA));
  eq(`${fixture.file}: emit -> parse -> emit is identical`, again, yaml);

  // Nothing a concept declared was silently downgraded to raw text on the way
  // through -- unless the fixture started that way.
  if (!fixture.form.concepts.items[0] || !fixture.form.concepts.items[0].raw) {
    eq(`${fixture.file}: every concept was modelled`, B.rawRowCount(reopened), 0);
  }

  // And reopening a concept that the builder can model must yield the same
  // value, so the form the user sees is the document the server receives.
  const first = B.conceptValue(fixture.form.concepts.items[0]);
  const reopenedFirst = B.conceptValue(reopened.concepts.items[0]);
  eq(
    `${fixture.file}: concept round-trips as data`,
    JSON.stringify(first),
    JSON.stringify(reopenedFirst),
  );
}

// A concept the builder cannot model must survive as the same text, not as a
// guess -- the same graceful-degradation guarantee the condition parser already
// provides. The YAML the server receives is the text the user wrote.
const rawDoc = B.documentFromConceptForm(conceptWithRawSelector(), SCHEMA);
ok(
  "a concept with an unmodelled selector is kept as raw text",
  B.toYaml(rawDoc).includes("{high: 0}"),
  B.toYaml(rawDoc),
);

// A concept whose name is not an identifier is refused locally, before a
// round trip to the server.
const badName = conceptForm();
badName.concepts.items[0].name = "Fair Value Gap";
ok(
  "a concept with a non-identifier name is refused",
  B.documentIssues(badName, SCHEMA).some((i) => i.includes("name")),
  B.documentIssues(badName, SCHEMA).join("; "),
);

// A concept whose window is out of range is refused.
const badWindow = conceptForm();
badWindow.concepts.items[0].window = "1";
ok(
  "a concept with an out-of-range window is refused",
  B.documentIssues(badWindow, SCHEMA).some((i) => i.includes("window")),
  B.documentIssues(badWindow, SCHEMA).join("; "),
);

// A concept whose cheaper edge is a volume selector is refused -- band edges
// are prices, and a volume would not mean what it looks like it means.
const volumeEdge = conceptForm();
volumeEdge.concepts.items[0].lower = { selector: "volume", index: "0" };
ok(
  "a concept with a volume cheaper edge is refused",
  B.documentIssues(volumeEdge, SCHEMA).some((i) => i.includes("price")),
  B.documentIssues(volumeEdge, SCHEMA).join("; "),
);

// A requirement that compares a price with a volume is refused.
const mismatchedReq = conceptForm();
mismatchedReq.concepts.items[0].require = [
  { left: { selector: "high", index: "0" }, op: "below", right: { selector: "volume", index: "2" } },
];
ok(
  "a requirement comparing price with volume is refused",
  B.documentIssues(mismatchedReq, SCHEMA).some((i) => i.includes("different kinds")),
  B.documentIssues(mismatchedReq, SCHEMA).join("; "),
);

// Two concepts with the same name are refused.
const dupName = conceptForm();
dupName.concepts.items.push({ name: "fvg", side: "buy", window: "3" });
ok(
  "two concepts with the same name are refused",
  B.documentIssues(dupName, SCHEMA).some((i) => i.includes("twice")),
  B.documentIssues(dupName, SCHEMA).join("; "),
);

// ---------------------------------------------------------------------------
// What the form refuses, before a round trip to the server
// ---------------------------------------------------------------------------

const blank = B.emptyForm(SCHEMA);
ok("an empty form is refused", B.formIssues(blank, SCHEMA).length > 0);

const noName = referenceForm();
noName.name = "  ";
ok("an empty name is refused", B.formIssues(noName, SCHEMA).some((i) => i.includes("name")));

const badRisk = referenceForm();
badRisk.risk.maxRiskPct = "one percent";
ok("a non-numeric risk is refused", B.formIssues(badRisk, SCHEMA).some((i) => i.includes("number")));

const noStopParam = referenceForm();
noStopParam.risk.stop = { kind: "atr", params: { multiple: "1.5" } };
ok("a missing stop parameter is refused",
  B.formIssues(noStopParam, SCHEMA).some((i) => i.includes("period")));

const noInvalidation = referenceForm();
noInvalidation.groups.invalidation = [];
ok("a strategy with no invalidation is refused",
  B.formIssues(noInvalidation, SCHEMA).some((i) => i.includes("invalidation")));

const undeclared = referenceForm();
undeclared.groups.entryAll[0].timeframe = "nope";
ok("a condition on an undeclared timeframe is refused",
  B.formIssues(undeclared, SCHEMA).some((i) => i.includes("not declared")));

const duplicate = referenceForm();
duplicate.timeframes = [{ name: "entry", tf: "5m" }, { name: "entry", tf: "1h" }];
ok("two timeframes with the same name are refused",
  B.formIssues(duplicate, SCHEMA).some((i) => i.includes("twice")));

// ---------------------------------------------------------------------------

console.log(`\nwrote ${FIXTURES.length} document(s) to ${outDir}`);
if (failures.length) {
  console.log(`${passed} passed, ${failures.length} FAILED`);
  process.exit(1);
}
console.log(`all checks passed (${passed})`);
