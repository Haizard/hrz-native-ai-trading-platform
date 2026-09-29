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

/// The reference script quoted verbatim in the system prompt. Every form in
/// it is implemented in `pine-lite`'s interpreter -- the prompt teaches the
/// VM's actual surface (its builtin table is copied from the dispatchers in
/// `interp.rs`), never the doc's wishlist, so the model is never baited into
/// calls that can only fail at run time.
const REFERENCE_SCRIPT: &str = "//@pine_lite version=1 overlay=true title=\"Bands + MACD signals\"\n\
    length = input.int(defval=20, title=\"Band length\")\n\
    mult = input.float(defval=2.0, title=\"Band mult\")\n\
    basis, upper, lower = ta.bb(close, length, mult)\n\
    macd_line, signal_line, hist = ta.macd(close, 12, 26, 9)\n\
    plot(basis, title=\"Basis\", color=color.gray)\n\
    plot(upper, title=\"Upper\", color=color.blue)\n\
    plot(lower, title=\"Lower\", color=color.blue)\n\
    buy = ta.crossover(macd_line, signal_line)\n\
    sell = ta.crossunder(macd_line, signal_line)\n\
    plotshape(buy, shape=\"triangleup\", color=color.green, location_value=low)\n\
    plotshape(sell, shape=\"triangledown\", color=color.red, location_value=high)";

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

- `overlay=true` draws on the price chart (levels, bands drawn in price units); `overlay=false` gives the script its own pane below the chart (oscillators like RSI, stoch).
- `title` names the pane and legend.

## Syntax rules (the compiler is strict)
1. ONE statement per line. Each statement ends at the newline. Never put two statements on one line; never use `;`.
2. Indent block bodies by exactly 4 spaces under `if` / `else` / `for`.
3. Declare with `=`. Reassign with `:=`. `var x = ...` initializes once and keeps its value across bars (running counters, state).
4. Square brackets are ONLY a history offset on a named series variable or a built-in series, with an integer: `close[1]`, `myVar[3]`, `high[0]`. NOTHING else may ever be in brackets: no list literals, no `ta.sma(close, 9)[1]`, no `foo()[0]`. To compare a function result with its past, assign it first: `s = ta.sma(close, 9)` then `s[1]`. Collections exist as ARRAY FUNCTIONS, never as bracket syntax: `a = array.new()`, `array.push(a, v)`, `array.get(a, i)`. Writing `a[i] = v` is a syntax error.
5. Multi-value calls are destructured on one line: `basis, upper, lower = ta.bb(close, 20, 2)`. The names must match the callee's output count. `ta.macd` and `ta.bb` return three values, `ta.stoch` returns two (%K, %D) -- and `ta.stoch` also reads as its %K alone.
6. Numbers are floats; bools are `true`/`false`; `na` is the missing value (it propagates; comparisons with it are false). Ternary: `cond ? a : b`. Logic: `and`, `or`, `not`. Comparisons: `< <= > >= == !=`.
7. `if cond` / `else` blocks contain assignments only (no `plot` inside them); a block body must not be empty.
8. Loops: `for i = 0 to 99` (optional `by 2`) with a 4-space indented body. Keep loops small; budgets are enforced.
9. `//` starts a comment.

## Built-in series (bare names)
`open`, `high`, `low`, `close`, `volume`, `hl2`, `hlc3`, `ohlc4`, `bar_index`, `time`

## Functions (these are ALL of them; a function not listed here does not exist)
Averages and oscillators: `ta.sma(source, length)`, `ta.ema(source, length)`, `ta.rma(source, length)`, `ta.wma(source, length)`, `ta.rsi(source, length)`, `ta.highest(source, length)`, `ta.lowest(source, length)`, `ta.mom(source, length)`, `ta.roc(source, length)`, `ta.change(source)`, `ta.atr(length)`, `ta.tr()`, `ta.vwap()`, `ta.stoch(source, length)` (returns 0..100).
Multi-value (destructure them): `macd_line, signal_line, hist = ta.macd(source, fast, slow, signal)`, `basis, upper, lower = ta.bb(source, length, mult)`, `k, d = ta.stoch(source, length)`.
Crosses (return bool): `ta.crossover(a, b)`, `ta.crossunder(a, b)`, `ta.cross(a, b)`.
Math: `math.abs(x)`, `math.floor(x)`, `math.ceil(x)`, `math.round(x)`, `math.sqrt(x)`, `math.log(x)`, `math.exp(x)`, `math.sign(x)`, `math.pow(a, b)`, `math.min(a, b, ...)`, `math.max(a, b, ...)`, `math.avg(a, b, ...)`, `math.sum(source, length)`.
Checks: `na(x)`, `nz(x)`, `nz(x, fallback)`, `fixnan(x)`.
## Arrays (for zones, pivots, levels -- SMC/market-structure building blocks)
A variable holds a whole array once created with `var` (allocate ONCE, at the top):

    var swing_highs = array.new()
    var swing_bars = array.new()
    if pivot_high
        array.push(swing_highs, high[w])
        array.push(swing_bars, bar_index - w)

Readers: `array.size(a)`, `array.get(a, i)`, `array.first(a)`, `array.last(a)`, `array.min(a)`, `array.max(a)`, `array.avg(a)`, `array.includes(a, v)`. Mutators: `array.push(a, v)`, `array.pop(a)`, `array.shift(a)`, `array.clear(a)`, `array.set(a, i, v)` (only existing indexes).
Guards: out-of-bounds `array.get` returns `na` (wrap with `nz(...)` when the value feeds arithmetic); never allocate without `var` (the heap cap kills the script); pop/shift when a list could grow unbounded. A state machine over two parallel arrays (values + bar stamps) builds order blocks, FVGs, breaker blocks and session levels.

## A second instrument (cross-market math: SMT divergence, spreads, ratios)
The chart is {symbol}; a correlated second instrument rides along when the header declares it:

    //@pine_lite version=1 overlay=false title="SMT" sec="ETHUSDT"

With `sec=` set, these zero-argument calls give the pair's series, bar-aligned to the chart's own bars: `request.symbol()` (the pair's ticker, as a nonzero number), `request.open()`, `request.high()`, `request.low()`, `request.close()`, `request.volume()`. `request.close()` and `close` at the same bar are the same moment, so spreads (`close - request.close()`), ratios (`close / request.close()`) and SMT divergence (our new swing high while the pair makes a lower high) are plain arithmetic. History works as on any series -- assign first, then offset: `rc = request.close()` then `rc[1]`.
- The pair trades on its own clock: where it printed no bar, its last bar carries forward flat (same OHLC repeats), so `request.*` values stall rather than gap. The host fetches and aligns the pair's candles automatically; the script never does.
- Every `request.*` call requires `sec=` in the header -- the vet refuses `request.close()` without it. `request.security(...)` does NOT exist; the pair is chosen once in the header, not per-call.

## Time of day (session filters)
`hour` (0..23 UTC), `minute` (0..59), `dayofweek` (1=Sunday .. 7=Saturday, Pine's convention). London open is about `hour == 7`; New York is about `hour >= 12 and hour < 21`. An Asian-session level: capture the high/low while `hour >= 0 and hour < 7` into `var` scalars and reset at `hour == 0`.

## Drawing
- `plot(series, title="...", color=color.blue, linewidth=1)` -- a line. Optional `style="histogram"` / `style="columns"` / `style="circles"` / `style="stepline"` / `style="areabr"` / `style="linebr"`.
- `hline(price, color=color.gray, linestyle=dashed)` -- a horizontal level (e.g. `hline(70)`).
- `plotshape(cond, shape="triangleup", color=color.green, location_value=low)` -- a marker on each bar where cond is true. `shape=` names the glyph ("triangleup", "triangledown", "circle", "cross"); `location_value=` is the price the marker sits at (`low` under a buy, `high` above a sell). Omit `location_value=` and the marker falls to the bottom of the pane (or the bottom of the price range, in an overlay script). `plotchar(cond, char="B")` prints a letter; `plotarrow(cond)` draws an arrow. Markers work in overlay scripts and pane scripts alike.
- Colors: color.red, color.green, color.blue, color.orange, color.yellow, color.purple, color.teal, color.lime, color.aqua, color.white, color.gray, color.maroon, color.navy, color.olive, color.silver, color.fuchsia, color.black.

## Inputs (user-adjustable parameters)
`len = input.int(defval=14, title="RSI Length")`, `input.float(defval=2.0, title="Mult")`, `input.bool(defval=true, title="Show signals")`. Use them instead of magic numbers.

## Drawing objects (anchored lines, labels, boxes)
Statements that add to a drawing heap (cap 64 per script):

    if pivot_high
        line.new(bar_index - w, high[w], bar_index, close, color=color.blue, style="solid", width=1)
        label.new(bar_index - w, high[w], "pivot", color=color.red)
    if demand_zone
        box.new(zone_left, zone_top, zone_right, zone_bottom, color=color.green)

Coordinates are bar indexes and PRICES. Draw inside `if` blocks or guard with `bar_index == 0` -- a top-level `line.new` runs EVERY bar and fills the heap in ~64 bars (a refusal). `style=` is "solid"/"dashed"/"dotted".

## Pattern detection WITHOUT arrays (there is no `a[i] = v` assignment)
You cannot build lists or arrays -- not with brackets, not any other way. Track state with `var` scalars that persist across bars and reassign inside `if` blocks. The two idioms you need:

1. Swing/pivot detection with a centered window (a swing high w bars ago that is the highest of its 2w+1 neighbors):

    w = input.int(defval=4, title="Pivot window")
    hh = ta.highest(high, 2 * w + 1)
    ll = ta.lowest(low, 2 * w + 1)
    pivot_high = hh[w] == high[w]
    pivot_low = ll[w] == low[w]

2. A state machine that remembers the last two pivots and where they happened:

    var ph1 = na
    var ph2 = na
    var ph1_bar = 0.0
    var ph2_bar = 0.0
    if pivot_high
        ph2 := ph1
        ph2_bar := ph1_bar
        ph1 := high[w]
        ph1_bar := bar_index - w

CRITICAL: stamp the pivot with `bar_index - w`, the bar where the swing actually happened -- a centered pivot is only confirmed w bars later. Stamping `bar_index` (the confirmation bar) anchors the old pivot value at today's bar, which displaces every projected line w bars right and makes it wobble across price instead of passing through the swing highs/lows.

Slopes come from the stamps: `slope_high = (not na(ph2) and ph1_bar != ph2_bar) ? (ph1 - ph2) / (ph1_bar - ph2_bar) : na`. A projected line through the last two pivots: `level = ph1 + slope_high * (bar_index - ph1_bar)`. Plot `level` to draw the line.

Slope thresholds must be SCALE-FREE -- never compare a price-per-bar slope against an absolute constant (that breaks between a 2-dollar stock and an 85000-dollar coin). Use ATR: `atr = ta.atr(14)`, then "flat" means `math.abs(slope_high) <= atr * 0.15` and "rising" means `slope_low >= atr * 0.05`. This pivot + state + slope pattern builds triangles, channels, flags and breakouts -- all without arrays.

## Trading simulation (only when the user asks for signals/entries)
Orders fire only where the line runs; there is NO `when=` parameter. Gate orders with an `if` block:

    if ta.crossover(close, macd_line)
        strategy.entry("Long", direction="long")

`strategy.entry(id, direction="long" or "short")`, `strategy.exit(id, from_entry="Long", stop=price)`, `strategy.close(id)`, `strategy.close_all()`. An order fills on the NEXT bar's open.

## Non-negotiables
- The `//@pine_lite` header is MANDATORY on the first line.
- Keep `plot`/`hline`/`plotshape` calls at the top level (not inside `for` loops or `if` bodies).
- The script is self-contained: no imports, no network calls. The ONLY cross-symbol access is `request.*` with `sec=` in the header, and the pair always runs on the chart's own timeframe -- there is no per-call symbol or timeframe argument.
- Never use brackets except the `series[int]` history form from rule 4. Never use commas outside a call's arguments.

## Reference script (exactly this shape of code, vetted and working)

```
{REFERENCE_SCRIPT}
```
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

/// The second-instrument symbol a script wants, without asking the model:
/// an explicit `sec=SEC` line the model sometimes writes as a comment, else
/// a quoted ALL-CAPS symbol from the user request. Models forget the header
/// knob constantly; the host can derive it deterministically.
fn wanted_sec_symbol(source: &str, user_request: &str) -> Option<String> {
    for line in source.lines() {
        let t = line.trim();
        if t.starts_with("//@") && !t.contains("@pine_lite") {
            if let Some(pos) = t.to_uppercase().find("SEC=") {
                let rest = t[pos + 4..].trim().trim_start_matches('"');
                let sym: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':' || *c == '.').collect();
                if !sym.is_empty() {
                    return Some(sym.to_uppercase());
                }
            }
        }
    }
    let m = user_request
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
        .find(|w| {
            let b = w.as_bytes();
            b.len() >= 6 && b[b.len() - 4..] == *b"USDT" && b.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
    ?;
    Some(m.to_string())
}

/// When the ONLY vet problems are missing-`sec=` refusals, fix the header
/// here instead of spending a model round trip on it: append `sec="SYM"` to
/// the annotation and re-vet. Returns the corrected source on success.
fn autofix_sec(source: &str, user_request: &str) -> Option<String> {
    let errs = pine_lite::vet(source).err()?;
    let only_missing_sec = errs
        .iter()
        .all(|e| e.message.contains("needs a second instrument"));
    if !only_missing_sec {
        return None;
    }
    let sym = wanted_sec_symbol(source, user_request)?;
    let mut lines: Vec<String> = source.lines().map(String::from).collect();
    let header = lines.first_mut()?;
    if !header.contains("@pine_lite") {
        return None;
    }
    *header = format!("{} sec=\"{}\"", header.trim_end(), sym);
    let fixed = lines.join("\n");
    pine_lite::vet(&fixed).is_ok().then_some(fixed)
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
                // The one failure with a deterministic fix is a missing
                // header knob: if every refusal is "needs a second
                // instrument" and the request names a pair, append `sec=`
                // here rather than burning a model round trip teaching
                // header syntax the model keeps getting wrong.
                if errs
                    .iter()
                    .all(|e| e.message.contains("needs a second instrument"))
                {
                    if let Some(fixed) = autofix_sec(&source, user_request) {
                        let (header, _script) =
                            pine_lite::vet(&fixed).expect("autofix re-vet");
                        repaired_errors.push(format!(
                            "autofix: appended sec=\"{}\" to the header",
                            header.sec.clone().unwrap_or_default()
                        ));
                        return Ok(GeneratedScript {
                            source: fixed,
                            attempts: attempt,
                            repaired_errors,
                            header,
                        });
                    }
                }
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

    #[test]
    fn the_reference_script_vets_clean() {
        // The prompt quotes this script as the shape to imitate; if it ever
        // stops vetting, the prompt is teaching failure.
        assert!(pine_lite::vet(REFERENCE_SCRIPT).is_ok(), "{}", vet_report(REFERENCE_SCRIPT));
    }

    #[test]
    fn a_multi_value_call_read_as_a_scalar_names_the_destructuring_fix() {
        // The old prompt's mistake, now caught by the language itself: a MACD
        // line is three values and the error says how to take them apart.
        let macd = "//@pine_lite version=1\nm = ta.macd(close, 12, 26, 9)\nplot(m)\n";
        let errs = pine_lite::vet(macd).expect_err("macd needs destructuring");
        assert!(errs.iter().any(|e| e.span.line == 2), "{errs:?}");
        assert!(
            errs.iter().any(|e| e.message.contains("a, b, c = ta.macd")),
            "{errs:?}"
        );
    }

    #[test]
    fn the_builtins_list_matches_the_interpreter() {
        // Every call shape the prompt teaches must vet. Keep this list in sync
        // with `script_system_prompt` -- a drifting table baits the model into
        // calls the repair loop can only bounce.
        let src = concat!(
            "//@pine_lite version=1 overlay=false title=\"t\"\n",
            "a = ta.sma(close, 9)\n",
            "b = ta.ema(close, 9)\n",
            "c = ta.rma(close, 9)\n",
            "d = ta.rsi(close, 14)\n",
            "e = ta.highest(high, 20)\n",
            "f = ta.lowest(low, 20)\n",
            "g = ta.mom(close, 10)\n",
            "h = ta.roc(close, 10)\n",
            "i = ta.change(close)\n",
            "j = ta.atr(14)\n",
            "k = ta.tr()\n",
            "l = ta.vwap()\n",
            "m = ta.stoch(close, 14)\n",
            "m2, d2 = ta.stoch(close, 14)\n",
            "w = ta.wma(close, 9)\n",
            "b1, b2, b3 = ta.bb(close, 20, 2)\n",
            "g1, g2, g3 = ta.macd(close, 12, 26, 9)\n",
            "n = ta.crossover(a, b)\n",
            "o = ta.crossunder(a, b)\n",
            "p = ta.cross(a, b)\n",
            "q = math.abs(a)\n",
            "r = math.pow(a, 2.0)\n",
            "s = math.min(a, b)\n",
            "t = math.max(a, b)\n",
            "u = math.avg(a, b)\n",
            "v = math.sum(close, 9)\n",
            "w = math.round(a) + math.floor(a) + math.ceil(a)\n",
            "x = math.sqrt(math.abs(a)) + math.log(a + 1.0) + math.exp(-a) + math.sign(a)\n",
            "y = na(a) ? 0.0 : nz(a, 1.0)\n",
            "plot(a)\n",
        );
        assert!(pine_lite::vet(src).is_ok(), "{}", vet_report(src));
    }

    #[test]
    fn history_off_a_call_is_refused_at_vet_time() {
        // `ta.sma(close, 9)[1]` -- the model's favorite shortcut; the VM only
        // indexes variables and builtin series, so vet must catch it.
        let src = "//@pine_lite version=1\nx = ta.sma(close, 9)[1]\nplot(x)\n";
        let errs = pine_lite::vet(src).expect_err("history needs a variable");
        assert!(!errs.is_empty(), "{errs:?}");
    }

    /// The idioms section of the system prompt, verbatim, wrapped in a
    /// runnable script. If the prompt teaches a form the VM refuses, this
    /// catches it before any model does.
    const IDIOM_SCRIPT: &str = concat!(
        "//@pine_lite version=1 overlay=false title=\"Pivots\"\n",
        "w = input.int(defval=4, title=\"Pivot window\")\n",
        "hh = ta.highest(high, 2 * w + 1)\n",
        "ll = ta.lowest(low, 2 * w + 1)\n",
        "pivot_high = hh[w] == high[w]\n",
        "pivot_low = ll[w] == low[w]\n",
        "var ph1 = na\n",
        "var ph2 = na\n",
        "var ph1_bar = 0.0\n",
        "var ph2_bar = 0.0\n",
        "if pivot_high\n",
        "    ph2 := ph1\n",
        "    ph2_bar := ph1_bar\n",
        "    ph1 := high[w]\n",
        "    ph1_bar := bar_index - w\n",
        "slope_high = (not na(ph2) and ph1_bar != ph2_bar) ? (ph1 - ph2) / (ph1_bar - ph2_bar) : na\n",
        "atr = ta.atr(14)\n",
        "flat = math.abs(slope_high) <= atr * 0.15\n",
        "level = ph1 + slope_high * (bar_index - ph1_bar)\n",
        "plot(level)\n",
    );

    #[test]
    fn the_taught_idioms_vet_and_run() {
        let errs = pine_lite::vet(IDIOM_SCRIPT);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
        let (_, parsed) = pine_lite::vet(IDIOM_SCRIPT).ok().unwrap();
        let candles: Vec<analytics_core::types::Candle> = (0..60)
            .map(|i| {
                let close = 100.0 + (i as f64) * 0.5 + (i as f64) * 0.25 % 3.0;
                analytics_core::types::Candle {
                    symbol: "TEST".into(),
                    timeframe: analytics_core::types::Timeframe::M1,
                    open_time: 0,
                    open: close,
                    high: close + 1.0,
                    low: close - 1.0,
                    close,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                }
            })
            .collect();
        let inputs = pine_lite::Inputs::default();
        let output = pine_lite::run(&parsed, &candles, &inputs).expect("the idioms run");
        assert!(!output.plots.is_empty(), "the projected line plots");
    }

    /// The multi-symbol idiom the prompt teaches, verbatim: an SMT divergence
    /// sweep between the chart and a declared `sec=` pair. If the prompt ever
    /// teaches a `request.*` form the language refuses, this catches it.
    const SMT_IDIOM_SCRIPT: &str = concat!(
        "//@pine_lite version=1 overlay=false title=\"SMT divergence\" sec=\"ETHUSDT\"\n",
        "w = input.int(defval=3, title=\"Pivot window\")\n",
        "hh = ta.highest(high, 2 * w + 1)\n",
        "ll = ta.lowest(low, 2 * w + 1)\n",
        "pivot_high = hh[w] == high[w]\n",
        "pivot_low = ll[w] == low[w]\n",
        "var ph1 = na\n",
        "var ph2 = na\n",
        "var ph1_bar = 0.0\n",
        "var ph2_bar = 0.0\n",
        "var ph1_sec = na\n",
        "if pivot_high\n",
        "    ph2 := ph1\n",
        "    ph2_bar := ph1_bar\n",
        "    ph1 := high[w]\n",
        "    ph1_bar := bar_index - w\n",
        "    ph1_sec := request.high()\n",
        "var pl1_sec = na\n",
        "if pivot_low\n",
        "    pl1_sec := request.low()\n",
        "smt_bear = not na(ph2) and request.high() < ph1_sec and ph1 > ph2\n",
        "smt_bull = not na(pl1_sec) and request.low() > pl1_sec\n",
        "spread = close - request.close()\n",
        "plot(smt_bear ? 1.0 : 0.0, title=\"SMT bear\", style=\"histogram\")\n",
        "plot(smt_bull ? 1.0 : 0.0, title=\"SMT bull\", style=\"histogram\")\n",
        "plot(spread, title=\"Spread\")\n",
    );

    #[test]
    fn the_taught_smt_idiom_vets_and_runs() {
        let errs = pine_lite::vet(SMT_IDIOM_SCRIPT);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
        let (_, parsed) = pine_lite::vet(SMT_IDIOM_SCRIPT).ok().unwrap();
        // Ours rises while the pair falls -- the divergence genuinely fires.
        let candles: Vec<analytics_core::types::Candle> = (0..60)
            .map(|i| {
                let close = 100.0 + (i as f64) * 0.5 + (i as f64) * 0.25 % 3.0;
                analytics_core::types::Candle {
                    symbol: "TEST".into(),
                    timeframe: analytics_core::types::Timeframe::M1,
                    open_time: 0,
                    open: close,
                    high: close + 1.0,
                    low: close - 1.0,
                    close,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                }
            })
            .collect();
        let security: Vec<analytics_core::types::Candle> = candles
            .iter()
            .enumerate()
            .map(|(i, c)| analytics_core::types::Candle {
                symbol: "ETHUSDT".into(),
                timeframe: analytics_core::types::Timeframe::M1,
                open_time: c.open_time,
                open: 50.0,
                high: 51.0 - (i as f64) * 0.2,
                low: 49.0,
                close: 50.0,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            })
            .collect();
        let inputs = pine_lite::Inputs { security, ..pine_lite::Inputs::default() };
        let output = pine_lite::run(&parsed, &candles, &inputs).expect("the SMT idiom runs");
        assert_eq!(output.plots.len(), 3, "two histograms + the spread");
    }

    #[test]
    fn autofix_adds_sec_when_the_body_needs_it() {
        // The live failure: correct `request.*` body, header without the
        // knob, request names the pair. One deterministic pass, no model.
        let src = "//@pine_lite version=1 overlay=true title=\"SMT\"\nrc = request.close()\nplot(close - rc)\n";
        let fixed = autofix_sec(src, "SMT divergence against ETHUSDT").expect("autofix");
        assert!(fixed.contains("sec=\"ETHUSDT\""), "{fixed}");
        assert!(pine_lite::vet(&fixed).is_ok());
    }

    #[test]
    fn autofix_prefers_the_comment_declared_symbol() {
        let src = "//@pine_lite version=1\n//@sec=BNBUSDT\nrc = request.close()\nplot(rc)\n";
        let fixed = autofix_sec(src, "SMT vs SOLUSDT").expect("autofix");
        assert!(fixed.contains("sec=\"BNBUSDT\""), "{fixed}");
    }

    #[test]
    fn autofix_leaves_other_errors_to_the_model() {
        // A real syntax error is NOT auto-fixable: no sec is appended.
        let src = "//@pine_lite version=1\nrc = request.close(\n";
        assert!(autofix_sec(src, "vs ETHUSDT").is_none());
    }

    #[test]
    fn indexed_assignment_is_refused() {
        // The exact construct from the live failure: `arr[i] = v`. It parses
        // as a call expression followed by `=`, so vet must refuse it with a
        // position the repair loop can name.
        let src = "//@pine_lite version=1\nvar arr = 0.0\narr[3] = close\nplot(arr)\n";
        let errs = pine_lite::vet(src).expect_err("no indexed assignment");
        assert!(errs.iter().any(|e| e.span.line == 3), "{errs:?}");
    }

    /// The array + session idioms the prompt teaches, runnable end to end:
    /// collect pivot highs into arrays, project the line through the last
    /// two, and gate the setup by hour. The SMC/liquidity shape, minus the
    /// second instrument SMT needs.
    const ARRAY_IDIOM_SCRIPT: &str = "//@pine_lite version=1 overlay=true title=\"Swing structure\"\n\
        w = input.int(defval=4, title=\"Pivot window\")\n\
        var ph = array.new()\n\
        var phb = array.new()\n\
        hh = ta.highest(high, 2 * w + 1)\n\
        pivot_high = hh[w] == high[w]\n\
        if pivot_high\n\
            array.push(ph, high[w])\n\
            array.push(phb, bar_index - w)\n\
        n = array.size(ph)\n\
        res = n >= 2 ? array.get(ph, n - 1) + (array.get(ph, n - 1) - array.get(ph, n - 2)) / (array.get(phb, n - 1) - array.get(phb, n - 2)) * (bar_index - array.get(phb, n - 1)) : na\n\
        in_session = hour >= 7 and hour < 20\n\
        plot(res, title=\"Resistance\", color=color.orange)\n\
        plotshape(close > res and in_session, shape=\"triangledown\", color=color.red, location_value=high)\n";

    #[test]
    fn the_taught_array_idioms_vet_and_run() {
        let errs = pine_lite::vet(ARRAY_IDIOM_SCRIPT);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
        let (_, parsed) = pine_lite::vet(ARRAY_IDIOM_SCRIPT).ok().unwrap();
        let candles: Vec<analytics_core::types::Candle> = (0..80)
            .map(|i| {
                let close = 100.0 + (i as f64) * 0.5 + ((i % 9) as f64) * 0.6;
                analytics_core::types::Candle {
                    symbol: "TEST".into(),
                    timeframe: analytics_core::types::Timeframe::M1,
                    open_time: 1_790_553_600_000_000_000i64 + (i as i64) * 60_000_000_000,
                    open: close,
                    high: close + 1.2,
                    low: close - 1.2,
                    close,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                }
            })
            .collect();
        let inputs = pine_lite::Inputs::default();
        let output = pine_lite::run(&parsed, &candles, &inputs).expect("the array idioms run");
        assert!(!output.plots.is_empty());
        // The resistance ray through the last two pivots is finite by the end.
        let res = &output.plots[0].values;
        assert!(res[79].is_finite(), "resistance projected: {}", res[79]);
    }
}
