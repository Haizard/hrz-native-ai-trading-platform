//! The deployment's SearXNG instance, as the agent's `search_web` backend.
//!
//! One `GET {base}/search?q=...&format=json` per tool call. The response is
//! trimmed before the model ever sees it -- a SearXNG page is dozens of hits
//! with full snippets, and the LLM reasons over the trimmed set (principle
//! #2: the model interprets structured results; it does not read pages).
//!
//! Configured with `SEARXNG_BASE_URL`. When the variable is unset the
//! gateway attaches no searcher and the tool says so, exactly like every
//! other optional capability.

use std::sync::Arc;
use std::time::Duration;

use ai_agent::{WebSearchHit, WebSearcher};
use async_trait::async_trait;
use serde::Deserialize;

/// How many hits reach the model. SearXNG ranks best-first; past the first
/// page of a decent query the marginal hit is noise with a token cost.
const MAX_HITS: usize = 8;

/// Snippet ceiling per hit. Engines return a sentence or two; anything past
/// this is truncation-safe context the model would skim anyway.
const MAX_SNIPPET_CHARS: usize = 320;

/// Title ceiling, for the same reason.
const MAX_TITLE_CHARS: usize = 160;

/// The whole call's ceiling. A search slower than this is an engine problem
/// the model should hear about, not wait on.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);

/// A SearXNG instance the agent can search through.
pub struct SearxngSearcher {
    /// The instance's root URL, without a trailing slash.
    base_url: String,
    http: reqwest::Client,
}

impl SearxngSearcher {
    /// Build a searcher from `SEARXNG_BASE_URL`, or `None` when the variable
    /// is unset or empty -- the tool then reports the capability as absent.
    #[must_use]
    pub fn from_env() -> Option<Arc<Self>> {
        let base_url = std::env::var("SEARXNG_BASE_URL")
            .ok()?
            .trim()
            .trim_end_matches('/')
            .to_string();
        if base_url.is_empty() {
            return None;
        }
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .ok()?;
        Some(Arc::new(Self { base_url, http }))
    }

    /// Create a searcher against an explicit base URL (tests).
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
        }
    }
}

#[async_trait]
impl WebSearcher for SearxngSearcher {
    async fn search(
        &self,
        query: &str,
        categories: Option<&str>,
        time_range: Option<&str>,
    ) -> Result<Vec<WebSearchHit>, String> {
        let mut url = format!(
            "{}/search?q={}&format=json",
            self.base_url,
            urlencoding(query)
        );
        if let Some(categories) = categories {
            url.push_str(&format!("&categories={}", urlencoding(categories)));
        }
        if let Some(time_range) = time_range {
            url.push_str(&format!("&time_range={}", urlencoding(time_range)));
        }
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("the SearXNG instance could not be reached: {e}"))?;
        let status = response.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            // The stock SearXNG default serves HTML only; the JSON API is one
            // setting away, and the message names it so an operator reading
            // the agent's answer knows exactly what to change.
            return Err(
                "the SearXNG instance refused `format=json` (403) -- add `json` to \
                 `search.formats` in its settings.yml and restart it"
                    .to_string(),
            );
        }
        if !status.is_success() {
            return Err(format!("the SearXNG instance answered {status}"));
        }
        let payload: SearchResponse = response
            .json()
            .await
            .map_err(|e| format!("the SearXNG instance returned an unreadable response: {e}"))?;
        Ok(trim_hits(payload.results))
    }
}

/// Percent-encode a query-string value. SearXNG's own URL building accepts
/// the standard set; everything else is encoded, space included.
fn urlencoding(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The response's `results` array, capped and trimmed for a prompt.
fn trim_hits(results: Vec<RawHit>) -> Vec<WebSearchHit> {
    results
        .into_iter()
        .filter(|hit| !hit.url.trim().is_empty())
        .take(MAX_HITS)
        .map(|hit| WebSearchHit {
            title: truncate(hit.title.trim(), MAX_TITLE_CHARS),
            url: hit.url.trim().to_string(),
            snippet: truncate(hit.content.trim(), MAX_SNIPPET_CHARS),
            engine: hit.engine.map(|e| e.trim().to_string()).filter(|e| !e.is_empty()),
            published: hit
                .published_date
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty()),
        })
        .collect()
}

/// Truncate at a char boundary, marking the cut.
fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let cut: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// The wire shape SearXNG answers with; fields we do not use are ignored.
#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<RawHit>,
}

/// One result as SearXNG serializes it. `publishedDate` arrives as an RFC
/// 3339 string when the engine knows it; `content` can be empty on
/// title-only hits.
#[derive(Debug, Deserialize)]
struct RawHit {
    #[serde(default)]
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default, rename = "publishedDate")]
    published_date: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoding_encodes_spaces_and_commas() {
        assert_eq!(urlencoding("btc etf, news"), "btc%20etf%2C%20news");
        assert_eq!(urlencoding("general,news"), "general%2Cnews");
    }

    #[test]
    fn trim_hits_caps_and_trims() {
        let raw: Vec<RawHit> = (0..20)
            .map(|i| RawHit {
                url: format!("https://example.com/{i}"),
                title: format!("Result {i}"),
                content: "x".repeat(500),
                engine: Some("brave".into()),
                published_date: Some("2026-10-16T00:00:00Z".into()),
            })
            .collect();
        let hits = trim_hits(raw);
        assert_eq!(hits.len(), MAX_HITS);
        assert!(hits[0].snippet.chars().count() <= MAX_SNIPPET_CHARS);
        assert!(hits[0].snippet.ends_with('…'));
        assert_eq!(hits[0].engine.as_deref(), Some("brave"));
        assert_eq!(hits[0].published.as_deref(), Some("2026-10-16T00:00:00Z"));
    }

    #[test]
    fn trim_hits_drops_urless_rows_and_empty_snippets_survive() {
        let raw = vec![
            RawHit {
                url: "  ".into(),
                title: "no url".into(),
                content: String::new(),
                engine: None,
                published_date: None,
            },
            RawHit {
                url: "https://example.com".into(),
                title: String::new(),
                content: String::new(),
                engine: Some(" ".into()),
                published_date: None,
            },
        ];
        let hits = trim_hits(raw);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com");
        assert_eq!(hits[0].snippet, "");
        assert_eq!(hits[0].engine, None);
    }

    #[test]
    fn parses_a_searxng_shaped_response() {
        let body = serde_json::json!({
            "query": "volume profile",
            "number_of_results": 2,
            "results": [
                {
                    "url": "https://www.sierrachart.com/index.php?page=doc/VolumeByPrice.html",
                    "title": "Volume by Price",
                    "content": "Displays volume at each price level...",
                    "engine": "duckduckgo",
                    "category": "general",
                    "publishedDate": "2025-01-02T00:00:00"
                },
                {
                    "url": "https://en.wikipedia.org/wiki/Market_profile",
                    "title": "Market profile",
                    "content": "",
                    "engine": "wikipedia",
                    "category": "general"
                }
            ]
        });
        let parsed: SearchResponse = serde_json::from_value(body).expect("the wire shape parses");
        let hits = trim_hits(parsed.results);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Volume by Price");
        assert_eq!(hits[1].snippet, "");
        assert_eq!(hits[1].published, None);
    }
}
