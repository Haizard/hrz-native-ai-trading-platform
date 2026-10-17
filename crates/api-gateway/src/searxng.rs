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

use ai_agent::{WebImage, WebSearchHit, WebSearcher};
use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
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

/// How many pattern images reach the model. Images dominate a turn's token
/// cost the way screenshots do, and three or four references are enough to
/// learn how a concept is conventionally drawn.
const MAX_IMAGES: usize = 4;

/// Byte ceiling per fetched image. A reference chart screenshot is legible
/// well under this; past it the model is paying tokens for resolution it
/// cannot use.
const MAX_IMAGE_BYTES: usize = 1536 * 1024;

/// Per-image fetch ceiling. A slow origin must not hold the whole run: the
/// hit is skipped and the next candidate serves.
const IMAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(8);


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
        let payload = self.search_json(&url).await?;
        Ok(trim_hits(payload.results))
    }

    async fn search_images(&self, query: &str) -> Result<Vec<WebImage>, String> {
        let url = format!(
            "{}/search?q={}&format=json&categories=images",
            self.base_url,
            urlencoding(query)
        );
        let payload = self.search_json(&url).await?;
        // Candidates carry a direct image URL (`img_src`); page-only results
        // cannot be shown to the model and are dropped before any fetching.
        let candidates: Vec<RawHit> = payload
            .results
            .into_iter()
            .filter(|hit| {
                hit.img_src
                    .as_deref()
                    .map(|src| src.starts_with("http://") || src.starts_with("https://"))
                    .unwrap_or(false)
            })
            .collect();
        // Fetch concurrently: four slow origins in series would burn the
        // run's whole budget on the wait alone. A failed or oversized fetch
        // is skipped, never fatal -- the best three images beat an apology
        // for the fourth.
        let fetches = candidates
            .into_iter()
            .take(MAX_IMAGES)
            .map(|hit| self.fetch_image(hit));
        let images = futures::future::join_all(fetches)
            .await
            .into_iter()
            .flatten()
            .collect();
        Ok(images)
    }
}

impl SearxngSearcher {
    /// One search call's shared path: request, status handling, parse.
    async fn search_json(&self, url: &str) -> Result<SearchResponse, String> {
        let response = self
            .http
            .get(url)
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
        response
            .json()
            .await
            .map_err(|e| format!("the SearXNG instance returned an unreadable response: {e}"))
    }

    /// Fetch one image hit's bytes. `None` -- the hit is skipped -- for
    /// anything unreachable, oversized, or not a format the model can see.
    async fn fetch_image(&self, hit: RawHit) -> Option<WebImage> {
        let src = hit.img_src?;
        let response = reqwest::Client::builder()
            .timeout(IMAGE_FETCH_TIMEOUT)
            .build()
            .ok()?
            .get(&src)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        // The declared length is a hint to skip early; the streamed cap
        // below is the enforcement, because origins lie about it.
        if let Some(len) = response.content_length() {
            if len > MAX_IMAGE_BYTES as u64 {
                return None;
            }
        }
        let mut bytes: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.ok()?;
            if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        // The bytes decide the type. Content-type headers and URL
        // extensions both lie -- an error page served as `image/jpeg`
        // passes those checks and then costs the run its most expensive
        // failure: the vision model rejecting an undecodable image.
        let media_type = sniff_media_type(&bytes)?;
        Some(WebImage {
            title: truncate(hit.title.trim(), MAX_TITLE_CHARS),
            page_url: hit.url.trim().to_string(),
            media_type: media_type.to_string(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }
}

/// The image format the bytes themselves declare, when they declare one the
/// model can see. This is the only type check that matters: it cannot be
/// spoofed by a header, and an unrecognized payload never reaches the model.
fn sniff_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
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
/// title-only hits. `img_src` is the direct image URL, present only on
/// results from the `images` category.
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
    #[serde(default)]
    img_src: Option<String>,
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
                img_src: None,
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
                img_src: None,
            },
            RawHit {
                url: "https://example.com".into(),
                title: String::new(),
                content: String::new(),
                engine: Some(" ".into()),
                published_date: None,
                img_src: None,
            },
        ];
        let hits = trim_hits(raw);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com");
        assert_eq!(hits[0].snippet, "");
        assert_eq!(hits[0].engine, None);
    }

    #[test]
    fn sniff_media_type_reads_the_bytes_not_the_claim() {
        assert_eq!(
            sniff_media_type(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A]),
            Some("image/png")
        );
        assert_eq!(sniff_media_type(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_media_type(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff_media_type(b"RIFF....WEBPVP8 "), Some("image/webp"));
        // The live failure: an HTML error page served with an image
        // content-type. The bytes give it away.
        assert_eq!(sniff_media_type(b"<html><body>404</body></html>"), None);
        assert_eq!(sniff_media_type(b""), None);
        assert_eq!(sniff_media_type(&[0x89, b'P']), None);
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
