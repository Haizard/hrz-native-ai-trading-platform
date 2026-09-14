//! Loading a multi-timeframe series for a strategy document.
//!
//! ## Why this is here and not in each CLI
//!
//! The database stores candles per resolution, but a backfill normally only
//! populates one. Every consumer of a strategy document -- the backtester CLI,
//! the paper-bot runner -- therefore needs the same answer to "give me the
//! series this document declares", including the same rule for what counts as
//! usable data. Two copies of that rule is two chances for a backtest and a
//! paper run to be fed different candles and disagree for no visible reason.
//!
//! ## A stored series is only used if it covers the window
//!
//! "Non-empty" is not the same as "usable". A handful of stale 4h candles left
//! by an earlier backfill covers two days of a six-month window, leaves the 4h
//! view cold everywhere else, and turns every condition written against it
//! permanently false -- a backtest that reports zero trades and looks
//! perfectly valid. So a stored series must span at least [`MIN_COVERAGE`] of
//! the requested window, or it is treated as absent and resampled.

use std::collections::BTreeMap;

use analytics_core::{resample, Candle, Timeframe};
use sqlx::PgPool;

use crate::error::DbError;
use crate::repositories::load_candles;

/// How much of the requested window a stored series must span before it is
/// trusted over a resample from the finer source.
///
/// Not 1.0: a window that starts mid-bucket legitimately loses its first
/// candle, and demanding exact coverage would force a pointless resample.
pub const MIN_COVERAGE: f64 = 0.9;

/// The fraction of `[from, to)` that `series` actually spans.
#[must_use]
pub fn coverage(series: &[Candle], width: i64, from_ns: i64, to_ns: i64) -> f64 {
    let (Some(first), Some(last)) = (series.first(), series.last()) else {
        return 0.0;
    };
    let requested = to_ns - from_ns;
    if requested <= 0 {
        return 1.0;
    }
    let spanned = last.open_time + width - first.open_time;
    (spanned as f64 / requested as f64).clamp(0.0, 1.0)
}

/// Load one series per declared timeframe, resampling where it must.
///
/// # Errors
/// Returns [`DbError::CandlesUnavailable`] when a declared timeframe can
/// neither be loaded nor rebuilt from the source resolution.
pub async fn load_timeframe_series(
    pool: &PgPool,
    symbol: &str,
    timeframes: &BTreeMap<String, Timeframe>,
    from_ns: i64,
    to_ns: i64,
    source_timeframe: Timeframe,
) -> Result<BTreeMap<String, Vec<Candle>>, DbError> {
    let mut out = BTreeMap::new();
    // The direct series is carried along even when it is unusable, so a
    // resample that cannot be performed still has something to fall back to.
    let mut missing: Vec<(String, Timeframe, Vec<Candle>)> = Vec::new();

    for (name, timeframe) in timeframes {
        let direct = load_candles(pool, symbol, *timeframe, from_ns, to_ns).await?;

        let covered = coverage(&direct, timeframe.nanos(), from_ns, to_ns);
        if covered >= MIN_COVERAGE {
            out.insert(name.clone(), direct);
            continue;
        }
        if !direct.is_empty() {
            tracing::warn!(
                "{timeframe} has only {} stored candles spanning {:.1}% of the window; \
                 resampling from {source_timeframe} instead",
                direct.len(),
                covered * 100.0
            );
        }
        missing.push((name.clone(), *timeframe, direct));
    }

    if missing.is_empty() {
        return Ok(out);
    }

    // Pad the source window by the coarsest declared resolution so the first
    // and last bucket of every resampled series are whole.
    let pad = timeframes
        .values()
        .copied()
        .max()
        .map_or(0, Timeframe::nanos);

    let source = load_candles(pool, symbol, source_timeframe, from_ns - pad, to_ns + pad).await?;

    for (name, timeframe, direct) in missing {
        // Only a *strictly finer* source is a problem. An equal one is a no-op
        // copy that `resample` handles, and the trim below then reduces it to
        // exactly what the direct query would have returned.
        if timeframe < source_timeframe {
            return Err(DbError::CandlesUnavailable(format!(
                "no {timeframe} candles for {symbol} in this window, and {timeframe} cannot be \
                 built by aggregating {source_timeframe} candles"
            )));
        }

        if source.is_empty() {
            if direct.is_empty() {
                return Err(DbError::CandlesUnavailable(format!(
                    "no {source_timeframe} candles for {symbol} in this window either"
                )));
            }
            // Unusable stored series and nothing to rebuild it from. Use it
            // rather than refusing: a short series still answers some bars.
            tracing::warn!(
                "{timeframe} covers only part of the window and no {source_timeframe} source is \
                 available to resample from; using the stored candles as they are"
            );
            out.insert(name, direct);
            continue;
        }

        let series: Vec<Candle> = resample(&source, timeframe)
            .into_iter()
            // Trim the padding back off, keeping only buckets in the window.
            .filter(|candle| candle.open_time >= from_ns && candle.open_time < to_ns)
            .collect();

        if series.is_empty() {
            return Err(DbError::CandlesUnavailable(format!(
                "{timeframe} aggregated from {source_timeframe} produced no candles"
            )));
        }
        out.insert(name, series);
    }

    Ok(out)
}

/// Warn about any series that does not span the window.
///
/// A series that does not span the window leaves its view cold for the rest of
/// it, and every condition written against it is then false -- which looks
/// exactly like "the strategy found nothing". Say so out loud.
pub fn warn_about_short_series(
    series: &BTreeMap<String, Vec<Candle>>,
    timeframes: &BTreeMap<String, Timeframe>,
    from_ns: i64,
    to_ns: i64,
) {
    for (name, candles) in series {
        let Some(timeframe) = timeframes.get(name) else {
            continue;
        };
        let covered = coverage(candles, timeframe.nanos(), from_ns, to_ns);
        if covered < MIN_COVERAGE {
            let span = match (candles.first(), candles.last()) {
                (Some(first), Some(last)) => {
                    format!("{} .. {}", first.open_time, last.open_time)
                }
                _ => "empty".to_string(),
            };
            tracing::warn!(
                "{timeframe} (`{name}`) covers only {:.1}% of the window: conditions on it are \
                 false outside {span}, so a low trade count here means missing data, not a \
                 selective strategy",
                covered * 100.0
            );
        }
    }
}
