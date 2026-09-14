//! The Skills system: the user's trading methodology, stored as versioned,
//! retrievable documents (`docs/10-SKILLS-SYSTEM.md`).
//!
//! ## Why skills are not in the system prompt
//!
//! Principle #7 and doc 10 both say the same thing: methodology is *retrieved
//! contextually*, never stuffed wholesale into every call. A prompt that
//! carries every skill is a prompt where every skill is half-attended to, and
//! it grows without bound as the user adds more.
//!
//! ## Why skill versions are never mutated
//!
//! A past thesis has to remain explainable against the skill version that
//! produced it. If "improving" a skill rewrote it in place, every historical
//! thesis would silently refer to rules that no longer exist. So
//! [`SkillLibrary::push`] appends; it never replaces. Two documents with the
//! same name and category but different versions coexist, and retrieval
//! returns the newest by version.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::AgentError;

/// One versioned methodology document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Skill {
    /// Human-readable name, e.g. `Liquidity Sweep + Absorption`.
    pub name: String,
    /// Version string, e.g. `2.1`.
    pub version: String,
    /// Category: `liquidity`, `footprint`, `market-structure`, `risk`, ...
    pub category: String,
    /// Prose explanation of the edge. This is what teaches the model the
    /// concept; the rules below are what it must actually check.
    pub knowledge: String,
    /// Numbered conditions that must hold for the setup to count.
    pub rules: Vec<String>,
    /// Machine-readable framing: which timeframes, what risk ceiling.
    pub conditions: SkillConditions,
    /// Worked historical examples.
    #[serde(default)]
    pub examples: Vec<SkillExample>,
    /// What would prove the thesis wrong.
    #[serde(default)]
    pub invalidation: Vec<String>,
    /// Symbols this skill was written for.
    #[serde(default)]
    pub preferred_markets: Vec<String>,
    /// Timeframes this skill was written for.
    #[serde(default)]
    pub preferred_timeframes: Vec<String>,
}

impl Default for Skill {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: "1.0".into(),
            category: "general".into(),
            knowledge: String::new(),
            rules: Vec::new(),
            conditions: SkillConditions::default(),
            examples: Vec::new(),
            invalidation: Vec::new(),
            preferred_markets: Vec::new(),
            preferred_timeframes: Vec::new(),
        }
    }
}

/// The machine-readable part of a skill.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillConditions {
    /// Timeframe ladder this skill reasons over, coarse to fine.
    pub timeframes: Vec<String>,
    /// Risk ceiling for strategies derived from this skill.
    pub risk: SkillRisk,
}

/// Risk limits a skill imposes on strategies derived from it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillRisk {
    /// Maximum account risk per trade, in percent.
    pub max_risk_pct: f64,
}

impl Default for SkillRisk {
    fn default() -> Self {
        Self { max_risk_pct: 1.0 }
    }
}

/// A worked example attached to a skill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillExample {
    /// What happened, with numbers where they are known.
    pub description: String,
}

impl Skill {
    /// The stable id used in `skill_ref` and cited in a thesis.
    ///
    /// `Liquidity Sweep + Absorption` at `2.1` becomes
    /// `liquidity-sweep-absorption-v2`. The minor version is dropped on
    /// purpose: an id that changed on every patch would break the link between
    /// a stored thesis and the skill that produced it.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}-v{}", slug(&self.name), self.version_key().0)
    }

    /// `(major, minor)` parsed from the version string.
    ///
    /// Unparseable versions sort as `(0, 0)` rather than being rejected: a
    /// skill with a malformed version is still usable, it just sorts last.
    #[must_use]
    pub fn version_key(&self) -> (u32, u32) {
        let mut parts = self.version.split('.');
        let major = parts
            .next()
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or(0);
        let minor = parts
            .next()
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or(0);
        (major, minor)
    }

    /// Whether this skill declares the given market.
    #[must_use]
    pub fn covers_market(&self, symbol: &str) -> bool {
        if self.preferred_markets.is_empty() {
            return true;
        }
        let upper = symbol.to_ascii_uppercase();
        self.preferred_markets
            .iter()
            .any(|market| market.to_ascii_uppercase() == upper)
    }

    /// Whether this skill declares the given timeframe.
    #[must_use]
    pub fn covers_timeframe(&self, timeframe: &str) -> bool {
        if self.conditions.timeframes.is_empty() && self.preferred_timeframes.is_empty() {
            return true;
        }
        let wanted = timeframe.trim().to_ascii_lowercase();
        self.conditions
            .timeframes
            .iter()
            .chain(self.preferred_timeframes.iter())
            .any(|tf| tf.trim().to_ascii_lowercase() == wanted)
    }

    /// Render the skill for injection into a prompt.
    ///
    /// Deliberately terse and structured. Prose the model has to skim is prose
    /// the model will paraphrase; numbered rules are the part it actually
    /// checks against, so they are the part given the most prominence.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "SKILL: {} (version {})\nCATEGORY: {}\nID: {}\n",
            self.name,
            self.version,
            self.category,
            self.id()
        );

        if !self.knowledge.is_empty() {
            out.push_str(&format!("\nWHAT IT IS\n{}\n", self.knowledge.trim()));
        }

        if !self.rules.is_empty() {
            out.push_str("\nRULES TO CHECK\n");
            for (i, rule) in self.rules.iter().enumerate() {
                out.push_str(&format!("  {}. {}\n", i + 1, rule.trim()));
            }
        }

        if !self.conditions.timeframes.is_empty() {
            out.push_str(&format!(
                "\nTIMEFRAME LADDER: {}\n",
                self.conditions.timeframes.join(" -> ")
            ));
        }
        out.push_str(&format!(
            "MAX RISK PER TRADE: {}%\n",
            self.conditions.risk.max_risk_pct
        ));

        if !self.preferred_markets.is_empty() {
            out.push_str(&format!("MARKETS: {}\n", self.preferred_markets.join(", ")));
        }

        if !self.invalidation.is_empty() {
            out.push_str("\nINVALIDATION\n");
            for item in &self.invalidation {
                out.push_str(&format!("  - {}\n", item.trim()));
            }
        }

        if !self.examples.is_empty() {
            out.push_str("\nPAST EXAMPLES\n");
            for example in &self.examples {
                out.push_str(&format!("  - {}\n", example.description.trim()));
            }
        }

        out
    }
}

/// Lowercase, alphanumeric-plus-hyphen slug.
fn slug(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_dash = true; // suppress a leading dash
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// What a request is looking for.
#[derive(Debug, Clone, Default)]
pub struct SkillQuery {
    /// Restrict to one category.
    pub category: Option<String>,
    /// The symbol under discussion.
    pub market: Option<String>,
    /// The timeframe under discussion.
    pub timeframe: Option<String>,
    /// Free-text terms from the user's question.
    pub terms: Vec<String>,
}

impl SkillQuery {
    /// A query for one market.
    #[must_use]
    pub fn for_market(symbol: &str) -> Self {
        Self {
            market: Some(symbol.to_string()),
            ..Self::default()
        }
    }

    /// Add free-text terms.
    #[must_use]
    pub fn with_terms(mut self, terms: impl IntoIterator<Item = String>) -> Self {
        self.terms.extend(terms);
        self
    }
}

/// A collection of skill versions with contextual retrieval.
#[derive(Debug, Clone, Default)]
pub struct SkillLibrary {
    skills: Vec<Skill>,
}

impl SkillLibrary {
    /// An empty library.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from already-parsed documents.
    #[must_use]
    pub fn from_skills(skills: Vec<Skill>) -> Self {
        Self { skills }
    }

    /// Add a skill version. Never replaces: see the module docs.
    pub fn push(&mut self, skill: Skill) {
        self.skills.push(skill);
    }

    /// Parse one document from YAML or JSON.
    ///
    /// # Errors
    /// [`AgentError::InvalidSkill`] when the text does not parse.
    pub fn parse_one(text: &str) -> Result<Skill, AgentError> {
        // Try YAML first: it is a superset of JSON, so one parser covers both
        // and a `.json` skill file still works.
        serde_yaml::from_str::<Skill>(text).map_err(|e| AgentError::InvalidSkill(e.to_string()))
    }

    /// Load every skill document under a directory, recursively.
    ///
    /// One unreadable file is a hard error rather than a skip. A skill library
    /// that silently omits a methodology is worse than one that refuses to
    /// start: the agent would answer confidently without it.
    ///
    /// # Errors
    /// [`AgentError::InvalidSkill`] on a parse failure, or a wrapped IO error.
    pub fn load_dir(path: impl AsRef<Path>) -> Result<Self, AgentError> {
        let root = path.as_ref();
        if !root.exists() {
            return Ok(Self::new());
        }

        let mut skills = Vec::new();
        collect(root, &mut skills)?;
        Ok(Self { skills })
    }

    /// Every skill version, in load order.
    #[must_use]
    pub fn all(&self) -> &[Skill] {
        &self.skills
    }

    /// Number of stored versions (not distinct skills).
    #[must_use]
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Whether the library is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Look up a skill by its stable id.
    #[must_use]
    pub fn by_id(&self, id: &str) -> Option<&Skill> {
        self.skills
            .iter()
            .filter(|skill| skill.id() == id)
            .max_by_key(|skill| skill.version_key())
    }

    /// The newest version of every distinct skill name.
    #[must_use]
    pub fn latest_versions(&self) -> Vec<&Skill> {
        let mut latest: Vec<&Skill> = Vec::new();
        for skill in &self.skills {
            match latest
                .iter_mut()
                .find(|existing| existing.name == skill.name)
            {
                Some(existing) => {
                    if skill.version_key() > existing.version_key() {
                        *existing = skill;
                    }
                }
                None => latest.push(skill),
            }
        }
        latest
    }

    /// Retrieve skills matching the query, best first.
    ///
    /// Only skills with a positive score are returned. An unmatched request
    /// produces an empty list, which the caller turns into
    /// [`AgentError::NoMatchingSkill`] -- the guardrail from doc 10: the agent
    /// must not invent methodology the user never defined.
    #[must_use]
    pub fn retrieve(&self, query: &SkillQuery) -> Vec<&Skill> {
        let mut scored: Vec<(i32, &Skill)> = self
            .latest_versions()
            .into_iter()
            .map(|skill| (score(skill, query), skill))
            .filter(|(score, _)| *score > 0)
            .collect();

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.version_key().cmp(&a.1.version_key()))
                .then_with(|| a.1.name.cmp(&b.1.name))
        });
        scored.into_iter().map(|(_, skill)| skill).collect()
    }
}

/// Relevance score. Zero means "no evidence this skill applies".
fn score(skill: &Skill, query: &SkillQuery) -> i32 {
    let mut score = 0;

    if let Some(category) = &query.category {
        if skill.category.eq_ignore_ascii_case(category.trim()) {
            score += 5;
        }
    }

    if let Some(market) = &query.market {
        if skill.covers_market(market) {
            score += 3;
        } else {
            // A skill written for other markets is a poor match even if the
            // words line up.
            score -= 2;
        }
    }

    if let Some(timeframe) = &query.timeframe {
        if skill.covers_timeframe(timeframe) {
            score += 2;
        }
    }

    // Free-text terms: this is what lets "using my liquidity sweep skill"
    // find the right document without the caller parsing the sentence.
    if !query.terms.is_empty() {
        let haystack = format!(
            "{} {} {} {}",
            skill.name,
            skill.category,
            skill.knowledge,
            skill.rules.join(" ")
        )
        .to_ascii_lowercase();

        for term in &query.terms {
            let term = term.to_ascii_lowercase();
            if term.len() < 3 {
                continue;
            }
            if haystack.contains(&term) {
                score += 2;
            } else if slug(&skill.name).contains(&slug(&term)) {
                score += 3;
            }
        }
    }

    score
}

/// Recursively collect skill documents.
fn collect(dir: &Path, out: &mut Vec<Skill>) -> Result<(), AgentError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| AgentError::InvalidSkill(format!("cannot read {}: {e}", dir.display())))?;

    for entry in entries {
        let entry = entry.map_err(|e| AgentError::InvalidSkill(format!("bad entry: {e}")))?;
        let path = entry.path();

        if path.is_dir() {
            collect(&path, out)?;
            continue;
        }

        let is_skill = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "yaml" | "yml" | "json"));
        if !is_skill {
            continue;
        }

        let text = std::fs::read_to_string(&path).map_err(|e| {
            AgentError::InvalidSkill(format!("cannot read {}: {e}", path.display()))
        })?;
        let skill = SkillLibrary::parse_one(&text)
            .map_err(|e| AgentError::InvalidSkill(format!("{}: {e}", path.display())))?;
        out.push(skill);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SWEEP_V1: &str = r#"
name: "Liquidity Sweep + Absorption"
version: "1.0"
category: "liquidity"
knowledge: >
  A liquidity sweep occurs when price briefly breaks a prior swing high/low to
  trigger resting stop orders, then reverses.
rules:
  - "Higher timeframe structure must be bullish for long setups."
  - "Sell-side liquidity must be swept before entry consideration."
conditions:
  timeframes: ["4h", "1h", "5m"]
  risk:
    max_risk_pct: 1.0
invalidation:
  - "5m close below the swept low with no reclaim."
preferred_markets: ["BTCUSDT", "ETHUSDT"]
preferred_timeframes: ["4h", "1h", "5m"]
"#;

    const SWEEP_V2: &str = r#"
name: "Liquidity Sweep + Absorption"
version: "2.1"
category: "liquidity"
knowledge: >
  A liquidity sweep occurs when price briefly breaks a prior swing high/low to
  trigger resting stop orders, then reverses. Absorption confirms it.
rules:
  - "Higher timeframe structure must be bullish for long setups."
  - "Sell-side liquidity must be swept before entry consideration."
  - "Absorption must be detected at the sweep level."
conditions:
  timeframes: ["4h", "1h", "5m"]
  risk:
    max_risk_pct: 0.5
preferred_markets: ["BTCUSDT"]
preferred_timeframes: ["4h", "1h", "5m"]
"#;

    #[test]
    fn a_skill_parses_from_the_shape_in_docs_10() {
        let skill = SkillLibrary::parse_one(SWEEP_V2).unwrap();
        assert_eq!(skill.name, "Liquidity Sweep + Absorption");
        assert_eq!(skill.version, "2.1");
        assert_eq!(skill.category, "liquidity");
        assert_eq!(skill.rules.len(), 3);
        assert_eq!(skill.conditions.timeframes, vec!["4h", "1h", "5m"]);
        assert!((skill.conditions.risk.max_risk_pct - 0.5).abs() < 1e-9);
        assert_eq!(skill.preferred_markets, vec!["BTCUSDT"]);
    }

    #[test]
    fn the_id_is_stable_across_patch_versions() {
        let v2 = SkillLibrary::parse_one(SWEEP_V2).unwrap();
        let mut v3 = v2.clone();
        v3.version = "2.2".into();
        assert_eq!(v2.id(), v3.id(), "a patch must not change the id");

        let v1 = SkillLibrary::parse_one(SWEEP_V1).unwrap();
        assert_ne!(v1.id(), v2.id(), "a major bump must change the id");
        assert_eq!(v2.id(), "liquidity-sweep-absorption-v2");
    }

    #[test]
    fn versions_coexist_and_retrieval_returns_the_newest() {
        let library = SkillLibrary::from_skills(vec![
            SkillLibrary::parse_one(SWEEP_V1).unwrap(),
            SkillLibrary::parse_one(SWEEP_V2).unwrap(),
        ]);
        assert_eq!(library.len(), 2, "both versions are retained");

        let found = library.retrieve(&SkillQuery::for_market("BTCUSDT"));
        assert_eq!(found.len(), 1, "one skill, not one per version");
        assert_eq!(found[0].version, "2.1");
    }

    #[test]
    fn retrieval_is_contextual_not_wholesale() {
        let other = Skill {
            name: "VWAP Reversion".into(),
            version: "1.0".into(),
            category: "mean-reversion".into(),
            knowledge: "Fade extremes back to VWAP.".into(),
            rules: vec!["Price must be two ATRs from VWAP.".into()],
            preferred_markets: vec!["ETHUSDT".into()],
            ..Skill::default()
        };
        let library =
            SkillLibrary::from_skills(vec![SkillLibrary::parse_one(SWEEP_V2).unwrap(), other]);

        // BTCUSDT -> only the sweep skill claims that market.
        let found = library.retrieve(&SkillQuery::for_market("BTCUSDT"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Liquidity Sweep + Absorption");

        // ETHUSDT -> the reversion skill.
        let found = library.retrieve(&SkillQuery::for_market("ETHUSDT"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "VWAP Reversion");
    }

    #[test]
    fn an_unmatched_request_yields_nothing_rather_than_a_guess() {
        let library = SkillLibrary::from_skills(vec![SkillLibrary::parse_one(SWEEP_V2).unwrap()]);
        let found = library.retrieve(&SkillQuery::for_market("DOGEUSDT"));
        assert!(found.is_empty(), "no skill claims DOGEUSDT");
    }

    #[test]
    fn free_text_terms_find_a_skill_by_what_the_user_calls_it() {
        let library = SkillLibrary::from_skills(vec![SkillLibrary::parse_one(SWEEP_V2).unwrap()]);
        let query = SkillQuery::default().with_terms(vec!["liquidity".into(), "sweep".into()]);
        let found = library.retrieve(&query);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Liquidity Sweep + Absorption");
    }

    #[test]
    fn short_terms_are_ignored_so_they_cannot_match_everything() {
        let library = SkillLibrary::from_skills(vec![SkillLibrary::parse_one(SWEEP_V2).unwrap()]);
        // "a" is in everything; it must not be a signal.
        let found = library.retrieve(&SkillQuery::default().with_terms(vec!["a".into()]));
        assert!(found.is_empty());
    }

    #[test]
    fn lookup_by_id_finds_the_newest_matching_version() {
        let library = SkillLibrary::from_skills(vec![
            SkillLibrary::parse_one(SWEEP_V1).unwrap(),
            SkillLibrary::parse_one(SWEEP_V2).unwrap(),
        ]);
        let skill = library.by_id("liquidity-sweep-absorption-v2").unwrap();
        assert_eq!(skill.version, "2.1");
        assert!(library.by_id("nope").is_none());
    }

    #[test]
    fn the_rendered_skill_carries_rules_and_the_risk_ceiling() {
        let rendered = SkillLibrary::parse_one(SWEEP_V2).unwrap().render();
        assert!(rendered.contains("SKILL: Liquidity Sweep + Absorption (version 2.1)"));
        assert!(rendered.contains("1. Higher timeframe structure"));
        assert!(rendered.contains("MAX RISK PER TRADE: 0.5%"));
        assert!(rendered.contains("ID: liquidity-sweep-absorption-v2"));
    }

    #[test]
    fn invalidation_appears_only_when_the_skill_declares_it() {
        // v1 declares it, v2 does not. Rendering an empty "INVALIDATION"
        // heading would tell the model to look for rules that are not there.
        assert!(SkillLibrary::parse_one(SWEEP_V1)
            .unwrap()
            .render()
            .contains("INVALIDATION"));
        assert!(!SkillLibrary::parse_one(SWEEP_V2)
            .unwrap()
            .render()
            .contains("INVALIDATION"));
    }

    #[test]
    fn a_malformed_document_is_an_error_not_a_silent_empty_library() {
        let err = SkillLibrary::parse_one("name: [unclosed").unwrap_err();
        assert!(matches!(err, AgentError::InvalidSkill(_)), "got {err}");
    }

    #[test]
    fn version_keys_order_numerically_not_lexically() {
        let a = Skill {
            version: "2.10".into(),
            ..Skill::default()
        };
        let b = Skill {
            version: "2.9".into(),
            ..Skill::default()
        };
        assert!(a.version_key() > b.version_key(), "2.10 must beat 2.9");

        let broken = Skill {
            version: "not-a-version".into(),
            ..Skill::default()
        };
        assert_eq!(broken.version_key(), (0, 0));
    }

    #[test]
    fn a_missing_directory_is_an_empty_library_not_an_error() {
        let library = SkillLibrary::load_dir("./definitely-not-here").unwrap();
        assert!(library.is_empty());
    }
}
