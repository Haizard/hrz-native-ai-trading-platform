//! The FRED data layer (Federal Reserve Bank of St. Louis): the Fed's own
//! numbers -- the funds target (the FOMC's decision as data), the yield
//! curve, the broad dollar, PCE inflation, and weekly claims. Same doctrine
//! as the BLS engine: these are catalysts, and the surprise against
//! expectation is the measurable part.

use serde::Deserialize;

/// The FRED series the engine reads.
pub const SERIES_FUNDS_TARGET: &str = "DFEDTARU";
/// The 10-year Treasury yield.
pub const SERIES_DGS10: &str = "DGS10";
/// The 2-year Treasury yield.
pub const SERIES_DGS2: &str = "DGS2";
/// The 10-year TIPS (real) yield.
pub const SERIES_DFII10: &str = "DFII10";
/// The broad dollar index.
pub const SERIES_DOLLAR: &str = "DTWEXBGS";
/// PCE inflation (the Fed's preferred gauge), level.
pub const SERIES_PCE_LEVEL: &str = "PCEPI";
/// Core PCE (less food and energy), level.
pub const SERIES_CORE_PCE_LEVEL: &str = "PCEPILFE";
/// Initial jobless claims (weekly).
pub const SERIES_CLAIMS: &str = "ICSA";
/// Real GDP (quarterly), level.
pub const SERIES_REAL_GDP: &str = "GDPC1";

/// One FRED observation.
#[derive(Debug, Clone, PartialEq)]
pub struct FredPoint {
    /// The observation date (`2026-10-01`).
    pub date: String,
    /// The value (parsed; `.` on the wire is missing).
    pub value: f64,
}

/// The FRED wire shape.
#[derive(Debug, Deserialize)]
struct FredResponse {
    /// The observations, oldest first when `sort_order=asc`.
    observations: Vec<FredObservation>,
}

#[derive(Debug, Deserialize)]
struct FredObservation {
    /// The date string.
    date: String,
    /// The value string (`.` when missing).
    value: String,
}

/// Fetch the newest `limit` observations of a series, newest-first.
#[cfg(feature = "net")]
pub async fn fetch_fred(client: &reqwest::Client, api_key: &str, series: &str, limit: usize) -> Result<Vec<FredPoint>, String> {
    let url = format!(
        "https://api.stlouisfed.org/fred/series/observations?series_id={series}&api_key={api_key}&file_type=json&sort_order=desc&limit={limit}"
    );
    let response: FredResponse = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("FRED request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("FRED response not JSON: {e}"))?;
    let mut points: Vec<FredPoint> = response
        .observations
        .into_iter()
        .filter_map(|o| {
            let value = o.value.parse().ok()?;
            Some(FredPoint { date: o.date, value })
        })
        .collect();
    points.sort_by(|a, b| b.date.cmp(&a.date));
    Ok(points)
}

/// Parse a FRED date (`2026-10-01`) to unix nanos (00:00 UTC).
pub fn date_ns(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    Some(days_from_civil(y, m, d) * 3_600_000_000_000 * 24)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The macro context from the Fed's own numbers: one composite read of
/// policy (the funds rate's own level and its recent direction), the curve
/// (2s10s, inversion is the recession watch), the real yield (the liquidity
/// headwind), and the dollar. Each term on -1..1; the composite is their
/// documented weight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MacroContext {
    /// The funds target's latest level.
    pub funds_rate: f64,
    /// Its change over the trailing year (the hiking/easing direction).
    pub funds_direction: f64,
    /// The 2s10s curve in points (negative = inverted).
    pub curve: f64,
    /// The 10-year real yield.
    pub real_yield: f64,
    /// The broad dollar's trailing-year change, percent.
    pub dollar_change_pct: f64,
    /// The composite: positive = tight/hawkish policy mix.
    pub composite: f64,
}

/// Compose the context from the series' tails.
pub fn macro_context(
    funds: &[FredPoint],
    dgs10: &[FredPoint],
    dgs2: &[FredPoint],
    dfii10: &[FredPoint],
    dollar: &[FredPoint],
) -> Option<MacroContext> {
    let funds_rate = funds.first()?.value;
    let funds_direction = {
        let year_ago = funds.get(250.min(funds.len().saturating_sub(1)))?;
        funds_rate - year_ago.value
    };
    let curve = dgs10.first()?.value - dgs2.first()?.value;
    let real_yield = dfii10.first()?.value;
    let dollar_change_pct = {
        let year_ago = dollar.get(250.min(dollar.len().saturating_sub(1)))?;
        if year_ago.value > 0.0 {
            (dollar.first()?.value / year_ago.value - 1.0) * 100.0
        } else {
            0.0
        }
    };
    // The composite: the direction of policy dominates, the curve's
    // inversion subtracts (a deeply inverted curve is a policy warning),
    // the real yield and dollar add their tightening weight, scaled.
    let composite = (funds_direction / 2.0).clamp(-1.0, 1.0) * 0.5
        + (curve / 2.0).clamp(-1.0, 1.0) * 0.2
        + ((real_yield - 1.0) / 2.0).clamp(-1.0, 1.0) * 0.15
        + (dollar_change_pct / 10.0).clamp(-1.0, 1.0) * 0.15;
    Some(MacroContext {
        funds_rate,
        funds_direction,
        curve,
        real_yield,
        dollar_change_pct,
        composite: composite.clamp(-1.0, 1.0),
    })
}

/// The context in words: the trader reads the label, the quant the number.
pub fn context_label(composite: f64) -> &'static str {
    if composite >= 0.4 {
        "tight and tightening"
    } else if composite >= 0.15 {
        "tight"
    } else if composite <= -0.4 {
        "loose and loosening"
    } else if composite <= -0.15 {
        "loose"
    } else {
        "neutral"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(date: &str, value: f64) -> FredPoint {
        FredPoint { date: date.to_string(), value }
    }

    #[test]
    fn the_context_composes_the_policy_mix() {
        // Hiking year: funds up 1.5, the curve inverted, real yields high,
        // the dollar strong -- the composite reads tight.
        let funds = vec![p("2026-10-01", 5.5), p("2025-10-01", 4.0)];
        let mut funds_tail = funds.clone();
        funds_tail.extend((0..249).map(|i| p("2025-09-01", 4.0 + i as f64 * 0.0)));
        let dgs10 = vec![p("2026-10-01", 4.2)];
        let dgs2 = vec![p("2026-10-01", 4.8)];
        let dfii10 = vec![p("2026-10-01", 2.1)];
        let mut dollar = vec![p("2026-10-01", 120.0)];
        dollar.extend((0..250).map(|_| p("2025-10-01", 100.0)));
        let ctx = macro_context(&funds_tail, &dgs10, &dgs2, &dfii10, &dollar).expect("context");
        assert!((ctx.curve - (-0.6)).abs() < 1e-9, "the curve inverts");
        assert!(ctx.funds_direction > 1.4, "the hiking direction");
        assert!(ctx.composite > 0.3, "the mix reads tight: {}", ctx.composite);
        assert_eq!(context_label(0.5), "tight and tightening");
        assert_eq!(context_label(-0.5), "loose and loosening");
    }

    #[test]
    fn the_date_parser_reads_the_wire() {
        let ns = date_ns("2026-10-01").expect("parses");
        // 2026-10-01 is a Thursday; the ns must be a whole UTC day.
        assert_eq!(ns % (3_600_000_000_000 * 24), 0);
        assert!(date_ns("not-a-date").is_none());
    }
}
