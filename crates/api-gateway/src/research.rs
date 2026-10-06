//! The host's research tools for the agent (`docs/38`, Phase A).
//!
//! ## Why this adapter lives in the gateway
//!
//! The dependency rules say it structurally: `ai-agent` may not depend on
//! `backtester` (the agent reasons about strategies; it does not own the
//! engine), and `backtester` may not depend on `market-data` (a backtest is
//! a pure replay over candles it is *given*, so it can be tested without a
//! venue). The gateway is the crate that already holds both edges, so the
//! [`ai_agent::tools::BacktestRunner`] implementation is written here and
//! handed to the agent per request.
//!
//! ## Where the candles come from
//!
//! [`WindowService::candles_capped`], not the database. The window service
//! serves RAM first and backfills the shortfall from the venue, which means
//! the agent's backtest and the chart the user is looking at read the same
//! tape. The cap is [`::market_data::MAX_VENUE_BARS`] bars per timeframe,
//! clamped from the front — a fact the summary's `note` states, because a
//! base rate silently computed over fewer bars than asked for is exactly the
//! kind of quiet lie this codebase exists to remove.
//!
//! ## What `similar_setups` runs
//!
//! A skill id resolves to a *reference strategy document*: the runner scans
//! `strategies/` for the document whose `metadata.skill_ref` names the skill
//! (strategy documents declare the skill they implement — see
//! `strategies/liquidity-sweep.yaml`). A skill with no reference document is
//! an honest error, not an invented backtest.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ai_agent::tools::{BacktestRunner, BacktestSummary};
use ai_agent::AgentError;
use async_trait::async_trait;

/// Runs backtests for the agent over the window service's candles.
#[derive(Clone)]
pub struct WindowBacktestRunner {
    windows: ::market_data::WindowService,
    strategies: PathBuf,
}

impl std::fmt::Debug for WindowBacktestRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Same rule as the rest of the agent's host callbacks: the windows
        // handle is internal plumbing; the strategies directory is the fact
        // worth logging.
        f.debug_struct("WindowBacktestRunner")
            .field("strategies", &self.strategies)
            .finish()
    }
}

impl WindowBacktestRunner {
    /// A runner over this window service, resolving skills against
    /// `strategies` (the shipped strategy directory).
    #[must_use]
    pub fn new(windows: ::market_data::WindowService, strategies: impl Into<PathBuf>) -> Self {
        Self {
            windows,
            strategies: strategies.into(),
        }
    }

    /// Parse, fetch, replay, and summarize one document.
    async fn run_document(
        &self,
        tool: &'static str,
        source: &str,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
        provenance: Option<&str>,
    ) -> Result<BacktestSummary, AgentError> {
        let failed = |reason: String| AgentError::ToolFailed {
            tool: tool.to_string(),
            reason,
        };

        let validated = strategy_dsl::parse_and_validate(source)
            .map_err(|e| failed(format!("the strategy document does not validate: {e}")))?;
        let document = validated.document().clone();

        // One series per declared timeframe. The window service never invents
        // bars, so a symbol with no coverage produces an empty series and the
        // replay below reports zero trades — which the summary's note then has
        // to explain, because "0 trades" and "0 trades because there was no
        // data" are different findings.
        let mut series: BTreeMap<String, Vec<analytics_core::Candle>> = BTreeMap::new();
        let mut clamped = false;
        let mut empty_windows = Vec::new();
        for (name, timeframe) in &document.timeframes {
            let window = self
                .windows
                .candles_capped(
                    symbol,
                    *timeframe,
                    from_ns,
                    to_ns,
                    ::market_data::MAX_VENUE_BARS,
                )
                .await
                .map_err(|e| failed(format!("could not load {timeframe} candles: {e}")))?;
            if window.candles.is_empty() {
                empty_windows.push(format!("{name} ({timeframe})"));
            }
            // The cap clamps from the front: a full-cap answer means the start
            // of the requested window was dropped.
            if window.candles.len() >= ::market_data::MAX_VENUE_BARS {
                clamped = true;
            }
            series.insert(name.clone(), window.candles);
        }

        let input = backtester::ReplayInput::new(&document, series)
            .map_err(|e| failed(format!("the replay could not be assembled: {e}")))?;
        let config = backtester::ReplayConfig {
            symbol: symbol.to_string(),
            from: from_ns,
            to: to_ns,
            ..backtester::ReplayConfig::default()
        };
        let mut engine = strategy_runtime::StrategyEngine::new(
            &validated,
            strategy_runtime::RuntimeConfig::default(),
        )
        .map_err(|e| failed(format!("the strategy cannot run: {e}")))?;
        let report = backtester::run_backtest(&mut engine, &input, &config).map_err(|e| {
            // A no-data refusal gets the per-timeframe detail appended: "no
            // data for BTCUSDT" leaves the model guessing which of three
            // series was empty, and the fix (widen the window, check coverage)
            // depends on the answer.
            if matches!(e, backtester::BacktestError::MissingData { .. })
                && !empty_windows.is_empty()
            {
                failed(format!(
                    "{e} — no candles on {}. That is a data gap (the buffer holds nothing and the \
                     venue could not backfill it), not a verdict on the setup; widen the window or \
                     check the symbol's coverage before concluding anything.",
                    empty_windows.join(", ")
                ))
            } else {
                failed(format!("the backtest failed: {e}"))
            }
        })?;

        let mut notes: Vec<String> = Vec::new();
        // `net_return_pct`/`max_drawdown_pct` hold R multiples despite their
        // names (backtester::report's header says why); the summary's fields
        // say R because the agent quotes these numbers in prose.
        notes.push(report.assumptions.return_units.clone());
        if let Some(file) = provenance {
            notes.push(format!("reference document: strategies/{file}"));
        }
        if clamped {
            notes.push(format!(
                "the venue serves at most {} bars per fetch, so the start of the window was clamped; \
                 the base rate covers less history than requested",
                ::market_data::MAX_VENUE_BARS
            ));
        }
        if !empty_windows.is_empty() {
            notes.push(format!(
                "no candles at all on {}: any zero-trade count is a data gap, not a verdict on the setup",
                empty_windows.join(", ")
            ));
        }

        Ok(BacktestSummary {
            strategy: report.strategy,
            decision_timeframe: report.decision_timeframe,
            total_trades: report.total_trades,
            win_rate: report.win_rate,
            profit_factor: report.profit_factor,
            net_return_r: report.net_return_pct,
            max_drawdown_r: report.max_drawdown_pct,
            average_r: report.average_r,
            skipped_signals: report.skipped_signals_count,
            note: Some(notes.join(" ")),
        })
    }

    /// Find the shipped strategy document that declares this skill.
    ///
    /// The mapping lives in the documents (`metadata.skill_ref`), not in a
    /// table here: a table would be one more place a skill and its reference
    /// strategy can drift apart.
    fn reference_strategy(&self, skill_ref: &str) -> Result<(String, String), AgentError> {
        let failed = |reason: String| AgentError::ToolFailed {
            tool: "backtest_similar_setups".to_string(),
            reason,
        };
        let entries = std::fs::read_dir(&self.strategies).map_err(|e| {
            failed(format!(
                "the strategy directory {} is not deployed: {e}",
                self.strategies.display()
            ))
        })?;

        let mut matches: Vec<(String, String)> = Vec::new();
        let mut available: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().to_string();
            // Same positive rule as the examples route: a bare .yaml name,
            // nothing else. The directory is a deployment artifact, but the
            // rule keeps a surprise file (an editor backup, a nested dir)
            // from being read as a strategy.
            if !is_plain_yaml(&file) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(document) = strategy_dsl::parser::parse(&source) else {
                continue;
            };
            let Some(declared) = document.metadata.skill_ref.clone() else {
                continue;
            };
            if declared == skill_ref {
                matches.push((file, source));
            } else {
                available.push(declared);
            }
        }

        match matches.len() {
            0 => {
                available.sort();
                Err(failed(format!(
                    "no reference strategy document declares skill_ref `{skill_ref}`, so there is \
                     no base rate to report for it. Skills with a shipped reference document: {}. \
                     A strategy can still be backtested directly with backtest_strategy.",
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                )))
            }
            _ => {
                matches.sort_by(|a, b| a.0.cmp(&b.0));
                let (file, source) = matches.swap_remove(0);
                if !matches.is_empty() {
                    tracing::warn!(
                        skill_ref,
                        chosen = %file,
                        ignored = ?matches.iter().map(|(f, _)| f).collect::<Vec<_>>(),
                        "more than one reference document declares this skill; the first by file name wins"
                    );
                }
                Ok((file, source))
            }
        }
    }
}

/// Whether a name is a bare `.yaml` file — the same positive rule the
/// examples route applies (`strategy_routes::safe_example_name`), restated
/// because the runner reads the directory on its own.
fn is_plain_yaml(name: &str) -> bool {
    !name.contains(['/', '\\'])
        && !name.starts_with('.')
        && name.len() <= 128
        && name.ends_with(".yaml")
}

#[async_trait]
impl BacktestRunner for WindowBacktestRunner {
    async fn run(
        &self,
        document: &str,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<BacktestSummary, AgentError> {
        self.run_document("backtest_strategy", document, symbol, from_ns, to_ns, None)
            .await
    }

    async fn similar_setups(
        &self,
        skill_ref: &str,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<BacktestSummary, AgentError> {
        let (file, source) = self.reference_strategy(skill_ref)?;
        self.run_document(
            "backtest_similar_setups",
            &source,
            symbol,
            from_ns,
            to_ns,
            Some(&file),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A strategy directory with one document claiming a skill.
    fn dir_with_strategy(skill_ref: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");
        let source = format!(
            r#"name: "Test Sweep"
version: "1.0"
kind: strategy
market: "BTCUSDT"
timeframes:
  entry: "5m"
entry:
  all_of:
    - timeframe: entry
      condition: delta > threshold(1)
risk:
  max_risk_pct: 1.0
  stop: "below_sweep_low"
invalidation:
  - timeframe: entry
    condition: "close_below(stop_price)"
metadata:
  skill_ref: "{skill_ref}"
"#
        );
        std::fs::write(dir.path().join("test-sweep.yaml"), source).expect("written");
        dir
    }

    fn runner_at(dir: &Path) -> WindowBacktestRunner {
        // The window service is never reached by the resolution tests; the
        // supervisor with feeds off serves empty windows without a venue.
        let supervisor = crate::bots::BotSupervisor::new(crate::bots::FeedMode::Off);
        let windows = ::market_data::WindowService::new(
            supervisor.history(),
            supervisor.live(),
            ::market_data::BackfillClient::new("http://127.0.0.1:1"),
        );
        WindowBacktestRunner::new(windows, dir.to_path_buf())
    }

    #[test]
    fn a_skill_with_a_reference_document_resolves_to_it() {
        let dir = dir_with_strategy("test-sweep-v1");
        let runner = runner_at(dir.path());
        let (file, source) = runner
            .reference_strategy("test-sweep-v1")
            .expect("the document declares the skill");
        assert_eq!(file, "test-sweep.yaml");
        assert!(source.contains("Test Sweep"));
    }

    #[test]
    fn a_skill_without_one_is_an_honest_error_naming_what_exists() {
        let dir = dir_with_strategy("test-sweep-v1");
        let runner = runner_at(dir.path());
        let err = runner
            .reference_strategy("footprint-absorption-v1")
            .expect_err("no document declares this skill");
        match err {
            AgentError::ToolFailed { reason, .. } => {
                assert!(reason.contains("footprint-absorption-v1"), "{reason}");
                assert!(reason.contains("test-sweep-v1"), "{reason}");
                assert!(reason.contains("backtest_strategy"), "{reason}");
            }
            other => panic!("expected ToolFailed, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_directory_is_an_honest_error() {
        let runner = runner_at(Path::new("definitely/not/a/real/directory"));
        let err = runner
            .reference_strategy("anything")
            .expect_err("the directory does not exist");
        match err {
            AgentError::ToolFailed { reason, .. } => {
                assert!(reason.contains("not deployed"), "{reason}");
            }
            other => panic!("expected ToolFailed, got {other:?}"),
        }
    }

    #[test]
    fn non_strategy_files_are_ignored() {
        let dir = dir_with_strategy("test-sweep-v1");
        std::fs::write(dir.path().join("notes.txt"), "skill_ref: test-sweep-v1").unwrap();
        std::fs::write(dir.path().join(".hidden.yaml"), "metadata: {}").unwrap();
        let runner = runner_at(dir.path());
        let (file, _) = runner
            .reference_strategy("test-sweep-v1")
            .expect("exactly one match");
        assert_eq!(file, "test-sweep.yaml");
    }

    #[tokio::test]
    async fn a_backtest_with_no_data_names_the_gap_in_the_error() {
        let dir = dir_with_strategy("test-sweep-v1");
        let runner = runner_at(dir.path());
        let (file, _) = runner.reference_strategy("test-sweep-v1").unwrap();

        // The supervisor holds no candles and the venue refuses instantly, so
        // every window is empty. The backtester refuses an all-empty input
        // (BacktestError::MissingData) — the runner's job is to make that
        // refusal actionable for the model: which timeframes were empty, and
        // that the gap is data, not a verdict.
        let err = runner
            .run_document(
                "backtest_strategy",
                &std::fs::read_to_string(dir.path().join(&file)).unwrap(),
                "BTCUSDT",
                0,
                86_400 * 1_000_000_000,
                Some(&file),
            )
            .await
            .expect_err("an all-empty input is refused by the backtester");
        match err {
            AgentError::ToolFailed { reason, .. } => {
                assert!(reason.contains("no data for BTCUSDT"), "{reason}");
                assert!(reason.contains("no candles on entry (5m)"), "{reason}");
                assert!(reason.contains("data gap"), "{reason}");
            }
            other => panic!("expected ToolFailed, got {other:?}"),
        }
    }
}
