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

use analytics_core::concepts::{self, detect};
use analytics_core::regions::Region;
use analytics_core::types::{self, Timeframe};
use backtester::replay::{replay, ReplayConfig, ReplayInput, ReplaySignal};
use chart_engine::{self, IndicatorMarker, IndicatorOutput, IndicatorZone, MarkerKind, ReplayExit, ReplaySetup, SetupDirection};
use strategy_dsl::Direction;
use strategy_runtime::signal::Signal;
use crate::AppState;
use uuid::Uuid;

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

/// Build an indicator preview from a detector document's concepts over a loaded
/// series.
///
/// A `kind: indicator` document has no entry/risk logic, so it cannot produce
/// replay setups. What it *can* produce is detection evidence: one zone per
/// matched pattern window, plus an evidence node and a marker at the candle where
/// the pattern fired. This is the detector layer the chart renders, not a trading
/// plan.
pub fn build_indicator_preview(
    revision_id: impl Into<String>,
    concepts: &[concepts::Concept],
    series: &[types::Candle],
    source_timeframe: Option<Timeframe>,
) -> IndicatorOutput {
    let mut output = IndicatorOutput {
        revision_id: revision_id.into(),
        ..IndicatorOutput::default()
    };

    let mut next_evidence = 0usize;
    for concept in concepts {
        let bands = detect(series, concept);
        for region in bands {
            let evidence_id = format!("concept-{next_evidence}");
            next_evidence += 1;

            let label = concept
                .label
                .as_deref()
                .map_or_else(|| concept.name.as_str(), |l| l);

            output.evidence.push(chart_engine::Evidence {
                id: evidence_id.clone(),
                event: concept.name.clone(),
                time: region.from,
                price: region.price_low,
                explanation: format!(
                    "detected {label} on the {timeframe} chart (band {price_low}..{price_high})",
                    label = label,
                    timeframe = source_timeframe
                        .as_ref()
                        .map(|tf| tf.as_str())
                        .unwrap_or_else(|| series.first().map(|c| c.timeframe.as_str()).unwrap_or("?")),
                    price_low = region.price_low,
                    price_high = region.price_high,
                ),
            });

            output.zones.push(IndicatorZone {
                id: evidence_id.clone(),
                start_time: region.from,
                end_time: region.to,
                price_low: region.price_low,
                price_high: region.price_high,
                label: label.to_string(),
                state: zone_state(&region),
            });

            output.markers.push(IndicatorMarker {
                id: format!("{evidence_id}-marker"),
                evidence_id: evidence_id.clone(),
                time: region.from,
                price: region.price_low,
                label: label.to_string(),
                kind: marker_kind(region.side),
            });
        }
    }

    output
}

/// Map a detected region to a chart lifecycle state.
///
/// The chart already draws zones with these states; reuse the same vocabulary so
/// an indicator band and a built-in zone behave the same way visually.
fn zone_state(region: &Region) -> chart_engine::ZoneState {
    let mitigated = region.mitigated;
    if mitigated <= 0.0 {
        chart_engine::ZoneState::Active
    } else if mitigated > 1.0 {
        chart_engine::ZoneState::Tapped
    } else if (mitigated - 1.0).abs() < 1e-9 {
        chart_engine::ZoneState::Mitigated
    } else {
        chart_engine::ZoneState::Active
    }
}

/// Map a detection side to the marker vocabulary the chart already paints.
fn marker_kind(side: analytics_core::types::Side) -> MarkerKind {
    match side {
        analytics_core::types::Side::Buy => MarkerKind::Bullish,
        analytics_core::types::Side::Sell => MarkerKind::Bearish,
    }
}

/// Replay a generated document over a recent window and describe the result.
///
/// # Errors
/// Returns a human-readable reason when the candles cannot be loaded or the
/// replay refuses -- never for a reason the caller should treat as fatal.
pub async fn replay_preview(
    state: &AppState,
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

    // A fresh deployment has almost nothing stored -- the logs show 10 candles
    // spanning 0.5% of the window -- so a replay over stored data alone fires
    // on nothing and the preview is empty. Backfill the window from the venue
    // first; the upsert makes repeat backfills idempotent.
    {
        let backfilled = state
            .backfill
            .backfill_candles(
                symbol,
                source_timeframe,
                from_ns,
                to_ns,
                market_data::BackfillSource::Klines,
            )
            .await;
        match backfilled {
            Ok(candles) if !candles.is_empty() => {
                if let Err(error) = db::repositories::insert_candles(database.pool(), &candles).await {
                    tracing::warn!(%error, "could not store preview backfill; replaying on what is stored");
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "preview backfill from the venue failed; replaying on what is stored");
            }
        }
    }

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

    // Indicator documents have no entry/risk blocks by design, so the
    // sandbox and the trading engine rightfully refuse them.  Do not replay
    // them as trading setups.  Instead, evaluate the stored concepts as a
    // detector layer over the same backfilled series and return whatever bands
    // the window actually contains -- which may legitimately be none.
    if document.kind == strategy_dsl::DocumentKind::Indicator {
        let concepts: Vec<_> = document.concepts.iter().cloned().collect();
        let mut preview = build_indicator_preview(
            document.name.clone(),
            &concepts,
            &series.get("entry").cloned().unwrap_or_default(),
            Some(source_timeframe),
        );
        // A week of 5m candles with a loose concept matches thousands of
        // windows, but the chart's contract caps the layer at
        // MAX_PRIMITIVES and refuses the whole output past it -- a full
        // week of detections rendered as nothing at all. Keep the most
        // recent matches, the ones a chart is showing, the same rule
        // `IndicatorOutput::from_replay` applies to setups.
        preview.cull_to_budget();
        return Ok(preview);
    }

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

    // --- indicator detector preview -----------------------------------------

    fn indicator_candle(index: i64, open: f64, high: f64, low: f64, close: f64) -> analytics_core::types::Candle {
        analytics_core::types::Candle {
            symbol: "BTCUSDT".into(),
            timeframe: analytics_core::types::Timeframe::M5,
            open_time: index * analytics_core::types::Timeframe::M5.nanos(),
            open,
            high,
            low,
            close,
            volume: 10.0,
            buy_volume: 6.0,
            sell_volume: 4.0,
        }
    }

    fn bullish_gap_concept() -> concepts::Concept {
        concepts::Concept {
            name: "bullish_gap".into(),
            label: Some("bullish gap".into()),
            side: types::Side::Buy,
            window: 3,
            lower: concepts::Selector::High(0),
            upper: concepts::Selector::Low(2),
            require: vec![concepts::Requirement {
                left: concepts::Selector::High(0),
                op: concepts::Compare::Below,
                right: concepts::Selector::Low(2),
            }],
            min_band_ratio: None,
        }
    }

    fn gap_series() -> Vec<analytics_core::types::Candle> {
        vec![
            indicator_candle(0, 100.0, 100.5, 99.5, 100.0),
            indicator_candle(1, 100.0, 100.5, 99.5, 100.2),
            indicator_candle(2, 100.2, 101.0, 100.0, 100.5),
            indicator_candle(3, 100.5, 105.5, 100.5, 104.0),
            indicator_candle(4, 105.0, 105.6, 105.0, 105.2),
            indicator_candle(5, 105.2, 105.6, 105.0, 105.4),
        ]
    }

    #[test]
    fn build_indicator_preview_emits_zones_and_markers_for_matched_concepts() {
        let concepts = vec![bullish_gap_concept()];
        let series = gap_series();
        let output = build_indicator_preview(
            Uuid::new_v4(),
            &concepts,
            &series,
            Some(analytics_core::types::Timeframe::M5),
        );

        assert!(!output.revision_id.is_empty());
        assert_eq!(output.zones.len(), 1, "{output:#?}");
        assert_eq!(output.markers.len(), 1, "{output:#?}");
        assert_eq!(output.evidence.len(), 1, "{output:#?}");

        let zone = &output.zones[0];
        assert_eq!(zone.label, "bullish gap");
        assert_eq!(zone.price_low, 101.0, "{zone:#?}");
        assert_eq!(zone.price_high, 105.0, "{zone:#?}");
        assert_eq!(
            zone.start_time,
            indicator_candle(2, 0.0, 0.0, 0.0, 0.0).open_time,
            "the band opens at the first candle of the pattern",
        );

        let marker = &output.markers[0];
        assert_eq!(marker.evidence_id, output.evidence[0].id);
        assert_eq!(marker.label, "bullish gap");
        assert_eq!(marker.kind, MarkerKind::Bullish);

        assert_eq!(output.evidence[0].event, "bullish_gap");
        assert!(output.evidence[0].explanation.contains("bullish gap"));
    }

    #[test]
    fn build_indicator_preview_is_empty_when_no_concept_matches() {
        let concepts = vec![bullish_gap_concept()];
        let short = vec![indicator_candle(0, 100.0, 101.0, 99.0, 100.5)];
        let output = build_indicator_preview(
            Uuid::new_v4(),
            &concepts,
            &short,
            Some(analytics_core::types::Timeframe::M5),
        );

        assert_eq!(output.zones.len(), 0);
        assert_eq!(output.markers.len(), 0);
        assert_eq!(output.evidence.len(), 0);
        assert!(!output.revision_id.is_empty());
    }

    #[test]
    fn build_indicator_preview_emits_one_zone_per_matched_window() {
        let mut candles = gap_series();
        candles.push(indicator_candle(6, 105.4, 105.6, 105.1, 105.3));
        candles.push(indicator_candle(7, 105.3, 105.7, 105.2, 105.5));

        let concepts = vec![bullish_gap_concept()];
        let output = build_indicator_preview(
            Uuid::new_v4(),
            &concepts,
            &candles,
            Some(analytics_core::types::Timeframe::M5),
        );

        // The gap rule can fire on overlapping windows; each match is its own
        // zone/marker/evidence in the indicator output.
        assert!(output.zones.len() >= 1, "{output:#?}");
        assert_eq!(output.zones.len(), output.markers.len());
        assert_eq!(output.zones.len(), output.evidence.len());
    }

    #[test]
    fn build_indicator_preview_keeps_indicator_contracted_only() {
        let concepts = vec![bullish_gap_concept()];
        let series = gap_series();
        let output = build_indicator_preview(
            Uuid::new_v4(),
            &concepts,
            &series,
            Some(analytics_core::types::Timeframe::M5),
        );

        // An indicator preview must not emit Enter/Exit-style setup primitives.
        assert!(output.links.is_empty());
    }
}
