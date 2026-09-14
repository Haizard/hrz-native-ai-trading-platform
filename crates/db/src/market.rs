//! What market data the platform actually holds (`docs/04`, `docs/12`).
//!
//! ## Why an inventory is worth a query
//!
//! `GET /symbols` is not decoration. This project has already lost real time to
//! a database that held six months of 5m candles and *two days* of 1m, where a
//! strategy's coarse timeframe silently resampled to twelve candles and a
//! six-month backtest reported zero trades. A caller that can ask "what is
//! loaded, and over what window" answers that in one request instead of
//! inferring it from a backtest that found nothing.
//!
//! So the inventory reports the **span**, not just a count. "1,110 candles" is
//! reassuring and means nothing; "1,110 candles, 2026-03-13 to 2026-09-13" is
//! the fact that matters.
//!
//! ## Reads only
//!
//! This module never writes. Loading candles for a computation is
//! [`crate::repositories`]; this is the catalogue.

use sqlx::{PgPool, Row};

use analytics_core::types::OrderBookSnapshot;
use analytics_core::OrderBookLevel;

use crate::error::DbError;
use crate::repositories::{dt_to_ns, levels_from_json};

/// How much of one resolution is loaded for one symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeframeCoverage {
    /// The resolution, as stored: `1m`, `5m`, `1h`, `4h`.
    pub timeframe: String,
    /// How many candles are stored.
    pub candles: i64,
    /// The oldest candle's open time, unix nanos.
    pub first: i64,
    /// The newest candle's open time, unix nanos.
    pub last: i64,
}

impl TimeframeCoverage {
    /// The window this resolution spans, in nanoseconds.
    #[must_use]
    pub const fn span_nanos(&self) -> i64 {
        self.last.saturating_sub(self.first)
    }

    /// How many candles a complete series over this span would hold.
    ///
    /// `None` when the stored resolution cannot be parsed, which means the row
    /// was written by something that does not agree with
    /// [`analytics_core::Timeframe`] -- worth surfacing rather than guessing at.
    #[must_use]
    pub fn expected_candles(&self) -> Option<i64> {
        let width = self
            .timeframe
            .parse::<analytics_core::Timeframe>()
            .ok()?
            .nanos();
        if width <= 0 {
            return None;
        }
        // Inclusive of both ends: a series from t to t holds one candle.
        Some(self.span_nanos() / width + 1)
    }

    /// How many candles are missing from the span, if any.
    ///
    /// This is the number that matters. A count alone is reassuring and says
    /// nothing -- twelve 4h candles is a full series over two days and a hole
    /// over six months.
    #[must_use]
    pub fn missing_candles(&self) -> Option<i64> {
        Some((self.expected_candles()? - self.candles).max(0))
    }
}

/// Everything loaded for one symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolCoverage {
    /// The instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// One entry per resolution, ordered by resolution.
    pub timeframes: Vec<TimeframeCoverage>,
}

impl SymbolCoverage {
    /// The longest span any resolution covers.
    #[must_use]
    pub fn widest_span(&self) -> i64 {
        self.timeframes
            .iter()
            .map(TimeframeCoverage::span_nanos)
            .max()
            .unwrap_or(0)
    }

    /// A warning when one resolution covers much less history than another.
    ///
    /// [`TimeframeCoverage::missing_candles`] catches holes *within* a stored
    /// span. This catches the other failure, which is the one that actually
    /// happened here: every series complete over its own span, and one span a
    /// hundredth the length of another. Twelve 4h candles is a perfect series
    /// over two days, and a strategy that resamples its 4h view from a two-day
    /// 1m series gets exactly that.
    ///
    /// Returns `None` when the spans are comparable, so the field is absent
    /// rather than empty for the ordinary case.
    #[must_use]
    pub fn coverage_note(&self) -> Option<String> {
        let widest = self.widest_span();
        if widest <= 0 || self.timeframes.len() < 2 {
            return None;
        }

        let mut thin: Vec<&TimeframeCoverage> = self
            .timeframes
            .iter()
            .filter(|tf| tf.span_nanos() * 2 < widest)
            .collect();
        if thin.is_empty() {
            return None;
        }
        thin.sort_by_key(|tf| tf.span_nanos());

        let widest_tf = self
            .timeframes
            .iter()
            .max_by_key(|tf| tf.span_nanos())
            .map(|tf| tf.timeframe.clone())
            .unwrap_or_default();
        let days = |nanos: i64| nanos as f64 / (86_400.0 * 1_000_000_000.0);

        let listed = thin
            .iter()
            .map(|tf| format!("{} covers {:.1} days", tf.timeframe, days(tf.span_nanos())))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "{listed}, while {widest_tf} covers {:.1} days. A document declaring a coarse timeframe resamples from the shortest source available, so its view may warm up over a fraction of the window it is asked about.",
            days(widest)
        ))
    }
}

/// Every symbol with stored candles, and what is stored for it.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn list_symbols(pool: &PgPool) -> Result<Vec<SymbolCoverage>, DbError> {
    let rows = sqlx::query(
        "SELECT symbol, timeframe, count(*) AS candles, min(open_time) AS first, \
                max(open_time) AS last \
         FROM candles GROUP BY symbol, timeframe ORDER BY symbol, timeframe",
    )
    .fetch_all(pool)
    .await?;

    let mut out: Vec<SymbolCoverage> = Vec::new();
    for row in rows {
        let symbol: String = row.try_get("symbol")?;
        let coverage = TimeframeCoverage {
            timeframe: row.try_get("timeframe")?,
            candles: row.try_get("candles")?,
            first: dt_to_ns(row.try_get("first")?),
            last: dt_to_ns(row.try_get("last")?),
        };
        match out.last_mut() {
            Some(entry) if entry.symbol == symbol => entry.timeframes.push(coverage),
            _ => out.push(SymbolCoverage {
                symbol,
                timeframes: vec![coverage],
            }),
        }
    }
    Ok(out)
}

/// Whether the platform holds any order-book data for a symbol.
///
/// A separate query from the snapshot itself, because "there is none" and
/// "there is none *yet*" are different answers and the route has to tell them
/// apart.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn orderbook_snapshot_count(pool: &PgPool, symbol: &str) -> Result<i64, DbError> {
    let row = sqlx::query("SELECT count(*) AS n FROM orderbook_snapshots WHERE symbol = $1")
        .bind(symbol)
        .fetch_one(pool)
        .await?;
    Ok(row.try_get("n")?)
}

/// The newest stored order-book snapshot for a symbol.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn latest_orderbook(
    pool: &PgPool,
    symbol: &str,
) -> Result<Option<OrderBookSnapshot>, DbError> {
    let row = sqlx::query(
        "SELECT symbol, ts, bids, asks FROM orderbook_snapshots \
         WHERE symbol = $1 ORDER BY ts DESC LIMIT 1",
    )
    .bind(symbol)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else { return Ok(None) };
    let bids: serde_json::Value = row.try_get("bids")?;
    let asks: serde_json::Value = row.try_get("asks")?;

    Ok(Some(OrderBookSnapshot {
        symbol: row.try_get("symbol")?,
        timestamp: dt_to_ns(row.try_get("ts")?),
        bids: levels_from_json(&bids),
        asks: levels_from_json(&asks),
    }))
}

/// The best bid and ask in a snapshot, when it has both sides.
///
/// `bids` are best-first and `asks` are best-first (`docs/04`), so the spread is
/// the first of each -- and a snapshot missing either side has no spread rather
/// than a spread of zero.
#[must_use]
pub fn best_bid_ask(snapshot: &OrderBookSnapshot) -> Option<(OrderBookLevel, OrderBookLevel)> {
    match (snapshot.bids.first(), snapshot.asks.first()) {
        (Some(bid), Some(ask)) => Some((*bid, *ask)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(bids: Vec<OrderBookLevel>, asks: Vec<OrderBookLevel>) -> OrderBookSnapshot {
        OrderBookSnapshot {
            symbol: "BTCUSDT".into(),
            timestamp: 0,
            bids,
            asks,
        }
    }

    #[test]
    fn the_spread_comes_from_the_first_level_of_each_side() {
        let book = snapshot(
            vec![
                OrderBookLevel {
                    price: 100.0,
                    quantity: 2.0,
                },
                OrderBookLevel {
                    price: 99.0,
                    quantity: 5.0,
                },
            ],
            vec![OrderBookLevel {
                price: 101.0,
                quantity: 1.0,
            }],
        );
        let (bid, ask) = best_bid_ask(&book).expect("both sides");
        assert_eq!(bid.price, 100.0);
        assert_eq!(ask.price, 101.0);
    }

    #[test]
    fn a_one_sided_book_has_no_spread_rather_than_a_spread_of_zero() {
        // Reporting 0.0 would look like a perfectly tight market.
        let no_asks = snapshot(
            vec![OrderBookLevel {
                price: 100.0,
                quantity: 1.0,
            }],
            vec![],
        );
        assert!(best_bid_ask(&no_asks).is_none());

        let empty = snapshot(vec![], vec![]);
        assert!(best_bid_ask(&empty).is_none());
    }

    #[test]
    fn a_complete_series_reports_no_gaps() {
        // 5m over one hour is 13 candles inclusive of both ends.
        let coverage = TimeframeCoverage {
            timeframe: "5m".into(),
            candles: 13,
            first: 0,
            last: 12 * 300_000_000_000,
        };
        assert_eq!(coverage.expected_candles(), Some(13));
        assert_eq!(coverage.missing_candles(), Some(0));
    }

    #[test]
    fn a_series_with_a_hole_reports_the_hole() {
        // The same hour with only three candles in it.
        let coverage = TimeframeCoverage {
            timeframe: "5m".into(),
            candles: 3,
            first: 0,
            last: 12 * 300_000_000_000,
        };
        assert_eq!(coverage.expected_candles(), Some(13));
        assert_eq!(coverage.missing_candles(), Some(10));
    }

    #[test]
    fn a_resolution_that_does_not_parse_reports_nothing_rather_than_guessing() {
        let coverage = TimeframeCoverage {
            timeframe: "fortnightly".into(),
            candles: 3,
            first: 0,
            last: 1_000,
        };
        assert!(coverage.expected_candles().is_none());
        assert!(coverage.missing_candles().is_none());
    }

    #[test]
    fn comparable_spans_produce_no_note() {
        // Six months of 5m and six months of 4h: both complete, both long.
        let coverage = SymbolCoverage {
            symbol: "BTCUSDT".into(),
            timeframes: vec![
                TimeframeCoverage {
                    timeframe: "5m".into(),
                    candles: 50_000,
                    first: 0,
                    last: 180 * 86_400_000_000_000,
                },
                TimeframeCoverage {
                    timeframe: "4h".into(),
                    candles: 1_080,
                    first: 0,
                    last: 180 * 86_400_000_000_000,
                },
            ],
        };
        assert_eq!(coverage.coverage_note(), None);
    }

    #[test]
    fn a_thin_series_is_called_out_by_name() {
        // The shape of the real incident: six months of 5m, two days of 1m.
        let coverage = SymbolCoverage {
            symbol: "BTCUSDT".into(),
            timeframes: vec![
                TimeframeCoverage {
                    timeframe: "1m".into(),
                    candles: 2_880,
                    first: 0,
                    last: 2 * 86_400_000_000_000,
                },
                TimeframeCoverage {
                    timeframe: "5m".into(),
                    candles: 53_172,
                    first: 0,
                    last: 184 * 86_400_000_000_000,
                },
            ],
        };
        let note = coverage.coverage_note().expect("must warn");
        assert!(note.contains("1m covers 2.0 days"), "{note}");
        assert!(note.contains("5m covers 184.0 days"), "{note}");
    }

    #[test]
    fn a_single_resolution_has_nothing_to_compare_against() {
        let coverage = SymbolCoverage {
            symbol: "BTCUSDT".into(),
            timeframes: vec![TimeframeCoverage {
                timeframe: "5m".into(),
                candles: 10,
                first: 0,
                last: 1_000,
            }],
        };
        assert_eq!(coverage.coverage_note(), None);
    }

    #[test]
    fn coverage_span_is_the_window_not_the_count() {
        let coverage = TimeframeCoverage {
            timeframe: "5m".into(),
            candles: 1110,
            first: 1_000,
            last: 4_000,
        };
        assert_eq!(coverage.span_nanos(), 3_000);
    }
}
