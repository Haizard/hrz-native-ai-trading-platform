//! The provider-agnostic LLM abstraction (`docs/09-AI-AGENT-SYSTEM.md`).
//!
//! One trait, [`LlmClient`], and a message shape that carries **tool use as a
//! first-class concept** rather than as JSON smuggled through a text field.
//! That distinction is the whole point of this module: the agent's design
//! assumes the model *requests* a tool and receives a result, because that is
//! what makes the numbers in a thesis traceable. A provider that can only do
//! "here is some text that looks like a function call" would quietly break the
//! auditability guarantee, so `tool_use` / `tool_result` are modelled directly.
//!
//! The wire shape deliberately mirrors the AWS Bedrock **Converse** API
//! (system / messages / content blocks / toolConfig), because that is the
//! first provider implemented. It is also, not coincidentally, very close to
//! Anthropic's Messages API, so a second provider is a small adapter rather
//! than a redesign.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AgentError;

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Tool name, as the model must spell it in a `tool_use`.
    pub name: String,
    /// What the tool returns. This is the only thing steering the model's
    /// choice, so it is written for the model, not for a human API doc.
    pub description: String,
    /// JSON Schema for the tool's input object.
    pub input_schema: Value,
}

/// Who produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// The user (or, in this system, the orchestrator feeding a tool result).
    User,
    /// The model.
    Assistant,
}

/// One message in the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Author of the message.
    pub role: Role,
    /// Content blocks, in order.
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// A single-text user message.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text(text.into())],
        }
    }

    /// A single-text assistant message.
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text(text.into())],
        }
    }

    /// A user message with an image attached ahead of its text.
    ///
    /// The image goes **first**, which is not cosmetic: a model reading
    /// "the chart in the image" before the image has to hold a reference for a
    /// block it has not seen, and providers that stream blocks in order make
    /// that a visible difference rather than a stylistic one. Text after the
    /// image also means the grounding instruction ("do not read prices off it")
    /// is the last thing the model reads before answering.
    #[must_use]
    pub fn user_with_image(
        text: impl Into<String>,
        media_type: impl Into<String>,
        data: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::User,
            content: vec![
                ContentBlock::Image {
                    media_type: media_type.into(),
                    data: data.into(),
                },
                ContentBlock::Text(text.into()),
            ],
        }
    }

    /// The tool-result message for a batch of results.
    ///
    /// Every result must go back in **one** message: Bedrock rejects a
    /// `toolResult` that does not directly follow the `toolUse` it answers,
    /// and with several results split across messages only the first is
    /// accepted.
    #[must_use]
    pub fn tool_results(results: Vec<ToolResult>) -> Self {
        Self {
            role: Role::User,
            content: results
                .into_iter()
                .map(|r| ContentBlock::ToolResult {
                    tool_use_id: r.tool_use_id,
                    content: r.content,
                    is_error: r.is_error,
                })
                .collect(),
        }
    }

    /// Every `tool_use` block in this message.
    #[must_use]
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => Some(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Concatenated text blocks, for logging and for the final answer.
    #[must_use]
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// One block of message content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContentBlock {
    /// Plain text.
    Text(String),
    /// An image the model can see.
    ///
    /// Used for exactly one thing: a screenshot of the user's chart viewport
    /// (`crate::chart_context`). It is *illustration*, not data -- the model is
    /// told so in the prompt, because letting a vision model transcribe prices
    /// off a rendered canvas reintroduces the arithmetic-by-eye failure that
    /// principle #2 exists to forbid. The base64 is already encoded by the
    /// sender so the LLM layer never handles raw bytes.
    Image {
        /// Media type, e.g. `image/png`.
        media_type: String,
        /// Base64-encoded image data.
        data: String,
    },
    /// The model asking for a tool.
    ToolUse {
        /// Provider-generated id, echoed back with the result.
        id: String,
        /// Tool name.
        name: String,
        /// Arguments, as JSON. Validated against the tool's schema before use.
        input: Value,
    },
    /// The result of a tool the model asked for.
    ToolResult {
        /// The `tool_use` id this answers.
        tool_use_id: String,
        /// The payload the tool returned.
        content: Value,
        /// Whether the tool failed; the model is told to recover or explain.
        is_error: bool,
    },
}

/// A tool call extracted from an assistant message.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Provider-generated id.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Arguments.
    pub input: Value,
}

/// The result of executing a tool, ready to be sent back.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// The `tool_use` id this answers.
    pub tool_use_id: String,
    /// Payload.
    pub content: Value,
    /// Whether the tool failed.
    pub is_error: bool,
}

/// How the model should pick a tool for this turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoice {
    /// The model decides.
    Auto,
    /// It must call some tool.
    Any,
    /// It must call this specific tool.
    Tool(String),
}

/// A completion request.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    /// System prompt.
    pub system: Option<String>,
    /// The conversation so far.
    pub messages: Vec<Message>,
    /// Tools available to the model. Empty means a plain completion.
    pub tools: Vec<ToolSpec>,
    /// Overrides the provider default when set.
    pub tool_choice: Option<ToolChoice>,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
    /// Sampling temperature. This system defaults to 0.0 -- a thesis whose
    /// numbers must match a tool result is not a place for creativity.
    pub temperature: f32,
}

impl LlmRequest {
    /// A request with no tools and no system prompt.
    #[must_use]
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            system: None,
            messages,
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: 2048,
            temperature: 0.0,
        }
    }
}

/// Why the model stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// Finished its turn.
    EndTurn,
    /// Stopped because it wants tool results.
    ToolUse,
    /// Hit the token limit.
    MaxTokens,
    /// Provider-specific reason we do not interpret.
    Other(String),
}

/// Token usage reported by the provider, when it reports any.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Input tokens.
    pub input_tokens: Option<i32>,
    /// Output tokens.
    pub output_tokens: Option<i32>,
}

/// A completion response.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    /// The assistant message, which may contain text and/or `tool_use` blocks.
    pub message: Message,
    /// Why generation stopped.
    pub stop_reason: StopReason,
    /// Token usage, if the provider reported it.
    pub usage: Usage,
}

impl LlmResponse {
    /// The tool calls requested by the model.
    #[must_use]
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.message.tool_calls()
    }

    /// The text the model wrote, if any.
    #[must_use]
    pub fn text(&self) -> String {
        self.message.text()
    }
}

/// A chat-completion provider.
///
/// Implementations are responsible only for transport and translation: they
/// must not add reasoning of their own, retry with different prompts, or
/// rewrite tool arguments. Anything that shapes the model's behaviour belongs
/// in the orchestrator, where it is visible and testable.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Run one completion.
    ///
    /// # Errors
    /// Returns [`AgentError::Llm`] for transport failures and for payloads the
    /// provider accepted but this client cannot interpret.
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, AgentError>;

    /// A short name for logging, e.g. `"bedrock(qwen.qwen3-coder-next)"`.
    fn name(&self) -> &str;
}

/// A client that answers from a canned script, for tests.
///
/// The agent loop in this crate is the part most worth testing and the part
/// hardest to reach, because it normally needs a live model. `ScriptedClient`
/// lets a test drive it deterministically: queue the responses the "model"
/// would give and assert on the conversation the orchestrator built.
pub struct ScriptedClient {
    responses: std::sync::Mutex<std::collections::VecDeque<LlmResponse>>,
    requests: std::sync::Mutex<Vec<LlmRequest>>,
}

impl ScriptedClient {
    /// Build a client that returns `responses` in order, one per call.
    #[must_use]
    pub fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Every request the orchestrator sent, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().expect("lock poisoned").clone()
    }

    /// How many requests were made.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.requests.lock().expect("lock poisoned").len()
    }
}

#[async_trait]
impl LlmClient for ScriptedClient {
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, AgentError> {
        self.requests
            .lock()
            .expect("lock poisoned")
            .push(request.clone());
        self.responses
            .lock()
            .expect("lock poisoned")
            .pop_front()
            .ok_or_else(|| AgentError::Llm("scripted client ran out of responses".into()))
    }

    fn name(&self) -> &str {
        "scripted"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_use(id: &str, name: &str, input: Value) -> LlmResponse {
        LlmResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: id.into(),
                    name: name.into(),
                    input,
                }],
            },
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        }
    }

    #[test]
    fn tool_calls_are_extracted_from_an_assistant_message() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("let me check".into()),
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "analyze_timeframe".into(),
                    input: serde_json::json!({"symbol": "BTCUSDT", "timeframe": "5m"}),
                },
            ],
        };
        let calls = message.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "analyze_timeframe");
        assert_eq!(message.text(), "let me check");
    }

    #[test]
    fn every_tool_result_goes_in_one_message() {
        // Bedrock refuses a toolResult that does not directly follow the
        // toolUse message, so splitting results across messages would silently
        // drop all but the first.
        let message = Message::tool_results(vec![
            ToolResult {
                tool_use_id: "a".into(),
                content: serde_json::json!({"poc": 1.0}),
                is_error: false,
            },
            ToolResult {
                tool_use_id: "b".into(),
                content: serde_json::json!({"poc": 2.0}),
                is_error: false,
            },
        ]);
        assert_eq!(message.role, Role::User);
        assert_eq!(message.content.len(), 2);
    }

    #[test]
    fn a_text_only_message_has_no_tool_calls() {
        assert!(Message::user("hello").tool_calls().is_empty());
        assert_eq!(Message::assistant("hi").text(), "hi");
    }

    #[tokio::test]
    async fn the_scripted_client_replays_in_order() {
        let client = ScriptedClient::new(vec![
            tool_use("1", "get_candles", serde_json::json!({})),
            LlmResponse {
                message: Message::assistant("done"),
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
        ]);

        let first = client.complete(LlmRequest::new(vec![])).await.unwrap();
        assert_eq!(first.tool_calls()[0].name, "get_candles");
        let second = client.complete(LlmRequest::new(vec![])).await.unwrap();
        assert_eq!(second.text(), "done");
        assert_eq!(client.call_count(), 2);
        assert!(client.complete(LlmRequest::new(vec![])).await.is_err());
    }
}
