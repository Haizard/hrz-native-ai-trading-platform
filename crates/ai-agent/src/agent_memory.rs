//! Agent memory: what the AI believes, kept between conversations (`docs/09`).
//!
//! ## Why a trait, and why the user rides with it
//!
//! This is [`crate::user_drawings`]'s pattern a second time: the *crate* knows
//! what a memory is, the *host* knows who is asking. A trait object with an
//! opaque user id, attached by the gateway after authentication, means there is
//! no path by which the model could read or write another user's facts — the
//! identity is resolved server-side and never parsed from a request body.
//!
//! ## What lives here and what does not
//!
//! Storage (`agent_memory` table, newest-wins upsert, retention cap) is
//! `db`'s business; this module is the *agent's* vocabulary for it. The
//! three surfaces are:
//!
//! * **Tools** (`remember`, `recall_memories`, `forget_memory`) — the
//!   explicit path, so the model can store a level the user dictated or drop
//!   a fact it can see has been invalidated.
//! * **Auto-recall** — [`render_recall`] turns the user's newest facts into a
//!   prompt section before the first LLM call, because a memory the model
//!   must remember to ask for is a memory it will not use.
//! * **Auto-store** — [`facts_from_thesis`] extracts the key structural
//!   claims from a submitted thesis and [`MemoryWriter::remember`]s them
//!   after the turn ends, so "4H resistance at 108,500" survives even when
//!   the model never thought to call `remember`.
//!
//! Every write — explicit or automatic — goes through one trait method,
//! which is what makes the audit trail one shape.

use crate::error::AgentError;
use crate::thesis::{Bias, TradeThesis};

/// How many facts auto-recall injects into a prompt.
///
/// The recall section competes for attention with the ladder digest; past a
/// couple of dozen lines the oldest facts become noise the model correctly
/// ignores, which is worse than not sending them.
pub const RECALL_LIMIT: usize = 24;

/// The maximum length of one fact, in characters.
///
/// A memory is a claim, not an essay. Longer than this and it is a
/// conversation that belongs in the chat history; truncating keeps one
/// careless `remember` call from dominating the recall section.
pub const MAX_CONTENT_CHARS: usize = 280;

/// One stored fact, as the tools and the prompt see it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryRow {
    /// `symbol` or `global`.
    pub scope: String,
    /// The symbol, exactly when [`Self::scope`] is `symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// What the fact is about, e.g. `4h_resistance`.
    pub key: String,
    /// The fact, in the agent's own words.
    pub content: String,
    /// When it was last stated, unix nanoseconds. This is the recall sort.
    pub updated_at: i64,
}

/// A fact about to be stored.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMemory {
    /// `symbol` or `global`.
    pub scope: String,
    /// The symbol, required exactly when the scope is `symbol`.
    pub symbol: Option<String>,
    /// What the fact is about.
    pub key: String,
    /// The fact.
    pub content: String,
}

impl NewMemory {
    /// A fact about one market, validated as it is built.
    ///
    /// # Errors
    /// [`AgentError::InvalidToolArgs`] for an empty key or content, or a
    /// fact too long to be worth storing whole.
    pub fn about_symbol(
        symbol: &str,
        key: impl Into<String>,
        content: impl Into<String>,
    ) -> Result<Self, AgentError> {
        Self::checked("remember", "symbol", Some(symbol.to_string()), key, content)
    }

    /// A fact about how this user works.
    ///
    /// # Errors
    /// As [`Self::about_symbol`].
    pub fn global(key: impl Into<String>, content: impl Into<String>) -> Result<Self, AgentError> {
        Self::checked("remember", "global", None, key, content)
    }

    /// The tool-path constructor: the model names its own tool in errors.
    ///
    /// Same rules as [`Self::about_symbol`], but the refusal quotes `tool` so
    /// a correction names the call the model actually made.
    pub fn checked_for_tool(
        tool: &str,
        scope: &str,
        symbol: Option<String>,
        key: impl Into<String>,
        content: impl Into<String>,
    ) -> Result<Self, AgentError> {
        Self::checked(tool, scope, symbol, key, content)
    }

    fn checked(
        tool: &str,
        scope: &str,
        symbol: Option<String>,
        key: impl Into<String>,
        content: impl Into<String>,
    ) -> Result<Self, AgentError> {
        let key = key.into().trim().to_string();
        let mut content = content.into().trim().to_string();
        if key.is_empty() {
            return Err(AgentError::InvalidToolArgs {
                tool: tool.into(),
                reason: "`key` is required and must not be empty".into(),
            });
        }
        if content.is_empty() {
            return Err(AgentError::InvalidToolArgs {
                tool: tool.into(),
                reason: "`content` is required and must not be empty".into(),
            });
        }
        if content.chars().count() > MAX_CONTENT_CHARS {
            content = content.chars().take(MAX_CONTENT_CHARS).collect();
        }
        Ok(Self {
            scope: scope.to_string(),
            symbol,
            key,
            content,
        })
    }
}

/// Read-side: this user's newest facts, symbol scope first.
///
/// Implemented one layer up against `db::agent_memory::recall`, with the
/// *requesting* user's id as an opaque key — the same per-user scoping the
/// drawings source enforces.
#[async_trait::async_trait]
pub trait MemorySource: Send + Sync {
    /// The user's newest facts: symbol memories for `symbol` plus global
    /// preferences, newest first, at most `limit` of them.
    ///
    /// # Errors
    /// Any [`crate::AgentError`] when storage cannot answer — reported to the
    /// model rather than read as "no memories".
    async fn recall(
        &self,
        user_id: &str,
        symbol: &str,
        limit: usize,
    ) -> Result<Vec<MemoryRow>, AgentError>;
}

/// Write-side: store and drop facts, as the asking user.
///
/// One trait for both because a host that grants no write access simply does
/// not attach this — and every write, explicit or automatic, funnels through
/// [`Self::remember`], so there is exactly one door to audit.
#[async_trait::async_trait]
pub trait MemoryWriter: Send + Sync {
    /// Store a fact under this user's name, replacing any earlier claim on
    /// the same key (newest-wins; see `db::agent_memory`).
    ///
    /// # Errors
    /// Any [`crate::AgentError`] when storage refuses the write.
    async fn remember(&self, user_id: &str, memory: &NewMemory) -> Result<(), AgentError>;

    /// Drop one fact by key, reporting whether it existed.
    ///
    /// # Errors
    /// Any [`crate::AgentError`] when storage cannot answer.
    async fn forget(
        &self,
        user_id: &str,
        symbol: Option<&str>,
        key: &str,
    ) -> Result<bool, AgentError>;
}

/// Render the recall section for a system prompt.
///
/// Empty when there is nothing — the section header must not appear for a
/// user with no memories, because a header over nothing reads as a failure to
/// the model and invites an apology in the thesis.
#[must_use]
pub fn render_recall(rows: &[MemoryRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut out = String::from("## What you already know about this user\n");
    out.push_str(
        "Facts you established in earlier conversations. They are context, not \
         current market data: re-derive levels from tools before acting on \
         them, and say so when the market has moved past one.\n\n",
    );
    for row in rows {
        let scope = match (&row.scope[..], &row.symbol) {
            ("symbol", Some(symbol)) => symbol.clone(),
            _ => "preferences".to_string(),
        };
        out.push_str(&format!("- [{}] {}: {}\n", scope, row.key, row.content));
    }
    out
}

/// Extract the structural claims a thesis makes, as facts worth keeping.
///
/// This is the auto-store half of memory: a thesis **is** the agent standing
/// behind a set of levels, so the turn ending is the moment they are truest.
/// Only levels the model itself cited survive — a level read from tools is a
/// claim, a level invented in prose would have been rejected by grounding
/// before it got here.
#[must_use]
pub fn facts_from_thesis(thesis: &TradeThesis) -> Vec<NewMemory> {
    let side = match thesis.direction {
        Bias::Long => Some("long"),
        Bias::Short => Some("short"),
        // A stand-aside thesis names no levels worth keeping: its "entry" is
        // not an entry, and recalling it later would invite acting on it.
        Bias::None => None,
    };
    let mut facts = Vec::new();
    // The trade itself: key, level, stop. One fact per thesis rather than
    // three, because the three move together — a stale entry beside a fresh
    // stop is worse than either alone.
    if let Some(side) = side {
        if let Ok(fact) = NewMemory::about_symbol(
            &thesis.symbol,
            format!("{side}_levels"),
            format!(
                "{} thesis on {}: entry {:.4}, stop {:.4}, target {:.4} (R:R {:.2}); \
                 invalidates if {}",
                side,
                thesis.timeframe,
                thesis.entry_price,
                thesis.stop_price,
                thesis.target_price,
                thesis.risk_reward,
                thesis.invalidation
            ),
        ) {
            facts.push(fact);
        }
    }
    // Higher-timeframe checks name levels in their prose; those are the
    // structural claims worth carrying forward. Bounded to keep a thesis
    // with twelve checks from dominating the recall section.
    for check in thesis.higher_timeframe_checks.iter().take(4) {
        let key = format!(
            "htf_{}",
            check.label.to_lowercase().replace([' ', '-', '/'], "_")
        );
        if let Ok(fact) = NewMemory::about_symbol(&thesis.symbol, key, &check.detail) {
            facts.push(fact);
        }
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_global_fact_has_no_symbol_and_trims_itself() {
        let fact = NewMemory::global("risk_style", "  risks 0.5R, never averages down  ")
            .expect("a plain fact");
        assert_eq!(fact.scope, "global");
        assert_eq!(fact.symbol, None);
        assert_eq!(fact.content, "risks 0.5R, never averages down");
    }

    #[test]
    fn an_empty_key_or_content_is_refused_not_stored() {
        // A blank fact stored would surface in every later prompt as a line
        // of nothing; refusing at the builder is the one place the reason can
        // reach the model.
        assert!(NewMemory::about_symbol("BTCUSDT", "  ", "x").is_err());
        assert!(NewMemory::about_symbol("BTCUSDT", "key", "   ").is_err());
    }

    #[test]
    fn an_essay_is_truncated_to_the_claim_limit() {
        let essay = "x".repeat(MAX_CONTENT_CHARS + 500);
        let fact = NewMemory::global("ramble", essay).expect("long is legal, just capped");
        assert_eq!(fact.content.chars().count(), MAX_CONTENT_CHARS);
    }

    #[test]
    fn recall_renders_both_scopes_and_nothing_when_empty() {
        // The header over nothing would read to the model as a failure and
        // invite an apology in the thesis, so empty means *no section*.
        assert_eq!(render_recall(&[]), "");

        let rows = vec![
            MemoryRow {
                scope: "symbol".into(),
                symbol: Some("BTCUSDT".into()),
                key: "4h_resistance".into(),
                content: "108,500 held twice".into(),
                updated_at: 2,
            },
            MemoryRow {
                scope: "global".into(),
                symbol: None,
                key: "risk_style".into(),
                content: "risks 0.5R".into(),
                updated_at: 1,
            },
        ];
        let rendered = render_recall(&rows);
        assert!(rendered.contains("[BTCUSDT] 4h_resistance: 108,500 held twice"));
        assert!(rendered.contains("[preferences] risk_style: risks 0.5R"));
        assert!(rendered.starts_with("## What you already know"));
    }

    #[test]
    fn a_thesis_yields_its_levels_and_its_higher_timeframe_claims() {
        let thesis = test_thesis();
        let facts = facts_from_thesis(&thesis);
        let levels = facts
            .iter()
            .find(|f| f.key == "long_levels")
            .expect("the trade's own levels are the primary fact");
        assert_eq!(levels.scope, "symbol");
        assert_eq!(levels.symbol.as_deref(), Some(thesis.symbol.as_str()));
        assert!(levels
            .content
            .contains(&format!("{:.4}", thesis.entry_price)));
        assert!(levels.content.contains(&thesis.invalidation));
        // One fact per higher-timeframe check, keyed by its slugified label.
        for check in thesis.higher_timeframe_checks.iter().take(4) {
            let key = format!("htf_{}", check.label.to_lowercase().replace(' ', "_"));
            assert!(
                facts.iter().any(|f| f.key == key),
                "missing a fact for {key}: {:?}",
                facts.iter().map(|f| &f.key).collect::<Vec<_>>()
            );
        }
    }

    /// A minimal grounded thesis, local to this module: the shared fixture
    /// lives in `thesis`'s own tests, and importing test code across modules
    /// couples them for no shared behaviour.
    fn test_thesis() -> TradeThesis {
        serde_json::from_value(serde_json::json!({
            "symbol": "BTCUSDT",
            "timeframe": "1h",
            "direction": "long",
            "confidence_pct": 70.0,
            "higher_timeframe_checks": [
                {"label": "4h trend", "status": "pass", "detail": "4h trend bullish; poc 100,250", "source": "analyze_timeframe"},
                {"label": "1d range low", "status": "pass", "detail": "daily range low 96,000 held", "source": "analyze_timeframe"}
            ],
            "order_flow_checks": [],
            "entry_price": 104_000.0,
            "stop_price": 102_500.0,
            "target_price": 108_000.0,
            "risk_reward": 2.67,
            "invalidation": "a 1h close below 102,500",
            "narrative": "test",
            "provenance": []
        }))
        .expect("a valid thesis fixture")
    }
}
