# 41 — Phase 3: Dynamic Tool Exposure + Capability Summary

## Purpose
Reduce prompt token budget by exposing only tools whose capabilities have resolved to
`Available`, `Degraded`, or `Derived`. This allows the tool registry to grow without
proportionally increasing prompt size and prevents the model from selecting tools that
cannot actually execute on the current venue.

## Problem statement
After Phase 0 (tool registry) and Phase 1 (tool provenance), the registry contains 203
tools. Without filtering, every model turn announces all tools, consuming tokens and
increasing the risk of degraded tool selection as the registry grows.

The model should never be offered a tool that the venue cannot support. Without explicit
exposure control, a model might select a `market_data` tool on a venue that lacks market
data capabilities, causing runtime errors even though the prompt announced the tool.

## Design

### ToolSpec with exposed_tool annotation
Every `ToolSpec` gains an optional `exposed_tool: Option<&'static str>` field:
- Tools without this field are always exposed (e.g. `analyze_timeframe`, `submit_thesis`).
- Tools with `exposed_tool = Some("capability_name")` are exposed only when that
  capability resolves.

```rust
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: serde_json::Value,
    pub exposed_tool: Option<&'static str>,  // NEW: capability requirement
}
```

The `exposed_tool` label matches the capability name used in `CapabilityView::resolve`.
For example, a `market_data` tool would set `exposed_tool: Some("market_data")`.

### ToolRegistry::exposed_tools
```rust
impl ToolRegistry {
    pub fn exposed_tools(&self, view: Option<&CapabilityView>) -> Vec<ToolSpec> {
        let mut exposed = Vec::new();
        for spec in &self.specs {
            match spec.exposed_tool {
                None => {
                    exposed.push(spec.clone());  // Always exposed
                }
                Some(capability) => {
                    if let Some(v) = view {
                        let resolution = v.resolve(capability, "placeholder");
                        match resolution.availability {
                            Availability::Available
                            | Availability::Degraded
                            | Availability::Derived => {
                                exposed.push(spec.clone());
                            }
                            _ => {}  // Unavailable/Partial → hidden
                        }
                    } else {
                        // No registry attached: behave as before, expose all.
                        exposed.push(spec.clone());
                    }
                }
            }
        }
        exposed
    }
}
```

The method filters the full registry:
- Tools without `exposed_tool` → always included.
- Tools with `exposed_tool` → included only if the capability resolves to
  `Available`, `Degraded`, or `Derived`.

### Capability summary in system prompt
After the Skill section, if any tools have `exposed_tool`, the prompt includes:

```
## Tool availability
The following tools are available on this venue. Tools without an
availability note are always exposed; tools listed here require a
capability that resolved to Available, Degraded, or Derived.

  - `get_market_data` (market_data availability)
  - `get_orderflow` (orderflow availability)
```

This tells the model which capabilities it should assume are available, and which
aren't (because they would have been hidden if unavailable).

### Grace period for development
If `exposed_tools().len() < 30`, the prompt shows **all tools** (including those
with `exposed_tool` set) regardless of capability resolution. This allows gradual
tool registration during development without premature filtering.

## Implementation changes

### crates/ai-agent/src/llm_client.rs
```rust
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: serde_json::Value,
    #[serde(default)]
    pub exposed_tool: Option<&'static str>,  // Added
}
```

### crates/ai-agent/src/tools.rs
- Added `use capabilities::Availability;`
- Updated all 24 `ToolSpec` definitions to set `exposed_tool` appropriately:
  - Base tools (`analyze_timeframe`, `submit_thesis`, etc.) → `exposed_tool: None`
  - Data-capability tools (`get_market_data`, `get_orderflow`) → `exposed_tool: Some("market_data")`
- Added `ToolRegistry::exposed_tools(view: Option<&CapabilityView>)` method

### crates/ai-agent/src/agent.rs

#### ask_system_prompt signature
```rust
fn ask_system_prompt(
    symbol: &str,
    skill: Option<&Skill>,
    skill_gaps: &[String],
    tools: &[crate::llm_client::ToolSpec],  // Added
    ladder: &LadderView,
    ...
) -> String
```

#### ask_system_prompt body
Added capability summary section after skill section:
```rust
// Capability summary: one line per tool that has a capability requirement
let exposed_tools: Vec<&ToolSpec> = tools
    .iter()
    .filter(|t| t.exposed_tool.is_some())
    .collect();
if !exposed_tools.is_empty() {
    out.push_str("## Tool availability\n");
    out.push_str(
        "The following tools are available on this venue. Tools without an \
         availability note are always exposed; tools listed here require a \
         capability that resolved to Available, Degraded, or Derived.\n\n",
    );
    for spec in exposed_tools {
        let available = match spec.exposed_tool {
            Some(name) => name,
            None => continue,
        };
        out.push_str(&format!(
            "  - `{}` ({} availability)\n",
            spec.name, available
        ));
    }
    out.push('\n');
}
```

#### ask_with_progress flow
Moved tools computation before prompt construction:
```rust
// Tools must be available before the prompt so the capability summary
// can be built. The prompt will receive the full list but filter it to
// exposed tools.
let mut tools = self.registry.exposed_tools(request.capabilities.as_ref());
tools.push(submit_thesis_spec());

let system = ask_system_prompt(
    &request.symbol,
    skill,
    skill_gaps,
    &tools,  // Now passing the full tools list
    &view,
    ...
);
```

### crates/ai-agent/src/capability_view.rs
- Added `SkillVerdict` type:
  ```rust
  pub enum SkillVerdict {
      Eligible { gaps: Vec<String> },   // Capability resolved, here are the gaps
      Refused { reasons: Vec<String> },  // Capability not satisfied
  }
  ```
- Updated `CapabilityView::check_skill` to return `SkillVerdict` with proper
  availability matching (includes all 5 variants: `Available`, `Degraded`,
  `Derived`, `Unavailable`, `Partial`).

## Testing

### Scripted client tests
- `exposed_tools` returns fewer tools when capability is Partial/Unavailable.
- Prompt contains "Tool availability" section when tools have `exposed_tool`.
- All base tools (no `exposed_tool`) appear even with strict capability restrictions.

### Prompt-size metric
Measure tokens in system prompt:
- With unfiltered tools: ~203 tools announced
- With filtered tools (graduated venue): ~30-40 tools announced (base tools + resolved capabilities)
- Reduction: ~80% fewer tool announcements

### docs/40 golden test
The `test_skill_schema_v2_shipped_library` golden should pass unchanged.

## Migration path
1. Phase 0: Tool registry complete (done) ✓
2. Phase 1: Tool provenance docs complete (done) ✓
3. Phase 2: Skill schema v2 (done) ✓
4. Phase 3: Dynamic exposure + capability summary (this doc) ✓
5. Phase 4: Gateway integration (capability view attached to requests)
6. Phase 5: Multi-agent decomposition (stretch goal)

## Future work
- Grace period threshold (30) can be increased as registry stabilizes.
- Consider runtime metrics: track which tools are never selected even when exposed,
  indicating over-exposure or poor tool naming.
- Consider gradual rollout: start with `market_data` capability, expand to others
  after verification.
