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
import { dirname, join } from "node:path";

const require = createRequire(import.meta.url);
const Taxonomy = require("../frontend/app/concept_taxonomy.js");

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const gateway = process.env.HRZ_GATEWAY ?? "http://127.0.0.1:8080";

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

if (subcommand === "taxonomy") {
  await taxonomy();
} else {
  console.error(`unknown subcommand "${subcommand ?? ""}" -- expected: taxonomy`);
  process.exit(2);
}

export { EXPECTED, Taxonomy, root, gateway, concepts };
