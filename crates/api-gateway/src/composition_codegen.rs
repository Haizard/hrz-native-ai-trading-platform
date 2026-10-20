//! The AI composition flow: natural language → a validated composition.
//!
//! Mirrors `pine_codegen`'s shape exactly, because the constraint is the
//! same: the model is an *architect over a closed registry*, never an
//! inventor. The system prompt carries the concept catalog (availability-
//! gated for the ask's symbol, from [`market_data::concepts`]) and the
//! document schema; the submit tool is the only way out; every draft
//! validates against the registry before it reaches the user, and the
//! validation errors ride the repair pass verbatim — so "smc.ob does not
//! exist" comes back as the correction that fixes it, not a shrug.

use ai_agent::llm_client::{ContentBlock, LlmClient, LlmRequest, Message, Role, ToolSpec};
use serde::Serialize;

/// The repair budget (the pine flow's own).
pub const MAX_ATTEMPTS: usize = 3;

/// One generation attempt's outcome.
#[derive(Debug, Serialize)]
pub struct GeneratedComposition {
    /// Whether the final draft validates against the registry.
    pub valid: bool,
    /// The composition document, when it parsed.
    pub document: Option<market_data::composer::Composition>,
    /// Validation errors on the final draft, human-named.
    pub errors: Vec<String>,
    /// The model's report (what it composed and why).
    pub report: String,
    /// How many drafts it took.
    pub attempts: usize,
}

/// The system prompt: catalog, schema, rules, one example.
#[must_use]
pub fn composition_system_prompt(symbol: &str) -> String {
    let catalog = market_data::concepts::available_for(symbol)
        .iter()
        .map(|c| {
            let params = c
                .parameters
                .iter()
                .map(|p| format!("{} (default {}, {}..{})", p.name, p.default, p.min, p.max))
                .collect::<Vec<_>>()
                .join("; ");
            format!("- `{}` — {}. {}{}", c.id, c.name, c.description,
                if params.is_empty() { String::new() } else { format!(" Parameters: {params}.") })
        })
        .collect::<Vec<_>>()
        .join("\n");
    let unavailable = market_data::concepts::concept_registry()
        .iter()
        .filter(|c| !market_data::concepts::available_for(symbol).contains(c))
        .map(|c| format!("- `{}` — needs {} which {symbol} does not serve", c.id, c.required_channels.names().join(", ")))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "You are a strategy architect for the Concept Registry. The user describes a trading strategy in words; you compose it from REGISTERED concepts and nothing else. A concept that is not listed here DOES NOT EXIST — composing one is the same class of error as inventing an indicator.\n\n\
         ## Concepts available on {symbol}\n{catalog}\n\n\
         {unavailable_section}\n\n\
         ## The document schema (JSON, submitted via the tool)\n\
         ```json\n{{\n\
           \"name\": \"Liquidity Reversal\",\n\
           \"version\": \"1.0.0\",\n\
           \"market\": \"{symbol}\",\n\
           \"timeframe\": \"15m\",\n\
           \"side\": \"long\",\n\
           \"conditions\": {{\n\
             \"type\": \"sequence\",\n\
             \"max_bars\": 8,\n\
             \"steps\": [\n\
               {{\"condition\": {{\"type\": \"concept\", \"concept\": \"smc.liquidity_sweep\", \"direction\": \"bullish\"}}}},\n\
               {{\"condition\": {{\"type\": \"concept\", \"concept\": \"smc.bos\", \"direction\": \"bullish\"}}}}\n\
             ]\n\
           }},\n\
           \"risk\": {{\n\
             \"stop\": {{\"type\": \"structure\"}},\n\
             \"sizing\": {{\"type\": \"fixed_fractional\", \"percent\": 5}}\n\
           }}\n\
         }}\n```\n\
         Condition forms: `concept` (a leaf, with direction bullish/bearish/any and optional lookback_bars), `all` / `any` / `not` (groups), and `sequence` (ordered steps, each within max_bars of the last). Stops: `structure` (at the satisfying event's level) or `{{\"type\": \"atr_multiple\", \"multiple\": 2}}`. Sizing: `fixed_fractional` percent or `{{\"type\": \"fixed\", \"quantity\": 1}}`.\n\n\
         ## Rules\n\
         - Use ONLY concepts from the available list. The unavailable list exists so you can explain refusals, never so you can use them.\n\
         - Sequence steps must have a trader's logic: liquidity taken BEFORE structure shifts, structure BEFORE retracement entries.\n\
         - `market` must be exactly \"{symbol}\"; `timeframe` is one the platform serves (1m, 3m, 5m, 15m, 30m, 1h, 2h, 4h, 6h, 8h, 12h, 1d, 3d, 1w).\n\
         - Submit the complete document every attempt — a partial one validates nothing.\n\
         - The report covers: what the composition waits for in order, what the stop keys on, and what it deliberately does NOT try to catch. 3-6 sentences, no code.\n",
        unavailable_section = if unavailable.is_empty() {
            String::new()
        } else {
            format!("## Registered but unavailable on {symbol} (do NOT use)\n{unavailable}")
        }
    )
}

/// Generate a composition from a description, with the validation-repair loop.
pub async fn generate_composition(
    llm: &dyn LlmClient,
    symbol: &str,
    description: &str,
    max_tokens: u32,
) -> Result<GeneratedComposition, ai_agent::AgentError> {
    let system = composition_system_prompt(symbol);
    let tool = ToolSpec {
        name: "submit_composition".to_string(),
        description: "Submit the composition document for registry validation.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "document": {"type": "string", "description": "The complete composition document as a JSON string (name, version, market, timeframe, side, conditions, risk)."},
                "report": {"type": "string", "description": "The conversational report the user reads."}
            },
            "required": ["document", "report"]
        }),
        exposed_tool: None,
    };
    let tools = vec![tool];
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text(format!("## Strategy description\n{description}"))],
    }];
    let mut report = String::from("no composition was submitted");
    let mut last_document: Option<market_data::composer::Composition> = None;
    let mut last_errors: Vec<String> = Vec::new();

    for attempt in 1..=MAX_ATTEMPTS {
        let response = llm
            .complete(LlmRequest {
                system: Some(system.clone()),
                messages: messages.clone(),
                tools: tools.clone(),
                tool_choice: Some(ai_agent::llm_client::ToolChoice::Tool("submit_composition".to_string())),
                max_tokens,
                temperature: 0.2,
            })
            .await?;
        let calls = response.tool_calls();
        let Some(call) = calls.first() else {
            // The model answered without the tool: ask for it, once more.
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text(
                    "Submit the composition through the `submit_composition` tool — the document goes nowhere without it.".to_string(),
                )],
            });
            continue;
        };
        let input = call.input.clone();
        report = input
            .get("report")
            .and_then(|v| v.as_str())
            .unwrap_or(&report)
            .to_string();
        // The document arrives as a JSON string (provider tool schemas do not
        // carry nested free-form objects; the pine flow's string field is the
        // same accommodation).
        let parsed: Result<market_data::composer::Composition, String> = input
            .get("document")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "the tool call carried no document".to_string())
            .and_then(|text| serde_json::from_str(text).map_err(|e| format!("the document did not parse: {e}")));
        match parsed {
            Ok(document) => {
                last_errors = market_data::composer::validate(&document)
                    .err()
                    .map(|errs| errs.iter().map(ToString::to_string).collect())
                    .unwrap_or_default();
                last_document = Some(document);
            }
            Err(e) => {
                last_errors = vec![format!("the document did not parse: {e}")];
                last_document = None;
            }
        }
        if last_errors.is_empty() {
            return Ok(GeneratedComposition {
                valid: true,
                document: last_document,
                errors: Vec::new(),
                report,
                attempts: attempt,
            });
        }
        // The repair pass: the validation errors go back verbatim.
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text(format!(
                "The composition failed registry validation:\n{}\n\nFix it and submit the COMPLETE corrected document.",
                last_errors.join("\n")
            ))],
        });
    }
    Ok(GeneratedComposition {
        valid: false,
        document: last_document,
        errors: last_errors,
        report,
        attempts: MAX_ATTEMPTS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_repair_loop_fixes_an_invented_concept() {
        use ai_agent::llm_client::{LlmResponse, Message, Role, StopReason, Usage};

        // First draft invents `smc.ob`; the repair message must come back
        // with the registry's refusal, and the second draft uses a real one.
        let llm = ai_agent::llm_client::ScriptedClient::new(vec![
            LlmResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![ai_agent::llm_client::ContentBlock::ToolUse {
                        id: "c1".to_string(),
                        name: "submit_composition".to_string(),
                        input: serde_json::json!({
                            "document": "{\"name\":\"x\",\"version\":\"1.0.0\",\"market\":\"BTCUSDT\",\"timeframe\":\"15m\",\"side\":\"long\",\"conditions\":{\"type\":\"concept\",\"concept\":\"smc.ob\",\"direction\":\"bullish\"},\"risk\":{\"stop\":{\"type\":\"structure\"},\"sizing\":{\"type\":\"fixed\",\"quantity\":1}}}",
                            "report": "first"
                        }),
                    }],
                },
                stop_reason: StopReason::ToolUse,
                usage: Usage { input_tokens: None, output_tokens: None },
            },
            LlmResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![ai_agent::llm_client::ContentBlock::ToolUse {
                        id: "c2".to_string(),
                        name: "submit_composition".to_string(),
                        input: serde_json::json!({
                            "document": "{\"name\":\"x\",\"version\":\"1.0.0\",\"market\":\"BTCUSDT\",\"timeframe\":\"15m\",\"side\":\"long\",\"conditions\":{\"type\":\"concept\",\"concept\":\"smc.bos\",\"direction\":\"bullish\"},\"risk\":{\"stop\":{\"type\":\"structure\"},\"sizing\":{\"type\":\"fixed\",\"quantity\":1}}}",
                            "report": "fixed"
                        }),
                    }],
                },
                stop_reason: StopReason::ToolUse,
                usage: Usage { input_tokens: None, output_tokens: None },
            },
        ]);
        let generated = generate_composition(&llm, "BTCUSDT", "a bos play", 4096)
            .await
            .expect("generated");
        assert!(generated.valid, "the repair fixed it: {:?}", generated.errors);
        assert_eq!(generated.attempts, 2, "one refusal, one fix");
        assert_eq!(generated.document.expect("doc").conditions, market_data::composer::Condition::Concept {
            concept: "smc.bos".to_string(),
            direction: market_data::composer::DirectionReq::Bullish,
            lookback_bars: None,
        });
        // The repair message went back with the registry's own words.
        let requests = llm.requests();
        assert_eq!(requests.len(), 2, "a refusal, then the fix");
        let repair = requests[1].messages.last().expect("repair message");
        let repair_text = format!("{repair:?}");
        assert!(repair_text.contains("smc.ob"), "the refusal named the invented id: {repair_text}");
        assert!(repair_text.contains("not a registered concept"), "{repair_text}");
    }

    #[test]
    fn the_prompt_carries_the_gated_catalog_and_the_schema() {
        let prompt = composition_system_prompt("BTCUSDT");
        assert!(prompt.contains("`smc.bos`"), "the catalog is in the prompt");
        assert!(prompt.contains("`order_flow.delta_shift`"), "the full list too");
        assert!(prompt.contains("submit"), "the tool instruction");
        assert!(prompt.contains("\"type\": \"sequence\""), "the schema example");
        assert!(!prompt.contains("unavailable on BTCUSDT"), "spot gates nothing");

        let deriv = composition_system_prompt("DERIV:VOLATILITY 10");
        assert!(deriv.contains("unavailable on DERIV:VOLATILITY 10"), "the refusal list");
        assert!(deriv.contains("trades_aggressor"), "with the reason");
    }
}
