//! Skill authoring (`docs/47`): the interview that turns a trader's
//! description into a methodology document.
//!
//! The user's rejection that defines this module: a skill is *not* code and
//! not a generated concept document. It is normal language -- the concept the
//! trader trades, the way they explained it -- which the model follows at
//! runtime to detect the zones itself and draw them. So the artifact of an
//! authoring session is a [`Skill`], and the interview's only tool is
//! `save_skill_draft`, which validates the document and stores it through the
//! host's [`SkillWriter`].
//!
//! The conversation state is the caller's: the chat panel keeps the
//! transcript and sends it back with every message. The agent stays
//! stateless, which is what makes a resumed interview work after a deploy or
//! a re-login without any session table.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent::Agent;
use crate::error::AgentError;
use crate::llm_client::{ContentBlock, LlmRequest, Message, Role, ToolCall, ToolResult, ToolSpec, Usage};
use crate::skills::Skill;

/// Where authored skills go.
///
/// The gateway implements it over the `skills` table; tests implement it
/// over a vec. The port exists for the same reason `DrawingWriter` does:
/// the agent decides *what* is saved and *that* it validates, and the host
/// decides where rows live.
#[async_trait]
pub trait SkillWriter: Send + Sync {
    /// Store one new skill version; return its library id.
    ///
    /// # Errors
    /// [`AgentError::ToolFailed`] when the store refuses -- a duplicate
    /// version, a lost connection -- with the reason in the message, so the
    /// model can tell the user what happened instead of inventing success.
    async fn save(&self, user_id: &str, skill: &Skill) -> Result<String, AgentError>;
}

/// The writing capability attached to an authoring request.
///
/// Host-only, like `DrawingsContext`: built from the authenticated identity
/// one layer up, never parsed from a request body.
#[derive(Clone)]
pub struct AuthoringContext {
    user_id: String,
    writer: Arc<dyn SkillWriter>,
}

impl AuthoringContext {
    /// Bundle the caller's identity with the store that takes their skills.
    pub fn new(user_id: impl Into<String>, writer: Arc<dyn SkillWriter>) -> Self {
        Self {
            user_id: user_id.into(),
            writer,
        }
    }

    /// The authenticated author. Used for storage, never shown to the model.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The store authored skills are written to.
    pub fn writer(&self) -> &dyn SkillWriter {
        self.writer.as_ref()
    }
}

/// One message of the interview so far, in the order it happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    /// `user` or `assistant`.
    pub role: String,
    /// What was said.
    pub text: String,
}

/// One user message in an authoring interview.
pub struct AuthorRequest {
    /// What the user just said.
    pub message: String,
    /// The interview so far, oldest first. The caller keeps it; the agent
    /// keeps nothing between calls.
    pub history: Vec<ChatTurn>,
    /// Where a finished draft is saved.
    pub authoring: AuthoringContext,
}

/// What the agent says back.
pub struct AuthorAnswer {
    /// The interview's next beat: a question, a summary to confirm, or the
    /// confirmation that the skill was saved.
    pub message: String,
    /// Present exactly once: the skill the session produced, with the id it
    /// was stored under so the client can pin it.
    pub saved: Option<SavedSkill>,
    /// Model round trips this reply took (a corrected draft costs more than
    /// one).
    pub turns: usize,
    /// Token accounting across those turns.
    pub usage: Usage,
}

/// The skill an authoring session stored.
#[derive(Debug, Clone, Serialize)]
pub struct SavedSkill {
    /// Library id, e.g. `fvg-scalp-v1` -- what `skill_id` pins take.
    pub id: String,
    /// The name the trader gave it.
    pub name: String,
    /// The stored version.
    pub version: String,
}

const SAVE_DRAFT: &str = "save_skill_draft";

const AUTHOR_SYSTEM: &str = "You are the skill author for a trading terminal. The trader describes \
a chart pattern or zone they trade -- an order block, a fair value gap, a support/resistance \
shelf, a liquidity sweep, anything built from candles -- and your job is to interview them until \
you understand it well enough to write the skill they would have written, then save it.\n\n\
## How the interview runs\n\
- One question per message. Short, specific, about one thing. Never a checklist.\n\
- Bring your own expertise: propose the mechanics of the concept as you know it and ask the \
trader to confirm or correct them, rather than making them explain from zero.\n\
- Cover, across the conversation: what the pattern IS in candle terms (which candles, what \
relationship), what makes it VALID and what kills it, how the trader ENTERS (the trigger and \
the confirmation), where the STOP and TARGET go, and what the pattern is CALLED.\n\
- When you have enough, show the trader the complete skill in plain language and ask them to \
confirm it. Only save after they confirm -- or when they tell you to save.\n\n\
## What a skill is here\n\
- Pure normal language. No code, no pseudocode, no JSON grammar, no formula syntax. The AI agent \
is the detector: it reads the skill's knowledge and rules at runtime and finds the zones on the \
candles itself. Anything that reads like a program is a mistake.\n\
- `knowledge` teaches the concept: what the pattern is, in the trader's terms, precise enough \
that another trader would draw the same zone.\n\
- `rules` are the numbered checks, including DRAW rules that say exactly what goes on the chart: \
which drawing kind (rect for a zone, hline for a level, trendline for a slope) and which anchors \
-- from `get_candles` (each candle has an index `i` and a `time_ms`), from `detect_zones` \
(order blocks and support/resistance bands with millisecond anchors), from \
`detect_market_structure` (swing points), from `detect_pattern` (pattern anchors). A DRAW rule \
names the recipe, e.g. \"DRAW a rect from the gap candle's time_ms at its high to the current \
candle's time_ms at its low\".\n\
- `invalidation` says what kills the zone or setup; `examples` walk through one real occurrence.\n\n\
## Saving\n\
Call `save_skill_draft` with the complete document. Validation is strict; if it comes back with \
an error, fix the named field and call again -- never describe a skill you have not saved. After \
a successful save, tell the trader the skill's name and that it now appears in their library, \
ready to pin on a chat.";

/// The `save_skill_draft` tool: the whole skill document, validated on receipt.
fn save_draft_spec() -> ToolSpec {
    ToolSpec {
        name: SAVE_DRAFT.into(),
        description: "Save the interviewed skill to the trader's library. Call only with the \
            complete document after the trader confirmed it. `name`, `category`, `knowledge`, \
            and `rules` are required; the rest are optional. The document is pure language -- \
            no code -- and `rules` must include the DRAW rules that put the pattern's zones on \
            the chart."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "The skill's name, e.g. `FVG Scalp`."},
                "category": {"type": "string", "description": "e.g. `zones`, `liquidity`, `market-structure`."},
                "knowledge": {"type": "string", "description": "What the pattern is, in plain language, precise enough to draw."},
                "rules": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Numbered checks, including the DRAW rules with their anchor recipes."
                },
                "invalidation": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "What kills the zone or setup."
                },
                "examples": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"},
                            "walkthrough": {"type": "string"}
                        },
                        "required": ["title", "walkthrough"]
                    },
                    "description": "Worked occurrences, optional."
                },
                "preferred_markets": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Symbols the skill was written for, optional."
                },
                "preferred_timeframes": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Timeframes the skill was written for, optional."
                }
            },
            "required": ["name", "category", "knowledge", "rules"]
        }),
        exposed_tool: None,
    }
}

/// Validate a draft into a [`Skill`], forcing the fields an author never
/// sees: this is always a trading methodology that produces theses, and its
/// first stored version is `1.0`.
fn validate_draft(input: &Value) -> Result<Skill, Vec<String>> {
    let mut errors = Vec::new();
    let mut skill: Skill = match serde_json::from_value(input.clone()) {
        Ok(skill) => skill,
        Err(e) => {
            errors.push(format!("the document does not match the skill shape: {e}"));
            return Err(errors);
        }
    };
    skill.version = "1.0".into();
    skill.kind = crate::skills::SkillKind::Trading;
    skill.artifact_kind = crate::skills::ArtifactKind::Thesis;

    if skill.name.trim().is_empty() {
        errors.push("`name` is empty -- the skill needs the trader's name for it".into());
    }
    if skill.category.trim().is_empty() {
        errors.push("`category` is empty -- e.g. `zones`, `liquidity`, `market-structure`".into());
    }
    if skill.knowledge.trim().is_empty() {
        errors.push("`knowledge` is empty -- the concept must be taught, not implied".into());
    }
    if skill.rules.is_empty() {
        errors.push("`rules` is empty -- the checks the setup must pass".into());
    }
    if !skill
        .rules
        .iter()
        .any(|r| r.trim_start().to_ascii_uppercase().starts_with("DRAW"))
    {
        errors.push(
            "no DRAW rule -- a zone skill that never says what to draw leaves the chart bare; \
             add at least one rule starting with DRAW naming the drawing kind and its anchors"
                .into(),
        );
    }
    if errors.is_empty() {
        Ok(skill)
    } else {
        Err(errors)
    }
}

impl Agent {
    /// Run one beat of a skill-authoring interview (`docs/47`).
    ///
    /// The transcript belongs to the caller and comes back whole every time;
    /// the reply is either the next question (text) or the saved skill (the
    /// model called `save_skill_draft` and the store accepted it).
    ///
    /// # Errors
    /// Only for infrastructure failures -- the model itself failing. A
    /// rejected draft is a conversation turn, not an error.
    pub async fn author_skill(&self, request: &AuthorRequest) -> Result<AuthorAnswer, AgentError> {
        let spec = save_draft_spec();
        // History first, then the new message; consecutive same-role turns
        // are merged because providers reject them, and a client transcript
        // may have picked up two assistant beats in a row.
        let mut messages: Vec<Message> = Vec::with_capacity(request.history.len() + 1);
        for turn in &request.history {
            let role = if turn.role == "assistant" {
                Role::Assistant
            } else {
                Role::User
            };
            push_merged(&mut messages, role, turn.text.clone());
        }
        push_merged(&mut messages, Role::User, request.message.clone());

        let mut usage = Usage {
            input_tokens: None,
            output_tokens: None,
        };
        let mut turns = 0_usize;
        // One user message should cost at most a few round trips: a text
        // reply is one; a draft that failed validation twice is the unusual
        // case the budget exists for.
        const MAX_TURNS: usize = 5;

        while turns < MAX_TURNS {
            turns += 1;
            let response = self
                .llm()
                .complete(LlmRequest {
                    system: Some(AUTHOR_SYSTEM.into()),
                    messages: messages.clone(),
                    tools: vec![spec.clone()],
                    tool_choice: None,
                    max_tokens: self.max_tokens(),
                    temperature: 0.2,
                })
                .await?;
            usage = usage + response.usage;

            let text = response.text();
            let calls = response.tool_calls();

            if calls.is_empty() {
                // A plain-text beat is the interview moving: the next
                // question, or the summary awaiting confirmation.
                let message = if text.trim().is_empty() {
                    "Tell me more about the pattern you trade -- which candles make it, and what \
                     you wait for before you enter?"
                        .into()
                } else {
                    text
                };
                return Ok(AuthorAnswer {
                    message,
                    saved: None,
                    turns,
                    usage,
                });
            }

            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                if call.name != SAVE_DRAFT {
                    results.push(ToolResult {
                        tool_use_id: call.id.clone(),
                        content: json!({"error": format!("`{}` is not a tool of this interview", call.name)}),
                        is_error: true,
                    });
                    continue;
                }
                match save_draft(request, &call).await {
                    Ok(saved) => {
                        return Ok(AuthorAnswer {
                            message: format!(
                                "Saved **{}** (v{}) to your skill library. Pin it on any chat and \
                                 I will follow it -- finding its zones on the candles and drawing \
                                 them as its DRAW rules say.",
                                saved.name, saved.version
                            ),
                            saved: Some(saved),
                            turns,
                            usage,
                        });
                    }
                    Err(errors) => {
                        results.push(ToolResult {
                            tool_use_id: call.id.clone(),
                            content: json!({
                                "error": "the draft did not validate; fix the fields and call again",
                                "problems": errors,
                            }),
                            is_error: true,
                        });
                    }
                }
            }
            // The assistant message carries its tool_use blocks; the results
            // must follow it exactly (the provider rejects a tool_result that
            // does not directly answer a tool_use).
            messages.push(response.message.clone());
            messages.push(Message::tool_results(results));
        }

        Ok(AuthorAnswer {
            message: "The draft kept failing validation. Tell me the pattern's name and its \
                      rules once more, plainly, and I will write it up cleanly."
                .into(),
            saved: None,
            turns,
            usage,
        })
    }
}

/// Validate, then store. Validation problems go back to the model as the
/// tool result; store failures do the same, worded so the model can relay
/// them -- "a skill by this name already exists" is something the trader
/// resolves, not something the agent retries.
async fn save_draft(request: &AuthorRequest, call: &ToolCall) -> Result<SavedSkill, Vec<String>> {
    let skill = validate_draft(&call.input)?;
    let id = skill.id();
    request
        .authoring
        .writer()
        .save(request.authoring.user_id(), &skill)
        .await
        .map_err(|e| vec![format!("the library refused the save: {e}")])?;
    Ok(SavedSkill {
        id,
        name: skill.name,
        version: skill.version,
    })
}

/// Append a text turn, merging into the previous message when the role
/// repeats -- providers reject consecutive same-role messages.
fn push_merged(messages: &mut Vec<Message>, role: Role, text: String) {
    if let Some(last) = messages.last_mut() {
        if last.role == role {
            last.content.push(ContentBlock::Text(format!("\n\n{text}")));
            return;
        }
    }
    messages.push(Message {
        role,
        content: vec![ContentBlock::Text(text)],
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::{LlmResponse, ScriptedClient, StopReason};
    use crate::skills::SkillLibrary;
    use std::sync::Mutex;

    /// A writer that keeps what it was asked to save.
    struct VecWriter(Mutex<Vec<(String, Skill)>>);

    #[async_trait]
    impl SkillWriter for VecWriter {
        async fn save(&self, user_id: &str, skill: &Skill) -> Result<String, AgentError> {
            self.0
                .lock()
                .expect("lock poisoned")
                .push((user_id.to_string(), skill.clone()));
            Ok(skill.id())
        }
    }

    fn text_reply(text: &str) -> LlmResponse {
        LlmResponse {
            message: Message::assistant(text),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        }
    }

    fn draft_call(id: &str, input: Value) -> LlmResponse {
        LlmResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: id.into(),
                    name: SAVE_DRAFT.into(),
                    input,
                }],
            },
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        }
    }

    fn fvg_draft() -> Value {
        json!({
            "name": "FVG Scalp",
            "category": "zones",
            "knowledge": "A fair value gap is a three-candle gap: the high of candle 1 is below \
                the low of candle 3, so candle 2 traded through territory nobody revisited. The \
                gap between candle 1's high and candle 3's low is the zone; price returning into \
                it is the entry.",
            "rules": [
                "The zone exists only while the gap is unfilled.",
                "DRAW a rect from candle 1's time_ms at its high to the current candle's time_ms \
                 at candle 3's low -- that band is the gap.",
                "Enter on the first touch of the zone after the gap forms."
            ],
            "invalidation": ["A full fill of the gap kills the setup."]
        })
    }

    fn setup(responses: Vec<LlmResponse>) -> (Agent, Arc<VecWriter>, Arc<ScriptedClient>) {
        let client = Arc::new(ScriptedClient::new(responses));
        let writer = Arc::new(VecWriter(Mutex::new(Vec::new())));
        let agent = Agent::new(
            client.clone(),
            SkillLibrary::from_skills(Vec::new()),
            crate::agent::AgentConfig::default(),
        );
        (agent, writer, client)
    }

    fn request(writer: Arc<VecWriter>, message: &str, history: Vec<ChatTurn>) -> AuthorRequest {
        AuthorRequest {
            message: message.into(),
            history,
            authoring: AuthoringContext::new("user-1", writer),
        }
    }

    #[tokio::test]
    async fn a_text_reply_is_the_next_interview_question() {
        let (agent, writer, client) = setup(vec![text_reply("Which candles make the zone?")]);
        let answer = agent
            .author_skill(&request(writer.clone(), "I trade fair value gaps.", Vec::new()))
            .await
            .unwrap();
        assert_eq!(answer.message, "Which candles make the zone?");
        assert!(answer.saved.is_none());
        assert_eq!(answer.turns, 1);
        assert_eq!(client.call_count(), 1);
        assert!(writer.0.lock().unwrap().is_empty(), "no question saves a skill");
    }

    #[tokio::test]
    async fn a_confirmed_draft_is_validated_and_saved() {
        let (agent, writer, _) = setup(vec![draft_call("t1", fvg_draft())]);
        let answer = agent
            .author_skill(&request(
                writer.clone(),
                "Yes, that's exactly it. Save it.",
                Vec::new(),
            ))
            .await
            .unwrap();
        let saved = answer.saved.expect("a confirmed draft is stored");
        assert_eq!(saved.name, "FVG Scalp");
        assert_eq!(saved.version, "1.0");
        assert_eq!(saved.id, "fvg-scalp-v1");
        let stored = writer.0.lock().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].0, "user-1", "the skill lands under the author");
        assert_eq!(stored[0].1.kind, crate::skills::SkillKind::Trading);
    }

    #[tokio::test]
    async fn an_invalid_draft_comes_back_for_correction_instead_of_saving() {
        let mut bad = fvg_draft();
        // No DRAW rule: a zone skill that never says what to draw.
        bad["rules"] = json!(["Enter on the first touch."]);
        let (agent, writer, client) = setup(vec![
            draft_call("t1", bad),
            text_reply("I missed the drawing rule -- adding it now."),
        ]);
        let answer = agent
            .author_skill(&request(writer.clone(), "save it", Vec::new()))
            .await
            .unwrap();
        assert!(answer.saved.is_none(), "an invalid draft is not stored");
        assert!(writer.0.lock().unwrap().is_empty());
        // The second request must carry the validation problems as the tool
        // result, so the model can see exactly what to fix.
        let requests = client.requests();
        assert_eq!(requests.len(), 2);
        let correction = requests[1].messages.last().expect("a correction round");
        let rendered = serde_json::to_string(&correction.content).unwrap();
        assert!(
            rendered.contains("DRAW"),
            "the refusal names the missing rule: {rendered}"
        );
        assert_eq!(answer.turns, 2);
    }

    #[tokio::test]
    async fn repeated_roles_in_the_history_are_merged() {
        let (agent, writer, client) = setup(vec![text_reply("go on")]);
        let history = vec![
            ChatTurn { role: "user".into(), text: "I trade gaps.".into() },
            ChatTurn { role: "user".into(), text: "Three-candle ones.".into() },
            ChatTurn { role: "assistant".into(), text: "How do you enter?".into() },
        ];
        let _ = agent
            .author_skill(&request(writer, "On the retest.", history))
            .await
            .unwrap();
        let sent = &client.requests()[0].messages;
        // user+user merged, then assistant, then the new user message.
        assert_eq!(sent.len(), 3, "two consecutive user turns become one: {sent:?}");
        assert!(sent[0].text().contains("I trade gaps.") && sent[0].text().contains("Three-candle ones."));
        assert_eq!(sent[2].text(), "On the retest.");
    }

    #[test]
    fn a_draft_must_teach_the_concept_and_say_what_to_draw() {
        let err = validate_draft(&json!({"name": "X", "category": "zones"})).unwrap_err();
        assert!(err.iter().any(|e| e.contains("knowledge")), "{err:?}");
        assert!(err.iter().any(|e| e.contains("rules")), "{err:?}");
        assert!(err.iter().any(|e| e.contains("DRAW")), "{err:?}");
    }
}
