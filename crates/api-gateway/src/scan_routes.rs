//! `/scan` -- rank many symbols by one measurement.
//!
//! ## What this route is for
//!
//! Every other market endpoint answers about **one symbol you already chose**.
//! That is backwards for the question a trader actually starts with: *which*
//! symbol? `/scan` is the entry point that precedes the chart -- it measures a
//! set of instruments on one timeframe and returns them ordered, so the user
//! picks from evidence instead of from a list they have to guess at.
//!
//! ## It fetches, measures, and forgets
//!
//! A scan holds **no** market data and opens **no** feed -- [`market_data::scanner`]
//! visits each symbol behind a concurrency permit, takes the bars it needs,
//! reduces them to one number, and drops them. Peak footprint is
//! `concurrency * BARS_PER_SYMBOL` bars **regardless of how many symbols were
//! scanned**, so scanning 300 symbols costs the same memory as scanning 3.
//!
//! That is the only shape this could have taken. The platform's contract is that
//! market data is never persisted (`docs/04`): the database is a 6 GB free tier
//! against every symbol of every market, so a scan that wrote its bars would be
//! a write path that grows without bound, and a scan that kept them in RAM would
//! be the history buffer's budget spent on symbols nobody is looking at.
//!
//! ## Reading through `WindowService` is what makes it affordable
//!
//! The scan does not call the venue directly. It reads through the same
//! [`market_data::WindowService`] the chart and the agent read through, which
//! means a symbol already on a chart is answered **from RAM** and spends no REST
//! budget. The venue's limit is per IP and shared with every interactive chart,
//! so a scan that bypassed the buffer would make opening a chart slower every
//! time someone ran one.
//!
//! ## The universe defaults to the venue, not to `MARKET_SYMBOLS`
//!
//! `MARKET_SYMBOLS` is a *watchlist* -- "keep these feeds warm" -- and using it
//! as the set of things that exist was a defect this repo already fixed once
//! (see `market_routes::watchlist`). A scan defaulting to it would answer about
//! three symbols while the venue trades eight hundred, and the user would have
//! no way to tell that from a quiet market. The default universe is the venue's
//! indexed instruments; `symbols=` narrows it explicitly.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use analytics_core::types::Timeframe;
use market_data::scanner::{self, ScanMetric};

use crate::AppState;
use crate::error::ApiError;
use crate::extract::ApiQuery;

/// Which measurement to rank by, when the caller does not say.
///
/// RSI because it is the one a user can verify against the chart they are about
/// to open, and the default of a ranking should be the claim that is easiest to
/// check.
const DEFAULT_METRIC: ScanMetric = ScanMetric::Rsi;

/// The resolution a scan runs on when the caller does not say.
///
/// `1h` in the middle: `1m` is noise that ranks by the last few minutes, `1d`
/// is a hundred bars of nothing on a young listing, and 300 bars of `1h` is a
/// fortnight -- long enough that the ranking is about the instrument rather than
/// about today.
const DEFAULT_TIMEFRAME: Timeframe = Timeframe::H1;

/// Most symbols one request may name explicitly.
///
/// The same ceiling [`market_data::scanner`] enforces, restated here because a
/// caller who sends a longer list should be told *at the route* rather than
/// discovering from `requested` that most of what they asked for was ignored.
/// The scan truncating is a safety net, not the interface.
const MAX_REQUESTED_SYMBOLS: usize = scanner::MAX_SCAN_SYMBOLS;

/// How many instruments the default universe will take.
///
/// A venue lists hundreds of instruments and most of them are not worth ranking
/// -- a scan of everything is minutes of venue traffic for a list a human reads
/// the top of. The universe is taken in the venue's own (alphabetical) order so
/// the default is at least stable, and the response always says how many were
/// scanned against how many exist so a partial universe is never silent.
const DEFAULT_UNIVERSE: usize = 50;

// The relationship between those two, checked at *compile* time.
//
// This was a `#[test]` asserting two constants, which is a guard that cannot
// fail at run time -- clippy's `assertions_on_constants` is right about that.
// Moving it here is strictly stronger: a raised default is now a build error on
// every `cargo build`, not a red test in a suite someone may not run. The two
// failures it names are real ones -- a default above the ceiling would make the
// default request a 400, and a ceiling in the thousands would let one scan
// hammer a venue.
const _: () = assert!(DEFAULT_UNIVERSE <= MAX_REQUESTED_SYMBOLS);
const _: () = assert!(
    MAX_REQUESTED_SYMBOLS <= 500,
    "the ceiling is meant to bound venue traffic"
);

/// Query for `GET /scan`.
#[derive(Debug, Deserialize)]
pub struct ScanQuery {
    /// Which measurement to rank by: `rsi`, `atr_percent`, `change_percent`.
    ///
    /// Optional, with [`DEFAULT_METRIC`] as the default. Parsed rather than
    /// matched here so the refusal message names the alternatives.
    #[serde(default)]
    pub metric: Option<String>,
    /// Resolution, e.g. `1h`. Defaults to [`DEFAULT_TIMEFRAME`].
    #[serde(default)]
    pub timeframe: Option<String>,
    /// Symbols to scan, comma-separated. Absent means the venue's universe.
    ///
    /// Present-but-empty (`symbols=`) is treated as absent, because that is what
    /// a shell with an unset field sends and answering "you asked for nothing"
    /// would be pedantic about a request that plainly means "everything".
    #[serde(default)]
    pub symbols: Option<String>,
    /// How many symbols the *default* universe may contain.
    ///
    /// Ignored when `symbols` is given: an explicit list is the user's decision
    /// and is bounded only by [`MAX_REQUESTED_SYMBOLS`]. Capped at that ceiling
    /// so this cannot be used to sidestep it.
    #[serde(default)]
    pub universe: Option<usize>,
}

/// One ranked instrument, as a client sees it.
///
/// Mirrors [`market_data::scanner::ScanRow`] but is its own type: the scanner's
/// row is an internal shape shared with the AI's tool result, and a rename there
/// should not silently change a route's contract or vice versa.
#[derive(Debug, Serialize)]
pub struct ScanRowResponse {
    /// The instrument.
    pub symbol: String,
    /// The measured value, or `null` when it could not be measured.
    pub value: Option<f64>,
    /// When the measurement is from, in unix **milliseconds**.
    ///
    /// Without this a ranking reads as "now" -- and on a thin listing whose
    /// newest bar is an hour old, that is false in a way the user cannot see.
    pub as_of_ms: Option<i64>,
    /// How many bars the measurement was taken over.
    pub bars: usize,
    /// Why this symbol is not in the ranking, when it is not.
    pub error: Option<String>,
}

/// The answer to `GET /scan`.
#[derive(Debug, Serialize)]
pub struct ScanResponse {
    /// What was ranked.
    pub metric: String,
    /// The resolution every symbol was measured on.
    pub timeframe: String,
    /// Measured rows, best first.
    pub rows: Vec<ScanRowResponse>,
    /// Symbols that could not be measured, with the reason.
    ///
    /// A separate list rather than null-valued rows, because "we could not
    /// measure this" and "this ranked last" are different claims and a client
    /// rendering the second when the first is true is stating something false.
    pub failures: Vec<ScanRowResponse>,
    /// Symbols dropped for exceeding the ceiling, unnamed.
    pub skipped: usize,
    /// How many symbols the caller asked about.
    pub requested: usize,
    /// Whether `rows` covers everything `requested`.
    pub complete: bool,
    /// The sentence to show a user, with the consequences named.
    pub summary: String,
    /// Where the universe came from: `explicit` or `venue`.
    ///
    /// Part of the contract because the two mean different things. A venue
    /// universe is "the top 50 of everything listed"; an explicit one is "these
    /// 50 you named". A client that could not tell them apart would present a
    /// narrow scan as a market-wide one.
    pub universe: String,
    /// How many instruments the venue has indexed, when the universe was the
    /// venue's. `null` for an explicit list.
    pub universe_size: Option<usize>,
    /// A caution, when there is one worth giving.
    pub note: Option<String>,
}

/// `GET /scan`
///
/// # Errors
/// [`ApiError::bad_request`] when `metric` or `timeframe` is not one this
/// platform knows, or when an explicit `symbols` list exceeds the ceiling. A
/// symbol that merely fails to fetch is **not** an error: it is a row in
/// `failures`, because one thin listing must not cost the user their ranking.
pub async fn scan(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<ScanQuery>,
) -> Result<Json<ScanResponse>, ApiError> {
    let metric = match query.metric.as_deref().map(str::trim) {
        None | Some("") => DEFAULT_METRIC,
        Some(raw) => raw.parse::<ScanMetric>().map_err(|reason| {
            ApiError::bad_request("unknown_metric", reason).with_details(serde_json::json!({
                "metric": raw,
                "known": ScanMetric::ALL.iter().map(|m| m.name()).collect::<Vec<_>>(),
            }))
        })?,
    };

    let timeframe = match query.timeframe.as_deref().map(str::trim) {
        None | Some("") => DEFAULT_TIMEFRAME,
        Some(raw) => raw.parse::<Timeframe>().map_err(|_| {
            ApiError::bad_request(
                "unknown_timeframe",
                format!("`{raw}` is not a timeframe this platform supports"),
            )
            .with_details(serde_json::json!({
                "timeframe": raw,
                "known": Timeframe::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
            }))
        })?,
    };

    let (symbols, universe, universe_size, note) =
        resolve_universe(&state, query.symbols.as_deref(), query.universe).await?;

    if symbols.is_empty() {
        return Err(ApiError::unavailable(
            "there is nothing to scan: the venue's instrument index is empty and no symbols were given",
        )
        .with_details(serde_json::json!({ "metric": metric.name() })));
    }

    // The scanner spawns a task per symbol, so it needs an owned handle. The
    // `Arc` is built here, once, from the state's cloneable service -- the
    // clone is two `Arc` bumps and a pooling client, so this is not a copy of
    // anything that matters.
    let windows = std::sync::Arc::new(state.windows.clone());
    let result = scanner::scan(&windows, &symbols, timeframe, metric, crate::now_ns()).await;

    Ok(Json(ScanResponse {
        metric: metric.name().to_string(),
        timeframe: timeframe.as_str().to_string(),
        rows: result.rows.iter().map(row_response).collect(),
        failures: result.failures.iter().map(row_response).collect(),
        skipped: result.skipped.len(),
        requested: result.requested,
        complete: result.is_complete(),
        summary: result.summary(),
        universe,
        universe_size,
        note,
    }))
}

/// Decide which symbols to scan, and describe where they came from.
///
/// `async` for one reason: the venue-universe branch refreshes the instrument
/// index before reading it. The explicit branch does not touch the venue at all,
/// so naming `symbols=` is also the cheaper request.
///
/// # Errors
/// [`ApiError::bad_request`] when an explicit list is longer than the ceiling.
async fn resolve_universe(
    state: &AppState,
    requested: Option<&str>,
    universe: Option<usize>,
) -> Result<(Vec<String>, String, Option<usize>, Option<String>), ApiError> {
    // An explicit list wins outright -- including one shorter than the default
    // universe, which is the whole point of naming symbols.
    if let Some(raw) = requested {
        let symbols = parse_symbols(raw);
        if !symbols.is_empty() {
            if symbols.len() > MAX_REQUESTED_SYMBOLS {
                return Err(ApiError::bad_request(
                    "too_many_symbols",
                    format!(
                        "{} symbols were named and one scan measures at most {MAX_REQUESTED_SYMBOLS}",
                        symbols.len()
                    ),
                )
                .with_details(serde_json::json!({
                    "requested": symbols.len(),
                    "ceiling": MAX_REQUESTED_SYMBOLS,
                })));
            }
            return Ok((symbols, "explicit".to_string(), None, None));
        }
    }

    // The venue's universe. Refreshed best-effort first, exactly as the search
    // route does: a scan should answer from the copy already held rather than
    // fail because the venue is briefly unreachable.
    state.symbols.refresh_if_stale(&state.backfill).await;

    let take = universe
        .unwrap_or(DEFAULT_UNIVERSE)
        .clamp(1, MAX_REQUESTED_SYMBOLS);
    let indexed = state.symbols.all();
    let total = indexed.len();

    // Only instruments the venue will actually trade. A ranking that included a
    // halted instrument would put a symbol at the top of the list that the user
    // cannot act on, which is worse than leaving it out -- and it is not a
    // silent filter, it is the definition of a tradeable universe.
    let symbols: Vec<String> = indexed
        .into_iter()
        .filter(|instrument| instrument.trading)
        .map(|instrument| instrument.symbol)
        .take(take)
        .collect();

    let note = (symbols.len() < total).then(|| {
        format!(
            "ranked {} of {total} instruments the venue lists; pass `symbols=` to choose your own or `universe=` to widen this",
            symbols.len()
        )
    });

    Ok((symbols, "venue".to_string(), Some(total), note))
}

/// Split a comma-separated symbol list, uppercased and de-duplicated in order.
///
/// Duplicates are dropped rather than passed through: two copies of `BTCUSDT`
/// would occupy two concurrency permits and two rows in the ranking, so the
/// user would see a symbol listed twice and reasonably conclude the scan is
/// broken.
fn parse_symbols(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(',') {
        let symbol = part.trim().to_uppercase();
        if !symbol.is_empty() && !out.contains(&symbol) {
            out.push(symbol);
        }
    }
    out
}

/// Convert the scanner's row into the wire shape.
fn row_response(row: &scanner::ScanRow) -> ScanRowResponse {
    ScanRowResponse {
        symbol: row.symbol.clone(),
        value: row.value,
        as_of_ms: row.as_of_ns.map(|ns| ns / 1_000_000),
        bars: row.bars,
        error: row.error.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comma_separated_list_is_normalised_and_deduplicated() {
        // The same symbol twice would take two permits and appear twice in the
        // ranking -- which reads as the scan being broken, not as a duplicate
        // input being tolerated.
        let parsed = parse_symbols(" btcusdt, ETHUSDT ,btcusdt, ,SOLUSDT ");
        assert_eq!(parsed, vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
    }

    #[test]
    fn an_empty_list_parses_to_nothing_so_the_route_can_fall_back_to_the_venue() {
        // `symbols=` is what a shell with an unset field sends. It means
        // "everything", not "nothing".
        assert!(parse_symbols("   ").is_empty());
        assert!(parse_symbols(",,,").is_empty());
    }

    #[test]
    fn the_default_metric_is_the_one_a_user_can_check_by_eye() {
        // RSI on the chart they are about to open. A ranking's default should be
        // its most verifiable claim, because that is the one they will test.
        assert_eq!(DEFAULT_METRIC, ScanMetric::Rsi);
    }

    #[test]
    fn the_default_timeframe_is_neither_noise_nor_an_empty_history() {
        // 300 bars of 1h is a fortnight: long enough that the ranking is about
        // the instrument rather than about today, short enough that a young
        // listing has the bars to be measured at all.
        assert_eq!(DEFAULT_TIMEFRAME, Timeframe::H1);
        assert_eq!(DEFAULT_TIMEFRAME.nanos() * 300, 300 * 3_600_000_000_000);
    }

    #[test]
    fn a_row_reports_its_timestamp_in_milliseconds() {
        // The wire is milliseconds everywhere a client sees a time; nanoseconds
        // are the engine's unit. A swap here is a factor of a million, which
        // renders as a date in 1970 rather than as an error.
        let row = scanner::ScanRow::measured("BTCUSDT", 71.5, 1_700_000_000_000_000_000, 300);
        let response = row_response(&row);
        assert_eq!(response.as_of_ms, Some(1_700_000_000_000));
        assert_eq!(response.symbol, "BTCUSDT");
        assert_eq!(response.value, Some(71.5));
        assert!(response.error.is_none());
    }

    #[test]
    fn a_failed_row_carries_its_reason_and_no_value() {
        let row = scanner::ScanRow::failed("THINUSDT", "no data");
        let response = row_response(&row);
        assert_eq!(response.value, None);
        assert_eq!(response.error.as_deref(), Some("no data"));
    }

    #[test]
    fn the_response_wire_shape_is_pinned() {
        // Three consumers: the shell's table, the AI's tool result, and any
        // script over the API. A rename is not a compile error anywhere -- it is
        // a column that renders blank.
        let response = ScanResponse {
            metric: "rsi".into(),
            timeframe: "1h".into(),
            rows: vec![row_response(&scanner::ScanRow::measured(
                "BTCUSDT", 71.5, 1, 300,
            ))],
            failures: vec![],
            skipped: 0,
            requested: 1,
            complete: true,
            summary: "1 symbol ranked by RSI on 1h".into(),
            universe: "explicit".into(),
            universe_size: None,
            note: None,
        };
        let json = serde_json::to_value(&response).expect("serializes");
        let object = json.as_object().expect("an object");

        for key in [
            "metric",
            "timeframe",
            "rows",
            "failures",
            "skipped",
            "requested",
            "complete",
            "summary",
            "universe",
            "universe_size",
            "note",
        ] {
            assert!(object.contains_key(key), "the wire lost `{key}`: {json}");
        }

        let row = json["rows"][0].as_object().expect("a row object");
        for key in ["symbol", "value", "as_of_ms", "bars", "error"] {
            assert!(row.contains_key(key), "the row lost `{key}`: {json}");
        }
    }

    #[test]
    fn a_metric_is_named_on_the_wire_in_its_own_vocabulary() {
        // The shell switches on this string to label the ranking's column, and
        // the parser accepts exactly it -- so a drift between the two is a
        // column headed with a word nothing recognises.
        for metric in ScanMetric::ALL {
            let name = metric.name();
            assert_eq!(
                name.parse::<ScanMetric>(),
                Ok(metric),
                "{name} does not round-trip"
            );
        }
    }
}
