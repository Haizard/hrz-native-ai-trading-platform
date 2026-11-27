//! Run the multiscale validation harness on a real candle history.
//!
//! Usage:
//!   cargo run -p analytics-core --example validate_history -- history.json
//!
//! The JSON file is `{ "symbol": "BTCUSDT", "series": [
//! { "timeframe": "1h", "candles": [ ...Candle... ] }, ... ] }`. Candle JSON is
//! exactly what `GET /candles` returns, so a history can be captured from the
//! gateway without translating fields.
//!
//! This example prints the report as JSON. It deliberately does not decide a
//! winner: whether a hierarchy helps is a research conclusion, not a CLI flag.

use std::path::PathBuf;

use analytics_core::validation::{compare_hierarchies, ValidationConfig};
use analytics_core::{Candle, Timeframe};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Series {
    timeframe: Timeframe,
    candles: Vec<Candle>,
}

#[derive(Debug, Deserialize)]
struct History {
    symbol: String,
    series: Vec<Series>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("provide the path to a captured candle history JSON file")?,
    );
    let text = std::fs::read_to_string(&path)?;
    let history: History = serde_json::from_str(&text)?;

    let ladder: Vec<Timeframe> = history.series.iter().map(|s| s.timeframe).collect();
    let series: Vec<(Timeframe, Vec<Candle>)> = history
        .series
        .into_iter()
        .map(|s| (s.timeframe, s.candles))
        .collect();
    let finest = *ladder.iter().min().ok_or("no timeframes in the history")?;
    let config = ValidationConfig {
        horizon_bars: 6,
        neutral_threshold: 0.002,
        min_completed: 30,
        train_fraction: 0.6,
        embargo_bars: 6,
    };
    let discipline = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "bayesian".to_string());
    let report = compare_hierarchies(&series, &ladder, &discipline, &config)?;

    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "symbol": history.symbol,
        "finest": finest.as_str(),
        "ladder": ladder.iter().map(|timeframe| timeframe.as_str()).collect::<Vec<_>>(),
        "report": report,
    }))?);
    Ok(())
}
