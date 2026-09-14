//! AWS Bedrock **Converse** provider.
//!
//! Converse (not InvokeModel) is the right endpoint for this system because it
//! is the one with a provider-agnostic tool-use contract: `toolConfig` in,
//! `toolUse` out, `toolResult` back in. InvokeModel would mean speaking each
//! model family's own prompt format for function calling, which is exactly the
//! kind of per-provider special-casing the [`LlmClient`] trait exists to avoid.
//!
//! Verified against the live API (2026-09-13, `tools/bedrock_check.py`):
//!
//! * the configured model id is accepted **bare** -- no inference-profile ARN;
//! * a `toolConfig` produces a real `stopReason: "tool_use"` with a populated
//!   `toolUse` block, not prose that imitates a call;
//! * feeding a `toolResult` back yields `end_turn` with the numbers used.
//!
//! The third check also produced the finding that shapes `thesis.rs`: the model
//! **reformats** numbers it is given (`103250.5` came back as `$103,250.50`).
//! Nothing in this crate ever recovers a number by parsing prose.

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::error::AgentError;
use crate::llm_client::{
    ContentBlock, LlmClient, LlmRequest, LlmResponse, Message, Role, StopReason, ToolChoice, Usage,
};
use crate::sigv4;

/// Environment variable read for the region.
pub const ENV_REGION: &str = "AWS_BEDROCK_REGION";
/// Environment variable read for the model id.
pub const ENV_MODEL_ID: &str = "AWS_BEDROCK_MODEL_ID";

/// Configuration for the Bedrock provider.
#[derive(Debug, Clone)]
pub struct BedrockConfig {
    /// Region, e.g. `us-east-1`.
    pub region: String,
    /// Model id as Bedrock spells it, e.g. `qwen.qwen3-coder-next`.
    pub model_id: String,
    /// Access key id.
    pub access_key: String,
    /// Secret access key.
    pub secret_key: String,
    /// Session token, for temporary credentials.
    pub session_token: Option<String>,
    /// Request timeout in seconds. Long: a reasoning model walking a timeframe
    /// ladder can legitimately take a minute.
    pub timeout_secs: u64,
}

impl BedrockConfig {
    /// Read the configuration from the environment.
    ///
    /// # Errors
    /// Returns [`AgentError::NotConfigured`] naming the missing variable, so a
    /// half-filled `.env` fails with the one thing to fix rather than with a
    /// signature error five minutes later.
    pub fn from_env() -> Result<Self, AgentError> {
        let region = std::env::var(ENV_REGION).unwrap_or_else(|_| "us-east-1".to_string());
        let model_id = std::env::var(ENV_MODEL_ID)
            .map_err(|_| AgentError::NotConfigured(format!("{ENV_MODEL_ID} is not set")))?;
        let access_key = std::env::var("AWS_ACCESS_KEY_ID")
            .map_err(|_| AgentError::NotConfigured("AWS_ACCESS_KEY_ID is not set".into()))?;
        let secret_key = std::env::var("AWS_SECRET_ACCESS_KEY")
            .map_err(|_| AgentError::NotConfigured("AWS_SECRET_ACCESS_KEY is not set".into()))?;
        let session_token = std::env::var("AWS_SESSION_TOKEN").ok();

        Ok(Self {
            region,
            model_id,
            access_key,
            secret_key,
            session_token,
            timeout_secs: 180,
        })
    }
}

/// A Converse-backed [`LlmClient`].
pub struct BedrockClient {
    config: BedrockConfig,
    http: reqwest::Client,
    name: String,
}

impl BedrockClient {
    /// Build a client for the given configuration.
    ///
    /// # Errors
    /// Returns [`AgentError::Llm`] if the HTTP client cannot be constructed.
    pub fn new(config: BedrockConfig) -> Result<Self, AgentError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| AgentError::Llm(format!("cannot build http client: {e}")))?;
        let name = format!("bedrock({})", config.model_id);
        Ok(Self { config, http, name })
    }

    /// Build from the environment.
    ///
    /// # Errors
    /// See [`BedrockConfig::from_env`].
    pub fn from_env() -> Result<Self, AgentError> {
        Self::new(BedrockConfig::from_env()?)
    }

    /// The model id in use.
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.config.model_id
    }

    /// `(url, host, path)`.
    ///
    /// The path is built **once** and used for both the request URL and the
    /// SigV4 canonical URI. SigV4 signs the path the server sees; if the two
    /// differ by so much as one percent-escape the result is a 403 that looks
    /// like a credentials problem and costs an afternoon to diagnose.
    fn endpoint(&self) -> (String, String, String) {
        let path = format!(
            "/model/{}/converse",
            sigv4::uri_encode(&self.config.model_id, false)
        );
        let host = format!("bedrock-runtime.{}.amazonaws.com", self.config.region);
        (format!("https://{host}{path}"), host, path)
    }
}

#[async_trait]
impl LlmClient for BedrockClient {
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, AgentError> {
        let body = serde_json::to_vec(&WireRequest::from(&request))?;
        let (url, host, path) = self.endpoint();

        let credentials = sigv4::Credentials {
            access_key: self.config.access_key.clone(),
            secret_key: self.config.secret_key.clone(),
            session_token: self.config.session_token.clone(),
        };
        let signed = sigv4::sign(
            &sigv4::SigningRequest {
                method: "POST",
                host: &host,
                canonical_uri: &path,
                canonical_query: "",
                body: &body,
                service: "bedrock",
                region: &self.config.region,
            },
            &credentials,
            Utc::now(),
        );

        let mut request_builder = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .header("host", &host)
            .header("x-amz-date", &signed.amz_date)
            .header("x-amz-content-sha256", &signed.content_sha256)
            .header("authorization", &signed.authorization);
        if let Some(token) = &signed.security_token {
            request_builder = request_builder.header("x-amz-security-token", token);
        }

        debug!(
            target: "ai_agent",
            model = %self.config.model_id,
            bytes = body.len(),
            "bedrock converse"
        );

        let response = request_builder
            .body(body)
            .send()
            .await
            .map_err(|e| AgentError::Llm(format!("transport failure: {e}")))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| AgentError::Llm(format!("could not read response body: {e}")))?;

        if !status.is_success() {
            // The body usually carries a Bedrock error code (`ValidationException`,
            // `ThrottlingException`) which is far more useful than the status alone.
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("message").cloned())
                .and_then(|m| m.as_str().map(str::to_string))
                .unwrap_or_else(|| text.chars().take(500).collect());
            warn!(target: "ai_agent", %status, "bedrock rejected the request");
            return Err(AgentError::Llm(format!("HTTP {status}: {detail}")));
        }

        let wire: WireResponse = serde_json::from_str(&text)
            .map_err(|e| AgentError::Llm(format!("could not decode response: {e}")))?;

        Ok(wire.into_response())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// ---------------------------------------------------------------------------
// Wire types. These mirror the Converse API exactly, including its quirks:
// `system` is an array of blocks, and `toolConfig.tools` wraps each spec in a
// `toolSpec` envelope rather than taking it directly.
// ---------------------------------------------------------------------------

// Serialized by hand below: `toolChoice` has to be nested inside `toolConfig`,
// which a derived impl cannot express from this field layout.
#[derive(Debug)]
struct WireRequest<'a> {
    system: Option<Vec<SystemBlock>>,
    messages: Vec<WireMessage>,
    tools: Vec<ToolEnvelope<'a>>,
    tool_choice: Option<Value>,
    inference_config: InferenceConfig,
}

impl<'a> WireRequest<'a> {
    fn from(request: &'a LlmRequest) -> Self {
        Self {
            system: request
                .system
                .as_ref()
                .map(|text| vec![SystemBlock { text: text.clone() }]),
            messages: request.messages.iter().map(WireMessage::from).collect(),
            tools: request
                .tools
                .iter()
                .map(|tool| ToolEnvelope {
                    tool_spec: ToolSpecWire {
                        name: &tool.name,
                        description: &tool.description,
                        input_schema: SchemaEnvelope {
                            json: &tool.input_schema,
                        },
                    },
                })
                .collect(),
            tool_choice: request.tool_choice.as_ref().map(tool_choice_wire),
            inference_config: InferenceConfig {
                max_tokens: request.max_tokens,
                temperature: request.temperature,
            },
        }
    }
}

/// Converse nests the tools under `toolConfig`, so the pieces are serialised
/// separately and joined here.
impl Serialize for WireRequest<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serde_json::Map::new();
        if let Some(system) = &self.system {
            map.insert("system".into(), json!(system));
        }
        map.insert("messages".into(), json!(self.messages));
        if !self.tools.is_empty() {
            map.insert("toolConfig".into(), json!({ "tools": self.tools }));
        }
        if let Some(choice) = &self.tool_choice {
            // `toolChoice` lives inside `toolConfig`; Converse rejects it at
            // the top level.
            let entry = map
                .entry("toolConfig")
                .or_insert_with(|| json!({ "tools": [] }));
            entry["toolChoice"] = choice.clone();
        }
        map.insert("inferenceConfig".into(), json!(self.inference_config));
        map.serialize(serializer)
    }
}

fn tool_choice_wire(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({ "auto": {} }),
        ToolChoice::Any => json!({ "any": {} }),
        ToolChoice::Tool(name) => json!({ "tool": { "name": name } }),
    }
}

#[derive(Debug, Serialize)]
struct SystemBlock {
    text: String,
}

#[derive(Debug, Serialize)]
struct WireMessage {
    role: &'static str,
    content: Vec<Value>,
}

impl From<&Message> for WireMessage {
    fn from(message: &Message) -> Self {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let content = message
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text(text) => json!({ "text": text }),
                ContentBlock::ToolUse { id, name, input } => json!({
                    "toolUse": {
                        "toolUseId": id,
                        "name": name,
                        "input": input,
                    }
                }),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    // `status` is optional; sending it on every result makes
                    // failures visible to the model instead of silent.
                    let status = if *is_error { "error" } else { "success" };
                    json!({
                        "toolResult": {
                            "toolUseId": tool_use_id,
                            "content": [{ "json": content }],
                            "status": status,
                        }
                    })
                }
            })
            .collect();
        Self { role, content }
    }
}

#[derive(Debug, Serialize)]
struct ToolEnvelope<'a> {
    #[serde(rename = "toolSpec")]
    tool_spec: ToolSpecWire<'a>,
}

#[derive(Debug, Serialize)]
struct ToolSpecWire<'a> {
    name: &'a str,
    description: &'a str,
    #[serde(rename = "inputSchema")]
    input_schema: SchemaEnvelope<'a>,
}

#[derive(Debug, Serialize)]
struct SchemaEnvelope<'a> {
    json: &'a Value,
}

#[derive(Debug, Serialize)]
struct InferenceConfig {
    #[serde(rename = "maxTokens")]
    max_tokens: u32,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct WireResponse {
    output: WireOutput,
    #[serde(rename = "stopReason")]
    stop_reason: Option<String>,
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct WireOutput {
    message: WireOutputMessage,
}

#[derive(Debug, Deserialize)]
struct WireOutputMessage {
    content: Option<Vec<WireContentBlock>>,
}

#[derive(Debug, Deserialize)]
struct WireContentBlock {
    text: Option<String>,
    #[serde(rename = "toolUse")]
    tool_use: Option<WireToolUse>,
}

#[derive(Debug, Deserialize)]
struct WireToolUse {
    #[serde(rename = "toolUseId")]
    tool_use_id: String,
    name: String,
    input: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(rename = "inputTokens")]
    input_tokens: Option<i32>,
    #[serde(rename = "outputTokens")]
    output_tokens: Option<i32>,
}

impl WireResponse {
    fn into_response(self) -> LlmResponse {
        let mut content = Vec::new();
        for block in self.output.message.content.unwrap_or_default() {
            if let Some(text) = block.text {
                content.push(ContentBlock::Text(text));
            }
            if let Some(tool_use) = block.tool_use {
                content.push(ContentBlock::ToolUse {
                    id: tool_use.tool_use_id,
                    name: tool_use.name,
                    // A toolUse with no `input` object still has to become an
                    // object: the argument validators below assume one, and
                    // `Value::Null` would fail them with a confusing message.
                    input: tool_use.input.unwrap_or_else(|| json!({})),
                });
            }
        }

        let stop_reason = match self.stop_reason.as_deref() {
            Some("end_turn") => StopReason::EndTurn,
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::MaxTokens,
            Some(other) => StopReason::Other(other.to_string()),
            None => StopReason::Other("missing".to_string()),
        };

        LlmResponse {
            message: Message {
                role: Role::Assistant,
                content,
            },
            stop_reason,
            usage: self
                .usage
                .map(|u| Usage {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                })
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::{ToolResult, ToolSpec};

    fn request() -> LlmRequest {
        LlmRequest {
            system: Some("you are a trading analyst".into()),
            messages: vec![
                Message::user("what is the POC?"),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "get_volume_profile".into(),
                        input: json!({"symbol": "BTCUSDT"}),
                    }],
                },
                Message::tool_results(vec![ToolResult {
                    tool_use_id: "t1".into(),
                    content: json!({"poc": 103250.5}),
                    is_error: false,
                }]),
            ],
            tools: vec![ToolSpec {
                name: "get_volume_profile".into(),
                description: "POC/VAH/VAL".into(),
                input_schema: json!({"type": "object", "properties": {"symbol": {"type": "string"}}}),
            }],
            tool_choice: Some(ToolChoice::Tool("submit_thesis".into())),
            max_tokens: 512,
            temperature: 0.0,
        }
    }

    #[test]
    fn the_wire_request_matches_the_converse_shape() {
        let value = serde_json::to_value(WireRequest::from(&request())).unwrap();

        assert_eq!(value["system"][0]["text"], "you are a trading analyst");
        assert_eq!(value["messages"][0]["role"], "user");
        assert_eq!(value["messages"][1]["role"], "assistant");
        assert_eq!(
            value["messages"][1]["content"][0]["toolUse"]["name"],
            "get_volume_profile"
        );
        // toolResult goes back in a *user* message, wrapped in `json`.
        assert_eq!(value["messages"][2]["role"], "user");
        assert_eq!(
            value["messages"][2]["content"][0]["toolResult"]["content"][0]["json"]["poc"],
            103250.5
        );
        assert_eq!(
            value["messages"][2]["content"][0]["toolResult"]["status"],
            "success"
        );
        // toolConfig, not top-level tools; and toolChoice nested inside it.
        assert_eq!(
            value["toolConfig"]["tools"][0]["toolSpec"]["name"],
            "get_volume_profile"
        );
        assert_eq!(
            value["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"]["type"],
            "object"
        );
        assert_eq!(
            value["toolConfig"]["toolChoice"]["tool"]["name"],
            "submit_thesis"
        );
        assert!(value.get("tools").is_none());
        assert_eq!(value["inferenceConfig"]["temperature"], 0.0);
    }

    #[test]
    fn tools_are_omitted_entirely_for_a_plain_completion() {
        let mut plain = request();
        plain.tools.clear();
        plain.tool_choice = None;
        let value = serde_json::to_value(WireRequest::from(&plain)).unwrap();
        assert!(value.get("toolConfig").is_none());
    }

    #[test]
    fn a_response_with_a_tool_use_decodes_into_a_message() {
        let raw = json!({
            "output": {"message": {"role": "assistant", "content": [
                {"text": "checking"},
                {"toolUse": {"toolUseId": "a1", "name": "analyze_timeframe", "input": {"symbol": "BTCUSDT"}}}
            ]}},
            "stopReason": "tool_use",
            "usage": {"inputTokens": 10, "outputTokens": 20}
        });
        let response = serde_json::from_value::<WireResponse>(raw)
            .unwrap()
            .into_response();
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(response.text(), "checking");
        let calls = response.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "a1");
        assert_eq!(response.usage.input_tokens, Some(10));
    }

    #[test]
    fn a_tool_use_without_input_becomes_an_empty_object() {
        let raw = json!({
            "output": {"message": {"content": [
                {"toolUse": {"toolUseId": "a1", "name": "get_cvd"}}
            ]}},
            "stopReason": "tool_use"
        });
        let response = serde_json::from_value::<WireResponse>(raw)
            .unwrap()
            .into_response();
        assert_eq!(response.tool_calls()[0].input, json!({}));
    }

    #[test]
    fn unknown_stop_reasons_are_preserved_not_defaulted() {
        let raw = json!({
            "output": {"message": {"content": []}},
            "stopReason": "guardrail_intervened"
        });
        let response = serde_json::from_value::<WireResponse>(raw)
            .unwrap()
            .into_response();
        assert_eq!(
            response.stop_reason,
            StopReason::Other("guardrail_intervened".into())
        );
    }

    #[test]
    fn the_model_id_is_uri_encoded_into_the_path_and_host_matches() {
        let client = BedrockClient::new(BedrockConfig {
            region: "us-east-1".into(),
            model_id: "anthropic.claude-3-5-sonnet-20240620-v1:0".into(),
            access_key: "a".into(),
            secret_key: "b".into(),
            session_token: None,
            timeout_secs: 5,
        })
        .unwrap();
        let (url, host, path) = client.endpoint();
        assert_eq!(host, "bedrock-runtime.us-east-1.amazonaws.com");
        assert!(url.contains("/model/anthropic.claude-3-5-sonnet-20240620-v1%3A0/converse"));
        // The signed path and the requested path are the same string.
        assert!(url.ends_with(&path));
    }
}
