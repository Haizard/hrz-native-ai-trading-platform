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

use crate::AppState;
use analytics_core::concepts::{self, detect};
use analytics_core::regions::Region;
use analytics_core::types::{self, Timeframe};
use backtester::replay::{replay, ReplayConfig, ReplayInput, ReplaySignal};
use serde::Serialize;
use chart_engine::{
    self, IndicatorMarker, IndicatorOutput, IndicatorZone, MarkerKind, ReplayExit, ReplaySetup,
    SetupDirection,
};
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
    document_name: &str,
    concepts: &[concepts::Concept],
    series: &[types::Candle],
    source_timeframe: Option<Timeframe>,
) -> IndicatorOutput {
    let mut output = IndicatorOutput {
        revision_id: revision_id.into(),
        // The document's own name, and its concepts: stored with the revision
        // so a chart can re-detect the layer **live** on any symbol and
        // timeframe -- the coordinates below are the snapshot the generator
        // saw, the concepts are the definition that keeps running.
        name: Some(document_name.to_owned()),
        concepts: concepts.to_vec(),
        ..IndicatorOutput::default()
    };

    // Trendline-shaped concepts contribute fitted lines instead of -- not
    // alongside -- bands: the user asked for a line, so a box would be the
    // wrong answer even when the band fires. The fit is over the same series
    // the bands were detected on, so a stored preview and a later live layer
    // agree on the geometry.
    let mut next_line = 0usize;
    for concept in concepts {
        if concept.shape != concepts::ConceptShape::Trendline {
            continue;
        }
        let points: Vec<chart_engine::TrendPoint> =
            concepts::trendline_segments(series, chart_engine::indicator::TRENDLINE_STRENGTH)
                .into_iter()
                .map(|point| chart_engine::TrendPoint {
                    time: point.time,
                    price: point.price,
                })
                .collect();
        if points.len() < 2 {
            continue;
        }
        output.trendlines.push(chart_engine::IndicatorTrendline {
            id: format!("tl-{next_line}"),
            label: concept
                .label
                .clone()
                .unwrap_or_else(|| concept.name.clone()),
            points,
        });
        next_line += 1;
    }

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
                        .unwrap_or_else(|| series
                            .first()
                            .map(|c| c.timeframe.as_str())
                            .unwrap_or("?")),
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

/// Run a generated pine-lite script over the same window the document replay
/// uses, and fold its plots into the preview vocabulary the shell already
/// renders (`docs/23`).
///
/// The full plot series live on in the shell's own script runner (the chart
/// re-runs the source every frame); the preview's numbers exist for the chat
/// transcript -- "your script drew 2 plots over 672 bars" -- and for the
/// revision card, the same honesty the document replay buys.
///
/// # Errors
/// A human-readable reason when candles cannot be loaded or the script fails
/// at run time (a dynamic budget, a read-before-write) -- never a panic.
/// Fetch a second instrument's candles for the security path: read the
/// store first, backfill from the venue when thin. Failure is not fatal --
/// an empty vec means the script's `request.*` reads say the data is missing
/// (which is the truth), not that the platform broke.
async fn fetch_security_candles(
    state: &AppState,
    database: &db::Database,
    sec: &str,
    tf: Timeframe,
    from_ns: i64,
    to_ns: i64,
) -> Vec<types::Candle> {
    let timeframes = std::collections::BTreeMap::from([("sec".to_string(), tf)]);
    if let Ok(series) = db::loading::load_timeframe_series(
        database.pool(),
        sec,
        &timeframes,
        from_ns,
        to_ns,
        tf,
    )
    .await
    {
        let stored = series.get("sec").cloned().unwrap_or_default();
        let expected = ((to_ns - from_ns).max(1) / tf.nanos().max(1)) as usize;
        if stored.len() + 8 >= expected {
            return stored;
        }
    }
    // The store is thin: backfill from the venue, then read again.
    if let Ok(fresh) = state
        .backfill
        .backfill_candles(sec, tf, from_ns, to_ns, market_data::BackfillSource::Klines)
        .await
    {
        if !fresh.is_empty() {
            if let Err(error) = db::repositories::insert_candles(database.pool(), &fresh).await {
                tracing::warn!(%error, "could not store security backfill");
            }
            return fresh;
        }
    }
    Vec::new()
}

/// Time-align the second instrument's candles onto the primary series' bars:
/// candle i of the output covers exactly the primary candle i's window. A
/// secondary bar is used while its open_time matches; between secondary bars
/// (the pair traded less often) the last secondary close carries forward with
/// flat OHLC, and before the pair's first bar everything is flat at that
/// first close -- indexes never drift, which is what cross-market math needs.
/// Align a POOLED series onto the chart's bars (docs/23 Phase 11):
/// `request.security("SYM", tf, ...)` reads the pooled candle whose window
/// CONTAINS the chart bar, completed only -- while that pooled candle is
/// still forming the script sees the PREVIOUS one, which is what keeps a
/// coarser-timeframe read free of lookahead. Window math: the host stores
/// open times, so a pooled candle at `t` covers `[t, t + tf)`. A chart bar
/// before the pooled series' first candle is flat at that first close, the
/// same carry-forward the `sec=` aligner uses.
/// One venue funding-rate point (USDT-M futures endpoint, market DATA
/// even on a spot chart).
#[derive(Debug, serde::Deserialize)]
struct FundingPoint {
    #[serde(rename = "fundingRate")]
    funding_rate: String,
    #[serde(rename = "fundingTime")]
    funding_time: i64,
}

/// One open-interest snapshot.
#[derive(Debug, serde::Deserialize)]
struct OiPoint {
    #[serde(rename = "sumOpenInterest")]
    sum_open_interest: String,
    timestamp: i64,
}

/// Fetch the funding-rate series for request.data("SYM.funding"): venue
/// futures REST, ascending (time_ms, rate). Empty on any failure -- the
/// data-missing error downstream is the truth, not a zero.
async fn fetch_funding_series(sym: &str, from_ns: i64, to_ns: i64) -> Vec<(i64, f64)> {
    let url = format!(
        "https://fapi.binance.com/fapi/v1/fundingRate?symbol={sym}&startTime={}&endTime={}&limit=1000",
        from_ns / 1_000_000,
        to_ns / 1_000_000
    );
    let Ok(resp) = reqwest::get(&url).await else { return Vec::new() };
    let Ok(points) = resp.json::<Vec<FundingPoint>>().await else { return Vec::new() };
    points
        .into_iter()
        .filter_map(|p| {
            let rate = p.funding_rate.parse::<f64>().ok()?;
            Some((p.funding_time * 1_000_000, rate))
        })
        .collect()
}

/// Fetch open interest for request.data("SYM.oi"). The venue caps at 500
/// points per page, so the period adapts to the window: the coarsest
/// granularity that still covers it in one page.
async fn fetch_oi_series(sym: &str, from_ns: i64, to_ns: i64) -> Vec<(i64, f64)> {
    let minutes = ((to_ns - from_ns) / 60_000_000_000).max(1);
    let period = if minutes / 5 <= 480 {
        "5m"
    } else if minutes / 15 <= 480 {
        "15m"
    } else if minutes / 30 <= 480 {
        "30m"
    } else if minutes / 60 <= 480 {
        "1h"
    } else if minutes / 120 <= 480 {
        "2h"
    } else if minutes / 240 <= 480 {
        "4h"
    } else if minutes / 720 <= 480 {
        "12h"
    } else {
        "1d"
    };
    let url = format!(
        "https://fapi.binance.com/futures/data/openInterestHist?symbol={sym}&period={period}&startTime={}&endTime={}&limit=500",
        from_ns / 1_000_000,
        to_ns / 1_000_000
    );
    let Ok(resp) = reqwest::get(&url).await else { return Vec::new() };
    let Ok(points) = resp.json::<Vec<OiPoint>>().await else { return Vec::new() };
    points
        .into_iter()
        .filter_map(|p| {
            let v = p.sum_open_interest.parse::<f64>().ok()?;
            Some((p.timestamp * 1_000_000, v))
        })
        .collect()
}

/// Carry a sparse (time, value) series onto the chart's bars: each bar
/// reads the last point at or before its open.
fn carry_points(points: &[(i64, f64)], candles: &[types::Candle]) -> Vec<f64> {
    let mut out = Vec::with_capacity(candles.len());
    let mut cursor = 0usize;
    for c in candles {
        while cursor + 1 < points.len() && points[cursor + 1].0 <= c.open_time {
            cursor += 1;
        }
        let usable = !points.is_empty() && points[cursor.min(points.len() - 1)].0 <= c.open_time;
        out.push(if usable { points[cursor].1 } else { f64::NAN });
    }
    out
}

/// Parse a synthetic ticker (`BTCUSDT/ETHUSDT`, `BTCUSDT*ETHUSDT`,
/// `BTCUSDT-ETHUSDT`, `BTCUSDT+ETHUSDT`): two legs and one operator.
/// Anything else is not a synthetic -- the caller treats it as a plain
/// symbol (and a plain symbol never contains these characters).
fn parse_synthetic(sym: &str) -> Option<(String, char, String)> {
    for op in ['/', '*', '-', '+'] {
        if let Some((a, b)) = sym.split_once(op) {
            let (a, b) = (a.trim(), b.trim());
            if !a.is_empty() && !b.is_empty() && !b.contains(['/', '*', '-', '+']) {
                return Some((a.to_uppercase(), op, b.to_uppercase()));
            }
        }
    }
    None
}

/// Compose a synthetic series from two legs: the OHLC fields combine
/// pointwise over the shared time grid (both venues align per timeframe),
/// volume takes the first leg's, and a zero denominator leg yields a
/// zero-close candle the VM reads as flat -- never a panic.
fn compose_synthetic(
    a: &[types::Candle],
    op: char,
    b: &[types::Candle],
) -> Vec<types::Candle> {
    let b_by_time: std::collections::HashMap<i64, &types::Candle> =
        b.iter().map(|c| (c.open_time, c)).collect();
    let combine = |x: f64, y: f64| match op {
        '/' => {
            if y != 0.0 {
                x / y
            } else {
                0.0
            }
        }
        '*' => x * y,
        '-' => x - y,
        _ => x + y,
    };
    a.iter()
        .filter_map(|ca| {
            let cb = b_by_time.get(&ca.open_time)?;
            Some(types::Candle {
                symbol: ca.symbol.clone(),
                timeframe: ca.timeframe,
                open_time: ca.open_time,
                open: combine(ca.open, cb.open),
                high: combine(ca.high, cb.high),
                low: combine(ca.low, cb.low),
                close: combine(ca.close, cb.close),
                volume: ca.volume,
                buy_volume: ca.buy_volume,
                sell_volume: ca.sell_volume,
            })
        })
        .collect()
}

#[cfg(test)]
mod data_feed_tests {
    use super::*;

    #[test]
    fn sparse_points_carry_forward_by_bar_open() {
        // Points at hours 0 and 4; bars are hourly.
        let points = vec![
            (0, 0.0001),
            (4 * 3_600_000_000_000, -0.0002),
        ];
        let candles: Vec<types::Candle> = (0..6)
            .map(|i| types::Candle {
                symbol: "T".to_string(),
                timeframe: Timeframe::H1,
                open_time: i * Timeframe::H1.nanos(),
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            })
            .collect();
        let carried = carry_points(&points, &candles);
        assert_eq!(carried[0], 0.0001, "the first point lands at its bar");
        assert_eq!(carried[3], 0.0001, "carried across the gap");
        assert_eq!(carried[4], -0.0002, "the new point takes over at its bar");
        // Before any point: na, not zero.
        let later = vec![(10 * Timeframe::H1.nanos(), 1.0)];
        let carried = carry_points(&later, &candles);
        assert!(carried[0].is_nan(), "no point yet is na");
    }
}

#[cfg(test)]
mod synthetic_tests {
    use super::*;

    fn leg(sym: &str, closes: &[f64]) -> Vec<types::Candle> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| types::Candle {
                symbol: sym.to_string(),
                timeframe: Timeframe::H1,
                open_time: i as i64 * Timeframe::H1.nanos(),
                open: *c,
                high: *c * 1.01,
                low: *c * 0.99,
                close: *c,
                volume: 10.0,
                buy_volume: 5.0,
                sell_volume: 5.0,
            })
            .collect()
    }

    #[test]
    fn the_ratio_ticker_composes_pointwise() {
        let a = leg("BTCUSDT", &[100.0, 110.0, 120.0]);
        let b = leg("ETHUSDT", &[50.0, 55.0, 40.0]);
        let s = compose_synthetic(&a, '/', &b);
        assert_eq!(s.len(), 3);
        assert!((s[0].close - 2.0).abs() < 1e-9, "100/50");
        assert!((s[2].close - 3.0).abs() < 1e-9, "120/40");
        assert!((s[0].high - 101.0 / 50.5).abs() < 1e-9, "high fields compose too");
        // A missing leg bar drops the pair's bar (the shared grid only).
        let b_short = leg("ETHUSDT", &[50.0]);
        let s = compose_synthetic(&a, '/', &b_short);
        assert_eq!(s.len(), 1, "only the shared bar survives");
        // A zero denominator never panics.
        let zero = leg("ETHUSDT", &[0.0, 55.0, 40.0]);
        let s = compose_synthetic(&a, '/', &zero);
        assert_eq!(s[0].close, 0.0, "zero denominator reads flat");
        // The parser: one operator, two legs, nothing nested.
        assert_eq!(
            parse_synthetic("BTCUSDT/ETHUSDT"),
            Some(("BTCUSDT".to_string(), '/', "ETHUSDT".to_string()))
        );
        assert_eq!(parse_synthetic("BTCUSDT"), None);
        assert_eq!(parse_synthetic("A/B/C"), None);
    }
}

fn align_security_pooled(primary: &[types::Candle], pooled: &[types::Candle]) -> Vec<types::Candle> {
    if pooled.is_empty() || primary.is_empty() {
        return Vec::new();
    }
    let first = pooled[0].close;
    let mut out = Vec::with_capacity(primary.len());
    let mut cursor = 0usize;
    for bar in primary {
        // Advance while the NEXT pooled candle has COMPLETED by this bar's
        // open: its window starts at or before the bar, so it is usable.
        while cursor + 1 < pooled.len() && pooled[cursor + 1].open_time <= bar.open_time {
            cursor += 1;
        }
        // The chosen candle must have STARTED by the bar; otherwise the bar
        // predates the pooled series entirely.
        let usable = pooled[cursor].open_time <= bar.open_time;
        let src = if usable { &pooled[cursor] } else { &pooled[0] };
        let fill = if usable { src.close } else { first };
        out.push(types::Candle {
            symbol: src.symbol.clone(),
            timeframe: src.timeframe,
            open_time: bar.open_time,
            open: if usable { src.open } else { fill },
            high: if usable { src.high } else { fill },
            low: if usable { src.low } else { fill },
            close: fill,
            volume: if usable { src.volume } else { 0.0 },
            buy_volume: if usable { src.buy_volume } else { 0.0 },
            sell_volume: if usable { src.sell_volume } else { 0.0 },
        });
    }
    out
}

fn align_security(primary: &[types::Candle], sec: &[types::Candle]) -> Vec<types::Candle> {
    if sec.is_empty() || primary.is_empty() {
        return Vec::new();
    }
    let first = sec[0].close;
    let mut out = Vec::with_capacity(primary.len());
    let mut cursor = 0usize;
    for bar in primary {
        while cursor + 1 < sec.len() && sec[cursor + 1].open_time <= bar.open_time {
            cursor += 1;
        }
        let aligned = if sec[cursor].open_time <= bar.open_time {
            let c = &sec[cursor];
            types::Candle {
                symbol: c.symbol.clone(),
                timeframe: c.timeframe,
                open_time: bar.open_time,
                open: c.open,
                high: c.high,
                low: c.low,
                close: c.close,
                volume: c.volume,
                buy_volume: c.buy_volume,
                sell_volume: c.sell_volume,
            }
        } else {
            // Before the pair's first stored bar: flat at its first close.
            types::Candle {
                symbol: sec[0].symbol.clone(),
                timeframe: sec[0].timeframe,
                open_time: bar.open_time,
                open: first,
                high: first,
                low: first,
                close: first,
                volume: 0.0,
                buy_volume: 0.0,
                sell_volume: 0.0,
            }
        };
        out.push(aligned);
    }
    out
}

pub async fn replay_script_preview(
    state: &AppState,
    database: &db::Database,
    symbol: &str,
    timeframe: &str,
    source: &str,
) -> Result<
    (
        chart_engine::IndicatorOutput,
        crate::indicator_workspace_routes::ScriptPreviewStats,
        serde_json::Value,
    ),
    String,
>
{
    let (header, parsed) =
        pine_lite::vet(source).map_err(|errs| format!("the script no longer vets: {}", errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("; ")))?;
    let to_ns = crate::now_ns();
    let from_ns = to_ns - PREVIEW_WINDOW_NS;
    let tf = timeframe
        .parse::<Timeframe>()
        .unwrap_or(Timeframe::M1);
    // The document replay backfills its window from the venue first; a script
    // preview over an empty store would "draw nothing" for data reasons, so
    // the same backfill runs here -- idempotent on repeat.
    if let Ok(candles) = state
        .backfill
        .backfill_candles(
            symbol,
            tf,
            from_ns,
            to_ns,
            market_data::BackfillSource::Klines,
        )
        .await
    {
        if !candles.is_empty() {
            if let Err(error) = db::repositories::insert_candles(database.pool(), &candles).await {
                tracing::warn!(%error, "could not store script preview backfill");
            }
        }
    }
    let timeframes = std::collections::BTreeMap::from([(
        "entry".to_string(),
        tf,
    )]);
    let series = db::loading::load_timeframe_series(
        database.pool(),
        symbol,
        &timeframes,
        from_ns,
        to_ns,
        tf,
    )
    .await
    .map_err(|error| format!("could not load candles for the script preview: {error}"))?;
    let candles = series.get("entry").cloned().unwrap_or_default();
    if candles.is_empty() {
        return Err("no stored candles for this symbol and timeframe yet".to_string());
    }
    // The header's second instrument (`sec=`): fetch over the SAME window,
    // then time-align onto the primary series' bars. A bar the second market
    // did not trade carries its last close forward with flat OHLC, so
    // cross-market math never drifts by an index.
    let security = match &header.sec {
        Some(sec) => {
            let sec_candles =
                fetch_security_candles(state, database, sec, tf, from_ns, to_ns).await;
            if sec_candles.is_empty() {
                Vec::new()
            } else {
                align_security(&candles, &sec_candles)
            }
        }
        None => Vec::new(),
    };
    // Phase 11 (docs/23): every `request.security("SYM", "tf", ...)` pair
    // the script names, fetched and aligned onto the chart's own bars into
    // the keyed pool the VM reads. A pair on a COARSER timeframe is fetched
    // at that tf and aligned by window coverage (a chart bar inside a pooled
    // candle uses that candle -- completed bars only, no lookahead); the
    // chart's own symbol/timeframe is served from the primary fetch.
    let mut series_pool = std::collections::HashMap::new();
    for key in pine_lite::typecheck::collect_series_pool(&parsed, &header) {
        // `#ltf` keys come from request.security_lower_tf: the host pools
        // the RAW lower-timeframe series (no alignment -- each chart bar
        // slices its own window out of it).
        let (key, want_raw) = match key.strip_suffix("#ltf") {
            Some(base) => (base.to_string(), true),
            None => (key, false),
        };
        let Some((sym, tf_str)) = key.split_once('@') else { continue };
        // Pine spellings normalize to the venue set: bare numbers are
        // MINUTES ("60" = 1h, "240" = 4h, "1440" = 1d), "D"/"W" are the
        // daily/weekly singles. An unknown string still falls back to the
        // chart timeframe -- but now it has tried the Pine reading first.
        let pooled_tf = tf_str
            .parse::<Timeframe>()
            .ok()
            .or_else(|| match tf_str {
                "3" => Some(Timeframe::M3),
                "30" => Some(Timeframe::M30),
                "60" => Some(Timeframe::H1),
                "120" => Some(Timeframe::H2),
                "240" => Some(Timeframe::H4),
                "360" => Some(Timeframe::H6),
                "480" => Some(Timeframe::H8),
                "720" => Some(Timeframe::H12),
                "1440" | "D" => Some(Timeframe::D1),
                "4320" => Some(Timeframe::D3),
                "W" => Some(Timeframe::W1),
                _ => None,
            })
            .unwrap_or(tf);
        let is_own = sym.eq_ignore_ascii_case(symbol) && pooled_tf == tf;
        let raw = if is_own {
            candles.clone()
        } else if let Some((leg_a, op, leg_b)) = parse_synthetic(sym) {
            // A synthetic ticker: fetch both legs and compose pointwise
            // (BTCUSDT/ETHUSDT is the strength-ratio classic).
            let (a, b) = (
                fetch_security_candles(state, database, &leg_a, pooled_tf, from_ns, to_ns).await,
                fetch_security_candles(state, database, &leg_b, pooled_tf, from_ns, to_ns).await,
            );
            let composed = compose_synthetic(&a, op, &b);
            if composed.is_empty() {
                tracing::warn!(%sym, %tf_str, "synthetic pool legs share no bars");
                continue;
            }
            composed
        } else {
            let fetched =
                fetch_security_candles(state, database, sym, pooled_tf, from_ns, to_ns).await;
            if fetched.is_empty() {
                tracing::warn!(%sym, %tf_str, "request.security pool fetch came back empty");
                continue;
            }
            fetched
        };
        let pooled = if want_raw { raw } else { align_security_pooled(&candles, &raw) };
        series_pool.insert(if want_raw { format!("{key}#ltf") } else { key }, pooled);
    }
    // Phase 14 (docs/23): `request.data("NAME")` — platform-native feeds.
    // Today: ticker fields from the venue cache, named `SYMBOL.field`
    // (change_pct, quote_volume, high, low). Tickers are point-in-time, so
    // the aligned series is the current value carried flat across the
    // window; venue funding/OI plugs into the same map when those feeds
    // land, with no language change.
    let mut data_series = std::collections::HashMap::new();
    for name in pine_lite::typecheck::collect_data_names(&parsed) {
        let Some((sym, field)) = name.split_once('.') else { continue };
        match field {
            // Venue futures feeds: funding rate and open interest, carried
            // per bar. A failed fetch leaves the name missing -- the VM's
            // data-missing error stays the truth.
            "funding" => {
                let points = fetch_funding_series(sym, from_ns, to_ns).await;
                if !points.is_empty() {
                    data_series.insert(name.clone(), carry_points(&points, &candles));
                }
            }
            "oi" => {
                let points = fetch_oi_series(sym, from_ns, to_ns).await;
                if !points.is_empty() {
                    data_series.insert(name.clone(), carry_points(&points, &candles));
                }
            }
            _ => {
                if let Some(t) = state.tickers.get(sym).await {
                    let value = match field {
                        "change_pct" => Some(t.price_change_percent),
                        "quote_volume" => Some(t.quote_volume),
                        "high" => Some(t.high_price),
                        "low" => Some(t.low_price),
                        "last" => Some(t.last_price),
                        _ => None,
                    };
                    if let Some(v) = value {
                        data_series.insert(name.clone(), vec![v; candles.len()]);
                    }
                }
            }
        }
    }
    // The instrument's tick size feeds syminfo.mintick /
    // math.round_to_mintick. Static majors table until exchange-info wiring
    // lands; 0.01 covers the USDT majors customers chart most.
    let mintick = Some(match symbol.to_uppercase().as_str() {
        "XRPUSDT" | "DOGEUSDT" | "ADAUSDT" => 0.0001,
        "SOLUSDT" | "BNBUSDT" => 0.001,
        _ => 0.01,
    });
    let inputs = pine_lite::interp::Inputs { security, series_pool, data_series, mintick, ..pine_lite::interp::Inputs::default() };
    let output = pine_lite::interp::run(&parsed, &candles, &inputs)
        .map_err(|err| format!("the script failed at run time: {err}"))?;
    // Run notes (docs/24 S1: the two-pass strategy mismatch, and any later
    // simulation warnings) ride the preview_note slot -- non-fatal, but the
    // user must see them.
    let run_note = if output.notes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(output.notes.join("; "))
    };
    let stats = crate::indicator_workspace_routes::ScriptPreviewStats {
        window_days: (PREVIEW_WINDOW_NS / 86_400_000_000_000) as i64,
        bars: candles.len(),
        plots: output.plots.len(),
        levels: output.hlines.len(),
        shapes: output.shapes.len(),
        overlays: usize::from(header.overlay),
        strategy: output.simulation.is_some(),
        trades: output.simulation.as_ref().map_or(0, |s| s.report.total_trades as usize),
        net_profit: output.simulation.as_ref().map_or(0.0, |s| s.report.net_profit),
        max_drawdown: output.simulation.as_ref().map_or(0.0, |s| s.report.max_drawdown),
        alerts: output.alerts.len(),
        simulation_note: strategy_simulation_note(output.simulation.as_ref(), candles.len()),
    };
    let title = header.title.clone().unwrap_or_else(|| "script".to_string());
    // The preview folds the run into the shared vocabulary: one zone per
    // plot (its value range over the window), one marker per shape. The real
    // rendering is the chart engine's script runner -- these are the counts
    // and shapes the chat reports.
    let mut out = chart_engine::IndicatorOutput {
        revision_id: format!("script:{title}"),
        name: Some(title),
        concepts: Vec::new(),
        evidence: Vec::new(),
        zones: Vec::new(),
        markers: Vec::new(),
        links: Vec::new(),
        trendlines: Vec::new(),
    };
    for plot in &output.plots {
        let finite: Vec<f64> = plot.values.iter().copied().filter(|v| v.is_finite()).collect();
        let (Some(low), Some(high)) = (
            finite.iter().copied().reduce(f64::min),
            finite.iter().copied().reduce(f64::max),
        ) else {
            continue;
        };
        out.zones.push(chart_engine::IndicatorZone {
            id: format!("{}-plot", plot.id),
            start_time: candles.first().map(|c| c.open_time).unwrap_or(0),
            end_time: candles.last().map(|c| c.open_time).unwrap_or(0),
            price_low: low,
            price_high: high,
            label: plot.title.clone(),
            state: chart_engine::ZoneState::Active,
        });
    }
    for shape in &output.shapes {
        let Some(candle) = candles.get(shape.bar) else { continue };
        out.markers.push(chart_engine::IndicatorMarker {
            id: format!("{}-shape-{}", out.revision_id, shape.bar),
            evidence_id: String::new(),
            time: candle.open_time,
            price: shape.value,
            label: shape.glyph.clone(),
            kind: chart_engine::MarkerKind::Signal,
        });
    }
    Ok((out, stats, run_note))
}

/// docs/24 S3: a strategy that simulated ZERO fills is a drawing, not a
/// strategy -- the run succeeds, but the note must ride the stats card and
/// the chat row so the user (and the repair loop's next round) sees the
/// truth instead of a silent "net 0". `None` for indicator runs and for
/// strategies that actually traded.
#[must_use]
pub(crate) fn strategy_simulation_note(
    simulation: Option<&pine_lite::sim::Simulation>,
    bars: usize,
) -> Option<String> {
    let sim = simulation?;
    (sim.report.total_trades == 0.0).then(|| {
        format!(
            "0 fills in {bars} bars -- the entry condition never fired or every entry was skipped; loosen the entry condition or gate it on strategy.position_size == 0"
        )
    })
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
) -> Result<(IndicatorOutput, Option<PreviewStats>), String> {
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
                if let Err(error) =
                    db::repositories::insert_candles(database.pool(), &candles).await
                {
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
        let concepts: Vec<_> = document.concepts.clone();
        let mut preview = build_indicator_preview(
            document.name.clone(),
            &document.name,
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
        // A detector layer has no trades to score -- its honest stats are the
        // detection counts the preview already carries.
        let stats = PreviewStats {
            kind: PreviewKind::Detector,
            fires: preview.zones.len(),
            trades: 0,
            win_rate: None,
            average_r: None,
            max_drawdown_r: None,
            net_r: None,
            window_days: PREVIEW_WINDOW_NS as f64 / (24.0 * 3600.0 * 1e9),
        };
        return Ok((preview, Some(stats)));
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

    // The same metrics the backtester reports, computed from the preview
    // replay's own trades. This is the "is the idea even viable" answer, and
    // it costs nothing: the trades already exist, `compute_metrics` is the
    // shared arithmetic, and a preview that scored differently from a real
    // backtest would be a second implementation of the one thing this
    // workspace must never have two of.
    let metrics = backtester::report::compute_metrics(&output.trades, PREVIEW_WINDOW_NS);
    let stats = PreviewStats {
        kind: PreviewKind::Strategy,
        fires: output.signals.len(),
        trades: metrics.total_trades,
        win_rate: Some(metrics.win_rate),
        average_r: Some(metrics.average_r),
        max_drawdown_r: Some(metrics.max_drawdown_pct),
        net_r: Some(metrics.net_return_pct),
        window_days: PREVIEW_WINDOW_NS as f64 / (24.0 * 3600.0 * 1e9),
    };

    Ok((build_preview("", &output.signals), Some(stats)))
}

/// What kind of document the preview replayed -- which stats mean what.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    /// A `kind: strategy` replay: `fires` is signals, the R numbers are real.
    Strategy,
    /// A `kind: indicator` detection pass: `fires` is matched windows, and
    /// the R fields are `None` by design rather than by omission.
    Detector,
}

/// The numbers a preview replay produced, for the chat's auto-backtest card.
///
/// Deliberately small and honest: this is a one-week sandboxed replay, not a
/// historical backtest, and the card says so. `Serialize` because it travels
/// inside the assistant message's payload.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PreviewStats {
    /// Which kind of document produced these numbers.
    pub kind: PreviewKind,
    /// Entry signals fired (strategy) or windows matched (detector).
    pub fires: usize,
    /// Completed trades. Zero for a detector.
    pub trades: u32,
    /// Fraction of trades that made money; `None` for a detector.
    pub win_rate: Option<f64>,
    /// Mean R per trade; `None` for a detector.
    pub average_r: Option<f64>,
    /// Largest peak-to-trough fall of the cumulative R curve, positive; `None`
    /// for a detector.
    pub max_drawdown_r: Option<f64>,
    /// Total R over the window; `None` for a detector.
    pub net_r: Option<f64>,
    /// How many days the replay covered, so the card can say "per week".
    pub window_days: f64,
}

#[cfg(test)]
use uuid::Uuid;

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

    fn indicator_candle(
        index: i64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
    ) -> analytics_core::types::Candle {
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
            shape: concepts::ConceptShape::Band,
        }
    }

    #[test]
    fn a_trendline_concept_yields_fitted_lines_not_bands() {
        // A W over 40 candles: two confirmed highs, one valley.
        let mut series: Vec<analytics_core::types::Candle> = Vec::new();
        for i in 0..40i64 {
            let base = 100.0
                + if i <= 10 {
                    i as f64
                } else if i <= 20 {
                    (20 - i) as f64
                } else if i <= 30 {
                    (i - 20) as f64
                } else {
                    (40 - i) as f64
                };
            series.push(indicator_candle(i, base, base + 0.5, base - 0.5, base + 0.2));
        }
        let mut concept = bullish_gap_concept();
        concept.name = "swing_line".into();
        concept.label = Some("swing line".into());
        concept.window = 5;
        concept.lower = concepts::Selector::Low(0);
        concept.upper = concepts::Selector::High(4);
        concept.require = Vec::new();
        concept.shape = concepts::ConceptShape::Trendline;

        let output = build_indicator_preview("rev", "trendlines", &[concept], &series, None);
        assert!(
            !output.trendlines.is_empty(),
            "the W has pivots, so the fit succeeds"
        );
        for line in &output.trendlines {
            assert!(line.points.len() >= 2, "{line:?}");
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
            "test indicator",
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
            "test indicator",
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
            "test indicator",
            &concepts,
            &candles,
            Some(analytics_core::types::Timeframe::M5),
        );

        // The gap rule can fire on overlapping windows; each match is its own
        // zone/marker/evidence in the indicator output.
        assert!(!output.zones.is_empty(), "{output:#?}");
        assert_eq!(output.zones.len(), output.markers.len());
        assert_eq!(output.zones.len(), output.evidence.len());
    }

    #[test]
    fn build_indicator_preview_keeps_indicator_contracted_only() {
        let concepts = vec![bullish_gap_concept()];
        let series = gap_series();
        let output = build_indicator_preview(
            Uuid::new_v4(),
            "test indicator",
            &concepts,
            &series,
            Some(analytics_core::types::Timeframe::M5),
        );

        // An indicator preview must not emit Enter/Exit-style setup primitives.
        assert!(output.links.is_empty());
    }
}
