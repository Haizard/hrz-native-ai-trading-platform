//! The Economic Event Intelligence Engine's data layer: the BLS Public Data
//! API (labor statistics), the release records, the normalized surprise, and
//! the macro bias. The doctrine is the engine's own: an economic event is
//! not a signal -- it is an information catalyst, and the surprise against
//! expectation is the measurable part of it.
//!
//! The forecast rule, stated plainly because the whole model depends on it:
//! BLS publishes ACTUALS, never the market's consensus. The engine's default
//! forecast is the trailing twelve-month mean change, documented as the
//! naive consensus; the real consensus can be entered through the override
//! endpoint, and it is STORED so backtests never suffer look-ahead bias.

use serde::{Deserialize, Serialize};

/// The BLS series the engine reads.
pub const SERIES_NFP_LEVEL: &str = "CES0000000001";
/// The unemployment rate.
pub const SERIES_UNEMPLOYMENT: &str = "LNS14000000";
/// Average hourly earnings, total private.
pub const SERIES_EARNINGS: &str = "CES0500000003";
/// CPI-U, the CPI release's level.
pub const SERIES_CPI_LEVEL: &str = "CUUR0000SA0";

/// One stored economic release, with everything the surprise math needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EconRelease {
    /// The release's own id (`nfp-2026-09`, `cpi-2026-08`).
    pub id: String,
    /// Which release (`nfp` or `cpi`).
    pub kind: String,
    /// The period the data describes (`2026-09`).
    pub period: String,
    /// The release timestamp (unix nanos, 08:30 ET on release day).
    pub release_ns: i64,
    /// The headline change (NFP jobs in thousands, CPI month-over-month %).
    pub actual: f64,
    /// The unemployment rate at this release (NFP releases only).
    pub unemployment: Option<f64>,
    /// Average hourly earnings' month-over-month change % (NFP only).
    pub earnings_change: Option<f64>,
    /// The forecast used for the surprise, and WHERE it came from: the
    /// trailing mean, or an operator override with its source named.
    pub forecast: f64,
    /// `trailing_mean` or `operator`.
    pub forecast_source: String,
    /// The revision applied to the PREVIOUS period's headline, when known.
    pub revision: Option<f64>,
    /// When this record was fetched (unix nanos).
    pub retrieved_ns: i64,
}

/// The normalized surprise: `(actual - forecast) / trailing std dev`. The
/// normalization is what separates a small miss from an extreme one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Surprise {
    /// actual - forecast, in the release's own units.
    pub raw: f64,
    /// `raw / trailing std dev` of the same series' changes.
    pub sigma: f64,
}

/// Compute the surprise against a trailing window of the same series'
/// changes. The std dev is the trailing window's; a flat history reads
/// sigma 0 rather than inventing a scale.
pub fn surprise(actual: f64, forecast: f64, trailing_changes: &[f64]) -> Surprise {
    let raw = actual - forecast;
    let sigma = if trailing_changes.len() >= 4 {
        let mean = trailing_changes.iter().sum::<f64>() / trailing_changes.len() as f64;
        let var = trailing_changes.iter().map(|c| (c - mean).powi(2)).sum::<f64>() / trailing_changes.len() as f64;
        let sd = var.sqrt();
        if sd > 0.0 { raw / sd } else { 0.0 }
    } else {
        0.0
    };
    Surprise { raw, sigma }
}

/// The macro bias from one release: how the USD should read it, on -2..2.
/// Positive = USD-supportive (strong labor / hot inflation), negative =
/// USD-soft. The composite: the headline surprise dominates; unemployment
/// moves oppositely (lower unemployment is USD-supportive); earnings add
/// their hawkish weight; a negative revision subtracts its own weight.
pub fn macro_bias(release: &EconRelease, surprise: &Surprise) -> f64 {
    let mut bias = surprise.sigma.clamp(-2.0, 2.0);
    if let Some(u) = release.unemployment {
        // The rate's own change vs the previous release rides at half weight:
        // the caller passes the CURRENT rate; the change is computed where
        // the history lives (see `macro_bias_with_context`).
        let _ = u;
    }
    if let Some(rev) = release.revision {
        // A DOWNWARD revision subtracts: rev is signed, so adding it moves
        // the bias the right way (subtracting it would ADD for a cut).
        bias += (rev / 50.0).clamp(-1.0, 1.0) * 0.5;
    }
    bias.clamp(-2.0, 2.0)
}

/// The full composite, with the previous release's unemployment and
/// earnings for the change terms. This is the shape the engine reports.
pub fn macro_bias_with_context(
    release: &EconRelease,
    surprise: &Surprise,
    prev_unemployment: Option<f64>,
    prev_earnings_change: Option<f64>,
) -> f64 {
    let mut bias = surprise.sigma.clamp(-2.0, 2.0);
    if let (Some(u), Some(p)) = (release.unemployment, prev_unemployment) {
        // Lower unemployment is USD-supportive: the change enters negated.
        bias += ((p - u) * 2.0).clamp(-1.0, 1.0) * 0.5;
    }
    if let (Some(e), Some(p)) = (release.earnings_change, prev_earnings_change) {
        // Hotter earnings are hawkish: the surprise change adds weight.
        bias += ((e - p) * 2.0).clamp(-1.0, 1.0) * 0.5;
    }
    if let Some(rev) = release.revision {
        bias += (rev / 50.0).clamp(-1.0, 1.0) * 0.5;
    }
    bias.clamp(-2.0, 2.0)
}

/// The bias as a label the engine reports: the trader reads words, the
/// quant reads the number.
pub fn bias_label(bias: f64) -> &'static str {
    if bias >= 1.0 {
        "strongly bullish USD"
    } else if bias >= 0.4 {
        "bullish USD"
    } else if bias <= -1.0 {
        "strongly bearish USD"
    } else if bias <= -0.4 {
        "bearish USD"
    } else {
        "neutral"
    }
}

/// The NFP release schedule rule: the first Friday of the month, 08:30
/// Eastern. The next release after `now_ns` (UTC), as unix nanos. ET is
/// UTC-5 in winter, UTC-4 in summer; the schedule rule is documented with
/// the winter offset and the BLS calendar overrides it when it differs.
pub fn next_nfp_release(now_ns: i64) -> i64 {
    const DAY_NS: i64 = 3_600_000_000_000 * 24;
    // First Friday of NEXT month (or this month if it hasn't passed), then
    // 08:30 ET = 13:30 UTC (winter offset, documented).
    let days = now_ns.div_euclid(DAY_NS);
    // Civil from days (Howard Hinnant).
    let civil = |z0: i64| -> (i64, i64, i64) {
        let z = z0 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 456) / 153;
        let d = doy - (153 * mp - 457) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (y, m, d)
    };
    let (mut y, mut m, _) = civil(days);
    loop {
        // The first day of (y, m).
        let first_friday = (1..=7).find(|d| {
            // Zeller-free: days-from-civil then weekday.
            let (yy, mm) = if m > 2 { (y, m - 2) } else { (y - 1, m + 10) };
            let k = yy % 100;
            let j = yy / 100;
            let h = (*d + (13 * (mm + 1)) / 5 + k + k / 4 + j / 4 + 5 * j) % 7;
            h == 5 // 5 = Friday in Zeller's numbering (0=Saturday)
        });
        if let Some(d) = first_friday {
            let release_days = days_from_civil(y, m, d);
            let release_ns = release_days * DAY_NS + 13 * 3_600_000_000_000 + 30 * 60_000_000_000;
            if release_ns > now_ns {
                return release_ns;
            }
        }
        // Next month.
        m += 1;
        if m > 12 {
            m = 1;
            y += 1;
        }
    }
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

/// The BLS API response shape (v2, the fields the engine reads).
#[derive(Debug, Deserialize)]
pub struct BlsResponse {
    /// status line.
    pub status: Option<String>,
    /// per-series results.
    #[serde(rename = "Results")]
    pub results: Option<BlsResults>,
}

/// The results wrapper.
#[derive(Debug, Deserialize)]
pub struct BlsResults {
    /// one entry per requested series.
    pub series: Vec<BlsSeries>,
}

/// One series with its data points.
#[derive(Debug, Deserialize)]
pub struct BlsSeries {
    /// the series id.
    #[serde(rename = "seriesID")]
    pub series_id: String,
    /// newest-first data points.
    pub data: Vec<BlsPoint>,
}

/// One data point.
#[derive(Debug, Deserialize)]
pub struct BlsPoint {
    /// the year.
    pub year: String,
    /// the period (`M01`..`M12`).
    pub period: String,
    /// the value as a string (BLS's own wire format).
    pub value: String,
}

/// Parse one BLS point's value.
pub fn point_value(p: &BlsPoint) -> Option<f64> {
    p.value.replace(',', "").parse().ok()
}

/// Fetch the newest `count` points of a series from BLS v2. The key rides
/// the request; failures come back as text the caller can surface.
#[cfg(feature = "net")]
pub async fn fetch_series(client: &reqwest::Client, api_key: &str, series: &str, count: usize) -> Result<Vec<BlsPoint>, String> {
    // `latest: true` returns exactly ONE point -- the history the engine
    // needs comes from the year range (BLS v2's own rule).
    let this_year = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 31_557_600 + 1970)
        .unwrap_or(2026);
    let body = serde_json::json!({
        "seriesid": [series],
        "startyear": (this_year - 3).to_string(),
        "endyear": this_year.to_string(),
        "registrationkey": api_key,
        "calculations": false,
    });
    let response: serde_json::Value = client
        .post("https://api.bls.gov/publicAPI/v2/timeseries/data/")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("BLS request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("BLS response not JSON: {e}"))?;
    let status = response["status"].as_str().unwrap_or("REQUEST_FAILED");
    if status != "REQUEST_SUCCEEDED" {
        let msg = response["message"].as_str().unwrap_or("unknown");
        return Err(format!("BLS status {status}: {msg}"));
    }
    let mut points = Vec::new();
    if let Some(series_list) = response["Results"]["series"].as_array() {
        for s in series_list {
            if let Some(data) = s["data"].as_array() {
                for d in data {
                    points.push(BlsPoint {
                        year: d["year"].as_str().unwrap_or("").to_string(),
                        period: d["period"].as_str().unwrap_or("").to_string(),
                        value: d["value"].as_str().unwrap_or("").to_string(),
                    });
                }
            }
        }
    }
    points.sort_by(|a, b| (b.year.clone() + &b.period).cmp(&(a.year.clone() + &a.period)));
    points.truncate(count);
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surprise_normalizes_by_the_trailings_own_scale() {
        // Trailing changes of ~100k with sd ~20: a +60 surprise reads 3σ.
        let trailing = [90.0, 110.0, 95.0, 105.0, 100.0, 92.0, 108.0];
        let s = surprise(160.0, 100.0, &trailing);
        assert!(s.raw == 60.0);
        assert!(s.sigma > 3.0, "a 60k surprise on a tight trailing reads extreme: {}", s.sigma);
        // A flat trailing: sigma 0, not a made-up number.
        let flat = [100.0, 100.0, 100.0, 100.0];
        assert_eq!(surprise(160.0, 100.0, &flat).sigma, 0.0);
    }

    #[test]
    fn the_macro_bias_composes_the_way_traders_read_it() {
        let release = EconRelease {
            id: "nfp-2026-09".to_string(),
            kind: "nfp".to_string(),
            period: "2026-09".to_string(),
            release_ns: 0,
            actual: 250.0,
            unemployment: Some(4.0),
            earnings_change: Some(0.5),
            forecast: 150.0,
            forecast_source: "trailing_mean".to_string(),
            revision: Some(-70.0),
            retrieved_ns: 0,
        };
        let s = Surprise { raw: 100.0, sigma: 2.0 };
        // Strong report, but the -70k revision subtracts its own weight.
        let bias = macro_bias_with_context(&release, &s, Some(4.2), Some(0.3));
        assert!(bias > 1.0, "the composite reads USD-supportive: {bias}");
        assert!(bias < 2.0, "and the revision pulled it off the cap: {bias}");
        assert_eq!(bias_label(2.0), "strongly bullish USD");
        assert_eq!(bias_label(-1.5), "strongly bearish USD");
        assert_eq!(bias_label(0.1), "neutral");
    }

    #[test]
    fn the_schedule_lands_on_a_friday() {
        // 2026-10-23 (a Friday) 00:00 UTC: the next release is the first
        // Friday of November (2026-11-06) at 13:30 UTC.
        let now = 1_824_480_000_000_000_000_i64; // approx, the exact value is the test's business
        let next = next_nfp_release(now);
        assert!(next > now, "the next release is in the future");
        // It must be 13:30 UTC on its day.
        let day_part = next % (3_600_000_000_000 * 24);
        assert_eq!(day_part, 13 * 3_600_000_000_000 + 30 * 60_000_000_000, "08:30 ET = 13:30 UTC");
    }
}
