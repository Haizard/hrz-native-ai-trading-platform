//! Query helpers for market data.
//!
//! Writes are **idempotent by design**: backfills get re-run, collectors
//! reconnect and replay, and a REST-resync after a gap will re-deliver rows the
//! platform already has. Candles therefore upsert and trades/order-book
//! snapshots `DO NOTHING` on conflict, so a replay is a no-op rather than a
//! duplicate-key failure.
//!
//! All timestamps crossing this boundary are converted between the platform's
//! internal unix-nanos `i64` and Postgres `TIMESTAMPTZ`.

use std::str::FromStr;

use analytics_core::{Candle, OrderBookLevel, OrderBookSnapshot, Timeframe, Trade};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, QueryBuilder, Row};

use crate::error::DbError;

/// Rows per multi-row INSERT. Large enough to amortize round-trips, small
/// enough to stay under Postgres' 65535 bind-parameter ceiling.
const INSERT_CHUNK: usize = 500;

/// Convert unix nanoseconds to a UTC timestamp.
#[must_use]
pub fn ns_to_dt(ns: i64) -> DateTime<Utc> {
    let secs = ns.div_euclid(1_000_000_000);
    let nanos = ns.rem_euclid(1_000_000_000) as u32;
    DateTime::from_timestamp(secs, nanos).unwrap_or(DateTime::UNIX_EPOCH)
}

/// Convert a UTC timestamp to unix nanoseconds.
#[must_use]
pub fn dt_to_ns(dt: DateTime<Utc>) -> i64 {
    dt.timestamp() * 1_000_000_000 + i64::from(dt.timestamp_subsec_nanos())
}

/// Persist candles, upserting on `(symbol, timeframe, open_time)`.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn insert_candles(pool: &PgPool, candles: &[Candle]) -> Result<(), DbError> {
    for chunk in candles.chunks(INSERT_CHUNK) {
        let mut query = QueryBuilder::<sqlx::Postgres>::new(
            "INSERT INTO candles (symbol, timeframe, open_time, open, high, low, close, volume, buy_volume, sell_volume) ",
        );

        query.push_values(chunk, |mut row, c| {
            row.push_bind(c.symbol.as_str())
                .push_bind(c.timeframe.to_string())
                .push_bind(ns_to_dt(c.open_time))
                .push_bind(c.open)
                .push_bind(c.high)
                .push_bind(c.low)
                .push_bind(c.close)
                .push_bind(c.volume)
                .push_bind(c.buy_volume)
                .push_bind(c.sell_volume);
        });

        query.push(
            " ON CONFLICT (symbol, timeframe, open_time) DO UPDATE SET \
             open = EXCLUDED.open, high = EXCLUDED.high, low = EXCLUDED.low, \
             close = EXCLUDED.close, volume = EXCLUDED.volume, \
             buy_volume = EXCLUDED.buy_volume, sell_volume = EXCLUDED.sell_volume \
             WHERE candles.open_time = EXCLUDED.open_time",
        );

        query.build().execute(pool).await?;
    }
    Ok(())
}

/// Persist trades, ignoring ones already stored.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn insert_trades(pool: &PgPool, trades: &[Trade]) -> Result<(), DbError> {
    for chunk in trades.chunks(INSERT_CHUNK) {
        let mut query = QueryBuilder::<sqlx::Postgres>::new(
            "INSERT INTO trades (symbol, trade_id, price, quantity, is_buyer_maker, ts) ",
        );

        query.push_values(chunk, |mut row, t| {
            row.push_bind(t.symbol.as_str())
                .push_bind(t.trade_id as i64)
                .push_bind(t.price)
                .push_bind(t.quantity)
                .push_bind(t.is_buyer_maker)
                .push_bind(ns_to_dt(t.timestamp));
        });

        query.push(" ON CONFLICT (symbol, trade_id) DO NOTHING");
        query.build().execute(pool).await?;
    }
    Ok(())
}

/// Persist order-book snapshots, ignoring duplicates.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn insert_orderbook_snapshots(
    pool: &PgPool,
    snapshots: &[OrderBookSnapshot],
) -> Result<(), DbError> {
    for chunk in snapshots.chunks(INSERT_CHUNK) {
        let mut query = QueryBuilder::<sqlx::Postgres>::new(
            "INSERT INTO orderbook_snapshots (symbol, ts, bids, asks) ",
        );

        query.push_values(chunk, |mut row, s| {
            let bids = serde_json::to_value(&s.bids).unwrap_or(serde_json::Value::Array(vec![]));
            let asks = serde_json::to_value(&s.asks).unwrap_or(serde_json::Value::Array(vec![]));
            row.push_bind(s.symbol.as_str())
                .push_bind(ns_to_dt(s.timestamp))
                .push_bind(bids)
                .push_bind(asks);
        });

        query.push(" ON CONFLICT (symbol, ts) DO NOTHING");
        query.build().execute(pool).await?;
    }
    Ok(())
}

/// Load candles for `[from_ns, to_ns)` ordered by time.
///
/// # Errors
/// Returns [`DbError::Pool`] on query failure, or [`DbError::InvalidConfig`] if
/// a stored timeframe string is unrecognized.
pub async fn load_candles(
    pool: &PgPool,
    symbol: &str,
    timeframe: Timeframe,
    from_ns: i64,
    to_ns: i64,
) -> Result<Vec<Candle>, DbError> {
    let rows = sqlx::query(
        "SELECT symbol, timeframe, open_time, open, high, low, close, volume, buy_volume, sell_volume
         FROM candles
         WHERE symbol = $1 AND timeframe = $2 AND open_time >= $3 AND open_time < $4
         ORDER BY open_time",
    )
    .bind(symbol)
    .bind(timeframe.to_string())
    .bind(ns_to_dt(from_ns))
    .bind(ns_to_dt(to_ns))
    .fetch_all(pool)
    .await?;

    rows.iter().map(row_to_candle).collect()
}

/// Load trades for `[from_ns, to_ns)` ordered by time.
///
/// # Errors
/// Returns [`DbError::Pool`] on query failure.
pub async fn load_trades(
    pool: &PgPool,
    symbol: &str,
    from_ns: i64,
    to_ns: i64,
) -> Result<Vec<Trade>, DbError> {
    let rows = sqlx::query(
        "SELECT symbol, trade_id, price, quantity, is_buyer_maker, ts
         FROM trades
         WHERE symbol = $1 AND ts >= $2 AND ts < $3
         ORDER BY ts",
    )
    .bind(symbol)
    .bind(ns_to_dt(from_ns))
    .bind(ns_to_dt(to_ns))
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| {
            Ok(Trade {
                symbol: row.try_get("symbol")?,
                trade_id: i64::try_into(row.try_get::<i64, _>("trade_id")?).unwrap_or_default(),
                price: row.try_get("price")?,
                quantity: row.try_get("quantity")?,
                is_buyer_maker: row.try_get("is_buyer_maker")?,
                timestamp: dt_to_ns(row.try_get("ts")?),
            })
        })
        .collect()
}

/// Earliest and latest candle time stored for a symbol/timeframe.
///
/// Used by gap detection: the collector compares this against what it expects.
///
/// # Errors
/// Returns [`DbError::Pool`] on query failure.
pub async fn candles_range(
    pool: &PgPool,
    symbol: &str,
    timeframe: Timeframe,
) -> Result<Option<(i64, i64)>, DbError> {
    let row = sqlx::query(
        "SELECT MIN(open_time) AS lo, MAX(open_time) AS hi
         FROM candles WHERE symbol = $1 AND timeframe = $2",
    )
    .bind(symbol)
    .bind(timeframe.to_string())
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else { return Ok(None) };
    let lo: Option<DateTime<Utc>> = row.try_get("lo")?;
    let hi: Option<DateTime<Utc>> = row.try_get("hi")?;

    Ok(lo.and_then(|l| hi.map(|h| (dt_to_ns(l), dt_to_ns(h)))))
}

/// The span of stored trades for a symbol, if there are any.
///
/// Exists for the footprint's 404. "No trades in this window" is a dead end;
/// "no trades in this window, and you have 2026-09-10 00:00 to 06:00" is an
/// instruction -- and a footprint is the one chart where the user has to choose
/// the window deliberately, because trades are backfilled in capped chunks.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn trades_range(pool: &PgPool, symbol: &str) -> Result<Option<(i64, i64)>, DbError> {
    let row = sqlx::query("SELECT MIN(ts) AS lo, MAX(ts) AS hi FROM trades WHERE symbol = $1")
        .bind(symbol)
        .fetch_optional(pool)
        .await?;

    let Some(row) = row else { return Ok(None) };
    let lo: Option<DateTime<Utc>> = row.try_get("lo")?;
    let hi: Option<DateTime<Utc>> = row.try_get("hi")?;

    Ok(lo.and_then(|l| hi.map(|h| (dt_to_ns(l), dt_to_ns(h)))))
}

fn row_to_candle(row: &sqlx::postgres::PgRow) -> Result<Candle, DbError> {
    let timeframe: String = row.try_get("timeframe")?;
    let timeframe = Timeframe::from_str(&timeframe).map_err(|_| {
        DbError::InvalidConfig(format!("unknown timeframe `{timeframe}` stored in candles"))
    })?;

    Ok(Candle {
        symbol: row.try_get("symbol")?,
        timeframe,
        open_time: dt_to_ns(row.try_get("open_time")?),
        open: row.try_get("open")?,
        high: row.try_get("high")?,
        low: row.try_get("low")?,
        close: row.try_get("close")?,
        volume: row.try_get("volume")?,
        buy_volume: row.try_get("buy_volume")?,
        sell_volume: row.try_get("sell_volume")?,
    })
}

/// Re-exported so callers can build levels when reading snapshots back.
#[must_use]
pub fn levels_from_json(value: &serde_json::Value) -> Vec<OrderBookLevel> {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nanos_round_trip_through_datetime() {
        let ns: i64 = 1_672_515_782_136_000_000;
        assert_eq!(dt_to_ns(ns_to_dt(ns)), ns);
    }

    #[test]
    fn epoch_is_handled() {
        assert_eq!(dt_to_ns(ns_to_dt(0)), 0);
    }

    #[test]
    fn negative_timestamps_round_trip() {
        // Pre-1970 sanity check for div_euclid/rem_euclid correctness.
        let ns: i64 = -1_500_000_000;
        assert_eq!(dt_to_ns(ns_to_dt(ns)), ns);
    }

    #[test]
    fn timeframe_strings_round_trip() {
        for tf in Timeframe::all() {
            assert_eq!(Timeframe::from_str(&tf.to_string()).unwrap(), *tf);
        }
    }

    #[test]
    fn levels_deserialize_from_stored_json() {
        let json = serde_json::json!([{ "price": 100.0, "quantity": 2.0 }]);
        let levels = levels_from_json(&json);
        assert_eq!(levels.len(), 1);
        assert!((levels[0].price - 100.0).abs() < f64::EPSILON);
    }
}
