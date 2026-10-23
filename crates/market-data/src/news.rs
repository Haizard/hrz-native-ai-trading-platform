//! The news layer: RSS/Atom ingestion and the sentiment lexicon. The
//! doctrine is the engine's own: a headline is context, never a signal --
//! the lexicon score is the measurable part, and the market's own reaction
//! remains the confirmation. The lexicon is v1, transparent, and pinned:
//! every term is visible, every rule is testable, and nobody pretends a
//! word list is a language model.

use serde::{Deserialize, Serialize};

/// One scored headline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsItem {
    /// The source feed's name.
    pub source: String,
    /// The headline's title.
    pub title: String,
    /// The link.
    pub url: String,
    /// Published, unix nanos when the feed says (0 when it does not).
    pub published_ns: i64,
    /// The lexicon score, -1..1 (positive = risk-on).
    pub score: f64,
    /// The instrument tags the headline mentions (`btc`, `fed`, `gold`...).
    pub tags: Vec<String>,
}

/// The risk-on lexicon (USD/crypto/asset supportive).
const RISK_ON: &[&str] = &[
    "rally", "surge", "soar", "jump", "gain", "rises", "rise", "bull", "bullish",
    "breakout", "record high", "growth", "beat", "beats", "strong", "upgrade",
    "adopt", "adoption", "approve", "approval", "inflow", "inflows", "profit",
    "expansion", "recover", "recovery", "rebound", "optimism", "boost", "outperform",
];

/// The risk-off lexicon.
const RISK_OFF: &[&str] = &[
    "fall", "falls", "drop", "drops", "plunge", "crash", "sink", "sinks", "bear",
    "bearish", "selloff", "sell-off", "fear", "weak", "miss", "misses", "downgrade",
    "outflow", "outflows", "loss", "losses", "recession", "hawkish", "warning",
    "crisis", "collapse", "default", "ban", "hack", "hacked", "lawsuit", "fine",
    "fined", "liquidation", "liquidations", "fraud", "probe", "investigation",
    "inflation", "deficit", "layoffs", "bankruptcy",
];

/// Instrument tags and their keywords.
const TAGS: &[(&str, &[&str])] = &[
    ("btc", &["bitcoin", "btc"]),
    ("eth", &["ethereum", "ether", " eth "]),
    ("gold", &["gold", "xau", "bullion"]),
    ("dollar", &["dollar", "dxy", "greenback", "usd"]),
    ("fed", &["federal reserve", "fed", "fomc", "powell", "federal open market"]),
    ("oil", &["oil", "crude", "wti", "brent"]),
    ("stocks", &["stocks", "equities", "s&p", "nasdaq", "dow jones", "wall street"]),
    ("sol", &["solana", " sol "]),
    ("xrp", &["xrp", "ripple"]),
];

/// Score one headline: the count of risk-on terms minus risk-off terms,
/// normalized by the total hits (a one-word headline scores full strength;
/// a mixed one reads its own balance). Negation is v2's problem, documented.
pub fn score_headline(text: &str) -> f64 {
    let lower = format!(" {} ", text.to_lowercase());
    let count = |terms: &[&str]| -> usize {
        terms.iter().filter(|t| lower.contains(*t)).count()
    };
    let on = count(RISK_ON);
    let off = count(RISK_OFF);
    let total = on + off;
    if total == 0 {
        return 0.0;
    }
    (on as f64 - off as f64) / total as f64
}

/// Tag a headline with the instruments it mentions.
pub fn tag_headline(text: &str) -> Vec<String> {
    let lower = format!(" {} ", text.to_lowercase());
    TAGS
        .iter()
        .filter(|(_, keywords)| keywords.iter().any(|k| lower.contains(k)))
        .map(|(tag, _)| tag.to_string())
        .collect()
}

/// The rolling sentiment of a headline set: the mean score, the item count,
/// and the per-tag means. Reported as the context it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SentimentReport {
    /// The mean score over the window's headlines.
    pub mean: f64,
    /// How many headlines.
    pub count: usize,
    /// Risk-on share of the scored (non-zero) headlines.
    pub risk_on_share: f64,
    /// Per-tag mean score.
    pub per_tag: std::collections::BTreeMap<String, f64>,
}

/// Compose the report.
pub fn sentiment_report(items: &[NewsItem], tag: Option<&str>) -> SentimentReport {
    let filtered: Vec<&NewsItem> = match tag {
        Some(t) => items.iter().filter(|i| i.tags.iter().any(|x| x == t)).collect(),
        None => items.iter().collect(),
    };
    let count = filtered.len();
    let mean = if count > 0 {
        filtered.iter().map(|i| i.score).sum::<f64>() / count as f64
    } else {
        0.0
    };
    let scored: Vec<f64> = filtered.iter().map(|i| i.score).filter(|s| *s != 0.0).collect();
    let risk_on_share = if scored.is_empty() {
        0.5
    } else {
        scored.iter().filter(|s| **s > 0.0).count() as f64 / scored.len() as f64
    };
    let mut per_tag: std::collections::BTreeMap<String, Vec<f64>> = std::collections::BTreeMap::new();
    for item in &filtered {
        for t in &item.tags {
            per_tag.entry(t.clone()).or_default().push(item.score);
        }
    }
    SentimentReport {
        mean,
        count,
        risk_on_share,
        per_tag: per_tag
            .into_iter()
            .map(|(t, scores)| (t, scores.iter().sum::<f64>() / scores.len() as f64))
            .collect(),
    }
}

/// The feed set: central banks, crypto-native, and wires. Free RSS, no
/// keys, each named for the record.
pub const FEEDS: &[(&str, &str)] = &[
    ("federal_reserve", "https://www.federalreserve.gov/feeds/press_all.xml"),
    ("coindesk", "https://www.coindesk.com/arc/outboundfeeds/rss/"),
    ("cointelegraph", "https://cointelegraph.com/rss"),
    ("binance_blog", "https://www.binance.com/en/blog/rss"),
];

/// Parse an RSS/Atom feed's items, tolerant of both shapes (RSS `item` and
/// Atom `entry`), titles and links only -- the scoring reads the title, and
/// a summary full of markup would only add noise.
pub fn parse_feed(xml: &str, source: &str) -> Vec<NewsItem> {
    let mut items = Vec::new();
    // Split on item/entry boundaries; extract title and link textually. A
    // feed is not XML we trust -- it is text we skim.
    for chunk in xml.split("<item").skip(1).chain(xml.split("<entry").skip(1)) {
        let title = extract_tag(chunk, "title");
        let link = extract_link(chunk);
        let published = extract_tag(chunk, "pubDate")
            .or_else(|| extract_tag(chunk, "published"))
            .or_else(|| extract_tag(chunk, "updated"));
        if let Some(title) = title {
            let clean = clean_text(&title);
            if clean.len() < 4 {
                continue;
            }
            items.push(NewsItem {
                source: source.to_string(),
                score: score_headline(&clean),
                tags: tag_headline(&clean),
                title: clean,
                url: link.unwrap_or_default(),
                published_ns: published.map(|p| parse_date(&p)).unwrap_or(0),
            });
        }
    }
    items
}

fn extract_tag(chunk: &str, tag: &str) -> Option<String> {
    let open = chunk.find(&format!("<{tag}"))?;
    let start = chunk[open..].find('>')? + open + 1;
    let end = chunk[start..].find(&format!("</{tag}>"))? + start;
    Some(chunk[start..end].to_string())
}

fn extract_link(chunk: &str) -> Option<String> {
    // RSS <link>text</link>, or Atom <link href="..."/>.
    if let Some(link) = extract_tag(chunk, "link") {
        return Some(link);
    }
    let open = chunk.find("<link")?;
    let href = chunk[open..].find("href=\"")? + open + 6;
    let end = chunk[href..].find('"')? + href;
    Some(chunk[href..end].to_string())
}

fn clean_text(raw: &str) -> String {
    raw.replace("<![CDATA[", "")
        .replace("]]>", "")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&apos;", "'")
        .trim()
        .to_string()
}

/// Parse the common feed date forms (RFC 822 and RFC 3339), to unix nanos.
fn parse_date(raw: &str) -> i64 {
    // RFC 3339 first: 2026-10-23T12:00:00Z.
    if let Some(t_pos) = raw.find('T') {
        let date = &raw[..10.min(raw.len())];
        let time = &raw[t_pos + 1..];
        let y: i64 = date[0..4].parse().unwrap_or(2026);
        let m: i64 = date[5..7].parse().unwrap_or(1);
        let d: i64 = date[8..10].parse().unwrap_or(1);
        let h: i64 = time.get(0..2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let min: i64 = time.get(3..5).and_then(|s| s.parse().ok()).unwrap_or(0);
        return days_from_civil_pub(y, m, d) * 3_600_000_000_000 * 24
            + (h * 3600 + min * 60) * 1_000_000_000;
    }
    // RFC 822: Fri, 23 Oct 2026 12:00:00 GMT -- approximate by the parts.
    let months = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let lower = raw.to_lowercase();
    let month = months.iter().position(|m| lower.contains(m)).map(|p| p as i64 + 1).unwrap_or(1);
    let day: i64 = lower
        .split_whitespace()
        .find_map(|w| w.parse().ok())
        .unwrap_or(1);
    let year: i64 = lower
        .split_whitespace()
        .filter_map(|w| w.parse::<i64>().ok())
        .find(|y| *y > 1990)
        .unwrap_or(2026);
    days_from_civil_pub(year, month, day) * 3_600_000_000_000 * 24
}

fn days_from_civil_pub(y: i64, m: i64, d: i64) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Fetch one feed's items.
#[cfg(feature = "net")]
pub async fn fetch_feed(client: &reqwest::Client, name: &str, url: &str) -> Result<Vec<NewsItem>, String> {
    let body = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "hrz-econ-engine/1.0")
        .send()
        .await
        .map_err(|e| format!("{name}: {e}"))?
        .text()
        .await
        .map_err(|e| format!("{name}: {e}"))?;
    Ok(parse_feed(&body, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lexicon_scores_the_way_a_desk_reads() {
        assert!(score_headline("Bitcoin surges to record high on strong inflows") > 0.5);
        assert!(score_headline("Stocks plunge as recession fear grows") < -0.5);
        assert_eq!(score_headline("The committee met on Tuesday"), 0.0);
        // A genuinely mixed headline reads its own balance: two risk-on
        // terms against two risk-off terms is a zero, not a coin flip.
        let mixed = score_headline("Markets surge on profits, but fear of a crash grows");
        assert!(mixed.abs() < 0.01, "two against two is a zero: {mixed}");
    }

    #[test]
    fn the_tags_catch_the_instruments() {
        let tags = tag_headline("Bitcoin and gold rise as the Fed holds rates");
        assert!(tags.contains(&"btc".to_string()));
        assert!(tags.contains(&"gold".to_string()));
        assert!(tags.contains(&"fed".to_string()));
        assert!(!tags.contains(&"oil".to_string()));
    }

    #[test]
    fn the_parser_reads_rss_and_atom() {
        let rss = r#"<?xml version="1.0"?><rss><channel>
            <item><title><![CDATA[Bitcoin surges past $100k]]></title>
            <link>https://example.com/1</link>
            <pubDate>Fri, 23 Oct 2026 12:00:00 GMT</pubDate></item>
            <item><title>Fed holds rates steady</title>
            <link>https://example.com/2</link></item>
        </channel></rss>"#;
        let items = parse_feed(rss, "test");
        assert_eq!(items.len(), 2);
        assert!(items[0].title.contains("Bitcoin surges"));
        assert!(items[0].score > 0.0);
        assert!(items[0].published_ns > 0, "the RFC 822 date parsed");
        let atom = r#"<?xml version="1.0"?><feed>
            <entry><title>Ethereum upgrade approved</title>
            <link href="https://example.com/3" />
            <published>2026-10-23T12:00:00Z</published></entry>
        </feed>"#;
        let items = parse_feed(atom, "test");
        assert_eq!(items.len(), 1);
        assert!(items[0].score > 0.0);
        assert!(items[0].published_ns > 0, "the RFC 3339 date parsed");
    }

    #[test]
    fn the_report_composes() {
        let items = vec![
            NewsItem { source: "a".into(), title: "Bitcoin surges".into(), url: String::new(), published_ns: 0, score: 1.0, tags: vec!["btc".into()] },
            NewsItem { source: "b".into(), title: "Stocks plunge".into(), url: String::new(), published_ns: 0, score: -1.0, tags: vec!["stocks".into()] },
            NewsItem { source: "c".into(), title: "Committee meets".into(), url: String::new(), published_ns: 0, score: 0.0, tags: vec!["fed".into()] },
        ];
        let r = sentiment_report(&items, None);
        assert_eq!(r.count, 3);
        assert!(r.mean.abs() < 0.01);
        assert!((r.risk_on_share - 0.5).abs() < 0.01);
        let btc = sentiment_report(&items, Some("btc"));
        assert_eq!(btc.count, 1);
        assert_eq!(btc.mean, 1.0);
    }
}
