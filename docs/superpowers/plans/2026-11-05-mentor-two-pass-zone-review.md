# Mentor two-pass zone review — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** make a mentor run look at the chart it just marked — six plain frames, the analysis, six frames carrying the marks, then a confirmed plan — and render the plan as three TradingView-style zones instead of a stripe that covers the whole plot.

**Architecture:** `TradeThesis`'s three scalars become three `PriceZone`s (entry / stop / target), so grounding checks six prices instead of three and the shell draws three thin per-leg bands rather than one full-width rectangle. The engine gains a `zone` drawing part with the border/fill/label/price-tag treatment the reference shows. A new `review_marked_charts` tool emits six `capture_chart` commands for the marked second pass; the orchestrator collects them with a round-robin poll, and a duty (refused once, then recorded as a miss in the thesis trace) forces the confirmation to come from the marked frames.

**Tech stack:** Rust (`ai-agent`, `chart-engine` cdylib → wasm, `api-gateway`), vanilla-JS shell (`frontend/app/app.js`), Node harnesses under `tools/`.

**Spec:** [docs/superpowers/specs/2026-11-05-mentor-two-pass-zone-review-design.md](../specs/2026-11-05-mentor-two-pass-zone-review-design.md) — the four phases, the zone rules, the failure table and the eight product-owner decisions. This plan executes them; it does not re-litigate them.

## Global constraints

- Work directly on `main` (the local gateway at :8080 serves this working tree). No worktree.
- Do **not** run `cargo fmt --all` — the repo is not rustfmt-clean and it rewrites 100+ unrelated files.
- Do **not** run `tools/guard_check.py` mid-session (it mutates `frontend/app/app.js`).
- Every Node gate runs with `NODE_PATH="/Users/macbook/.workbuddy-node/node_modules"`: `tools/shell_check.mjs`, `tools/wasm_abi_check.mjs frontend/app/chart_engine.wasm`, `tools/ui_check.mjs`, `tools/component_check.mjs`, `tools/check_builder.mjs`, `tools/concept_visual_audit_check.mjs`.
- The wasm is checked in. After any Rust change to `chart-engine`: `cargo run -p xtask -- build-frontend` (copies to `frontend/app/chart_engine.wasm`), then re-run the wasm-dependent gates, and commit the rebuilt wasm with the change that needed it.
- One commit per task. Never push — the product owner pushes.
- Do not restart the gateway or run live acceptance unless the owner asks; Rust changes need a rebuild+restart to show, shell files are disk-served.
- Known pre-existing failure, out of scope, do not "fix": `cargo test -p api-gateway --test load_flow` (`stopping_one_bot_leaves_the_feed_working_for_the_others`, free-plan 1-bot cap → 402).
- Zone rendering must match the cited TradingView/LuxAlgo convention (docs/31 border principle, docs/46 zones doctrine): quiet, clean, one-line explainable. No colour literal outside `tokens.css`.
- The strategy/paper/backtest `entry_price` fields (strategy-dsl, pine_codegen, db/paper, trading-engine, backtester, strategy-runtime, pine-lite) are a **different** `entry_price`. Never touch them.

## File map

| File | Responsibility |
| --- | --- |
| `crates/ai-agent/src/thesis.rs` | `PriceZone` (low/high + normalise + describe), the three zone fields, `finalize`'s six grounding checks and strict wrong-side rule, `zone_schema`/`parse_zone`, `PRICE_KEYS` |
| `crates/ai-agent/src/agent.rs` | the capture relay (`CaptureRequest`), round-robin `poll_captures`, per-run budgets, the two duties, the mentor four-phase prompt |
| `crates/ai-agent/src/tools.rs` | `review_marked_charts` spec + tool, `next_capture_id()` |
| `crates/ai-agent/src/multi_timeframe.rs` | `MARKED_LADDER` — the six rungs pass two photographs |
| `crates/ai-agent/src/progress.rs` | `ReviewFrame.marked` — the discriminator the shell captions and gates cards on |
| `crates/ai-agent/src/agent_memory.rs` | stored prose reads zone midpoints |
| `crates/ai-agent/src/lib.rs` | re-export `PriceZone` |
| `frontend/chart-engine/src/drawing.rs` | `DrawingPart::Zone` |
| `frontend/chart-engine/src/draw/mod.rs` | `fresh_edge()`, the `parts()` dispatch, `assert_finite` |
| `frontend/chart-engine/src/draw/{shapes,measurement}.rs` | `rect` and `price_band` emit a `zone`; price tags move to the right edge |
| `frontend/app/app.js` | `zoneOf`, `thesisOverlays` (three bands), `thesisHtml`, `case "zone"`, hit-test, marked capture shows the AI layer |
| `tools/shell_check.mjs` | the geometry recorder (`painted.geom`), three-band contract, zone rendering, marked caption |
| `tools/wasm_abi_check.mjs` | `zone` part shape over the ABI |
| `tools/agent-cli/src/main.rs` | prints zone ranges |
| `docs/09-AI-AGENT-SYSTEM.md`, `docs/14-FRONTEND-CHART-ENGINE.md` | the struct block and the coordinate-mapping sentence |

## Task order

Tasks 1-2 make the plan three zones and keep those zones grounded; 3 renders a
zone as a zone; 4-7 build the marked pass; 8 makes looking at it a condition of
submitting; 9 tells the model the shape of the lesson. Tasks 1 and 3 each merge
the Rust/engine side and the shell side in one commit, because
`tools/shell_check.mjs` drives the real wasm and the running app reads what the
agent writes -- splitting either pair would leave a window where the harness
proves nothing and the chart shows `undefined`.

---
## Task 1: The thesis carries zones, and the plan is three bands

The rename moves the Rust model, the tool schema, the Rust consumers, the shell
reader and the docs in one commit. It cannot be split: the shell reads what Rust
writes, so a commit that changes only one side leaves the running app's thesis
card showing `undefined` or three zero-height bands. The engine's zone
*treatment* is Task 3; this task only stops painting a stripe over the plot.

**Files:** modify `crates/ai-agent/src/thesis.rs` (`:152-156` fields, `:218-311`
`finalize`, `:319-371` narrative, `:526-659` schema+parse, `:721-880` tests),
`crates/ai-agent/src/agent.rs` (`:3377-3379` `synthesize_stand_aside`),
`crates/ai-agent/src/agent_memory.rs` (`:240-256` prose, `:342` assertion,
`:369-371` test JSON), `crates/ai-agent/src/lib.rs:89-91`,
`tools/agent-cli/src/main.rs:467-470`, `frontend/app/app.js`
(`thesisOverlays` `:10439-10461`, `thesisHtml` `:9923-9950`),
`tools/shell_check.mjs` (`:159-188` recorder, `:5032-5186` answer section),
`docs/09-AI-AGENT-SYSTEM.md:96-114`, `docs/14-FRONTEND-CHART-ENGINE.md:855`.

**Interfaces:**
- Consumes: `PriceRange::check(&str, f64) -> Result<(), AgentError>` (unchanged
  signature), `Overlay{price,label,role,band_to,filled}` (unchanged).
- Produces: `pub struct PriceZone { pub low: f64, pub high: f64 }` with
  `PriceZone::new(f64, f64)` (orders the edges), `PriceZone::point(f64)`,
  `midpoint()`, `height()`, `is_point()`, `describe() -> String`,
  `Default` = `point(0.0)`; `TradeThesis.entry/stop/target: PriceZone`;
  `pub const MIN_ZONE_FRACTION: f64 = 0.05;`; `submit_thesis` properties
  `entry`/`stop`/`target` accepting a number **or** `{low, high}`; shell
  `zoneOf(value)` / `zoneText(value)`.

- [ ] **Step 1: Add `PriceZone` above `TradeThesis` in `thesis.rs`, and the new
  floor next to `MIN_RISK_FRACTION` (`:46`).**

```rust
/// Smallest zone height as a fraction of the entry-to-stop distance. A band
/// thinner than this is a single price wearing a zone's clothes, and the
/// honest way to say one price is one price (`low == high`).
pub const MIN_ZONE_FRACTION: f64 = 0.05;

/// A price *area*: the way a trader actually marks a level.
///
/// `low == high` is legal and means exactly one price, so nothing that today
/// reports a single number has to invent a second edge to keep working. The
/// edges are normalised by `new`, so a model that answers `{"low": 103_200,
/// "high": 102_800}` describes the same band as the other way round.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PriceZone {
    /// Lower edge.
    pub low: f64,
    /// Upper edge.
    pub high: f64,
}

impl PriceZone {
    #[must_use]
    pub fn new(low: f64, high: f64) -> Self {
        Self {
            low: low.min(high),
            high: low.max(high),
        }
    }

    /// One price, wearing no band.
    #[must_use]
    pub const fn point(value: f64) -> Self {
        Self {
            low: value,
            high: value,
        }
    }

    /// The centre, which is what the arithmetic uses: an R computed from the
    /// far edge of a fat entry zone would flatter the trade.
    #[must_use]
    pub fn midpoint(&self) -> f64 {
        (self.low + self.high) / 2.0
    }

    #[must_use]
    pub fn height(&self) -> f64 {
        self.high - self.low
    }

    #[must_use]
    pub fn is_point(&self) -> bool {
        self.low == self.high
    }

    /// One number for a one-price zone, a range otherwise. Every prose
    /// consumer (narrative, memory, CLI, card) goes through this so the three
    /// of them cannot disagree about what the plan was.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.is_point() {
            format!("{:.4}", self.low)
        } else {
            format!("{:.4} -> {:.4}", self.low, self.high)
        }
    }
}

impl Default for PriceZone {
    /// The "no levels" a `Bias::None` thesis carries. `finalize` zeroes to
    /// this, so a consumer that reads the legs without checking the direction
    /// sees nothing usable rather than a band around zero.
    fn default() -> Self {
        Self::point(0.0)
    }
}
```

- [ ] **Step 2: Replace the three fields (`:151-156`).**

```rust
    /// Planned entry area.
    pub entry: PriceZone,
    /// Planned stop area -- the invalidation *level*, which is why the
    /// narrative keeps the `invalidation` sentence for the why.
    pub stop: PriceZone,
    /// Planned target area.
    pub target: PriceZone,
```

- [ ] **Step 3: Rewrite `finalize`'s level checks.** Keep the confidence clamp
  and the `Bias::None` early return (zeroing the three zones with
  `PriceZone::default()`); replace everything between those and the grounding
  loop, and the grounding loop itself, with:

```rust
        // Normalise once, here, so every rule below can read the legs as
        // ordered and `low <= high` stops being a check.
        self.entry = PriceZone::new(self.entry.low, self.entry.high);
        self.stop = PriceZone::new(self.stop.low, self.stop.high);
        self.target = PriceZone::new(self.target.low, self.target.high);

        for (name, zone) in [
            ("entry", self.entry),
            ("stop", self.stop),
            ("target", self.target),
        ] {
            for (edge, value) in [("low", zone.low), ("high", zone.high)] {
                if !value.is_finite() || value <= 0.0 {
                    return Err(AgentError::Ungrounded(format!(
                        "{name}.{edge} is {value}, which is not a usable price"
                    )));
                }
            }
        }

        // Structural sanity, now stated as non-overlap: a stop that reaches
        // into the entry area is not a stop, it is the entry. `stop.high <=
        // entry.low` says "entirely under it", which also refuses a stop
        // parked above a long's entry without a second branch.
        {
            let long = matches!(self.direction, Bias::Long);
            let stop_clear = if long {
                self.stop.high <= self.entry.low
            } else {
                self.stop.low >= self.entry.high
            };
            let target_clear = if long {
                self.target.low >= self.entry.high
            } else {
                self.target.high <= self.entry.low
            };
            if !stop_clear {
                return Err(AgentError::Ungrounded(format!(
                    "{:?} thesis has entry {} and stop {}: the stop is on the wrong side",
                    self.direction,
                    self.entry.describe(),
                    self.stop.describe()
                )));
            }
            if !target_clear {
                return Err(AgentError::Ungrounded(format!(
                    "{:?} thesis has entry {} and target {}: the target is on the wrong side",
                    self.direction,
                    self.entry.describe(),
                    self.target.describe()
                )));
            }

            // Midpoint to midpoint: the size of the trade is the distance
            // between the two levels as the trader means them, not between
            // whichever edges happen to be closest.
            let risk = if long {
                self.entry.midpoint() - self.stop.midpoint()
            } else {
                self.stop.midpoint() - self.entry.midpoint()
            };
            let reward = if long {
                self.target.midpoint() - self.entry.midpoint()
            } else {
                self.entry.midpoint() - self.target.midpoint()
            };

            let min_risk = self.entry.midpoint() * MIN_RISK_FRACTION;
            if risk < min_risk {
                return Err(AgentError::Ungrounded(format!(
                    "{:?} thesis risks {} between entry {} and stop {}, which is under \
                     the {} minimum ({:.4}): the stop may sit on a level, but it must sit \
                     beyond it with room",
                    self.direction,
                    risk,
                    self.entry.describe(),
                    self.stop.describe(),
                    MIN_RISK_FRACTION,
                    min_risk
                )));
            }

            // A zone thinner than 5% of the risk is decoration. Refusing it
            // (rather than silently widening) is what keeps the marks on the
            // chart the ones the trade was actually taken at.
            for (name, zone) in [
                ("entry", self.entry),
                ("stop", self.stop),
                ("target", self.target),
            ] {
                if zone.is_point() {
                    continue;
                }
                let min_height = risk * MIN_ZONE_FRACTION;
                if zone.height() < min_height {
                    return Err(AgentError::Ungrounded(format!(
                        "{name} spans {} (height {:.4}), thinner than {:.4}, which is {} of \
                         the {:.4} entry-to-stop distance: to price one level exactly, give \
                         the same number for both edges",
                        zone.describe(),
                        zone.height(),
                        min_height,
                        MIN_ZONE_FRACTION,
                        risk
                    )));
                }
            }

            self.risk_reward = reward / risk;
        }

        if !self.risk_reward.is_finite() {
            self.risk_reward = 0.0;
        }

        // Grounding: every edge is a claim about the data, so every edge is
        // checked. Three prices became six numbers, and a fat zone whose far
        // edge is invented is exactly as ungrounded as an invented price was.
        for (name, zone) in [
            ("entry", self.entry),
            ("stop", self.stop),
            ("target", self.target),
        ] {
            observed.check(&format!("{name}.low"), zone.low)?;
            observed.check(&format!("{name}.high"), zone.high)?;
        }

        Ok(())
```

- [ ] **Step 4: Narrative (`:331-334`) becomes**

```rust
            out.push_str(&format!(
                "Entry {}, stop {}, target {} -> {:.2}R.\n",
                self.entry.describe(),
                self.stop.describe(),
                self.target.describe(),
                self.risk_reward
            ));
```

- [ ] **Step 5: Schema and parser.** Replace the three `"*_price": {"type":
  "number"}` properties in `check_schema` with
  `"entry": zone_schema(), "stop": zone_schema(), "target": zone_schema(),` and
  the required list with `"entry", "stop", "target", "invalidation",`. Add,
  beside `check_array_schema` (which supplies the file's array-type idiom):

```rust
/// One leg of the plan.
///
/// A bare number is legal and means both edges. The alternative -- forcing an
/// object -- would make a model that has decided "stop at 102000.00" invent a
/// second price to satisfy syntax, which is how invented levels get into a
/// thesis. `finalize` is where a wrong number is refused.
fn zone_schema() -> Value {
    json!({
        "description": "A price area as {\"low\": .., \"high\": ..}. A bare number is \
            accepted and means one price for both edges. Copy the numbers verbatim \
            from tool output.",
        "type": ["number", "object"],
        "properties": {
            "low": {"type": "number"},
            "high": {"type": "number"},
        },
        "required": ["low", "high"],
    })
}
```

and, beside `parse_thesis`'s `number` closure:

```rust
    let zone = |key: &str| -> Result<PriceZone, AgentError> {
        parse_zone(args.get(key)).ok_or_else(|| AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: format!(
                "`{key}` is required and must be a number or a {{\"low\": .., \"high\": ..}} pair"
            ),
        })
    };
```

replacing the three `number("*_price")?` calls with `zone("entry")?`,
`zone("stop")?`, `zone("target")?`, plus:

```rust
/// Read one leg in either accepted shape.
///
/// Tolerant for the same reason as `parse_status`, which already bends for a
/// bare `"passed": true`: the wire is the model's, and a shape slip should not
/// discard an otherwise checkable thesis.
fn parse_zone(value: Option<&Value>) -> Option<PriceZone> {
    match value? {
        Value::Number(number) => number.as_f64().map(PriceZone::point),
        Value::Object(map) => {
            let low = map.get("low").and_then(Value::as_f64)?;
            let high = map.get("high").and_then(Value::as_f64)?;
            Some(PriceZone::new(low, high))
        }
        _ => None,
    }
}
```

- [ ] **Step 6: Export and fix the remaining Rust consumers.** Add `PriceZone`
  to the two `pub use thesis::` lines in `lib.rs:89-91`. In `agent.rs`,
  `synthesize_stand_aside` (`:3377-3379`) becomes
  `entry: PriceZone::default(), stop: PriceZone::default(), target:
  PriceZone::default(),` (import it from `crate::thesis`). In
  `agent_memory.rs:248-250` the prose arguments become
  `thesis.entry.describe(), thesis.stop.describe(), thesis.target.describe()`
  and the format string's three `{:.4}` become `{}`. In `tools/agent-cli`,
  `:467-470` becomes

```rust
    println!(
        "\nentry {}  stop {}  target {}  R:R {:.2}",
        t.entry.describe(),
        t.stop.describe(),
        t.target.describe(),
        t.risk_reward
    );
```

- [ ] **Step 7: Run it and expect the compile errors to name the test helper.**
  `cargo test -p ai-agent thesis` → the `thesis()` helper (`:741-743`) still
  assigns `entry_price`. That is the failing state Step 8 fixes.

- [ ] **Step 8: Update the tests in place.** `thesis(direction, entry, stop,
  target)` keeps its three-f64 signature and builds point zones -- the helpers
  and every existing expectation stay readable, and the point case is the one
  the rename must not break:

```rust
            entry: PriceZone::point(entry),
            stop: PriceZone::point(stop),
            target: PriceZone::point(target),
```

Then: `:850` `assert_eq!(t.entry_price, 0.0)` → `assert_eq!(t.entry,
PriceZone::default());`; `:861` `contains("entry_price")` →
`assert!(err.to_string().contains("entry.low"), "got {err}");` (the message
must name the *edge*, because that is what a zone makes checkable);
`the_submit_thesis_schema_requires_the_thesis_fields` (`:1040-1059`) required
list renames to `["symbol", "timeframe", "direction", "confidence_pct", "entry",
"stop", "target", "invalidation", "higher_timeframe_checks",
"order_flow_checks"]`; `agent_memory.rs:342` assertion →
`.contains(&thesis.entry.describe())` and its JSON keys at `:369-371` become
`"entry": 104_000.0, "stop": 102_500.0, "target": 108_000.0` (bare numbers,
which is the tolerant path proving itself end to end).

- [ ] **Step 9: Add the zone tests** to `thesis.rs`'s `mod tests`.

```rust
    #[test]
    fn a_zone_leg_is_stored_with_both_edges_and_scored_from_its_midpoint() {
        let mut t = thesis(Bias::Long, 103000.0, 102000.0, 106000.0);
        t.entry = PriceZone::new(102800.0, 103200.0);
        t.finalize(&observed()).unwrap();
        assert_eq!(t.entry, PriceZone::new(102800.0, 103200.0));
        // midpoint 103000 -- the same trade as the single price above.
        assert!((t.risk_reward - 3.0).abs() < 1e-9, "got {}", t.risk_reward);
    }

    #[test]
    fn reversed_edges_describe_the_same_band() {
        assert_eq!(
            PriceZone::new(103_200.0, 102_800.0),
            PriceZone::new(102_800.0, 103_200.0)
        );
    }

    #[test]
    fn a_stop_that_reaches_into_the_entry_area_is_not_a_stop() {
        let mut t = thesis(Bias::Long, 103000.0, 102000.0, 106000.0);
        t.entry = PriceZone::new(102800.0, 103200.0);
        t.stop = PriceZone::new(102900.0, 103100.0);
        let err = t.finalize(&observed()).unwrap_err();
        assert!(err.to_string().contains("wrong side"), "got {err}");
    }

    #[test]
    fn a_zone_thinner_than_the_floor_is_refused_toward_a_honest_point() {
        let mut t = thesis(Bias::Long, 103000.0, 102000.0, 106000.0);
        // 2 wide against 1000 of risk: a price with a decoration on it.
        t.entry = PriceZone::new(102999.0, 103001.0);
        let err = t.finalize(&observed()).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("same number for both edges"), "got {text}");

        // The exact answer the message invites is accepted.
        let mut exact = thesis(Bias::Long, 103000.0, 102000.0, 106000.0);
        exact.finalize(&observed()).unwrap();
    }

    #[test]
    fn every_edge_is_grounded() {
        let mut t = thesis(Bias::Long, 103000.0, 102000.0, 106000.0);
        // The near edge is plausible; the far edge is from another chart.
        t.entry = PriceZone::new(102800.0, 250000.0);
        let err = t.finalize(&observed()).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("entry.high"), "got {text}");
        assert!(text.contains("250000"), "got {text}");
    }

    #[test]
    fn a_bare_number_from_the_model_is_one_price_not_a_rejection() {
        // Old-shaped arguments (and a model that simply prefers one number)
        // must still parse: the shape tolerance exists so that `finalize` is
        // the only place a thesis can be refused.
        let t = parse_thesis(&json!({
            "symbol": "BTCUSDT", "timeframe": "5m", "direction": "long",
            "confidence_pct": 60.0,
            "entry": 103000.0, "stop": 102000.0, "target": 106000.0,
            "invalidation": "x",
            "higher_timeframe_checks": [], "order_flow_checks": [],
        }))
        .expect("a number is a leg");
        assert!(t.entry.is_point());
        assert_eq!(t.entry.low, 103000.0);
    }

    #[test]
    fn a_zone_pair_parses() {
        let t = parse_thesis(&json!({
            "symbol": "BTCUSDT", "timeframe": "5m", "direction": "long",
            "confidence_pct": 60.0,
            "entry": {"low": 102800.0, "high": 103200.0},
            "stop": 102000.0, "target": 106000.0,
            "invalidation": "x",
            "higher_timeframe_checks": [], "order_flow_checks": [],
        }))
        .expect("a pair is a leg");
        assert_eq!(t.entry.height(), 400.0);
    }
```

- [ ] **Step 10: `cargo test -p ai-agent` then `cargo test --workspace`** → all
  green (except the known `load_flow` free-plan failure). Then
  `cargo clippy -p ai-agent --all-targets -- -D warnings`.

- [ ] **Step 11: The shell reader, in the same commit.** In `app.js`, add beside
  `thesisOverlays` (`:10439`):

```js
// Both shapes on the wire: {low, high} from a current run, a bare number from a
// stored row written before a leg was an area (docs/superpowers/specs
// /2026-11-05-mentor-two-pass-zone-review-design.md §1). Chat history is a
// record of what was said, so the reader bends rather than a migration
// rewriting it.
function zoneOf(value) {
  if (typeof value === "number") {
    return Number.isFinite(value) ? { low: value, high: value } : null;
  }
  if (value && typeof value === "object") {
    const low = Number(value.low ?? value.high);
    const high = Number(value.high ?? value.low);
    if (Number.isFinite(low) && Number.isFinite(high)) {
      return { low: Math.min(low, high), high: Math.max(low, high) };
    }
  }
  return null;
}

function zoneText(value) {
  const zone = zoneOf(value);
  if (!zone) return "—";
  return zone.high > zone.low
    ? `${zone.low.toFixed(2)} → ${zone.high.toFixed(2)}`
    : zone.low.toFixed(2);
}
```

and replace `thesisOverlays` with three per-leg bands -- no band spans
entry→target any more, which is the complaint:

```js
function thesisOverlays(thesis) {
  const overlays = [];
  for (const [role, value] of [
    ["entry", thesis.entry ?? thesis.entry_price],
    ["stop", thesis.stop ?? thesis.stop_price],
    ["target", thesis.target ?? thesis.target_price],
  ]) {
    const zone = zoneOf(value);
    if (!zone || !(zone.low > 0) || !Number.isFinite(zone.high)) continue;
    const isArea = zone.high > zone.low;
    overlays.push({
      price: zone.high,
      label: role,
      role,
      // A one-price leg is a line, not a band: `drawOverlays` only fills when
      // `band_to` arrives, so a point leg keeps its rule-level honesty.
      band_to: isArea ? zone.low : null,
      filled: isArea,
    });
  }
  return overlays;
}
```

Read `drawOverlays` (`:3418-3465`) before wiring this: it derives `top`/`height`
from `price` and `band_to`, and if it assumes `price` is the upper edge already,
keep that assumption rather than changing it. In `thesisHtml` (`:9938-9940`) the
three `<dd>` values become `${zoneText(t.entry ?? t.entry_price)}` and
friends.

- [ ] **Step 12: Give the harness geometry, then assert three bands.** The
  recorder at `shell_check.mjs:159-188` stores op *names*, which cannot express
  "thin". Add to the Proxy's `get` handler, next to the `fillText` push:

```js
      // The geometry too, not only the op name: "three thin bands rather than
      // one stripe over the plot" is a claim about numbers, and an op list
      // cannot make it.
      if (prop === "fillRect" || prop === "strokeRect") {
        painted.geom.push([prop, ...args.map((a) => (typeof a === "number" ? Math.round(a * 100) / 100 : a))]);
      }
```

and reset it where `painted` is cleared. In the agent-answer section
(`:5032-5186`), change the fake thesis at `:5084-5087` to
`entry: { low: 99.6, high: 100.4 }, stop: { low: 98.4, high: 99.0 }, target:
{ low: 101.5, high: 102.5 }` and replace the `banded.length === 1` check
(`:5173-5178`) with:

```js
// A plan band is full-width by construction, so width picks the bands out of
// every rectangle the shell filled; the height budget is the actual claim --
// three legs, each one its own thin area, none of them the entry→target span.
const bands = painted.geom
  .filter(([op, , , w, h]) => op === "fillRect" && w > VIEW.width * 0.5 && h > 0)
  .map(([, , , w, h]) => ({ w, h }));
check(
  "the plan arrives as three thin bands, not one stripe over the plot",
  bands.length === 3 && bands.every((band) => band.h < VIEW.height / 5),
  `${bands.length} bands, heights ${bands.map((band) => band.h).join(", ")}`
);
```

and keep the existing labels check (`:5134-5140`) reading
`["stop 99.00", "entry 100.40", "target 102.50"]` -- the label sits on the
leg's upper edge, which is the other half of the same contract.

Run `NODE_PATH=/Users/macbook/.workbuddy-node/node_modules node
tools/shell_check.mjs` → green. If the width filter or the height budget does
not match what `drawOverlays` actually fills, read that function and set the
predicate to what a full-width band *is* here -- do not loosen the assertion
until it has failed for the right reason.

- [ ] **Step 13: Docs.** `docs/09-AI-AGENT-SYSTEM.md:96-114`: the struct block's
  three `pub entry_price: f64,` lines become the `PriceZone` fields, and add one
  sentence: legs are areas, `low == high` means one price, `risk_reward` comes
  from midpoints, and every edge is grounded. `docs/14-FRONTEND-CHART-ENGINE.md:855`:
  the `entry_price`/timestamps sentence becomes "the legs' prices/timestamps".
  `docs/13-DATABASE-SCHEMA.md:102-104` is **not** a thesis consumer -- those
  `entry_price`/`stop_price`/`target_price` columns belong to the `trades` table,
  which is written by the execution path, not by `submit_thesis`. Do not rename
  anything there; a schema change would need a migration and is out of scope.

- [ ] **Step 14: Commit** `feat: the thesis plans in price zones, and the chart shows three bands`.
---

## Task 2: Grounding learns what a zone edge is

`PRICE_KEYS` (`thesis.rs:400-424`) is the list of tool-output keys whose values
count as citable prices. It has never included `top` or `bottom` -- the names
`detect_zones` actually returns (`tools.rs:2867-2868`, `:2882-2883`) -- so a zone
edge the tool genuinely reported was not a price the model could cite. Task 1's
six grounding checks are aspirational until this list grows.

**Files:** modify `crates/ai-agent/src/thesis.rs` (`:399-424` and the tests).

**Interfaces:**
- Consumes: `PriceRange::observe_result` / `walk` (unchanged).
- Produces: nothing new; `observed()`-shaped ranges get wider because zone edges
  now feed them.

- [ ] **Step 1: Write the failing test** in `thesis.rs`'s `mod tests`, next to
  `observed()`:

```rust
    #[test]
    fn a_reported_zone_edge_is_a_citable_price() {
        // `detect_zones` answers with "top"/"bottom", and a thesis level taken
        // straight from one used to fail grounding because the folding list did
        // not know those keys held prices -- which is how single invented
        // prices crept in to "simplify" a reported zone.
        let mut range = PriceRange::new();
        range.observe_result(&json!({
            "zones": [{ "kind": "demand", "top": 103400.0, "bottom": 102900.0 }],
        }));
        assert!(!range.is_empty(), "a zone edge must feed the range");
        assert_eq!(range.min(), Some(102900.0));
        assert_eq!(range.max(), Some(103400.0));

        let mut t = thesis(Bias::Long, 103150.0, 102000.0, 106000.0);
        t.entry = PriceZone::new(102900.0, 103400.0);
        t.finalize(&range).expect("the edges came from the tool");
    }
```

- [ ] **Step 2: Run it and watch it fail** the way that proves the point:
  `cargo test -p ai-agent a_reported_zone_edge` → "a zone edge must feed the
  range". (An empty range accepts everything, so the `finalize` line would pass
  by accident -- that is why the assertion is on the range, not only the thesis.)
- [ ] **Step 3: Grow the list.** In `PRICE_KEYS`, after `"price_level",`, add:

```rust
    // Zone edges, under the names the tools actually emit: `detect_zones` and
    // the box/region rows answer with `top`/`bottom`, `price_low`/`price_high`.
    // A thesis leg must be citable from the reported area, not only from the
    // midpoint someone chose to quote.
    "top",
    "bottom",
    "price_low",
    "price_high",
```

- [ ] **Step 4: `cargo test -p ai-agent`** → green. Read any grounding test that
  now passes differently and fix the *expectation* only if the new range is
  correct; `observed()` at `:760-767` uses no zone keys, so the existing band
  `[81200, 130625]` is unchanged and `a_level_nowhere_near_the_data_is_rejected`
  still fires.
- [ ] **Step 5: Commit** `fix: a zone edge the tools reported is a citable price`

---

## Task 3: A zone is drawn as a zone

Today a persisted `rect` is filled at `globalAlpha = 0.1` with a `strokeRect`
(`app.js:3329-3336`) -- the smear `docs/31` names, and not what the XAUUSD
reference shows. The reference's four features are a bordered box, a solid
sub-band on the untested part, a name, and edge price tags.

The engine emitter and the shell renderer land in **one commit**:
`tools/shell_check.mjs` drives the real wasm (its fake engine wraps
`real.build_scene`, `:1436-1502`), so a `Zone` part with no shell case is a
harness that proves nothing, and a shell case with no emitter is dead code the
completeness gate cannot see.

**Files:** modify `frontend/chart-engine/src/drawing.rs` (after `:1780`),
`frontend/chart-engine/src/draw/mod.rs` (`:120` area, `:412-437`),
`frontend/chart-engine/src/draw/shapes.rs:22-34`,
`frontend/chart-engine/src/draw/measurement.rs:117-149`,
`frontend/app/app.js:3329-3336` (new case) and `:4928-4942` (hit-test),
`tools/shell_check.mjs`, `tools/wasm_abi_check.mjs`, `frontend/app/chart_engine.wasm`.

**Interfaces:**
- Consumes: `Frame::{y_at, x_at_ms, plot, bar_nanos}` (`scene.rs:5490-5550`,
  `pub(crate)`), `Geometry::{bars_between, x, y, price, text, candles}`,
  `super::ms`.
- Produces: `DrawingPart::Zone { x, y, w, h, fresh_from: Option<f64>,
  top_label: String, bottom_label: String }`, serialising as
  `{"shape":"zone","x":…,"y":…,"w":…,"h":…,"fresh_from":…,"top_label":"…",
  "bottom_label":"…"}`; `pub(super) fn fresh_edge(geometry, from_ms: f64,
  to_ms: f64, price_hi: f64, price_lo: f64) -> Option<f64>`; the `zoneTag` key in
  both chart palettes.

- [ ] **Step 1: The variant.** In `drawing.rs`, after the `Polygon` arm (`:1775-1780`):

```rust
    /// A price zone: a band of prices that means something.
    ///
    /// `Rect` is a shape; a zone is a *level with a height*, and the treatment
    /// `docs/31` and `docs/46` ask for is four things a `Rect` cannot carry -- a
    /// border with its own weight, a solid sub-band for the part price has not
    /// traded through yet, and the two edges as numbers. The engine supplies the
    /// geometry and the formatted labels, because the price scale lives here:
    /// a shell that printed its own edge prices would be a second scale, which
    /// is the bug `drawThesis` used to have.
    Zone {
        /// Left edge.
        x: f64,
        /// Top edge -- the **higher** price's y, since canvas y runs down.
        y: f64,
        /// Width.
        w: f64,
        /// Height.
        h: f64,
        /// Where the untested part of the band starts. `None` means nothing in
        /// this span has traded into the zone, so all of it is untested and the
        /// shell fills from `x`. Left of it is the faded, spent portion.
        fresh_from: Option<f64>,
        /// The upper edge, formatted. Ready to draw, like every other string
        /// the engine emits.
        top_label: String,
        /// The lower edge, formatted.
        bottom_label: String,
    },
```

- [ ] **Step 2: `fresh_edge` in `draw/mod.rs`, after `ms` (`:120-122`).**

```rust
/// Where the untested part of a price band begins, in canvas x.
///
/// The last bar that traded into the band, plus one bar: what a zone has
/// already given back is everything to the left of that, which is the shape of
/// a supply or demand area in every reference this project cites. `None` is not
/// "unknown" -- it means no bar touched the band, so the whole band is fresh
/// and the caller fills from its left edge.
///
/// Takes the span in milliseconds rather than as two points because both
/// callers have a time range and neither has it in the shape the other uses: a
/// `rect` has its two anchors, a full-width band has the window.
///
/// Reads [`Geometry::candles`] (the traded series, not the drawn one) for the
/// same reason every other measurement here does: switching the chart to
/// Heikin-Ashi must not move a level.
pub(super) fn fresh_edge(
    geometry: Geometry<'_>,
    from_ms: f64,
    to_ms: f64,
    price_hi: f64,
    price_lo: f64,
) -> Option<f64> {
    let last_test = geometry
        .bars_between((from_ms, 0.0), (to_ms, 0.0))
        .iter()
        .rev()
        .find(|candle| candle.high >= price_lo && candle.low <= price_hi)?;
    let bar_width_ms = geometry.frame.bar_nanos as f64 / 1_000_000.0;
    let plot = geometry.frame.plot;
    // Clamped to the plot: a zone dragged to the right edge has no untested
    // part left to show, and a sub-band drawn past the plot is a paint bug.
    Some(
        geometry
            .frame
            .x_at_ms(ms(last_test) + bar_width_ms)
            .min(plot.x + plot.w)
            .max(plot.x),
    )
}
```

- [ ] **Step 3: `shapes::rect` emits a zone** (`shapes.rs:22-34`) -- `rect` is the
  zone tool per `docs/46`, so the box the user or the agent drags across a level
  gets the level's treatment. `triangle`, `circle`, `arc` and `rotated_rect` keep
  their parts; only the two price-band kinds change.

```rust
#[must_use]
pub fn rect(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let Some(box_) = corners(geometry) else {
        return Vec::new();
    };
    let price_hi = geometry.price(0).max(geometry.price(1));
    let price_lo = geometry.price(0).min(geometry.price(1));
    let mut parts = vec![DrawingPart::Zone {
        x: box_.0,
        y: box_.1,
        w: box_.2,
        h: box_.3,
        fresh_from: fresh_edge(geometry, geometry.points[0].0, geometry.points[1].0, price_hi, price_lo),
        top_label: format!("{price_hi:.2}"),
        bottom_label: format!("{price_lo:.2}"),
    }];
    // The name goes where the reference puts it: beside the box, not inside it.
    if let Some(label) = geometry.text {
        parts.push(DrawingPart::Text {
            x: box_.0 + 4.0,
            y: box_.1 - 4.0,
            text: label.to_string(),
        });
    }
    parts
}
```

and bring it in: `use super::{fresh_edge, Geometry};` (the file's import line is
`:15`). Add the doc-comment line "A `rect` is a zone: `docs/46`'s doctrine, and
the reason the part is a `Zone` and not a `Rect`." to the function's comment.

- [ ] **Step 4: `measurement::price_band` emits the same part** and its two left
  labels go away -- the tags are in the part now, on the right, where the price
  scale is read (`measurement.rs:117-149`):

```rust
/// A horizontal band between two price levels, spanning the full plot width.
///
/// The price band is the zone itself, and it is drawn as one: the same `Zone`
/// part the rectangle gets, across the whole plot. Its edge prices used to be
/// two `Text`s at the left; they are the part's own tags now, at the right,
/// because that is where a price is read.
#[must_use]
pub fn price_band(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let y0 = geometry.y(0);
    let y1 = geometry.y(1);
    let price_lo = geometry.price(0).min(geometry.price(1));
    let price_hi = geometry.price(0).max(geometry.price(1));
    // The band spans the window, so the span to test is the window: `from` and
    // `to` are nanoseconds and `fresh_edge` counts in the milliseconds a
    // drawing carries, which is the unit seam `ms` exists for.
    let left_ms = (geometry.frame.from / 1_000_000) as f64;
    let right_ms = (geometry.frame.to / 1_000_000) as f64;
    vec![DrawingPart::Zone {
        x: plot.x,
        y: y0.min(y1),
        w: plot.w,
        h: (y1 - y0).abs(),
        fresh_from: fresh_edge(geometry, left_ms, right_ms, price_hi, price_lo),
        top_label: format!("{price_hi:.2}"),
        bottom_label: format!("{price_lo:.2}"),
    }]
}
```

`dateprice_range` (`:72-110`) and `time_band` (`:153+`) stay `Rect` -- they are
measurements, not levels.

`positions::risk_box` (`draw/positions.rs:23`, reached from
`draw/mod.rs:364-365` for `position_long`/`position_short`) is **not** touched by
this task. It is a position overlay -- entry line, reward box, risk box -- not a
level, so it keeps its own `Rect` + `Text` parts. Its existing tests must stay
green exactly as written; if one of them fails, the `match` in `parts` or the
`assert_finite` arm broke something that did not need changing.

- [ ] **Step 5: The completeness gate learns the variant.** `assert_finite`
  (`draw/mod.rs:412-437`) matches every part; add:

```rust
        DrawingPart::Zone { x, y, w, h, .. } => {
            for value in [x, y, w, h] {
                assert!(value.is_finite(), "{kind:?} emitted a non-finite zone edge: {value}");
            }
        }
```

- [ ] **Step 6: Engine tests.** Add to `draw/mod.rs`'s test module (it already
  has `Stage::ramp` / `flat` / `over` / `geometry`):

```rust
    #[test]
    fn a_rect_is_a_zone_that_carries_its_own_edge_prices_and_a_name() {
        // 20 M5 bars closing 100.00 -> 119.00, so the band 101..103 is traded
        // through: bar 3 (close 103, low 102.75) is the last touch inside the
        // box, and everything right of it is what price has not tested yet.
        let stage = Stage::ramp(20, 100.0, 1.0);
        let options = crate::drawing::DrawingOptions::default();
        let geometry = crate::draw::Geometry {
            text: Some("demand"),
            ..stage.geometry(
                &[(stage.ms(2), 103.0), (stage.ms(10), 101.0)],
                &options,
            )
        };
        let parts = parts(DrawingKind::Rect, false, geometry);
        let zone = parts
            .iter()
            .find(|part| matches!(part, DrawingPart::Zone { .. }))
            .expect("a rect draws a zone");
        let DrawingPart::Zone {
            top_label,
            bottom_label,
            fresh_from,
            y,
            h,
            ..
        } = zone
        else {
            unreachable!()
        };
        assert_eq!(top_label, "103.00");
        assert_eq!(bottom_label, "101.00");
        assert!(fresh_from.is_some(), "a tested zone has a spent portion");
        assert!(h > 0.0);
        // Canvas y runs down, so the *higher* price is the smaller y. A zone
        // whose top is its low edge is a band drawn one price off.
        assert!(*y < stage.frame.y_at(101.0));
        assert!(
            parts.iter().any(|part| matches!(part, DrawingPart::Text { text, .. } if text == "demand")),
            "the label is drawn beside the box, not lost"
        );
    }

    #[test]
    fn a_band_the_series_never_touched_is_fresh_from_its_left_edge() {
        // flat(10, 100.0) trades 99.75..100.25 under a 0..200 window, so a band
        // at 120..130 has nothing tested in it. `None` is the answer, and the
        // shell reads it as "fill the whole thing solid".
        let stage = Stage::flat(10, 100.0);
        let options = crate::drawing::DrawingOptions::default();
        let parts = parts(
            DrawingKind::Rect,
            false,
            stage.geometry(&[(stage.ms(1), 130.0), (stage.ms(8), 120.0)], &options),
        );
        let DrawingPart::Zone { fresh_from, .. } = &parts[0] else {
            panic!("expected a zone, got {:?}", parts[0]);
        };
        assert!(fresh_from.is_none());
    }

    #[test]
    fn a_price_range_is_the_same_zone_part_as_a_rect() {
        // Two kinds, one treatment: a supply/demand area drawn as a full-width
        // band must not read differently from the same area dragged as a box.
        let stage = Stage::ramp(20, 100.0, 1.0);
        let options = crate::drawing::DrawingOptions::default();
        let parts = parts(
            DrawingKind::PriceRange,
            false,
            stage.geometry(&[(stage.ms(2), 103.0), (stage.ms(10), 101.0)], &options),
        );
        assert!(
            parts.iter().any(|part| matches!(part, DrawingPart::Zone { .. })),
            "price_range still draws a Rect: {parts:?}"
        );
        assert!(
            !parts.iter().any(|part| matches!(part, DrawingPart::Text { .. })),
            "the left-edge labels are the part's tags now"
        );
    }
```

`Stage::geometry` returns a `Geometry` with `text: None`, which is why the first
test rebuilds it with `..` (all five fields are `pub`). `DrawingKind::PriceRange`
is the `price_range` kind, `Rect` the `rect` kind -- both already in the
dispatch at `:307` and `:354`, so Step 3 and Step 4 change their emitters and
nothing routes them.

- [ ] **Step 7: `cargo test -p chart-engine`** → green (the exhaustive `match` in
  `assert_finite` and in the shell's dispatch is what makes an unhandled variant
  a compile error, not a runtime surprise). Then rebuild and commit the wasm:

```bash
cargo run -p xtask -- build-frontend
```

- [ ] **Step 8: The shell renders it.** First add one key to **both** palettes
  (`app.js:587` and `CHART_PALETTE_LIGHT`), beside `poc`, because the tag's text
  has to be the colour of the field the zone sits on and that is a palette fact,
  not a call-site literal:

```js
  // The plate a zone's price tag writes on: the canvas field, so the number
  // reads as the axis reads and not as a sticker over the candles.
  zoneTag: "#0a0d12",
```

and the light palette's `"#f6f8fa"` -- the two values `tokens.css:19` and
`:111` give `--bg-canvas`. Then, immediately after the `case "rect"` block
(`:3336`):

```js
          case "zone": {
            // The border is the zone and the fill is its shading -- docs/31's
            // point, and the difference between a level and a smear. Only the
            // colour's alpha changes, so no hue leaves the palette.
            const spent = part.fresh_from ?? part.x;
            ctx.globalAlpha = drawing.selected ? 0.2 : 0.1;
            ctx.fillRect(part.x, part.y, Math.max(0, spent - part.x), part.h);
            ctx.globalAlpha = drawing.selected ? 0.32 : 0.22;
            ctx.fillRect(spent, part.y, Math.max(0, part.x + part.w - spent), part.h);
            ctx.globalAlpha = 1;
            ctx.save();
            ctx.lineWidth = drawing.selected ? 2 : 1.5;
            ctx.strokeRect(part.x + 0.5, part.y + 0.5, part.w - 1, part.h - 1);
            ctx.restore();

            // The two edge prices, just inside the right edge -- where the
            // trader reads a price. The engine formatted them and placed the
            // box; this only paints, so the scale stays in Rust. This is the
            // same plate the crosshair price rides (`:1259-1265`), kept inside
            // the plot instead of in the gutter, which is the part that is out
            // of scope here.
            const tag = (text, y) => {
              ctx.font = "600 10px ui-monospace, monospace";
              const w = ctx.measureText(text).width + 8;
              const bx = part.x + part.w - w - 2;
              const by = Math.min(Math.max(y - 7, part.y), part.y + part.h - 14);
              ctx.globalAlpha = 0.85;
              ctx.fillRect(bx, by, w, 14);
              ctx.globalAlpha = 1;
              ctx.fillStyle = COLORS.zoneTag;
              ctx.fillText(text, bx + 4, by + 10);
              ctx.fillStyle = colour;
            };
            tag(part.top_label, part.y);
            tag(part.bottom_label, part.y + part.h);
            break;
          }
```

A degenerate zone -- both edges the same price, which is exactly what a
one-price leg from the thesis is -- has `h == 0` and so clips both tags to a
14px sliver at the same place. That is correct: one price, one tag. Leave it.

- [ ] **Step 9: A zone is grabbable.** The hit-test at `:4928-4942` matches part
  shapes; `case "rect"`-style inside-rect must cover the new part. Change the
  rect arm's condition to:

```js
        if ((part.shape === "rect" || part.shape === "zone") && insideRect(at, part)) {
```

Read the surrounding arms first: if the shape test is a `switch` rather than
chained `if`s, add `case "zone":` falling through to the rect arm instead.

- [ ] **Step 10: Harnesses.** In `tools/wasm_abi_check.mjs`, next to the part
  helpers (`:826-827`), add

```js
const zone = (drawing) => drawing.parts.find((p) => p.shape === "zone");
```

and assert, for a `kind: "rect"` drawing built over a real series, that `zone`
exists and `zone(d).top_label` / `bottom_label` are non-empty strings and
`typeof zone(d).fresh_from === "number" || zone(d).fresh_from === null`.
In `shell_check.mjs`, in the drawings section (`:3766-3960`) where a `rect` is
already placed by drag, assert with the geometry recorder from Task 1 that one
of the filled rectangles is the zone's width and that the two edge labels appear
in `painted.text`.

```js
// A zone the user dragged has to look like the reference: bordered box, a
// solid portion where price has not gone back, and its two edges printed.
check(
  "a dragged rectangle is drawn as a zone with its edge prices",
  painted.geom.some(([op, , , w]) => op === "strokeRect" && w > 20) &&
    ["101.00", "103.00"].every((label) => painted.text.includes(label)),
  `text: ${painted.text.slice(-6).join(" | ")}`
);
```

Use the prices the gesture in that section actually places; the literals above
are the ones the surrounding fixture yields. Confirm by reading the section, and
if the drag lands on different prices, take the numbers the fixture produces --
the assertion is about the labels existing, not about 101.
Then `NODE_PATH=… node tools/wasm_abi_check.mjs frontend/app/chart_engine.wasm`
and `NODE_PATH=… node tools/shell_check.mjs` → green.

- [ ] **Step 11: Commit** `feat: a price zone is drawn as a zone`, **including
  the rebuilt `frontend/app/chart_engine.wasm`.**

---

## Task 4: The two passes are tellable apart

`ReviewFrame` (`progress.rs:61-70`) says which rung is being read but not which
pass it belongs to. Without a discriminator the user watches the same ladder
twice and cannot tell the marked pass from the plain one -- and the shell would
show the *plain* attached screenshot under a "marked" claim, which is a lie the
card tells on purpose.

**Files:** modify `crates/ai-agent/src/progress.rs` (`:61-70`, tests at
`:210-230`) and `frontend/app/app.js:10289-10317`.

**Interfaces:**
- Consumes: nothing new.
- Produces: `Progress::ReviewFrame { symbol, timeframe, index, total, marked:
  bool }`, serialising as `"marked": true|false` -- read by `app.js` as
  `progress.marked`; `showAiReviewFrame(turn, progress)` keeps its signature.

- [ ] **Step 1: Extend the wire test first** (`progress.rs:217`,
  `a_review_frame_is_the_shape_the_shell_reads`): add to the asserted JSON keys

```rust
        assert_eq!(value["marked"], serde_json::json!(false), "the shell reads `marked`");
```

and for a second frame with `marked: true`, that the key serialises as `true`.
Run `cargo test -p ai-agent a_review_frame_is_the_shape_the_shell_reads` → fails
to compile (missing field), which is the failing state.

- [ ] **Step 2: Add the field** to the variant, after `total`:

```rust
        /// Which pass this frame belongs to. `false` is the plain look at the
        /// chart; `true` is the look at the chart *the agent has marked*.
        ///
        /// `#[serde(default)]` so a progress line from a binary older than this
        /// field still decodes, and the shell treats a missing key as the plain
        /// pass -- the honest default, because a plain pass is what every run
        /// before this one was.
        #[serde(default)]
        marked: bool,
    },
```

Then fix the three construction sites the compiler names: `tools.rs:1914-1921`
(`analyze_timeframe`) and `:1967-1975` (`analyze_multi_timeframe`) and
`multi_timeframe.rs:567-577` (`analyze_ladder`) all pass `marked: false`.

- [ ] **Step 3: The shell announces the marked pass without borrowing the plain
  pass's picture.** `showAiReviewFrame` (`app.js:10289-10305`) gains, after the
  `counter` line and **before** `enqueueAiReview`:

```js
  if (progress.marked) {
    // The marked picture is taken by the `capture_chart` request that follows
    // this frame (Task 7), and that handler owns the card -- it is the one
    // place the chart is actually carrying the agent's marks. Showing the
    // screenshot attached to the request here would caption a plain chart
    // "MARKED", and showing a second card per rung would double the ladder.
    // So this frame says which rung pass two is on, and stops.
    pageToast(`AI REVIEW · MARKED · ${symbol} · ${timeframe.toUpperCase()}${counter}`);
    return;
  }
```

`pageToast` (`app.js:8997`) is the module-level announcement -- `showAiReviewFrame`
is module-level too, so the pane closure's `note()` is out of scope here and
would be a `ReferenceError` at runtime. No change to `reviewImageFor`.

- [ ] **Step 4: Harness.** In `shell_check.mjs`'s agent-answer section, deliver a
  marked frame alongside the plain ones and assert both halves of the rule --
  it is announced, and it does not put a card on the chart:

```js
deliver(agentSocket, {
  type: "progress",
  payload: {
    stage: "review_frame",
    symbol: "BTCUSDT",
    timeframe: "4h",
    index: 1,
    total: 6,
    marked: true,
  },
});
await settle();

// A toast is a log line (`HRZLog.info` inside `pageToast`), which is how the
// dock section already reads one.
const markedAnnounced = window.HRZLog.entries()
  .map((line) => line.message)
  .some((message) => message.includes("AI REVIEW · MARKED · BTCUSDT · 4H"));
check(
  "a marked frame is announced as pass two and takes no card of its own",
  markedAnnounced && !reviewCardTitles().some((title) => title.includes("MARKED")),
  `log: ${window.HRZLog.entries().slice(-3).map((l) => l.message).join(" | ")} | cards: ${reviewCardTitles().join(", ")}`
);
```

`reviewCardTitles()` is a new two-line helper in the same section: read the
caption text out of the element `paintAiReviewCard` writes
(`app.js:10367-10380`) under `reviewHost` (`:10360-10365`), returning `[]` when
no card is mounted. Add it next to the section's existing `note`/`badge` helpers
(`:196-222`). Task 8 is where a marked card *does* appear -- from the capture
upload -- and this assertion is what keeps those two paths from both firing.

- [ ] **Step 5: `cargo test -p ai-agent` and the two Node gates** → green.
- [ ] **Step 6: Commit** `feat: a review frame carries which pass it belongs to`

---

## Task 5: Six captures wait at once, and a mentor run may spend twelve

The rendezvous (`agent.rs:1600-1653`) polls each id for 24 × 250 ms **in series**,
under a budget of 3. Six marked frames would take up to 36 seconds of dead screen
and be cut off after the third. Both numbers change here, and the request has to
carry the pass with it so the caption in the tool result is not guessed.

`ai-agent/Cargo.toml` has no `futures`, so "concurrently" is one deadline loop
that asks every outstanding id each round -- which is also what makes it testable
without a runtime feature.

**Files:** modify `crates/ai-agent/src/agent.rs` (`:1133-1134`,
`:1383-1416`, `:1600-1653`, tests).

**Interfaces:**
- Consumes: `CapturesContext::source() -> &Arc<dyn ChartCaptureSource>`
  (`captures.rs:47`), `ChartCaptureSource::take(&str) -> Option<ChartScreenshot>`.
- Produces: `struct CaptureRequest { id: String, timeframe: String, marked:
  bool }` (fields read by the relay in Step 6 and the rendezvous in Step 4);
  `async fn poll_captures(source: &dyn ChartCaptureSource, outstanding: &mut
  Vec<CaptureRequest>, rounds: usize, gap: Duration) -> Vec<(CaptureRequest,
  Option<ChartScreenshot>)>`, which drains `outstanding` and returns one pair per
  request in the order they were asked; `MENTOR_CAPTURES_PER_RUN: usize = 12`
  beside `MAX_CAPTURES_PER_RUN: usize = 3`; `pending_captures:
  Vec<CaptureRequest>` where it was `Vec<String>`.

- [ ] **Step 1: The failing test for the wait shape.** A source that answers each
  id only on its **second** appearance proves round-robin: a serial poller spends
  all its rounds on the first id and never reaches the others.

```rust
    /// Answers an id only once it has been asked `needed + 1` times, so a poller
    /// that finishes one capture before starting the next never sees any of the
    /// later ones -- which is the whole difference this function exists for.
    /// `needed: usize::MAX` never answers, which is how the miss test is taken.
    struct SlowSource {
        counts: std::sync::Mutex<std::collections::HashMap<String, usize>>,
        asked: std::sync::Mutex<Vec<String>>,
        needed: usize,
    }

    impl SlowSource {
        fn new(needed: usize) -> Self {
            Self {
                counts: std::sync::Mutex::new(std::collections::HashMap::new()),
                asked: std::sync::Mutex::new(Vec::new()),
                needed,
            }
        }
    }

    #[async_trait::async_trait]
    impl ChartCaptureSource for SlowSource {
        async fn take(&self, capture_id: &str) -> Option<ChartScreenshot> {
            let mut counts = self.counts.lock().expect("lock poisoned");
            let count = counts.entry(capture_id.into()).or_insert(0);
            *count += 1;
            self.asked.lock().expect("lock poisoned").push(capture_id.into());
            if *count > self.needed {
                return Some(ChartScreenshot {
                    media_type: "image/png".into(),
                    data: "p".into(),
                    label: Some(format!("{capture_id} shot")),
                });
            }
            None
        }
    }

    #[tokio::test]
    async fn six_captures_are_waited_on_together_not_one_after_another() {
        let source = SlowSource::new(1); // each id lands on its second ask
        let mut outstanding = vec![
            CaptureRequest { id: "a".into(), timeframe: "1w".into(), marked: true },
            CaptureRequest { id: "b".into(), timeframe: "1d".into(), marked: true },
        ];
        let settled = poll_captures(
            &source,
            &mut outstanding,
            4,
            std::time::Duration::ZERO,
        )
        .await;

        assert_eq!(settled.len(), 2, "both frames settle in one loop");
        assert!(outstanding.is_empty());
        assert!(settled.iter().all(|(_, shot)| shot.is_some()), "{settled:?}");
        // Interleaved: a serial poller would read ["a","a","a",.. ,"b",..].
        let order = source.asked.lock().expect("lock poisoned").clone();
        assert_eq!(order, vec!["a", "b", "a", "b"], "round-robin, not one then the other");
    }

    #[tokio::test]
    async fn a_frame_that_never_answers_is_reported_not_dropped() {
        let source = SlowSource::new(usize::MAX);
        let mut outstanding = vec![CaptureRequest {
            id: "c".into(),
            timeframe: "5m".into(),
            marked: true,
        }];
        let settled = poll_captures(&source, &mut outstanding, 2, std::time::Duration::ZERO).await;
        assert_eq!(settled.len(), 1, "the run still owes the model an answer about it");
        assert!(settled[0].1.is_none());
        assert!(outstanding.is_empty(), "a missed frame is not asked again next turn");
    }
```

`ChartScreenshot`'s fields are `media_type`, `data`, `label`
(`crates/ai-agent/src/chart_context.rs`).

- [ ] **Step 2: Run it** → does not compile (`cannot find struct CaptureRequest`),
  the expected failure.
- [ ] **Step 3: The request type and the loop.** In `agent.rs`, above the run
  loop:

```rust
/// One capture a run asked the screen for, and the pass it belongs to.
///
/// The id alone is not enough any more: six plain frames and six marked ones
/// can name the same timeframe, and the caption the model reads has to say
/// which of the two it is looking at.
#[derive(Debug, Clone)]
struct CaptureRequest {
    id: String,
    timeframe: String,
    marked: bool,
}

/// Wait on every outstanding capture at once, up to `rounds` polls `gap` apart.
///
/// Serial waiting is the bug: six ids polled one after another is up to six
/// full timeouts of nothing happening on screen, and a marked pass asks for all
/// six in the same turn. One deadline, every id checked each round, is the same
/// wall clock as the fastest upload and keeps the misses honest.
///
/// Returns one entry per request, in the order they were asked, so the
/// captions the model reads line up with the frames it was told to expect. A
/// `None` is a reported miss, never a silently dropped frame.
async fn poll_captures(
    source: &dyn crate::captures::ChartCaptureSource,
    outstanding: &mut Vec<CaptureRequest>,
    rounds: usize,
    gap: std::time::Duration,
) -> Vec<(CaptureRequest, Option<crate::chart_context::ChartScreenshot>)> {
    let mut waiting = std::mem::take(outstanding);
    let mut settled: Vec<Option<crate::chart_context::ChartScreenshot>> =
        (0..waiting.len()).map(|_| None).collect();
    let mut asked = 0usize;
    while asked < rounds {
        for round in 0..waiting.len() {
            if settled[round].is_some() {
                continue;
            }
            if let Some(shot) = source.take(&waiting[round].id).await {
                settled[round] = Some(shot);
            }
        }
        asked += 1;
        if asked < rounds && settled.iter().any(Option::is_none) {
            tokio::time::sleep(gap).await;
        }
    }
    waiting
        .into_iter()
        .zip(settled)
        .map(|(request, shot)| (request, shot))
        .collect()
}
```

The `asked` count is per round, and the test's expected `["a","b","a","b"]`
depends on it: do not "optimise" the inner loop into a `while` that re-asks one
id until it lands, which is the serial behaviour this replaced.

- [ ] **Step 4: Use it at the rendezvous.** Replace the body of the
  `if !pending_captures.is_empty()` block (`:1605-1652`) with a budget check
  first, then the shared wait:

```rust
            let mut visual_blocks: Vec<ContentBlock> = std::mem::take(&mut pending_images);
            if !pending_captures.is_empty() {
                // `ctx.captures` is already the source -- `Option<&'a dyn
                // ChartCaptureSource>` (tools.rs:361), not a context object, and
                // it is `Copy`, so `if let` binds it without an `expect`.
                if let Some(source) = ctx.captures {
                    // The mentor budget is the same guard with more room: the
                    // product owner asked for six frames twice, and an ordinary
                    // run keeps the three it has always had.
                    let budget = if request.teaching {
                        MENTOR_CAPTURES_PER_RUN
                    } else {
                        MAX_CAPTURES_PER_RUN
                    };
                    let mut asks: Vec<CaptureRequest> = Vec::new();
                    for pending in pending_captures.drain(..) {
                        if captures_used >= budget {
                            visual_blocks.push(ContentBlock::Text(
                                "the capture budget for this run is spent; answer from the \
                                 data you have."
                                    .into(),
                            ));
                            continue;
                        }
                        captures_used += 1;
                        asks.push(pending);
                    }
                    for (capture, shot) in poll_captures(
                        source,
                        &mut asks,
                        24,
                        std::time::Duration::from_millis(250),
                    )
                    .await
                    {
                        let frame = capture.timeframe.to_ascii_uppercase();
                        match shot {
                            Some(found) => {
                                // The pass is named first because the model is
                                // comparing two looks at the same timeframe and
                                // has to know which one this is.
                                let caption = if capture.marked {
                                    format!("PASS 2 · {frame} · marked")
                                } else {
                                    found
                                        .label
                                        .clone()
                                        .unwrap_or_else(|| format!("{frame} chart capture"))
                                };
                                visual_blocks.push(ContentBlock::Text(format!(
                                    "The chart capture you requested ({caption}) is attached \
                                     below -- read it like the screenshots that ride a \
                                     question: illustration, not data."
                                )));
                                visual_blocks.push(ContentBlock::Image {
                                    media_type: found.media_type.clone(),
                                    data: found.data.clone(),
                                });
                            }
                            None => visual_blocks.push(ContentBlock::Text(format!(
                                "no image for {frame} -- the screen did not answer that \
                                 capture in time; say so rather than describing a chart you \
                                 did not see."
                            ))),
                        }
                    }
                } else {
                    // The tool already told the model there is no screen; drop
                    // the requests so a later turn does not wait on them.
                    pending_captures.clear();
                }
            }
```

`request.teaching` is the run's flag (`AgentRequest::teaching`,
[crates/ai-agent/src/agent.rs:342](../../../crates/ai-agent/src/agent.rs)), already
read the same way at `:1212` for the drawing duty, and `request` is in scope for
the whole run loop. The loop variables are named `pending` / `capture`, not
`request`, so they never shadow it.

- [ ] **Step 5: The budget constant** (`:1133`):

```rust
/// Captures one ordinary run may ask the screen for.
const MAX_CAPTURES_PER_RUN: usize = 3;

/// Captures a mentor run may ask for: the ladder twice, at six timeframes.
///
/// The budget is a cost guard, not a correctness rule -- twelve pictures is the
/// price of the two-pass review the product owner asked for, and the two extra
/// turns it spends are the difference between describing a chart and checking
/// the marks against it.
const MENTOR_CAPTURES_PER_RUN: usize = 12;
```

- [ ] **Step 6: Push the pass with the request.** The relay at `:1383-1416`
  reads one `ui_command` with one `capture_id`. Task 6 makes a tool return six at
  once, so the relay reads them as a list in this commit, with a single command
  still working. Replace the whole `if !result.is_error { if let Some(command) =
  result.content.get("ui_command") { … } }` block with:

```rust
                if !result.is_error {
                    // `review_marked_charts` asks for the whole ladder in one
                    // call, so the commands arrive as a list. `look_at_chart`
                    // still answers with a single `ui_command`, and both shapes
                    // are the same fact: "photograph this frame for me".
                    let mut commands: Vec<serde_json::Value> = result
                        .content
                        .get("ui_commands")
                        .and_then(serde_json::Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if let Some(single) = result.content.get("ui_command") {
                        commands.push(single.clone());
                    }
                    for command in commands {
                        let action = command
                            .get("action")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if action == "capture_chart" {
                            if let Some(id) = command
                                .get("capture_id")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                            {
                                pending_captures.push(CaptureRequest {
                                    id,
                                    timeframe: command
                                        .get("timeframe")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                    marked: command
                                        .get("marked")
                                        .and_then(serde_json::Value::as_bool)
                                        .unwrap_or(false),
                                });
                            }
                        }
                        progress.report(Progress::UiCommand {
                            action,
                            symbol: command
                                .get("symbol")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string),
                            timeframe: command
                                .get("timeframe")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string),
                            capture_id: command
                                .get("capture_id")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string),
                            extra: Some(command.clone()),
                        });
                    }
                }
```

One `Progress::UiCommand` per command, payload unchanged: the shell's
`capture_chart` handler (`app.js:9402-9439`) reads `cmd.capture_id`, `cmd.symbol`
and `cmd.timeframe`, so six marked frames ride the path it already serves.
- [ ] **Step 7: `cargo test -p ai-agent`** then `cargo test --workspace` → green,
  including `a_run_reports_the_steps_it_took` and the capture tests; then
  `cargo clippy -p ai-agent --all-targets -- -D warnings`.
- [ ] **Step 8: Commit** `feat: six captures wait together, and a mentor run may ask for twelve`

---

## Task 6: `review_marked_charts` -- one call, the whole marked ladder

The third phase needs a tool that asks for six frames in one turn. It is
`look_at_chart`'s shape (a request, not a capture: the images land with the turn's
results through the rendezvous Task 5 built), widened to a list and pinned to a
fixed six-rung ladder so the model does not have to remember six strings.

Why a constant and not the run's ladder: the shell sends
`timeframes: [activeTimeframe()]` (`app.js:11908`), so a mentor question asked on
the 5m chart builds a **one-rung** run ladder (`agent.rs:864-878`). The plain pass
is six pictures because the client takes them itself over `LADDER_TIMEFRAMES`
(`app.js:12088-12115`, `captureLadder`) -- so pass two has to be the same six
strings, or the loop is six looks and then one.

**Files:** modify `crates/ai-agent/src/multi_timeframe.rs` (the new const, beside
`DEFAULT_LADDER` at `:12-13`) and `crates/ai-agent/src/tools.rs` (`:754-766`
specs, `:1061-1073` dispatch, `:2958-3014` the capture tool, tests).

**Interfaces:**
- Consumes: `CaptureRequest`/relay from Task 5 (`ui_commands` array, `marked`
  field on each command); `TimeframeLadder::new(Vec<Timeframe>)` /
  `::timeframes() -> &[Timeframe]` (`multi_timeframe.rs:31-85`);
  `Progress::ReviewFrame{marked:true}` from Task 4.
- Produces: `pub const MARKED_LADDER: [Timeframe; 6]` in `multi_timeframe.rs`,
  coarse to fine, mirroring the shell's `LADDER_TIMEFRAMES`;
  `fn next_capture_id() -> String`;
  `async fn review_marked_charts_tool(ctx: &ToolContext<'_>, args: &Value) ->
  Result<Value, AgentError>` returning
  `{"ok":true,"ui_commands":[{action:"capture_chart",symbol,timeframe,
  capture_id,marked:true}, …],"frames":["1w",…],"note":…}`; the tool name
  `"review_marked_charts"`, which Task 8's duty matches on.

- [ ] **Step 1: Failing tests first.** In `tools.rs`'s test module, the
  `Fixture::rising` context builder (`:4443`) and the `with_progress(&sink)`
  pattern (`:4467`) are the ones to copy; the capture source needs a fixture
  because none exists yet -- the tool only reads its *presence*, so a source that
  never answers is exactly right. Add at the top of the module, beside `Fixture`:

```rust
    /// A screen that is attached but never answers. `review_marked_charts` only
    /// asks whether a capture source exists -- the waiting is the run's job --
    /// so this is the whole fixture the tool's own tests need.
    struct SilentScreen;

    #[async_trait::async_trait]
    impl crate::captures::ChartCaptureSource for SilentScreen {
        async fn take(&self, _capture_id: &str) -> Option<crate::chart_context::ChartScreenshot> {
            None
        }
    }
```

then the three tests:

```rust
    /// The marked pass is six frames or it is not the marked pass: the product
    /// owner's complaint was that a run marked the charts and then described
    /// them from memory.
    #[tokio::test]
    async fn review_marked_charts_asks_for_the_whole_ladder_at_once() {
        let fixture = Fixture::rising(40);
        let screen = SilentScreen;
        let ctx = ToolContext::new(&fixture).with_captures(&screen);

        let out = review_marked_charts_tool(&ctx, &json!({"symbol": "BTCUSDT"}))
            .await
            .expect("the marked pass is requested");

        let commands = out["ui_commands"].as_array().expect("a list");
        assert_eq!(commands.len(), 6, "one frame per rung: {commands:?}");
        let ids: Vec<&str> = commands
            .iter()
            .map(|c| c["capture_id"].as_str().expect("an id"))
            .collect();
        assert_eq!(
            ids.len(),
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            "six frames, six rendezvous ids: {ids:?}"
        );
        let rungs: Vec<&str> = commands
            .iter()
            .map(|c| c["timeframe"].as_str().expect("a timeframe"))
            .collect();
        // The exact list, in the exact order, because the plain pass the user
        // already saw is this same list taken by the shell.
        assert_eq!(rungs, vec!["1w", "1d", "4h", "1h", "15m", "5m"]);
        for command in commands {
            assert_eq!(command["action"], "capture_chart");
            assert_eq!(command["symbol"], "BTCUSDT");
            assert_eq!(command["marked"], true, "the shell photographs the marked chart");
        }
        assert_eq!(
            out["frames"].as_array().expect("frames").len(),
            6,
            "the run is told which six it asked for"
        );
    }

    #[tokio::test]
    async fn a_marked_review_can_narrow_to_the_rungs_the_model_asks_about() {
        // "re-check just the 4h and the 1h" is a legitimate request, and the
        // tool that ignores it photographs six charts nobody asked for.
        let fixture = Fixture::rising(40);
        let screen = SilentScreen;
        let ctx = ToolContext::new(&fixture).with_captures(&screen);

        let out = review_marked_charts_tool(
            &ctx,
            &json!({"symbol": "BTCUSDT", "timeframes": ["1h", "4h"]}),
        )
        .await
        .expect("two frames");
        let got: Vec<&str> = out["ui_commands"]
            .as_array()
            .expect("a list")
            .iter()
            .map(|c| c["timeframe"].as_str().expect("a timeframe"))
            .collect();
        // Coarse first, whatever order the model typed: the cards and the
        // captions have to match the order the analysis was read in.
        assert_eq!(got, vec!["4h", "1h"]);
    }

    #[tokio::test]
    async fn a_marked_review_without_a_screen_says_so() {
        let fixture = Fixture::rising(40);
        let ctx = ToolContext::new(&fixture);
        let err = review_marked_charts_tool(&ctx, &json!({}))
            .await
            .expect_err("nothing to photograph");
        assert!(matches!(err, AgentError::ToolFailed { .. }), "got {err}");
        assert!(err.to_string().contains("no screen"), "got {err}");
    }
```

- [ ] **Step 2: `cargo test -p ai-agent review_marked`** → fails to compile
  (`cannot find function review_marked_charts_tool`), which is the expected state.
- [ ] **Step 3: Extract the id mint** (`tools.rs:2991-3001`) so both capture
  tools use one generator:

```rust
/// A fresh rendezvous id for one capture request.
///
/// No uuid dependency in this crate: millis plus a process-wide counter is
/// collision-free for a per-run, per-user rendezvous, and six ids minted in the
/// same millisecond are still six different ids because of the counter.
fn next_capture_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    format!(
        "cap-{millis:x}-{}",
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}
```

and make `look_at_chart_tool` call it, deleting its inline `static COUNTER`.

- [ ] **Step 4: The six rungs, as a constant.** In `multi_timeframe.rs`, beside
  `DEFAULT_LADDER` (`:11-13`):

```rust
/// The rungs the marked pass photographs when the model names none.
///
/// This is the shell's `LADDER_TIMEFRAMES` (`frontend/app/app.js:12088`) in Rust,
/// and it has to be: the plain pass is six pictures because the client takes them
/// itself over that list, so pass two re-photographs the same six or the loop is
/// six looks followed by one. It is not the *run's* ladder -- a mentor question
/// from the chart is sent with `timeframes: [activeTimeframe()]` (`app.js:11908`),
/// which makes the run ladder one rung wide.
///
/// Coarse to fine, so a caller that iterates it reads top-down like the analysis.
pub const MARKED_LADDER: [Timeframe; 6] = [
    Timeframe::W1,
    Timeframe::D1,
    Timeframe::H4,
    Timeframe::H1,
    Timeframe::M15,
    Timeframe::M5,
];
```

Export it from `lib.rs` next to `DEFAULT_LADDER` if the crate re-exports that one
(check `lib.rs`; `multi_timeframe` is `pub mod`, so `crate::multi_timeframe::MARKED_LADDER`
also works and needs no export). `Timeframe` is `Copy`, so `.to_vec()` on the
const is enough for the tool.

- [ ] **Step 5: The description const and the tool**, beside `LOOK_AT_CHART`
  (`:1756`) and `look_at_chart_tool` (`:2964`):

```rust
const REVIEW_MARKED_CHARTS: &str = "Photograph every timeframe of this analysis again, now \
    carrying the zones and levels you have drawn, and see them before you commit the plan. \
    Call this once, after your drawings are on the chart, in the same turn you would have \
    written the thesis: the images arrive with this call's results. Omit `timeframes` to \
    re-photograph the whole ladder. This never moves the user's screen.";

/// `review_marked_charts`: the second pass, asked for as one call.
///
/// Six commands, not six turns. A model that has to ask for each frame
/// separately spends the run's budget on bookkeeping and gets three of six
/// before the cap; one call that expands to the ladder is the difference between
/// a review that looked and a review that described.
async fn review_marked_charts_tool(
    ctx: &ToolContext<'_>,
    args: &Value,
) -> Result<Value, AgentError> {
    const TOOL: &str = "review_marked_charts";
    if ctx.captures.is_none() {
        return Err(AgentError::ToolFailed {
            tool: TOOL.into(),
            reason: "no screen is attached to this run, so there is no marked chart to \
                     photograph. Say in the answer that the marked pass could not run, \
                     and ground the plan in the data tools instead."
                .into(),
        });
    }

    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_uppercase());

    // The six rungs are the default: the plain pass the user watched was these
    // six, and a marked pass over a different set would not be a second look at
    // the same work.
    let rungs: Vec<Timeframe> = match args.get("timeframes").and_then(Value::as_array) {
        None => crate::multi_timeframe::MARKED_LADDER.to_vec(),
        Some(list) => {
            let mut parsed = Vec::with_capacity(list.len());
            for raw in list {
                let text = raw.as_str().unwrap_or_default();
                parsed.push(text.parse::<Timeframe>().map_err(|_| AgentError::InvalidToolArgs {
                    tool: TOOL.into(),
                    reason: format!("`timeframes` contains `{text}`, not a resolution the engine accepts"),
                })?);
            }
            parsed
        }
    };
    if rungs.is_empty() {
        return Err(AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: "no timeframes to re-photograph: the list was empty and none parse"
                .into(),
        });
    }
    // Coarse to fine, and deduplicated, whatever the model typed: the six cards
    // and the six captions the model reads are in the order it thinks in.
    // `TimeframeLadder::new` already does exactly this (multi_timeframe.rs:31-40,
    // sorting descending by duration because `Timeframe`'s `Ord` is by duration),
    // so the ladder type is the one place that ordering lives.
    let rungs = crate::multi_timeframe::TimeframeLadder::new(rungs)
        .timeframes()
        .to_vec();

    let mut commands = Vec::with_capacity(rungs.len());
    let mut frames = Vec::with_capacity(rungs.len());
    let total = rungs.len();
    for (index, rung) in rungs.iter().enumerate() {
        let timeframe = rung.to_string();
        // Announced before the photograph is taken, exactly as the plain pass
        // announces each rung before reading it: the user watches pass two
        // arrive rather than seeing six charts change at once at the end.
        if let Some(progress) = ctx.progress {
            progress.report(Progress::ReviewFrame {
                symbol: symbol.clone().unwrap_or_default(),
                timeframe: timeframe.clone(),
                index: index + 1,
                total,
                marked: true,
            });
        }
        let capture_id = next_capture_id();
        frames.push(timeframe.clone());
        commands.push(json!({
            "action": "capture_chart",
            // Cloned, not moved: six commands read the same `Option<String>`, and
            // the second iteration would be reading a moved value. Null in the
            // JSON means "the pane the user is on", which is what the shell's
            // capture handler already does for `look_at_chart`.
            "symbol": symbol.clone(),
            "timeframe": timeframe,
            "capture_id": capture_id,
            "marked": true,
        }));
    }

    Ok(json!({
        "ok": true,
        "ui_commands": commands,
        "frames": frames,
        "note": "the marked frames will arrive with this turn's results, captioned \
                 `PASS 2 · <TIMEFRAME> · marked`. Any frame that does not arrive is \
                 reported as missing -- confirm the trend, entry, target and stop from \
                 the frames you actually see, and say which you could not see.",
    }))
}
```

`TimeframeLadder::default()` exists (`multi_timeframe.rs:35-39`) and is the
fallback when a run has no ladder on its context; `ctx.progress` is
`Option<&'a dyn ProgressSink>` (`tools.rs:370`), which is why the announcement is
`if let Some(progress) = ctx.progress` -- the same form
`analyze_multi_timeframe` uses at `:1968`. `every_registered_tool_is_dispatchable`
(`:4442`) runs every registered tool with empty args against a context with no
screen, so the new arm must fail as `ToolFailed` there -- which it does, and which
is that test's job: an advertised tool with no dispatch arm is a model offered a
button that is not wired.

- [ ] **Step 6: Register and dispatch.** Add the `ToolSpec` after
  `look_at_chart` (`:754-766`):

```rust
                ToolSpec {
                    name: "review_marked_charts".into(),
                    description: REVIEW_MARKED_CHARTS.into(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "symbol": {"type": "string", "description": "Instrument whose charts to photograph; omit to use the one under analysis. This does not move the user's screen."},
                            "timeframes": {"type": "array", "items": {"type": "string"}, "description": "Rungs to re-photograph. Omit for the whole ladder this run is reading."}
                        },
                        "required": []
                    }),
                    exposed_tool: None,
                },
```

and the dispatch arm beside `"look_at_chart"` (`:1067`):

```rust
                "review_marked_charts" => review_marked_charts_tool(ctx, &call.input).await,
```

- [ ] **Step 7: `cargo test -p ai-agent`** → the Step 1 tests green. Then
  `cargo clippy -p ai-agent --all-targets -- -D warnings`.
- [ ] **Step 8: Commit** `feat: review_marked_charts photographs the marked ladder in one call`

---

## Task 7: A marked capture is a photograph of the marked chart

`showAiDrawingsOnCharts` only touches panes that already exist
(`app.js:10420-10425`), and `renderOffscreen` builds a **fresh** pane
(`:12141`, `createChartPane(node, {})`) whose `aiLayerOn` starts `false`. The AI
layer is a per-pane closure value, and the filter that withholds the agent's
drawings runs when the request is built (`:3853-3858`) -- so a marked capture of
a timeframe the user is not looking at photographs a plain chart and uploads it
as marked. That is the load-bearing dependency the spec asks for a **test**, not
a comment.

**Files:** modify `frontend/app/app.js` (`:9402-9439`, `:9447-9461`,
`:12130-12154`) and `tools/shell_check.mjs`.

**Interfaces:**
- Consumes: `pane.aiLayerOn()` / `pane.setAiLayerOn(on)` (`:8061`, `:8077-8083`);
  `Progress::UiCommand.extra` carries the command verbatim, so the shell reads
  the flag as `cmd.extra.marked` (`executeUiCommand` is called with
  `frame.payload`, `:10185`).
- Produces: `captureSeries(symbol, timeframe, { marked = false } = {})` and
  `renderOffscreen(symbol, timeframe, { marked = false } = {})` -- one optional
  options object, both call sites that pass nothing unchanged.

- [ ] **Step 1: The failing harness test.** In `shell_check.mjs`, the drawings
  section already stores an `created_by: "ai"` row (the `backend.drawings`
  fixture at `:1098-1109`) and the AI-layer section (`:4623-4690`) already shows
  how the toggle is read. Add a marked-capture section after the agent-answer
  section:

```js
// --- a marked capture carries the AI layer -----------------------------------
console.log("\na marked capture carries the AI layer");

// A marked capture has to carry the agent's shapes. The pane the photograph is
// taken from is not the one the user is looking at, and the AI layer is a
// per-pane switch that defaults off -- so "marked" has to mean the layer was on
// while the scene was built, or the run is told it looked at its own marks and
// was handed a bare chart.

// Deliver a capture command for a timeframe no pane is showing, which is the
// offscreen path -- the one that would otherwise inherit a fresh pane's `false`.
deliver(agentSocket, {
  type: "progress",
  payload: {
    stage: "ui_command",
    action: "capture_chart",
    symbol: "BTCUSDT",
    timeframe: "1d",
    capture_id: "cap-marked-1",
    extra: { action: "capture_chart", symbol: "BTCUSDT", timeframe: "1d", capture_id: "cap-marked-1", marked: true },
  },
});
await settle();
await settle();

const markedRequest = engine.requests.find(
  (request) => request.timeframe === "1d" && requestOffscreen(request)
);
check(
  "the offscreen render for a marked capture asks for the AI drawings",
  markedRequest && markedRequest.drawings.some((d) => d.created_by === "ai"),
  markedRequest
    ? `drawings: ${markedRequest.drawings.map((d) => d.created_by).join(",")}`
    : "no offscreen 1d request was built"
);
```

`requestOffscreen(request)` is the helper this section needs and does not have
yet: an offscreen render is the one whose `width`/`height` are the 1024x700
holder's, so write it as

```js
const requestOffscreen = (request) => request.width === 1024 && request.height === 700;
```

The section banner is the file's own idiom: a `// --- <name> ---` comment above a
`console.log("\n<name>")`, then `check(name, ok, detail)` calls -- no other
harness helper exists, and `agentSocket` (`:5050`), `deliver` (`:462`) and
`settle` (`:1656`) are already in module scope by the time this section runs.
Run `NODE_PATH=… node tools/shell_check.mjs` and confirm it fails with
`no offscreen 1d request was built` or an empty drawings list -- either is the
right failure, because both say the marks are not being asked for.

- [ ] **Step 2: Thread the flag through the two capture paths.** `captureSeries`
  (`:9447-9461`):

```js
async function captureSeries(symbol, timeframe, { marked = false } = {}) {
  const openPane = panes.find(
    (pane) => pane.symbol() === symbol && pane.timeframe() === timeframe
  );
  if (openPane) {
    // A beat even for a pane already on it: the chart repaints continuously,
    // and a capture taken mid-frame reads to a vision model as a smear.
    await new Promise((resolve) => setTimeout(resolve, 150));
    if (!marked) return captureChart(openPane.canvas, timeframe);
    // The user's own layer switch is theirs: turn it on for the photograph and
    // put it back exactly as it was, rather than leaving their chart showing
    // the agent's marks because the agent asked for a picture.
    const wasOn = openPane.aiLayerOn();
    openPane.setAiLayerOn(true);
    try {
      await new Promise((resolve) => setTimeout(resolve, 150));
      return captureChart(openPane.canvas, timeframe);
    } finally {
      openPane.setAiLayerOn(wasOn);
    }
  }
  const entry = coverage.find((instrument) => instrument.symbol === symbol);
  const frame = entry && entry.timeframes.find((f) => f.timeframe === timeframe);
  if (!frame || frame.candles === 0) return null;
  return renderOffscreen(symbol, timeframe, { marked });
}
```

The second 150 ms beat is not a copy-paste of the first: `setAiLayerOn` schedules
a render (`:8083`) and the request it rebuilds is what carries the drawings, so a
capture taken in the same tick photographs the pre-toggle scene.

- [ ] **Step 3: The offscreen pane switches its own layer on** before it fetches
  (`:12130-12154`):

```js
async function renderOffscreen(symbol, timeframe, { marked = false } = {}) {
  // ...the existing holder/template setup, `createChartPane`, `fillSeries` and
  // the wanted-series guard are unchanged and stay exactly as they are...
    const pane = createChartPane(node, {});
    pane.fillSeries(coverage, symbol, timeframe);
    if (pane.symbol() !== symbol || pane.timeframe() !== timeframe) return null;
    // A scratch pane starts with the AI layer off, like every pane. For the
    // marked pass that is the difference between a photograph of the analysis
    // and a photograph of candles: the layer decides whether the agent's own
    // shapes are in the request the engine builds the scene from.
    if (marked) pane.setAiLayerOn(true);
    await pane.refresh();
  // ...`pane.draw()`, `captureChart(pane.canvas, timeframe)`, and the
  // `finally { holder.remove() }` are unchanged; the `return` they feed is now
  // `return renderOffscreen(symbol, timeframe, { marked })` from `captureSeries`.
}
```

- [ ] **Step 4: The handler passes it and captions it** (`:9402-9423`):

```js
        const marked = Boolean(cmd.extra && cmd.extra.marked);
        const shot = await captureSeries(symbol, timeframe, { marked });
        if (!shot) return;
        enqueueAiReview({
          symbol,
          timeframe,
          image: shot,
          caption: marked
            ? `AI REVIEW · MARKED · ${symbol} · ${timeframe.toUpperCase()}`
            : `AI LOOK · ${symbol} · ${timeframe.toUpperCase()}`,
        });
```

This is the card Task 4 deliberately declined to show: the picture here is the
one that has the marks on it. Extend the Step 1 section with one more assertion
that the card mounted for `cap-marked-1` carries the `MARKED` caption.
- [ ] **Step 5: `NODE_PATH=… node tools/shell_check.mjs` and `… node
  tools/ui_check.mjs`** → green, and the existing plain-capture assertions still
  pass (a capture with no `extra.marked` is `marked === false`, which is the old
  behaviour on both paths).
- [ ] **Step 6: Commit** `fix: a marked capture photographs the chart that carries the marks`

---

## Task 8: A mentor run that never looked at its marks cannot submit

The tool exists; nothing obliges it. This is the duty: the first `submit_thesis`
of a mentor run that has not seen a single marked frame is refused **once**, with
one line saying what is missing, and if the model submits anyway the answer
carries the record that it did.

**Files:** modify `crates/ai-agent/src/agent.rs` (`:1210-1236` answer tool set,
`:1295`, `:1469-1564` the submit block, tests).

**Interfaces:**
- Consumes: `CaptureRequest.marked` (Task 5), the tool name
  `"review_marked_charts"` (Task 6), `ToolTrace{tool,args,result}` and
  `thesis.provenance` (`thesis.rs:127-134`, `agent.rs:1501-1502`),
  `Progress::Correcting{reason}`.
- Produces: two locals in the run loop -- `marked_frames_seen: usize` and
  `marked_refusal_used: bool` -- and `marked_review_owed: bool` computed at the
  answer door, next to the existing `drawing_owed` (`:1210-1212`).

- [ ] **Step 1: The failing tests** in `agent.rs`'s test module, beside
  `a_lesson_draws_in_the_answer_phase_without_a_skill` (`:4964`). They use the
  existing `ScriptedClient`, `Fixture`, `RecordingWriter`, `NoDrawings`,
  `read_call`, `draw_call` and `thesis_call`. Two budget facts decide the script:
  the run is `DEFAULT_MAX_TURNS` (6) analysis turns + `ANSWER_TURNS` (2) = 8
  turns, and the refusal must land before the last one -- so **five** read calls,
  not six, leaving turns 5/6/7 for draw, thesis-refused, thesis-accepted.

```rust
    /// How many times the marked refusal is in the conversation the orchestrator
    /// actually sent. Read off the last request, whose `messages` carry the whole
    /// history: the drawing-duty test at `:4851` reads the same fact the same
    /// way, and a count is what proves the "once" in "refused once".
    fn marked_refusals(llm: &ScriptedClient) -> usize {
        let last = llm
            .requests()
            .last()
            .expect("a run that submits must have sent a request");
        serde_json::to_string(&last.messages)
            .unwrap_or_default()
            .matches("Call review_marked_charts, read the six")
            .count()
    }

    /// Mentor mode's promise is that the plan was checked against the marked
    /// charts. Without the duty the model can -- and live runs do -- mark the
    /// chart, then write the thesis from the first pass's pictures.
    #[tokio::test]
    async fn a_lesson_must_look_at_its_marks_before_it_submits() {
        let source = Fixture::new();
        let writer = Arc::new(RecordingWriter::default());
        let captures = crate::captures::CapturesContext::new(Arc::new(AnsweringCaptures));
        let mut responses: Vec<LlmResponse> = (0..AgentConfig::default().max_turns - 1)
            .map(|i| read_call(&format!("r{i}")))
            .collect();
        responses.push(draw_call("d1"));
        // Straight to the thesis: the marked pass never ran.
        responses.push(thesis_call(100_100.0, 100_000.0, 100_400.0));
        // The refusal is spent; this one lands on the last turn.
        responses.push(thesis_call(100_100.0, 100_000.0, 100_400.0));
        let llm = Arc::new(ScriptedClient::new(responses));
        let agent = Agent::new(llm.clone(), SkillLibrary::new(), AgentConfig::default());

        let request = AskRequest::new("BTCUSDT", "teach me this structure")
            .with_drawings(
                DrawingsContext::new(Arc::new(NoDrawings), "test-user").with_writer(writer),
            )
            .with_captures(captures)
            .with_teaching(true);
        agent
            .ask(&request, &source)
            .await
            .expect("the lesson completes after one refusal");

        assert_eq!(
            marked_refusals(&llm),
            1,
            "the model was refused exactly once, and told which call it skipped"
        );
    }

    /// The other half: once it has asked, the door is open. A duty that never
    /// discharges is a run that burns turns until it dies.
    #[tokio::test]
    async fn the_marked_duty_opens_once_a_marked_frame_has_landed() {
        let source = Fixture::new();
        let writer = Arc::new(RecordingWriter::default());
        let captures = crate::captures::CapturesContext::new(Arc::new(AnsweringCaptures));
        let mut responses: Vec<LlmResponse> = (0..AgentConfig::default().max_turns - 1)
            .map(|i| read_call(&format!("r{i}")))
            .collect();
        responses.push(draw_call("d1"));
        // The marked pass runs, and `AnsweringCaptures` answers every id, so the
        // rendezvous delivers the marked frames before the next turn.
        responses.push(marked_call("m1"));
        responses.push(thesis_call(100_100.0, 100_000.0, 100_400.0));
        let llm = Arc::new(ScriptedClient::new(responses));
        let agent = Agent::new(llm.clone(), SkillLibrary::new(), AgentConfig::default());

        let request = AskRequest::new("BTCUSDT", "teach me this structure")
            .with_drawings(
                DrawingsContext::new(Arc::new(NoDrawings), "test-user").with_writer(writer),
            )
            .with_captures(captures)
            .with_teaching(true);
        let answer = agent
            .ask(&request, &source)
            .await
            .expect("the lesson completes on its own");

        assert_eq!(marked_refusals(&llm), 0, "a run that looked was refused anyway");
        // The first answer-phase request is the one where the duty is still owed,
        // so it must offer the call the refusal would have asked for. Read the
        // same way the drawing-duty test reads it at `:5046`.
        let answer_phase = &llm.requests()[AgentConfig::default().max_turns];
        let announced: Vec<&str> = answer_phase.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            announced.contains(&"review_marked_charts"),
            "the marked duty was owed but its tool was not on the table: {announced:?}"
        );
        assert!(
            !answer
                .thesis
                .provenance
                .iter()
                .any(|row| row.tool == "review_marked_charts"
                    && row.result["skipped"] == serde_json::json!(true)),
            "a duty that discharged must not be recorded as skipped: {:?}",
            answer.thesis.provenance
        );
    }

    /// Refused once, never twice: the submission that lands carries the record,
    /// because an answer that admits it did not re-check beats a run that dies
    /// at the turn limit.
    #[tokio::test]
    async fn a_second_thesis_without_the_marked_pass_is_accepted_with_the_miss_recorded() {
        let source = Fixture::new();
        let writer = Arc::new(RecordingWriter::default());
        let captures = crate::captures::CapturesContext::new(Arc::new(AnsweringCaptures));
        let mut responses: Vec<LlmResponse> = (0..AgentConfig::default().max_turns - 1)
            .map(|i| read_call(&format!("r{i}")))
            .collect();
        responses.push(draw_call("d1"));
        responses.push(thesis_call(100_100.0, 100_000.0, 100_400.0));
        responses.push(thesis_call(100_100.0, 100_000.0, 100_400.0));
        let llm = Arc::new(ScriptedClient::new(responses));
        let agent = Agent::new(llm.clone(), SkillLibrary::new(), AgentConfig::default());

        let request = AskRequest::new("BTCUSDT", "teach me this structure")
            .with_drawings(
                DrawingsContext::new(Arc::new(NoDrawings), "test-user").with_writer(writer),
            )
            .with_captures(captures)
            .with_teaching(true);
        let answer = agent
            .ask(&request, &source)
            .await
            .expect("the answer lands rather than dying at the turn limit");

        let skipped = answer
            .thesis
            .provenance
            .iter()
            .filter(|row| row.tool == "review_marked_charts")
            .count();
        assert_eq!(
            skipped, 1,
            "exactly one record of the pass that never ran: {:?}",
            answer.thesis.provenance
        );
        // The refusal is one, not two: the model is never punished twice for the
        // same omission.
        assert_eq!(
            marked_refusals(&llm),
            1,
            "the duty must not refuse the same omission twice"
        );
    }
```

Add the three pieces they need, in the same module:

```rust
    /// A screen that answers every capture the moment it is asked, because the
    /// duty cares whether a marked frame landed, not how long it took. It holds
    /// no state: the run's own trace is the record of what was asked.
    struct AnsweringCaptures;

    #[async_trait]
    impl crate::captures::ChartCaptureSource for AnsweringCaptures {
        async fn take(&self, capture_id: &str) -> Option<crate::chart_context::ChartScreenshot> {
            Some(crate::chart_context::ChartScreenshot {
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
                label: Some(format!("{capture_id} shot")),
            })
        }
    }

    /// The marked pass as one tool call, modelled on `draw_call` (`:4994` area).
    fn marked_call(id: &str) -> LlmResponse {
        LlmResponse {
            message: Message {
                role: crate::llm_client::Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: id.into(),
                    name: "review_marked_charts".into(),
                    input: json!({}),
                }],
            },
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        }
    }
```

`marked_refusals` (the counting helper at the top of Step 1) reads
`ScriptedClient::requests()` (`llm_client.rs:386`), which already exists -- do not
add a method to `ScriptedClient` for this. `async_trait` is already imported in
the test module (`agent.rs:3397`), so the bare `#[async_trait]` is the local idiom.

- [ ] **Step 2: Run it** → `cargo test -p ai-agent a_lesson_must_look` fails on
  the missing correction, which is the point: today nothing refuses the thesis.
- [ ] **Step 3: Count what landed.** `pending_captures` (`:1295`) is declared
  *inside* the turn loop, so the two flags must not live there -- a counter that
  resets each turn makes the duty permanent. Declare them beside the run-level
  `captures_used` (`:1134`), which is outside the loop for the same reason:

```rust
    /// Marked frames this run has actually been shown. The duty discharges on
    /// the first one, not on the tool call: six photographs and two that came
    /// back is a review of two charts.
    let mut marked_frames_seen = 0usize;
    /// The refusal is spent once. A second one is a run burning turns on a
    /// mistake it has already been told about.
    let mut marked_refusal_used = false;
```

and increment in the rendezvous loop from Task 5 -- inside the `Some(found)` arm,
before the image block is pushed (`capture` is Task 5's loop variable, not
`request`):

```rust
                                if capture.marked {
                                    marked_frames_seen += 1;
                                }
```

- [ ] **Step 4: The duty at the answer door.** Beside `drawing_owed`
  (`:1210-1212`):

```rust
            // Mentor mode owes the second look. A run that drew the zones and
            // then wrote the plan from the plain pictures is the exact failure
            // the two-pass review exists to prevent, and the model will do it:
            // the analysis is finished as far as its numbers go.
            let marked_review_owed =
                request.teaching && ctx.captures.is_some() && marked_frames_seen == 0;
```

`ctx.captures.is_some()` is the discharge the spec asks for: a server-side run
with no screen never owes a photograph. (`answering` is `turn >= max_turns`, so
this bool is recomputed every turn -- a frame that lands switches the duty off on
the next one, which is what test 2 above proves.)

Then widen the branch that already narrows the answer-phase tool set
(`:1213-1236`), so a refused model is handed the call it was told to make:

```rust
            let duties_owed = drawing_owed || marked_review_owed;
```

```rust
                if duties_owed {
                    let mut set = vec![submit_thesis_spec()];
                    if drawing_owed {
                        // submit + the write side of the drawing family. The read
                        // side is deliberately absent: a live run stalled here
                        // re-reading get_user_drawings twice instead of drawing,
                        // and the drawings it might re-read were already listed in
                        // the analysis phase.
                        set.extend(tools.iter().filter(|s| {
                            matches!(
                                s.name.as_str(),
                                "create_drawing" | "update_drawing" | "delete_drawing"
                            )
                        }).cloned());
                    }
                    if marked_review_owed {
                        // The refusal's instruction is "call review_marked_charts".
                        // A tool set without it makes that impossible to obey, and
                        // the run would spend both answer turns hitting a wall the
                        // gate built -- the record would say the model refused when
                        // the orchestrator never offered the tool.
                        set.extend(
                            tools.iter()
                                .filter(|s| s.name.as_str() == "review_marked_charts")
                                .cloned(),
                        );
                    }
                    (set, Some(ToolChoice::Any))
                } else {
                    (
                        vec![submit_thesis_spec()],
                        Some(ToolChoice::Tool(SUBMIT_THESIS.into())),
                    )
                }
```

The `else` side and the forced `ToolChoice::Tool(SUBMIT_THESIS)` stay exactly as
they are; the drawing family keeps its own condition so a run that owes only the
drawing does not suddenly gain a capture call, and vice versa.
- [ ] **Step 5: The refusal, once.** After the existing drawing-duty refusal
    block (`:1512-1538`) and before `finalize` (`:1545`):

```rust
                    // The marked duty, acted on once. Refusing a thesis on the
                    // last turn would end the run with no answer at all, which
                    // is worse for the user than an answer that records it never
                    // re-checked -- so the record is written either way and only
                    // the refusal is gated on `!last`. Same posture as the
                    // drawing duty and the grounding correction above it. `owed`
                    // is computed at the top of this turn, so a model that called
                    // the review tool in the same response as the submit has
                    // already been shown frames by now -- re-read the counter
                    // rather than the stale bool, or the record would claim a pass
                    // that ran.
                    if marked_review_owed && marked_frames_seen == 0 && !marked_refusal_used {
                        marked_refusal_used = true;
                        trace.push(ToolTrace {
                            tool: "review_marked_charts".into(),
                            args: serde_json::json!({}),
                            result: serde_json::json!({
                                "skipped": true,
                                "reason": "the thesis was submitted before any marked \
                                           frame landed",
                            }),
                        });
                        // `thesis.provenance` was cloned from `trace` at the top
                        // of this block, so the row only reaches the answer when
                        // it is re-cloned here: the card has to be able to say
                        // the run never re-checked, not just the server log.
                        thesis.provenance = trace.clone();
                        if !last {
                            tracing::warn!(
                                target: "ai_agent",
                                "thesis submitted before the marked pass; asking for the second look"
                            );
                            progress.report(Progress::Correcting {
                                reason: "You have not looked at the marked charts. Call \
                                         `review_marked_charts` and confirm the trend, entry, \
                                         target and stop against the frames that carry your \
                                         own drawings, then submit again."
                                    .into(),
                            });
                            messages.push(Message::tool_results(vec![ToolResult {
                                tool_use_id: call.id.clone(),
                                content: serde_json::json!({
                                    "accepted": false,
                                    "error": "no marked frame has been seen this run -- the \
                                              zones are on the chart but the plan was written \
                                              from the plain pictures",
                                    "instruction": "Call review_marked_charts, read the six \
                                                    frames it returns, and submit a thesis the \
                                                    marked charts actually support.",
                                }),
                                is_error: true,
                            }]));
                            continue;
                        }
                    }
```

`trace` (`:929`), `last`, `call.id`, `messages`, `progress` and `thesis` are all
already in scope at that point -- the drawing-duty block above uses the same
`progress.report` + `messages.push(Message::tool_results(..))` + `continue`
shape, and `trace` is the run-level `Vec<ToolTrace>` that `:1502` clones into
`thesis.provenance`.
- [ ] **Step 6: A run that discharges cleanly** needs no note -- `marked_frames_seen
  > 0` means the duty was never owed. Add the assertion to Step 1's second test
  that its `provenance` contains **no** `skipped` row, which is what keeps the
  record from becoming a log of things that did not happen.
- [ ] **Step 7: `cargo test -p ai-agent`** → green, including
  `teaching_mode_rewrites_the_job_in_the_prompt` and
  `a_lesson_draws_in_the_answer_phase_without_a_skill` (a mentor test that never
  submits a thesis must not be broken by a duty that fires at the submit door;
  its tool-set assertions at `:5049-5050` are `contains`, so a wider answer set
  cannot fail them),
  `an_ordinary_question_gets_no_lesson_prompt` (no teaching, no duty) and
  `a_run_reports_the_steps_it_took`. Then `cargo clippy -p ai-agent --all-targets
  -- -D warnings`.
- [ ] **Step 8: Commit** `feat: a lesson confirms its plan against the marked charts`

---

## Task 9: The lesson is told in two passes

The mechanism is built; the model still has to be told the shape of the run.
Mentor mode's prompt is the only place a phase order exists, and the duty in
Task 8 refuses a thesis without saying what the lesson was supposed to look like.

**Files:** modify `crates/ai-agent/src/agent.rs` (`:2164-2174` the prompt
signature, `:951-984` its call site, `:2280-2319` the `can_draw` branch and its
fallback), `frontend/app/app.js` (`:11891` the turn literal, `:10298-10304` the
review caption), `tools/shell_check.mjs`.

**Interfaces:**
- Consumes: `review_marked_charts` (Task 6), `marked` on the wire (Task 4),
  `mentorMode` (`app.js:11996`) -- the ask already sends it as
  `teaching: mentorMode` on the socket (`:11910`), and Step 3 records it on the
  turn object so the caption can read it.
- Produces: prompt text containing `Two passes, in this order`, `PASS 1` and
  `PASS 2`; `turn.teaching`; the plain-pass caption `AI REVIEW · PLAIN · …` for a
  teaching run only.

- [ ] **Step 1: The failing prompt test**, beside `teaching_mode_rewrites_the_job_in_the_prompt`
  (`:4901`) -- same fixture, same one-turn thesis, one more assertion block:

```rust
    /// The two passes are a promise the shell renders and the duty enforces,
    /// and a model that is never told the order will not guess it. Asserted on
    /// the prompt the orchestrator actually sent, as the drawing assertions are.
    #[tokio::test]
    async fn a_lesson_is_told_to_look_twice() {
        let source = Fixture::new();
        let writer = Arc::new(RecordingWriter::default());
        let drawings = DrawingsContext::new(Arc::new(NoDrawings), "test-user").with_writer(writer);
        // No capture source: this test is about the words the model is handed,
        // and an attached screen would put Task 8's duty on the door, which is
        // Task 8's own test's subject.
        let llm = Arc::new(ScriptedClient::new(vec![thesis_call(
            100_100.0, 100_000.0, 100_400.0,
        )]));
        let agent = Agent::new(llm.clone(), SkillLibrary::new(), AgentConfig::default());

        let request = AskRequest::new("BTCUSDT", "walk me through this chart")
            .with_drawings(drawings)
            .with_teaching(true);
        agent
            .ask(&request, &source)
            .await
            .expect("the lesson completes");

        let system = system_prompt_of(&llm);
        assert!(
            system.contains("Two passes, in this order"),
            "a lesson with no stated order: {system}"
        );
        assert!(
            system.contains("PASS 1") && system.contains("PASS 2"),
            "the passes have to be nameable, or the model cannot say which it is on"
        );
        assert!(
            system.contains("review_marked_charts"),
            "the second pass must be named as the tool it is: {system}"
        );
    }
```

No new `ScriptedClient` API is needed -- one thesis response is enough, because
this run attaches no capture source and so owes no refusal. Run
`cargo test -p ai-agent a_lesson_is_told_to_look_twice` → all three assertions
fail, which is the expected state.

- [ ] **Step 2: The prompt block.** `ask_system_prompt`
  (`agent.rs:2164-2174`) already takes `can_draw: bool, teaching: bool`; add
  `can_look: bool` after `can_draw`, and pass it at the single call site
  (`:974-984`) from the request, beside where `can_draw` is computed (`:951`):

```rust
        // Whether the run can photograph the user's screen at all. The teaching
        // prompt promises a second look, and promising it to a run with no
        // screen attached is the same broken promise the drawing text refuses to
        // make for a run with no writer.
        let can_look = request.captures.is_some();
```

```rust
            can_draw,
            can_look,
            request.teaching,
```

Then in the `teaching` branch (`:2280`), which already splits on `can_draw`,
append to the `can_draw` text after the polyline paragraph and before the closing
`\n\n`:

```rust
                 Two passes, in this order, when a screen is attached:\n\
                 PASS 1 -- the plain look. The screenshots riding this question \
                 are every timeframe before anything is drawn on them. Read them \
                 top-down and name what each one shows; the numbers behind them \
                 come from the data tools, as always.\n\
                 MARK -- draw what you just described, as objects, one point at \
                 a time.\n\
                 PASS 2 -- call `review_marked_charts`. It photographs every \
                 timeframe of the ladder again, now carrying your zones, and the \
                 images arrive with that call's results captioned `PASS 2 · <TF> \
                 · marked`. Confirm the trend, the entry, the target and the stop \
                 against THOSE frames, not the plain ones: a level that reads \
                 somewhere else once it is drawn next to the candles is a level \
                 you have to move, and moving it is the lesson. A frame that did \
                 not arrive is named as missing -- never describe a chart you \
                 were not shown.\n\
                 Only then submit the thesis.\n\n",
```

That text is the `can_look` half of the branch, so the `if can_draw {` body
becomes `if can_draw { if can_look { …the block above… } else { …the sentence
below… } }` -- the same shape the `teaching` split already uses, and the reason
`can_look` is a parameter rather than a word in the prose:

```rust
                 There is no screen attached to this run, so there is no second \
                 pass: nothing you draw can be photographed back to you. Teach \
                 from the plain frames and the tool numbers, and say so.\n\n",
```

and to the `!can_draw` fallback (`:2311-2318`), one sentence so the no-writer
case does not go looking for a marked chart it cannot make:

```rust
                 "## Teaching mode\n\
                  The user wants to understand this chart. Walk the ladder \
                  top-down and explain each resolution in plain words. You \
                  cannot draw on their chart in this session -- say which \
                  objects you would have marked (the level, the zone, the slope) \
                  instead of pretending the marks exist. With nothing drawn \
                  there is no second pass to look at: `review_marked_charts` \
                  would photograph the same plain charts, so do not spend a turn \
                  on it.\n\n",
```

- [ ] **Step 3: The plain frames say they are plain.** `turn` (`app.js:11891`) is
  created as `{ question, symbol, steps, answer, error }` -- it has no `teaching`
  field, so add one there, next to `symbol`, in the same literal:

```js
  const turn = {
    question,
    symbol: activeSymbol(),
    // The run keeps the flag it was asked with. Reading the module-level
    // `mentorMode` from the review code would caption a lesson's frames plain
    // the moment the user flips the chip off mid-run.
    teaching: mentorMode,
    steps: [],
    answer: null,
    error: null,
  };
```

Then in `showAiReviewFrame` (`:10298`), beside the `counter` line:

```js
  // A lesson shows the same six timeframes twice, and the two passes mean
  // opposite things: one is the chart as it is, the other is the chart with the
  // agent's marks on it. Only a teaching run has a second pass, so only it needs
  // the first one labelled -- an ordinary multi-timeframe review is one pass and
  // would be captioned as if it were missing the other.
  const pass = turn.teaching ? " · PLAIN" : "";
```

and used in the existing `caption` template, between the two words it already
joins: `` `AI REVIEW${pass} · ${symbol.toUpperCase()} · ${timeframe.toUpperCase()}${counter}` ``.
Leave Task 4's `if (progress.marked) return;` above it: the marked path never
reaches here.

- [ ] **Step 4: Harness.** In `shell_check.mjs`'s agent-answer section, ask a
  second time with the mentor chip on -- `document.getElementById("mentorMode").click()`
  (`app.js:17955` flips the module-level `mentorMode` the ask reads), then
  `questionBox.value = "teach me this setup"; askButton.click(); await settle();`
  and take `socketsFor("/ws/agent").at(-1)` as the socket for this run. Deliver
  a plain ladder frame on it and assert the card it mounts is labelled:

```js
const mentorSocket = socketsFor("/ws/agent").at(-1);
deliver(mentorSocket, {
  type: "progress",
  payload: { stage: "review_frame", symbol: askedSymbol, timeframe: "4h", index: 1, total: 6 },
});
await settle();

check(
  "a lesson labels its first pass plain",
  reviewCardTitles().some((title) => title.includes("AI REVIEW · PLAIN")),
  reviewCardTitles().join(" | ")
);
```

Click the chip again at the end of the block so `mentorMode` is off for every
section that runs after it -- a leaked teaching flag would silently change what
the later capture sections ask for.
- [ ] **Step 5: The whole sweep.** Everything above, in the order the repo's
  gates run:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
export NODE_PATH="/Users/macbook/.workbuddy-node/node_modules"
node tools/shell_check.mjs
node tools/wasm_abi_check.mjs frontend/app/chart_engine.wasm
node tools/ui_check.mjs
node tools/component_check.mjs
node tools/check_builder.mjs
node tools/concept_visual_audit_check.mjs
```

Expected: all green except the known `api-gateway` `load_flow` free-plan bot-cap
failure (`stopping_one_bot_leaves_the_feed_working_for_the_others`), which is
documented debt and must not be "fixed" here.
- [ ] **Step 6: Commit** `feat: a lesson looks at the chart twice, and says which look it is on`
- [ ] **Step 7: Report to the product owner.** State that the feature is complete
  in code and what live acceptance would cost -- a rebuild
  (`cargo build -p api-gateway`), a gateway restart **only on their word**, and
  one mentor question through the :8080 UI: six plain cards, marks landing, six
  marked cards, then a plan drawn as bounded zones. Do not run it unasked.

---

## Cost, stated plainly

A mentor question now spends 12 images and two extra turns (the marked call and
the refused thesis) on top of what it spent before. That is the price of the loop
the product owner asked for, and the budget constants in Task 5 are where it is
set: `MENTOR_CAPTURES_PER_RUN` and the ordinary `MAX_CAPTURES_PER_RUN`, which an
ordinary run never leaves at 3.
