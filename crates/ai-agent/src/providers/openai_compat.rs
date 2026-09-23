//! The OpenAI **Chat Completions** provider, and every endpoint that speaks its
//! wire format.
//!
//! One adapter rather than one per vendor, and that is the point: OpenAI,
//! Anthropic's OpenAI-compatible endpoint, xAI (Grok), DeepSeek, OpenRouter,
//! HuggingFace's router and self-hosted gateways (vLLM, Ollama, LiteLLM) all
//! accept a `POST {base_url}/chat/completions` body that differs by *which
//! host* it is sent to and little else. The differences that remain are
//! auth-header shape and optional extra headers, which are configuration
//! ([`OpenAiCompatConfig::auth_style`], [`OpenAiCompatConfig::extra_headers`])
//! rather than code paths. A tenth provider must not mean a tenth translation
//! of tool calls.
//!
//! ## Translation contract
//!
//! [`crate::llm_client`] models tool use as first-class blocks, mirroring
//! Bedrock Converse. Chat Completions models it as `tool_calls` on an assistant
//! message and `role: "tool"` messages for results. This module is the only
//! place those two shapes meet:
//!
//! * [`crate::ContentBlock::ToolUse`] -> `tool_calls[]` (`id`, `name` ->
//!   `function.name`, `input` -> `function.arguments` as a JSON string);
//! * `tool_calls[]` -> [`crate::ContentBlock::ToolUse`], with `arguments`
//!   parsed back to JSON (an unparseable string becomes `{"_raw": ...}` rather
//!   than a dropped call -- the argument validator downstream reports it);
//! * [`crate::Message::tool_results`] -> one `role: "tool"` message per result
//!   (the wire has no multi-result message; `tool_call_id` keeps the pairing);
//! * stop reasons map `stop`/`tool_calls`/`length`, everything else through
//!   [`crate::StopReason::Other`] uninterpreted.
//!
//! Images ride as `image_url` data URLs on a user message, the one spelling
//! every compatible endpoint accepts.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

use crate::error::AgentError;
use crate::llm_client::{
    ContentBlock, LlmClient, LlmRequest, LlmResponse, Message, Role, StopReason, ToolChoice, Usage,
};

/// How the API key is presented.
///
/// `Bearer` covers OpenAI, DeepSeek, Grok, OpenRouter, HuggingFace and every
/// OpenAI-compatible gateway. Anthropic's native Messages API wants
/// `x-api-key`, and Azure OpenAI wants `api-key`, so the choice is data, not
/// code: a gateway that fronts a provider natively is reached by spelling the
/// header differently, not by a new client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>` (the default, and nearly universal).
    #[default]
    Bearer,
    /// `x-api-key: <key>` (Anthropic-native endpoints).
    XApiKey,
    /// `api-key: <key>` (Azure OpenAI deployments).
    Azure,
}

impl AuthStyle {
    /// Parse the wire string used in stored configs; unknown values default.
    pub fn from_name(name: &str) -> Self {
        match name {
            "x-api-key" => Self::XApiKey,
            "api-key" => Self::Azure,
            _ => Self::Bearer,
        }
    }

    /// The stable name persisted in user configs.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::XApiKey => "x-api-key",
            Self::Azure => "api-key",
        }
    }

    fn header(&self) -> &'static str {
        match self {
            Self::Bearer => "Authorization",
            Self::XApiKey => "x-api-key",
            Self::Azure => "api-key",
        }
    }

    fn value(&self, key: &str) -> String {
        match self {
            Self::Bearer => format!("Bearer {key}"),
            _ => key.to_string(),
        }
    }
}

/// Configuration for one Chat Completions endpoint.
///
/// Deliberately dumb data: nothing here knows about the agent, and a test can
/// build one against a local mock server without touching the environment.
#[derive(Debug, Clone)]
pub struct OpenAiCompatConfig {
    /// Provider this endpoint represents. Logging and error messages only --
    /// the wire treatment is identical.
    pub provider: super::ProviderId,
    /// Model id as the endpoint spells it, e.g. `gpt-4o`, `deepseek-chat`.
    pub model_id: String,
    /// API key, sent per [`Self::auth_style`]. Empty is allowed only for
    /// gateways that do their own auth (a local Ollama, a sidecar proxy).
    pub api_key: String,
    /// Root of the API, no trailing slash, e.g. `https://api.openai.com/v1`.
    /// `/chat/completions` is appended by the client.
    pub base_url: String,
    /// Auth-header shape.
    pub auth_style: AuthStyle,
    /// Extra headers every request carries (e.g. OpenRouter's
    /// `HTTP-Referer`/`X-Title`, or a required `anthropic-version`).
    pub extra_headers: Vec<(String, String)>,
    /// Request timeout in seconds. Long: a reasoning model walking a timeframe
    /// ladder can legitimately take a minute.
    pub timeout_secs: u64,
}

impl OpenAiCompatConfig {
    /// Build a config for a known provider with its default endpoint and
    /// header treatment.
    ///
    /// `api_key` may be empty for providers whose gateways accept anonymous
    /// calls; production callers pass a real key.
    #[must_use]
    pub fn for_provider(provider: super::ProviderId, model_id: &str, api_key: &str) -> Self {
        let (auth_style, extra_headers) = match provider {
            super::ProviderId::Anthropic => (
                AuthStyle::XApiKey,
                vec![("anthropic-version".to_string(), "2023-06-01".to_string())],
            ),
            super::ProviderId::OpenRouter => (
                AuthStyle::Bearer,
                vec![
                    ("HTTP-Referer".to_string(), "https://freebuff.local".to_string()),
                    ("X-Title".to_string(), "Freebuff".to_string()),
                ],
            ),
            _ => (AuthStyle::Bearer, Vec::new()),
        };
        Self {
            provider,
            model_id: model_id.to_string(),
            api_key: api_key.to_string(),
            base_url: provider.default_base_url().to_string(),
            auth_style,
            extra_headers,
            timeout_secs: 180,
        }
    }

    /// `(chat/completions URL, origin)` from the base URL.
    fn endpoint(&self) -> (String, String) {
        let base = self.base_url.trim_end_matches('/');
        let origin = base
            .split("//")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or(base)
            .to_string();
        (format!("{base}/chat/completions"), origin)
    }
}

/// A Chat Completions-backed [`LlmClient`].
pub struct OpenAiCompatClient {
    config: OpenAiCompatConfig,
    http: reqwest::Client,
    name: String,
}

impl OpenAiCompatClient {
    /// Build a client for the given configuration.
    ///
    /// # Errors
    /// [`AgentError::Llm`] if the HTTP client cannot be constructed.
    pub fn new(config: OpenAiCompatConfig) -> Result<Self, AgentError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| AgentError::Llm(format!("cannot build http client: {e}")))?;
        let name = format!("{}({})", config.provider.wire_name(), config.model_id);
        Ok(Self { config, http, name })
    }

    /// The model id in use.
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.config.model_id
    }
}

/// Serialize one [`LlmRequest`] as a Chat Completions body.
///
/// A free function so the wire shape is testable without a transport.
#[must_use]
pub fn chat_body(request: &LlmRequest, model: &str) -> Value {
    let mut messages = Vec::new();
    for message in &request.messages {
        messages.extend(message_to_wire(message));
    }
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": request.max_tokens,
        "temperature": request.temperature,
    });
    if let Some(system) = &request.system {
        // Prepend as a system-role message: the oldest and most portable
        // spelling of "instructions", accepted by every compatible endpoint.
        if let Some(messages) = body["messages"].as_array_mut() {
            messages.insert(0, json!({ "role": "system", "content": system }));
        }
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(
            request
                .tools
                .iter()
                .map(|tool| json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    }
                }))
                .collect::<Vec<_>>()
        );
    }
    if let Some(choice) = &request.tool_choice {
        body["tool_choice"] = match choice {
            ToolChoice::Auto => json!("auto"),
            ToolChoice::Any => json!("required"),
            ToolChoice::Tool(name) => json!({ "type": "function", "function": { "name": name } }),
        };
    }
    body
}

/// One message as the wire sees it.
///
/// A single message can map to *one or more* wire messages: a user message
/// carrying tool results expands to one `role: "tool"` message per result, and
/// an assistant turn that both spoke and called tools stays one message.
fn message_to_wire(message: &Message) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let mut text_parts: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut pending_results: Vec<Value> = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text(text) => text_parts.push(json!({ "type": "text", "text": text })),
            ContentBlock::Image { media_type, data } => text_parts.push(json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{media_type};base64,{data}") }
            })),
            ContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": input.to_string() }
            })),
            ContentBlock::ToolResult { tool_use_id, content, .. } => {
                pending_results.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": content.to_string(),
                }));
            }
        }
    }

    // Results flush as their own tool messages, after the assistant turn that
    // asked for them (if this message carries both) and before anything else
    // that would follow: the pairing a model sees is "asked, then answered".
    if !pending_results.is_empty() {
        if !tool_calls.is_empty() {
            let mut assistant = json!({ "role": "assistant" });
            if !text_parts.is_empty() {
                assistant["content"] = json!(text_parts);
            }
            assistant["tool_calls"] = json!(tool_calls);
            out.push(assistant);
            tool_calls.clear();
            text_parts.clear();
        }
        out.extend(pending_results);
    }

    if !tool_calls.is_empty() || !text_parts.is_empty() {
        let mut msg = json!({ "role": role });
        if !text_parts.is_empty() {
            msg["content"] = json!(text_parts);
        }
        if !tool_calls.is_empty() {
            msg["tool_calls"] = json!(tool_calls);
        }
        out.push(msg);
    }

    out
}

/// The response body, decoded leniently: `usage` and `finish_reason` are
/// optional across implementations.
#[derive(Debug, Deserialize)]
struct WireResponse {
    choices: Vec<WireChoice>,
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct WireChoice {
    message: Option<WireAssistant>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireAssistant {
    content: Option<Value>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Debug, Deserialize)]
struct WireToolCall {
    id: Option<String>,
    function: WireFunction,
}

#[derive(Debug, Deserialize)]
struct WireFunction {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
}

/// Decode a successful body into an [`LlmResponse`].
fn into_response(wire: WireResponse) -> LlmResponse {
    let mut content = Vec::new();
    if let Some(choice) = wire.choices.first() {
        if let Some(assistant) = &choice.message {
            match &assistant.content {
                Some(Value::String(text)) if !text.is_empty() => {
                    content.push(ContentBlock::Text(text.clone()));
                }
                // Some gateways return the Anthropic-style block array.
                Some(Value::Array(blocks)) => {
                    for block in blocks {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            content.push(ContentBlock::Text(text.to_string()));
                        }
                    }
                }
                _ => {}
            }
            for call in &assistant.tool_calls {
                let input = match &call.function.arguments {
                    Value::String(raw) => serde_json::from_str::<Value>(raw).unwrap_or_else(|_| {
                        warn!(target: "ai_agent", name = %call.function.name, "tool arguments were not valid JSON; wrapping raw");
                        json!({ "_raw": raw })
                    }),
                    other => other.clone(),
                };
                content.push(ContentBlock::ToolUse {
                    id: call
                        .id
                        .clone()
                        .unwrap_or_else(|| format!("call_{}", call.function.name)),
                    name: call.function.name.clone(),
                    input,
                });
            }
        }
    }

    let stop_reason = wire
        .choices
        .first()
        .and_then(|choice| choice.finish_reason.as_deref())
        .map(|reason| match reason {
            "stop" => StopReason::EndTurn,
            "tool_calls" | "function_call" => StopReason::ToolUse,
            "length" => StopReason::MaxTokens,
            other => StopReason::Other(other.to_string()),
        })
        .unwrap_or(StopReason::Other("missing".to_string()));

    LlmResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason,
        usage: wire
            .usage
            .map(|u| Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            })
            .unwrap_or_default(),
    }
}

#[async_trait]
impl LlmClient for OpenAiCompatClient {
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, AgentError> {
        let body = chat_body(&request, &self.config.model_id);
        let payload = serde_json::to_vec(&body)?;
        let (url, _origin) = self.config.endpoint();

        let mut builder = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .header(
                self.config.auth_style.header(),
                self.config.auth_style.value(&self.config.api_key),
            );
        for (name, value) in &self.config.extra_headers {
            builder = builder.header(name.as_str(), value.as_str());
        }

        let response = builder
            .body(payload)
            .send()
            .await
            .map_err(|e| AgentError::Llm(format!("transport failure: {e}")))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| AgentError::Llm(format!("could not read response body: {e}")))?;

        if !status.is_success() {
            // OpenAI-family errors carry `{error: {message}}`; keep that, or
            // the first 500 bytes of whatever came back.
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("error").cloned())
                .and_then(|e| e.get("message").cloned())
                .and_then(|m| m.as_str().map(str::to_string))
                .unwrap_or_else(|| text.chars().take(500).collect());
            warn!(target: "ai_agent", %status, provider = %self.config.provider.wire_name(), "provider rejected the request");
            return Err(AgentError::Llm(format!("HTTP {status}: {detail}")));
        }

        let wire: WireResponse = serde_json::from_str(&text)
            .map_err(|e| AgentError::Llm(format!("could not decode response: {e}")))?;
        Ok(into_response(wire))
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::{ToolResult, ToolSpec};
    use serde_json::json;

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
    fn the_body_matches_the_chat_completions_shape() {
        let body = chat_body(&request(), "gpt-test");

        assert_eq!(body["model"], "gpt-test");
        // System prompt prepended as its own message.
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        // The tool call rides on the assistant message as `tool_calls`.
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["name"],
            "get_volume_profile"
        );
        // Arguments are the JSON string of the input.
        let arguments = body["messages"][2]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(arguments).unwrap(),
            json!({"symbol": "BTCUSDT"})
        );
        // The tool result is a `role: tool` message paired by id.
        assert_eq!(body["messages"][3]["role"], "tool");
        assert_eq!(body["messages"][3]["tool_call_id"], "t1");
        // Tools and tool_choice as the spec spells them.
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "get_volume_profile");
        assert_eq!(body["tool_choice"]["function"]["name"], "submit_thesis");
        assert_eq!(body["temperature"], 0.0);
    }

    #[test]
    fn a_plain_completion_omits_tools_and_a_tool_choice() {
        let mut plain = request();
        plain.tools.clear();
        plain.tool_choice = None;
        let body = chat_body(&plain, "gpt-test");
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn a_response_with_tool_calls_decodes_into_blocks() {
        let raw = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "checking",
                    "tool_calls": [{
                        "id": "call_a1",
                        "type": "function",
                        "function": {"name": "analyze_timeframe", "arguments": "{\"symbol\":\"BTCUSDT\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20}
        });
        let response = into_response(serde_json::from_value(raw).unwrap());
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(response.text(), "checking");
        let calls = response.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_a1");
        assert_eq!(calls[0].input, json!({"symbol": "BTCUSDT"}));
        assert_eq!(response.usage.input_tokens, Some(10));
    }

    #[test]
    fn unparseable_tool_arguments_are_wrapped_not_dropped() {
        let raw = json!({
            "choices": [{
                "message": {"tool_calls": [{
                    "id": "call_b1",
                    "function": {"name": "get_cvd", "arguments": "not json"}
                }]},
                "finish_reason": "tool_calls"
            }]
        });
        let response = into_response(serde_json::from_value(raw).unwrap());
        assert_eq!(response.tool_calls()[0].input, json!({"_raw": "not json"}));
    }

    #[test]
    fn unknown_finish_reasons_are_preserved() {
        let raw = json!({
            "choices": [{"message": {"content": "x"}, "finish_reason": "content_filter"}]
        });
        let response = into_response(serde_json::from_value(raw).unwrap());
        assert_eq!(
            response.stop_reason,
            StopReason::Other("content_filter".into())
        );
    }

    #[test]
    fn endpoints_append_the_chat_path_exactly_once() {
        let mut config =
            OpenAiCompatConfig::for_provider(super::super::ProviderId::OpenAi, "gpt-4o", "k");
        config.base_url = "https://api.openai.com/v1/".into();
        let (url, origin) = config.endpoint();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(origin, "api.openai.com");
    }

    #[test]
    fn a_message_carrying_both_a_call_and_results_expands_in_order() {
        // The orchestrator never builds this shape today, but the translation
        // must be safe if it ever does: the assistant half goes first, then the
        // answers, or a strict provider drops the results.
        let message = Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolUse {
                    id: "t9".into(),
                    name: "get_cvd".into(),
                    input: json!({}),
                },
                ContentBlock::ToolResult {
                    tool_use_id: "t9".into(),
                    content: json!({"cvd": 1.0}),
                    is_error: false,
                },
            ],
        };
        let wire = message_to_wire(&message);
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[0]["role"], "assistant");
        assert_eq!(wire[0]["tool_calls"][0]["id"], "t9");
        assert_eq!(wire[1]["role"], "tool");
        assert_eq!(wire[1]["tool_call_id"], "t9");
    }

    #[tokio::test]
    async fn a_mock_server_round_trips_a_tool_use_conversation() {
        // A local mock proves the transport path (headers, auth, decode) with
        // no network: the same reqwest stack, against 127.0.0.1.
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 16384];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = json!({
                "choices": [{
                    "message": {"role": "assistant", "content": null, "tool_calls": [{
                        "id": "call_x", "type": "function",
                        "function": {"name": "get_volume_profile", "arguments": "{}"}
                    }]},
                    "finish_reason": "tool_calls"
                }]
            });
            let payload = serde_json::to_string(&body).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            stream.write_all(response.as_bytes()).unwrap();
            request
        });

        let mut config = OpenAiCompatConfig::for_provider(
            super::super::ProviderId::OpenAi,
            "gpt-test",
            "sk-test",
        );
        config.base_url = format!("http://{addr}/v1");
        let client = OpenAiCompatClient::new(config).unwrap();
        let response = client.complete(request()).await.unwrap();

        assert_eq!(response.tool_calls()[0].name, "get_volume_profile");
        let sent = server.join().unwrap().to_ascii_lowercase();
        // reqwest lower-cases header names on the wire; match the same way.
        assert!(sent.contains("authorization: bearer sk-test"), "{sent}");
        assert!(sent.contains("/v1/chat/completions"), "{sent}");
    }
}

