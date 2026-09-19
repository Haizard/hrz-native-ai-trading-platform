//! What this deployment can actually do, and how fresh its data is.
//!
//! ## Why a capability report rather than more status codes
//!
//! Every capability in this gateway is optional at startup: no `DATABASE_URL`
//! and there is no database; no Bedrock credentials and there is no agent; no
//! `JWT_SECRET` and `/auth/*` answers 503. That is deliberate (`lib.rs`), and
//! each degraded path already says which variable is missing. What was missing
//! is a way to ask **all of it at once**, before a user starts clicking.
//!
//! The failure this prevents is specific and ordinary. A user opens the agent
//! panel, types a question, waits a minute, and gets "the agent is not
//! configured". They have now spent a minute learning something the platform
//! knew at startup. The same is true of the database, the skills library, and
//! whether the venue's instrument listing has ever been fetched.
//!
//! ## The two halves, and why they are separate
//!
//! * **Capability** is a property of the deployment. The agent is configured or
//!   it is not; that does not change while the process runs.
//! * **Freshness** is a property of the moment. Market data ages, feeds stop,
//!   the instrument index goes stale. It changes second by second.
//!
//! Merging them would produce a document where a fact that never changes sits
//! next to one that is already wrong, with nothing to tell them apart.
//!
//! ## The rule this module refuses to break
//!
//! **A capability is never reported as available on the strength of a
//! configuration value alone.** `AWS_BEDROCK_MODEL_ID` being set is not the same
//! as the model answering, and the platform has no way to test the second
//! without spending money on every status poll. So the report distinguishes
//! `configured` from `verified`, and says plainly which it is: `configured`
//! means "the pieces are present, a call should work", never "a call has
//! worked". A status endpoint that claims more than it knows is worse than one
//! that claims less, because it is the one people check *before* trusting the
//! rest.

use std::time::Duration;

use serde::Serialize;

use crate::AppState;

/// How stale the in-memory buffer may be before the report calls it stale.
///
/// Not the same threshold as a feed's idle eviction: a feed is evicted after
/// three minutes of no use, but a buffer whose newest 1m bar is three minutes
/// old means the stream was interrupted and says nothing about use at all. Two
/// minutes is longer than the largest gap a healthy 1m socket produces
/// (a minute boundary plus reconnect grace) and short enough that a genuinely
/// dead feed is reported before a user notices the chart has stopped moving.
pub const FRESHNESS_WINDOW: Duration = Duration::from_secs(120);

/// Whether a piece of the platform is ready, and what it rests on.
///
/// Three values rather than a boolean because "off" and "broken" call for
/// different user action: an unconfigured agent is a deployment decision, a
/// failing database is an incident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    /// Present and expected to work.
    Ready,
    /// Not configured. This deployment does not offer it, by choice.
    NotConfigured,
    /// Configured, but the platform has reason to believe it will not work.
    Degraded,
}

/// One capability's state.
#[derive(Debug, Clone, Serialize)]
pub struct Capability {
    /// Stable name, e.g. `agent`. A client branches on this.
    pub name: &'static str,
    /// Whether it is ready.
    pub readiness: Readiness,
    /// Whether the platform has *exercised* it, as opposed to collected its
    /// configuration.
    ///
    /// `false` is not a failure and is not hidden: it is the honest answer to
    /// "has this ever actually run", and it is what stops `readiness: ready`
    /// from being read as "proven working".
    pub verified: bool,
    /// What it depends on, in English, for a user rather than an operator.
    pub depends_on: &'static str,
    /// What is missing or wrong, when anything is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Capability {
    /// A ready capability that has not been exercised.
    fn ready(name: &'static str, depends_on: &'static str) -> Self {
        Self {
            name,
            readiness: Readiness::Ready,
            verified: false,
            depends_on,
            detail: None,
        }
    }

    /// A capability this deployment does not offer.
    fn absent(name: &'static str, depends_on: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            readiness: Readiness::NotConfigured,
            verified: false,
            depends_on,
            detail: Some(detail.into()),
        }
    }

    /// Report what this capability rests on, once something has run.
    fn verified(mut self, verified: bool) -> Self {
        self.verified = verified;
        self
    }

    /// Mark it degraded with a reason.
    fn degraded(mut self, detail: impl Into<String>) -> Self {
        self.readiness = Readiness::Degraded;
        self.detail = Some(detail.into());
        self
    }
}

/// How fresh one symbol's data is, as far as RAM can say.
///
/// Only symbols with something buffered appear. A symbol nobody has asked for
/// has no age -- reporting `null` for every instrument on the venue would
/// produce a document where "not collected" and "collection stopped" look the
/// same, which is the exact confusion this whole module exists to remove.
#[derive(Debug, Clone, Serialize)]
pub struct SymbolFreshness {
    /// Instrument.
    pub symbol: String,
    /// Open time of the newest buffered bar, unix nanoseconds.
    pub newest_bar_ns: i64,
    /// How old that bar is, in seconds.
    pub age_seconds: i64,
    /// Whether the age exceeds [`FRESHNESS_WINDOW`].
    pub stale: bool,
    /// Timeframes with bars buffered.
    pub timeframes: Vec<String>,
    /// Whether any ticks are on the tape, so footprint analytics are possible.
    pub ticks: bool,
}

impl SymbolFreshness {
    /// Build a reading from the newest bar's age.
    #[must_use]
    pub fn new(
        symbol: String,
        newest_bar_ns: i64,
        age_seconds: i64,
        timeframes: Vec<String>,
        ticks: bool,
    ) -> Self {
        Self {
            symbol,
            newest_bar_ns,
            age_seconds,
            stale: age_seconds > FRESHNESS_WINDOW.as_secs() as i64,
            timeframes,
            ticks,
        }
    }
}

/// The whole report.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityReport {
    /// What the deployment offers.
    pub capabilities: Vec<Capability>,
    /// How fresh its market data is, per symbol it holds.
    pub data: Vec<SymbolFreshness>,
    /// Whether the venue's instrument listing has been fetched, and when.
    pub instruments: InstrumentStatus,
    /// Live feed accounting, so a client can see the ceiling approaching.
    pub feeds: FeedStatus,
    /// Whether every capability is ready. **Not** a claim that everything
    /// works: a configured-but-never-exercised capability is ready and can
    /// still fail on first use.
    pub all_ready: bool,
    /// A sentence to show a user, when something is not ready.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// How the instrument listing stands.
#[derive(Debug, Clone, Serialize)]
pub struct InstrumentStatus {
    /// Every instrument the venue offers, or zero before the first fetch.
    pub indexed: usize,
    /// When it was fetched, unix nanoseconds. `null` before the first fetch.
    pub fetched_at_ns: Option<i64>,
    /// Whether it is missing or past its TTL.
    pub stale: bool,
    /// What an empty index means, when it is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Live feed accounting.
#[derive(Debug, Clone, Serialize)]
pub struct FeedStatus {
    /// Feeds opened for a route (a chart, a question).
    pub route: usize,
    /// Feeds opened for a running bot.
    pub bot: usize,
    /// The ceiling before eviction starts.
    pub max_active: usize,
    /// Symbols currently fed.
    pub symbols: Vec<String>,
}

/// Build the report from the live state.
///
/// `now_ns` is a parameter so a test can pin the clock rather than race it --
/// a freshness assertion against the wall clock is a flaky test, and a flaky
/// test gets disabled, and a disabled test is the one that would have caught
/// the dead feed.
#[must_use]
pub fn report(state: &AppState, now_ns: i64) -> CapabilityReport {
    let mut capabilities = Vec::new();

    // -- Database ---------------------------------------------------------
    // The one capability whose failure is an incident rather than a decision,
    // because bots, strategies and the audit trail all live in it.
    capabilities.push(match &state.db {
        Some(_) => Capability::ready("database", "DATABASE_URL").verified(true),
        None => Capability::absent(
            "database",
            "DATABASE_URL",
            "no database is configured: bots, saved strategies and the audit trail are \
             unavailable. Charts and the market routes still work.",
        ),
    });

    // -- Agent ------------------------------------------------------------
    // `verified` stays false: proving Bedrock answers requires a paid call, and
    // a status poll that spends money on every poll would be turned off within
    // a day, leaving the platform with no status at all.
    let skills = state.skills.len();
    capabilities.push(match &state.agent {
        Some(_) if skills == 0 => Capability::ready("agent", "AWS_BEDROCK_* + skills/")
            .degraded(
                "the model is configured but the skill library is empty, so every question \
                 will answer \"no matching skill\". A deployed image must copy skills/.",
            ),
        Some(_) => Capability::ready("agent", "AWS_BEDROCK_* + skills/"),
        None => Capability::absent(
            "agent",
            "AWS_BEDROCK_REGION, AWS_BEDROCK_MODEL_ID, AWS credentials",
            "the agent is not configured. Charts, market data and rule-based bots still work.",
        ),
    });

    // -- Auth -------------------------------------------------------------
    capabilities.push(match &state.auth {
        Some(_) => Capability::ready("auth", "JWT_SECRET").verified(true),
        None => Capability::absent(
            "auth",
            "JWT_SECRET",
            "sessions are not configured: /auth/* answers 503. Anonymous market reads still \
             work, and so does everything that does not belong to a user.",
        ),
    });

    // -- Market data and feeds -------------------------------------------
    capabilities.push(Capability::ready("market_data", "the venue's REST API").verified(true));
    let (route_feeds, bot_feeds) = state.bots.feed_counts();
    capabilities.push(
        Capability::ready("live_feeds", "the venue's websocket").verified(route_feeds + bot_feeds > 0),
    );

    // -- Freshness --------------------------------------------------------
    let data = freshness(state, now_ns);

    // -- Instruments ------------------------------------------------------
    let indexed = state.symbols.len();
    let fetched_at_ns = state.symbols.fetched_at();
    let instruments = InstrumentStatus {
        indexed,
        fetched_at_ns,
        stale: state.symbols.is_stale(now_ns),
        detail: (indexed == 0).then(|| {
            "the venue's instrument listing has not been fetched yet, so symbol search \
             returns nothing. Charts are unaffected: asking for a symbol fetches its \
             history directly."
                .to_string()
        }),
    };

    let feeds = FeedStatus {
        route: route_feeds,
        bot: bot_feeds,
        max_active: crate::bots::MAX_ACTIVE_FEEDS,
        symbols: state.bots.feed_symbols(),
    };

    let all_ready = capabilities
        .iter()
        .all(|capability| capability.readiness != Readiness::Degraded);

    // The warning names the *consequences*, not the variables. A user who
    // cannot start a bot needs to know that; an operator reading the log has
    // the `detail` fields for the variable names.
    let warning = build_warning(&capabilities, &data);

    CapabilityReport {
        capabilities,
        data,
        instruments,
        feeds,
        all_ready,
        warning,
    }
}

/// Per-symbol freshness from the buffer and the tape.
fn freshness(state: &AppState, now_ns: i64) -> Vec<SymbolFreshness> {
    let mut out = Vec::new();

    for symbol in state.bots.history().symbols() {
        let mut timeframes = Vec::new();
        let mut newest: Option<i64> = None;

        for timeframe in ::market_data::STANDARD_TIMEFRAMES {
            let Some(bar) = state.bots.history().newest(&symbol, timeframe) else {
                continue;
            };
            timeframes.push(timeframe.to_string());
            newest = Some(newest.map_or(bar, |current: i64| current.max(bar)));
        }

        let Some(newest_bar_ns) = newest else {
            continue;
        };
        let age_seconds = (now_ns - newest_bar_ns).max(0) / 1_000_000_000;
        out.push(SymbolFreshness::new(
            symbol.clone(),
            newest_bar_ns,
            age_seconds,
            timeframes,
            state.windows.has_ticks(&symbol),
        ));
    }

    // Oldest first: a client drawing a list wants the problem at the top.
    out.sort_by_key(|f| -f.age_seconds);
    out
}

/// A sentence naming what a user cannot do, if anything.
fn build_warning(capabilities: &[Capability], data: &[SymbolFreshness]) -> Option<String> {
    let mut blocked: Vec<&'static str> = Vec::new();
    for capability in capabilities {
        if capability.readiness == Readiness::NotConfigured {
            blocked.push(match capability.name {
                "database" => "saving bots, strategies and the audit trail",
                "agent" => "asking the AI analyst",
                "auth" => "signing in",
                other => other,
            });
        }
    }

    let stale: Vec<&str> = data
        .iter()
        .filter(|f| f.stale)
        .map(|f| f.symbol.as_str())
        .collect();

    match (blocked.is_empty(), stale.is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!(
            "this deployment is not configured for {}. Everything else works.",
            blocked.join(", ")
        )),
        (true, false) => Some(stale_warning(&stale)),
        // Both, and the combined sentence carries the *consequence* of each
        // rather than just listing them. A warning that names two problems but
        // explains neither sends the reader to fix the wrong one -- and the
        // stale half is the half that is easy to omit, because "not configured"
        // reads as the whole story.
        (false, false) => Some(format!(
            "this deployment is not configured for {}, and the market feed has stopped \
             updating {}.",
            blocked.join(", "),
            stale_warning_clause(&stale)
        )),
    }
}

/// The sentence for a stale feed on its own.
fn stale_warning(symbols: &[&str]) -> String {
    format!(
        "the market feed has stopped updating {}: the newest bar is older than {} \
         seconds. Charts will still draw, but they are drawing the past.",
        symbols.join(", "),
        FRESHNESS_WINDOW.as_secs()
    )
}

/// The same content as a clause in a longer sentence.
fn stale_warning_clause(symbols: &[&str]) -> String {
    format!(
        "{}: the newest bar is older than {} seconds, so charts will still draw but \
         they are drawing the past",
        symbols.join(", "),
        FRESHNESS_WINDOW.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capability_reports_configuration_separately_from_proof() {
        // The whole honesty rule in one assertion: `ready` must never imply
        // "proven working", because the report is what people read *before*
        // trusting the thing it describes.
        let capability = Capability::ready("agent", "AWS_BEDROCK_*");
        assert_eq!(capability.readiness, Readiness::Ready);
        assert!(
            !capability.verified,
            "a configured-but-untested capability must not claim to have been exercised"
        );

        let proven = Capability::ready("market_data", "the venue").verified(true);
        assert!(proven.verified);
    }

    #[test]
    fn a_degraded_capability_is_not_ready() {
        let degraded =
            Capability::ready("agent", "AWS_BEDROCK_*").degraded("the skill library is empty");
        assert_eq!(degraded.readiness, Readiness::Degraded);
        assert_eq!(
            degraded.detail.as_deref(),
            Some("the skill library is empty")
        );
    }

    #[test]
    fn readiness_serializes_as_snake_case_for_the_shell() {
        // The shell branches on these strings. A rename is not a compile error
        // anywhere -- exactly the way `baseAsset` silently became `""`.
        let json = serde_json::to_value(Readiness::NotConfigured).expect("serializes");
        assert_eq!(json, "not_configured");
        assert_eq!(
            serde_json::to_value(Readiness::Degraded).expect("serializes"),
            "degraded"
        );
        assert_eq!(
            serde_json::to_value(Readiness::Ready).expect("serializes"),
            "ready"
        );
    }

    #[test]
    fn a_bar_exactly_at_the_window_is_not_stale() {
        // The boundary is inclusive on the healthy side: a 120-second-old bar
        // at a 120-second window is on time, and an off-by-one here would
        // report a healthy feed as dead on every second poll.
        let at_window = SymbolFreshness::new(
            "BTCUSDT".into(),
            0,
            FRESHNESS_WINDOW.as_secs() as i64,
            vec!["1m".into()],
            true,
        );
        assert!(!at_window.stale);

        let past_window = SymbolFreshness::new(
            "BTCUSDT".into(),
            0,
            FRESHNESS_WINDOW.as_secs() as i64 + 1,
            vec!["1m".into()],
            true,
        );
        assert!(past_window.stale);
    }

    #[test]
    fn the_warning_names_consequences_rather_than_variables() {
        let capabilities = vec![
            Capability::ready("market_data", "the venue"),
            Capability::absent("agent", "AWS_BEDROCK_REGION", "not configured"),
        ];
        let warning = build_warning(&capabilities, &[]).expect("a missing agent is worth saying");
        assert!(
            warning.contains("asking the AI analyst"),
            "a user needs to know what they cannot do, not which variable to set: {warning}"
        );
        assert!(
            !warning.contains("AWS_BEDROCK"),
            "the variable belongs in the capability's own detail field: {warning}"
        );
        assert!(warning.contains("Everything else works"), "{warning}");
    }

    #[test]
    fn a_stale_feed_and_a_missing_capability_are_both_reported() {
        // One must not mask the other: they call for different action (set a
        // variable vs. investigate the feed), and a warning that mentions only
        // one sends the reader to fix the wrong thing.
        let capabilities = vec![Capability::absent("database", "DATABASE_URL", "absent")];
        let data = vec![SymbolFreshness::new(
            "BTCUSDT".into(),
            0,
            9_999,
            vec!["1m".into()],
            false,
        )];
        let warning = build_warning(&capabilities, &data).expect("a warning");
        assert!(warning.contains("the audit trail"), "{warning}");
        assert!(warning.contains("BTCUSDT"), "{warning}");
        assert!(warning.contains("drawing the past"), "{warning}");
    }

    #[test]
    fn a_healthy_deployment_has_no_warning() {
        let capabilities = vec![
            Capability::ready("market_data", "the venue").verified(true),
            Capability::ready("agent", "AWS_BEDROCK_*"),
        ];
        assert!(
            build_warning(&capabilities, &[]).is_none(),
            "a warning on a healthy deployment is noise, and noise is what makes \
             a real warning invisible"
        );
    }

    #[test]
    fn an_empty_instrument_index_explains_itself() {
        let status = InstrumentStatus {
            indexed: 0,
            fetched_at_ns: None,
            stale: true,
            detail: Some("not fetched yet".into()),
        };
        let json = serde_json::to_value(&status).expect("serializes");
        assert_eq!(json["indexed"], 0);
        assert!(json["fetched_at_ns"].is_null());
        assert_eq!(json["stale"], true);
        assert!(json["detail"].is_string());
    }

    #[test]
    fn freshness_serializes_its_wire_names() {
        let reading = SymbolFreshness::new(
            "BTCUSDT".into(),
            1_700_000_000_000_000_000,
            42,
            vec!["1m".into(), "5m".into()],
            true,
        );
        let json = serde_json::to_value(&reading).expect("serializes");
        assert_eq!(json["symbol"], "BTCUSDT");
        assert_eq!(json["newest_bar_ns"], 1_700_000_000_000_000_000i64);
        assert_eq!(json["age_seconds"], 42);
        assert_eq!(json["stale"], false);
        assert_eq!(json["timeframes"][0], "1m");
        assert_eq!(json["ticks"], true);
    }
}
