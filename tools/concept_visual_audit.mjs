#!/usr/bin/env node
/*
 * The concept visual fidelity audit's driver.
 *
 * The spec it serves (`docs/superpowers/specs/2026-10-29-concept-visual-fidelity-audit-design.md`)
 * asks for a photograph of what our engine draws today, per concept, next to a cited
 * description of what the wider world draws. So this tool holds **no rendering rules of
 * its own**: the shape routing and the painting come from `concept_shapes.js`, the
 * family table from `concept_taxonomy.js`, and both are the bytes the page loads. A tool
 * that re-implemented either would photograph the tool instead of the chart, which is
 * the one way this audit could be wrong and still look tidy.
 *
 * Subcommands:
 *   taxonomy  -- the twelve-pass table against the live registry (no browser, no ledger)
 *   seed      -- write one ledger shard per family, every row at its seeded value
 *   render    -- one page per concept, built by the real wasm from real events
 *   check    -- the completeness gate over the shards
 *
 * Usage: node tools/concept_visual_audit.mjs <subcommand> [family] [--symbol S --timeframe T --limit N]
 */
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import { mkdir, readFile, writeFile } from "node:fs/promises";

const require = createRequire(import.meta.url);
const Taxonomy = require("../frontend/app/concept_taxonomy.js");

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const gateway = process.env.HRZ_GATEWAY ?? "http://127.0.0.1:8080";
const ledgerDir = join(root, "reports/concept-visual-audit/ledger");

/// The spec's twelve-pass table, copied verbatim.
///
/// It lives here rather than in prose because a family count that moves without the
/// spec moving with it means the audit is disagreeing with itself -- and the passes are
/// scheduled by these numbers. `more` must be empty: a concept the panel cannot name is
/// a gap in the taxonomy, and the pass that notices it says so instead of inventing a
/// family for the run.
const EXPECTED = {
  smc: 32, sd: 4, pa: 11, session: 14, fib: 6, chart: 15, harmonics: 9,
  candles: 34, "volume-profile": 9, "order-flow": 1, trend: 12, momentum: 10,
  volatility: 4, volume: 5, statistical: 10, econ: 6, more: 0,
};

async function concepts() {
  const response = await fetch(`${gateway}/concepts`);
  if (!response.ok) throw new Error(`GET /concepts -> ${response.status}; is the gateway up on ${gateway}?`);
  return response.json();
}

async function taxonomy() {
  const entries = await concepts();
  const counts = new Map(Object.keys(EXPECTED).map((id) => [id, 0]));
  const unexpected = [];
  for (const entry of entries) {
    const family = Taxonomy.familyOf(entry.id);
    if (!counts.has(family)) unexpected.push(`${entry.id} -> ${family}`);
    else counts.set(family, counts.get(family) + 1);
  }
  const drift = Object.entries(EXPECTED).filter(([id, want]) => counts.get(id) !== want);
  console.log(`${entries.length} concepts across ${counts.size} families`);
  for (const [id, want] of Object.entries(EXPECTED)) {
    const got = counts.get(id);
    if (got !== want) console.log(`  ${id}: expected ${want}, the registry says ${got}`);
  }
  for (const line of unexpected) console.log(`  unmapped ${line}`);
  if (drift.length || unexpected.length) {
    console.error("FAIL the taxonomy and the audit's pass table disagree");
    process.exit(1);
  }
  console.log("ok   every concept lands in the family the audit will write it under");
}

const [, , subcommand, ...rest] = process.argv;

/// The row as the registry seeds it: everything the platform already knows, filled
/// from the live answer, and everything a pass must decide left at its empty value.
///
/// The empty fields are not a placeholder to be embarrassed about -- they are what
/// `check` fails on, which is how the ledger stays complete as the registry grows. A
/// concept registered tomorrow appears here as a row nobody has judged, so
/// completeness cannot rot.
function seedRow(entry) {
  const family = Taxonomy.familyOf(entry.id);
  return {
    concept_id: entry.id,
    family,
    version: entry.version,
    registry: {
      pane: entry.pane,
      overlay: entry.overlay,
      channels: entry.required_channels,
      parameters: (entry.parameters || []).map((p) => p.name),
    },
    kind: "",
    reference: { source: "", url: "", quote: "", checked: "" },
    draw: { today: "", canonical: "", verdict: "" },
    fires: { our_rule: "", their_rule: "", verdict: "" },
    rule_as_data: { form: "predicate", rule: "", inputs: [] },
    warmup: { bars: 0, before: "" },
    target_shape: { name: "", status: "wiring", scale: "price" },
    detector: "",
    render: `reports/concept-visual-audit/${family}/${entry.id}.html`,
    preview_agrees: "",
  };
}

async function seed() {
  const entries = await concepts();
  const want = Object.values(EXPECTED).reduce((n, c) => n + c, 0);
  if (entries.length !== want) {
    console.error(`FAIL the registry answers ${entries.length} concepts; the pass table sums to ${want}`);
    process.exit(1);
  }

  const shards = new Map();
  for (const entry of entries) {
    const family = Taxonomy.familyOf(entry.id);
    if (!shards.has(family)) shards.set(family, []);
    shards.get(family).push(seedRow(entry));
  }

  for (const [family, rows] of Object.entries(EXPECTED)) {
    const got = (shards.get(family) || []).length;
    if (got !== rows) console.log(`  ${family}: the pass table says ${rows}, the registry says ${got}`);
  }
  const unmapped = [...shards.keys()].filter((f) => !(f in EXPECTED));
  if (unmapped.length) {
    for (const family of unmapped) console.log(`  unmapped family ${family}: ${shards.get(family).length} concept(s)`);
    console.error("FAIL the taxonomy grew a family the audit has no pass for");
    process.exit(1);
  }
  if (shards.has("more")) {
    console.error(`FAIL ${shards.get("more").length} concept(s) the panel cannot name: ` +
      shards.get("more").map((r) => r.concept_id).join(", "));
    process.exit(1);
  }

  await mkdir(ledgerDir, { recursive: true });
  const force = rest.includes("--force");
  let written = 0;
  let kept = 0;
  for (const [family, rows] of shards) {
    const path = join(ledgerDir, `${family}.json`);
    const next = JSON.stringify({ family, rows }, null, 2) + "\n";
    let existing = null;
    try {
      existing = await readFile(path, "utf8");
    } catch {
      /* the common case: no shard yet */
    }
    if (existing !== null && existing !== next) {
      // A pass writes its verdicts into this file. Re-seeding over them would be the
      // quietest way to lose twelve passes of research, so it takes an explicit order.
      if (!force) {
        console.error(`FAIL ${family}.json holds work the seed did not write. ` +
          `Re-seed with --force only if you mean to throw those rows away.`);
        process.exit(1);
      }
      console.log(`  ${family}: re-seeded over a shard that had been written into (--force)`);
    }
    if (existing === next) {
      kept += 1;
      continue;
    }
    await writeFile(path, next);
    written += 1;
  }
  console.log(`${entries.length} rows across ${shards.size} shard(s): ${written} written, ${kept} already exact`);
  console.log("ok   every registry concept has a row, and every judgement in it is empty for `check` to fail on");
}

const here = fileURLToPath(import.meta.url);
// Task 5's fixture imports `seedRow` and `EXPECTED` from this file, and a top-level
// dispatch would answer it with "unknown subcommand" and `process.exit(2)`. Run the
// CLI only when this file is the one Node was pointed at.
const invokedDirectly = process.argv[1] ? resolve(process.argv[1]) === here : false;

if (invokedDirectly) {
  if (subcommand === "taxonomy") {
    await taxonomy();
  } else if (subcommand === "seed") {
    await seed();
  } else {
    console.error(`unknown subcommand "${subcommand ?? ""}" -- expected: taxonomy | seed | render | check`);
    process.exit(2);
  }
}

export { EXPECTED, Taxonomy, root, gateway, concepts, ledgerDir, seedRow };
