//! The agent's window on the web: a host-attached search capability.
//!
//! The market tools answer what the venue printed; they cannot answer what
//! the world knows -- an indicator's definition, a methodology's rules, the
//! news behind a move. The `search_web` tool fills that gap by delegating to
//! a [`WebSearcher`] the host attaches (in practice the deployment's SearXNG
//! instance), the same posture as drawings, memory, and backtests: `None` is
//! a fact the tool reports, never an empty result set the model may assume.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One ranked search hit, trimmed for a prompt.
///
/// The host is expected to cap count and snippet length before the hits
/// reach the model: the LLM reasons over these, and a web result page is
/// token bait the same way a raw candle dump is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSearchHit {
    /// The page title, as the engine rendered it.
    pub title: String,
    /// Where the hit points. The model cites this in its answer.
    pub url: String,
    /// The engine's snippet for the hit.
    pub snippet: String,
    /// Which underlying engine returned it, when known.
    pub engine: Option<String>,
    /// The hit's published date, when the engine supplied one.
    pub published: Option<String>,
}

/// The host's web search implementation.
///
/// In practice a SearXNG instance, but the trait says nothing about the
/// engine: anything that turns a query into ranked hits qualifies, so tests
/// and future providers need no client changes.
#[async_trait]
pub trait WebSearcher: Send + Sync {
    /// Search the web for `query`, ranked best-first.
    ///
    /// `categories` and `time_range` are pass-through hints the
    /// implementation may honor (SearXNG does); both are optional because
    /// most questions want the instance's defaults.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when the search could not run --
    /// the tool surfaces it to the model verbatim, so it should say what
    /// an operator would act on (e.g. "the SearXNG instance refused
    /// `format=json`; enable `json` in its `search.formats`").
    async fn search(
        &self,
        query: &str,
        categories: Option<&str>,
        time_range: Option<&str>,
    ) -> Result<Vec<WebSearchHit>, String>;
}

impl std::fmt::Debug for dyn WebSearcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The searcher is a callback into the host's HTTP client; printing it
        // would print an address. Its presence is the fact worth logging.
        f.write_str("WebSearcher(..)")
    }
}
