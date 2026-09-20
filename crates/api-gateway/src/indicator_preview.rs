//! Turn a replayed strategy into a chart evidence-chain preview.
//!
//! This is the missing half of the indicator-workspace generation path. A
//! workspace message already produced a validated Strategy DSL document and
//! proved it runs in the sandbox, but it stored an **empty** preview -- the
//! revision was real while the chart it claimed to describe had nothing on it.
//!
//! The fix is to *replay* the document, the same way a backtest and a bot do,
//! and translate the signals it actually emitted into [`IndicatorOutput`]: the
//! fired conditions become evidence nodes, the entry becomes a marker, the risk
//! band becomes a zone, and the causality is expressed as links. Nothing here
//! computes trading math or position math -- the interpreter decides, the chart
//! describes.
//!
//! ## The replay is sandboxed, like every other execution of generated logic
//!
//! Principle #6 does not bend for a preview. The document is AI-authored, so it
//! runs inside [`trading_engine::Decisions::sandboxed`], which is the exact
//! driver a paper bot uses. The preview therefore shows what the *bot* would
//! decide, not what a privileged native run would.
//!
//! ## A preview must never fail generation
//!
//! Loading candles can fail (a symbol with no stored rows) and a replay can
//! refuse (a declared timeframe with no data). Neither is a reason to throw away
//! a validated revision, so [`replay_preview`] returns the failure as a string
//! and the caller stores an honest empty preview plus the reason, rather than a
//! chart that silently pretends it has evidence.

use analytics_core::types::Timeframe;
use backtester::replay::{replay, ReplayConfig, ReplayInput, ReplaySignal};
use chart_engine::{IndicatorOutput, ReplayExit, ReplaySetup, SetupDirection};
use strategy_dsl::Direction;
use strategy_runtime::signal::Signal;

/// How far back a preview replays.
///
/// A week is the compromise between a chart that shows recent structure and a
/// sandbox crossing per decision candle: seven days of one-minute bars is ten
/// thousand evaluations, which is a preview rather than a report. It is a
/// constant, not a request field, so the preview a user sees does not depend on
/// what they happened to type.
const PREVIEW_WINDOW_NS: i64 = 7 * 24 * 60 * 60 * 1_000_000_000;

/// Pair a replay's flat signal list into setups with their exits.
///
/// The interpreter emits at most one signal per candle and only asks for an exit
/// while it is in a position, so an `Exit` closes the most recent still-open
/// `Enter`. A stop or target is a *simulator* event, not a condition signal, so
/// it does not appear here -- a setup whose protective level was hit keeps an
/// open risk band rather than claiming an exit the strategy never decided.
#[must_use]
pub fn setups_from_signals(signals: &[ReplaySignal]) -> Vec<ReplaySetup> {
    let mut setups: Vec<ReplaySetup> = Vec::new();
    let mut open: Option<usize> = None;

    for record in signals {
        match &record.signal {
            Signal::Enter(enter) => {
                let direction = match enter.direction {
                    Direction::Long => SetupDirection::Long,
                    Direction::Short => SetupDirection::Short,
                };
                setups.push(ReplaySetup {
                    id: format!("setup-{}", setups.len() + 1),
                    direction,
                    decision_time: record.time,
                    entry_price: enter.reference_price,
                    stop_price: enter.stop_price,
                    target_price: enter.take_profit_price,
                    reasons: enter.reasons.clone(),
                    exit: None,
                });
                open = Some(setups.len() - 1);
            }
            Signal::Exit(exit) => {
                if let Some(index) = open.take() {
                    if let Some(setup) = setups.get_mut(index) {
                        setup.exit = Some(ReplayExit {
                            time: record.time,
                            price: record.price,
                            trigger: exit.trigger.name().to_string(),
                        });
                    }
                }
            }
        }
    }

    setups
}

/// Build a validated preview from a replay's signals.
#[must_use]
pub fn build_preview(revision_id: &str, signals: &[ReplaySignal]) -> IndicatorOutput {
    IndicatorOutput::from_replay(revision_id, &setups_from_signals(signals))
}

/// Replay a generated document over a recent window and describe the result.
///
/// # Errors
/// Returns a human-readable reason when the candles cannot be loaded or the
/// replay refuses -- never for a reason the caller should treat as fatal.
pub async fn replay_preview(
    state: &crate::AppState,
    database: &db::Database,
    symbol: &str,
    source_timeframe: &str,
    document: &strategy_dsl::StrategyDocument,
    validated: &strategy_dsl::ValidatedStrategy,
) -> Result<IndicatorOutput, String> {
    let to_ns = crate::now_ns();
    let from_ns = to_ns - PREVIEW_WINDOW_NS;
    // The workspace's own chart timeframe is the natural source to aggregate
    // declared timeframes from; one minute is the safe fallback, because it can
    // build every coarser series by resampling.
    let source_timeframe = source_timeframe
        .parse::<Timeframe>()
        .unwrap_or(Timeframe::M1);

    let series = db::loading::load_timeframe_series(
        database.pool(),
        symbol,
        &document.timeframes,
        from_ns,
        to_ns,
        source_timeframe,
    )
    .await
    .map_err(|error| format!("could not load candles for the preview: {error}"))?;

    let input = ReplayInput::new(document, series)
        .map_err(|error| format!("the preview replay input was incomplete: {error}"))?;

    let mut decisions = trading_engine::Decisions::sandboxed(state.sandbox.as_ref(), validated)
        .map_err(|error| format!("the sandbox refused the preview replay: {error}"))?;

    let output = replay(
        &mut decisions,
        &input,
        &ReplayConfig {
            symbol: symbol.to_string(),
            from: from_ns,
            to: to_ns,
            ..ReplayConfig::default()
        },
    )
    .map_err(|error| format!("the preview replay did not complete: {error}"))?;

    Ok(build_preview("", &output.signals))
}

#[cfg(test)]
mod tests {
    use super::*;
    use strategy_runtime::signal::{EnterSignal, ExitSignal, ExitTrigger};

    fn signal(time: i64, price: f64, signal: Signal) -> ReplaySignal {
        ReplaySignal {
            time,
            price,
            signal,
        }
    }

    fn enter(direction: Direction, stop: f64) -> Signal {
        Signal::Enter(EnterSignal {
            direction,
            reference_price: 100.0,
            stop_price: stop,
            take_profit_price: Some(110.0),
            max_risk_pct: 1.0,
            reasons: vec!["delta > 0".into()],
        })
    }

    #[test]
    fn an_exit_closes_the_open_setup() {
        let signals = vec![
            signal(10, 100.0, enter(Direction::Long, 95.0)),
            signal(
                20,
                110.0,
                Signal::Exit(ExitSignal::from_conditions(
                    ExitTrigger::Invalidation,
                    vec!["close_below(stop_price)".into()],
                )),
            ),
        ];
        let setups = setups_from_signals(&signals);
        assert_eq!(setups.len(), 1);
        assert_eq!(setups[0].direction, SetupDirection::Long);
        let exit = setups[0].exit.as_ref().expect("the exit was paired");
        assert_eq!(exit.time, 20);
        assert_eq!(exit.price, 110.0);
        assert_eq!(exit.trigger, "invalidation");
    }

    #[test]
    fn a_second_entry_starts_a_new_setup() {
        let signals = vec![
            signal(10, 100.0, enter(Direction::Long, 95.0)),
            signal(20, 101.0, enter(Direction::Short, 105.0)),
        ];
        let setups = setups_from_signals(&signals);
        assert_eq!(setups.len(), 2);
        assert!(setups[0].exit.is_none());
        assert_eq!(setups[1].direction, SetupDirection::Short);
        assert_eq!(setups[1].stop_price, 105.0);
    }
}
