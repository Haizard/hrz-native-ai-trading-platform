//! AI generation of pine-lite scripts (`docs/23` Phase 9, the studio flip).
//!
//! The studio's chat used to ask the model for a `kind: indicator` *document*;
//! it now asks for a **pine-lite script** -- imperative code with `plot()`,
//! the shape the user asked for when they asked for TradingView-style
//! generation. The pipeline is the document one's mirror:
//!
//! 1. a system prompt that teaches the language's real surface (builtins,
//!    header, grammar) and the platform's non-negotiables (the header, no
//!    unbounded loops),
//! 2. script extraction from the model's reply -- a fenced ```pine_lite
//!    block, or the whole reply when it is bare code,
//! 3. the validate-repair loop over [`pine_lite::vet`]: every refusal goes
//!    back verbatim (kind, line, col, message), and the artifact that
//!    survives is guaranteed to lex, parse, type-check and fit the limits --
//!    the same guarantee `POST /scripts/vet` gives, applied to the model.
//!
//! The loop lives here rather than in `ai-agent` because it needs no LLM
//! plumbing of its own -- just [`ai_agent::LlmClient`] -- and because the
//! gateway owns the vet pipeline.

use ai_agent::llm_client::{
    ContentBlock, LlmClient, LlmRequest, Message, Role, ToolChoice, ToolSpec,
};

/// How many repair rounds the model gets. The document loop uses 5; scripts
/// fail on more specific, more fixable complaints (a missing import, a wrong
/// arity), so the same budget is generous.
const MAX_ATTEMPTS: usize = 5;

/// The system prompt: the language's whole surface, taught compactly.
///
/// Everything listed here exists in the interpreter -- the builtin table is
/// copied from `pine-lite`'s dispatchers, so the model is never taught a
/// function that would refuse at run time. The non-negotiables (mandatory
/// header, bounded loops, one statement per line) are the platform's vetting
/// rules stated as authoring advice, which is where they cost the least.
pub(crate) fn script_system_prompt(symbol: &str, timeframe: &str) -> String {
    format!(
        r#"You write indicators as **pine-lite** scripts: TradingView-Pine-style imperative code the platform compiles and runs bar-by-bar. The user's chart is {symbol} on the {timeframe} timeframe; write for that.

## Output contract
Reply with ONE fenced code block tagged pine_lite containing the whole script. No prose outside the block. The FIRST line must be the header:

//@pine_lite version=1 overlay=false title="My Indicator"

- `overlay=true` draws on the price chart (levels, bands drawn in price units); `overlay=false` gives the script its own pane below the chart (oscillators like RSI, MACD, stoch).
- `title` names the pane and legend.

## Language
- One statement per line. Indentation (4 spaces) groups `if`/`else`/`for` bodies. `//` comments.
- Declare with `=`. Reassign with `:=`. `var x = ...` initializes once and keeps its value across bars (running counters, state).
- Series indexing: `close[1]` is the previous bar's close. `na` is the missing value; it propagates, and comparisons against it are false.
- Numbers are floats. `int` promotes to `float`; a bool is never a number. Strings only inside `title=`/`char=`.
- Ternary: `cond ? a : b`. Logic: `and`, `or`, `not`. Comparison: `< <= > >= == !=`.
- `if cond` / `else` blocks may contain assignments and plot calls.
- Loops: `for i = 0 to 10` (optionally `by 2`), bounded -- iteration and step budgets are enforced and a script that exceeds them is killed.

## Built-in series
`open`, `high`, `low`, `close`, `volume`

## Functions (exactly these exist; arguments in this order)
Trend: ta.sma(source, length), ta.ema(source, length), ta.rma(source, length), ta.rsi(source, length), ta.atr(length), ta.tr, ta.vwap(source), ta.stoch(source, high, low, length), ta.macd(source, fast, slow, signal) [returns 3 values: destructure as a, b, c = ta.macd(...)]
Momentum/structure: ta.change(source), ta.mom(source, length), ta.roc(source, length), ta.cross(a, b), ta.crossover(a, b), ta.crossunder(a, b), ta.highest(source, length), ta.lowest(source, length)
Math: math.abs, math.min, math.max, math.avg, math.sum, math.round, math.floor, math.ceil, math.sqrt, math.pow, math.exp, math.log, math.sign (all two-arg where two args apply)

## Drawing
- `plot(series, title="...", color=color.blue, linewidth=1)` -- a line in the pane (or on price when overlay=true).
- `hline(price, color=color.gray)` -- a horizontal reference level.
- `plotshape(cond, style=style.triangleup, color=color.green, location=location.belowbar)` -- a marker when cond is true. Styles: style.triangleup, style.triangledown, style.triangleup_small, style.triangledown_small, style.circle, style.cross, style.labelup, style.labeldown. Locations: location.abovebar, location.belowbar. Use plotchar with `char="X"` for a letter marker.
- Colors: color.blue, color.green, color.red, color.orange, color.purple, color.aqua, color.fuchsia, color.lime, color.gray, color.white, color.yellow.

## Inputs (user-adjustable parameters)
`len = input.int(defval=14, title="RSI Length")` (also input.float, input.bool). Use them instead of magic numbers.

## Trading simulation (only when the user asks for signals/entries)
`strategy.entry("Long", strategy.long, when=cond)`, `strategy.entry("Short", strategy.short, when=cond)`, `strategy.exit("Stop", from_entry="Long", stop=price, when=cond)`, `strategy.close("Long", when=cond)`. Pine's fill rule is built in: an order fills on the NEXT bar's open.

## Non-negotiables
- The `//@pine_lite` header is MANDATORY; a script without it is refused.
- plot()/hline()/plotshape() calls at the top level (not inside for loops).
- The script must be self-contained: no imports, no requests for other symbols or timeframes.
"#,
        symbol = symbol,
        timeframe = timeframe,
    )
}

/// Pull the script out of the model's reply.
///
/// Preference order: a fenced ```pine_lite block (what the system prompt
/// demands), then any fenced block (some models drop the tag), then the whole
/// text -- which handles bare code and lets the vet pipeline reject prose by
/// its own light. The fence scan is line-oriented rather than a global
/// regex because a script may legitimately contain backticks in strings.
#[must_use]
pub(crate) fn extract_script(reply: &str) -> Option<String> {
    let lines: Vec<&str> = reply.lines().collect();
    // First pass: a block explicitly tagged pine_lite (or pine/pinescript,
    // which models reach for out of habit and which is still code).
    for opening in ["```pine_lite", "```pine", "```pinescript"] {
        if let Some(start) = lines.iter().position(|l| l.trim().eq_ignore_ascii_case(opening)) {
            if let Some(end_rel) = lines[start + 1..]
                .iter()
                .position(|l| l.trim() == "```")
            {
                let end = start + 1 + end_rel;
                return Some(lines[start + 1..end].join("\n"));
            }
        }
    }
    // Second pass: any fence pair whose opening is bare ```.
    let mut open: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "```" {
            match open {
                Some(s) => return Some(lines[s + 1..i].join("\n")),
                None => open = Some(i),
            }
        } else if trimmed.starts_with("```") && open.is_none() {
            open = Some(i);
        }
    }
    // Fall through: the whole reply. `vet` refuses it with precise errors if
    // it is not code, and those errors are exactly what the repair loop wants.
    let whole = reply.trim();
    (!whole.is_empty()).then(|| whole.to_string())
}

/// The outcome of a generation turn.
#[derive(Debug, Clone)]
pub(crate) struct GeneratedScript {
    /// The source that passed every vetting layer.
    pub source: String,
    /// How many model round trips it took (1 = first try).
    pub attempts: usize,
    /// Every validator refusal that was repaired along the way, for the
    /// transcript's honesty fields.
    pub repaired_errors: Vec<String>,
    /// The vetted header, for display and overlay routing.
    pub header: pine_lite::Header,
}

/// One generation attempt's failure, verbatim from the vet pipeline.
fn vet_report(source: &str) -> String {
    match pine_lite::vet(source) {
        Ok(_) => String::new(),
        Err(errs) => errs
            .iter()
            .map(|e| format!("line {} col {} [{}]: {}", e.span.line, e.span.col, pine_lite::kind_name(e.kind), e.message))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Generate and repair a script: the studio's code path.
///
/// Sends the system prompt with the user's request, extracts the script,
/// vets it, and on failure feeds the FULL issue list back as a tool result --
/// the same shape the document loop keeps, because the model corrects from
/// precise, complete error lists far better than from "invalid".
///
/// # Errors
/// [`AgentError`] when the LLM transport fails or every attempt is spent
/// without a script that vets; the error text carries the last report.
pub(crate) async fn generate_script(
    llm: &dyn LlmClient,
    system: String,
    user_request: &str,
    base_script: Option<&str>,
    max_tokens: u32,
    temperature: f32,
) -> Result<GeneratedScript, ai_agent::AgentError> {
    let description = match base_script {
        // Edit mode: the current script rides the first message, so "make the
        // bands tighter" tightens the bands instead of starting over -- the
        // document loop's same rule, in code.
        Some(base) => format!(
            "## Current script\n```pine_lite\n{base}\n```\n\n## Requested change\n{user_request}"
        ),
        None => user_request.to_string(),
    };
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text(description)],
    }];
    let tool = ToolSpec {
        name: "submit_script".to_string(),
        description: "Submit the pine-lite script for validation and chart attachment."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "script": {"type": "string", "description": "The complete pine-lite script source, including the //@pine_lite header line."}
            },
            "required": ["script"]
        }),
    };
    let tools = vec![tool];
    let mut repaired_errors = Vec::new();
    let mut last_report = String::from("no script was submitted");

    for attempt in 1..=MAX_ATTEMPTS {
        let response = llm
            .complete(LlmRequest {
                system: Some(system.clone()),
                messages: messages.clone(),
                tools: tools.clone(),
                tool_choice: Some(ToolChoice::Tool("submit_script".to_string())),
                max_tokens,
                temperature,
            })
            .await?;
        let calls = response.message.tool_calls();
        let Some(call) = calls.iter().find(|c| c.name == "submit_script") else {
            last_report = "the reply did not call submit_script".to_string();
            messages.push(response.message.clone());
            messages.push(Message::user(
                "Respond by calling submit_script with the complete pine-lite script.",
            ));
            continue;
        };
        // The tool argument is the primary channel; the fenced block in any
        // accompanying text is the fallback (models do both).
        let source = call.input["script"]
            .as_str()
            .map(str::to_string)
            .or_else(|| extract_script(&response.message.text()));
        let Some(source) = source else {
            last_report = "`script` must be a string".to_string();
            messages.push(response.message.clone());
            messages.push(Message::user(
                "Call submit_script with the whole script as the string value of `script`.",
            ));
            continue;
        };

        match pine_lite::vet(&source) {
            Ok((header, _script)) => {
                return Ok(GeneratedScript {
                    source,
                    attempts: attempt,
                    repaired_errors,
                    header,
                });
            }
            Err(errs) => {
                last_report = vet_report(&source);
                repaired_errors.extend(errs.iter().map(|e| e.message.clone()));
                messages.push(response.message.clone());
                messages.push(Message::tool_results(vec![ai_agent::llm_client::ToolResult {
                    tool_use_id: call.id.clone(),
                    content: serde_json::json!({
                        "valid": false,
                        "errors": errs.iter().map(|e| serde_json::json!({
                            "kind": pine_lite::kind_name(e.kind),
                            "line": e.span.line,
                            "col": e.span.col,
                            "message": e.message,
                        })).collect::<Vec<_>>(),
                        "instruction": "Fix every listed error in the full script and call submit_script again with the COMPLETE corrected script.",
                    }),
                    is_error: true,
                }]));
            }
        }
    }
    Err(ai_agent::AgentError::InvalidStrategyDocument(
        strategy_dsl::DslError::Validation {
            issues: vec![strategy_dsl::ValidationIssue::new(
                "script",
                format!(
                    "the script did not pass validation after {MAX_ATTEMPTS} attempts. Last errors: {last_report}"
                ),
            )],
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSI: &str = concat!(
        "```pine_lite\n",
        "//@pine_lite version=1 overlay=false title=\"RSI\"\n",
        "len = input.int(defval=14, title=\"Length\")\n",
        "r = ta.rsi(close, len)\n",
        "plot(r, title=\"RSI\", color=color.purple)\n",
        "hline(70)\n",
        "hline(30)\n",
        "```\n",
    );

    #[test]
    fn extracts_a_tagged_block() {
        let reply = format!("Here is your indicator:\n{RSI}Let me know if you want changes.");
        let script = extract_script(&reply).expect("script");
        assert!(script.starts_with("//@pine_lite"));
        assert!(script.contains("ta.rsi(close, len)"));
        assert!(!script.contains("```"));
    }

    #[test]
    fn extracts_an_untagged_block() {
        let reply = "Sure:\n```\n//@pine_lite version=1\nplot(close)\n```\ndone";
        let script = extract_script(reply).expect("script");
        assert!(script.contains("plot(close)"));
    }

    #[test]
    fn prose_alone_yields_itself_for_vetting() {
        let script = extract_script("no code here").expect("whole reply");
        assert_eq!(script, "no code here");
    }

    #[test]
    fn vet_reports_accept_the_good_script() {
        let script = extract_script(RSI).expect("script");
        assert!(pine_lite::vet(&script).is_ok(), "{}", vet_report(&script));
    }

    #[test]
    fn vet_reports_carry_positions_for_repair() {
        let bad = "//@pine_lite version=1\nx = close + true\nplot(x)\n";
        let report = vet_report(bad);
        assert!(report.contains("line 2"), "{report}");
    }
}
