//! Deterministic options-positioning and dealer-hedging proxies.
//!
//! The research programme asks about options positioning and dealer behaviour.
//! This module consumes an explicitly supplied options chain — the Rust core
//! never fetches one — and reports closed-form proxies. Without a chain it
//! refuses to answer rather than inventing positioning.

use serde::{Deserialize, Serialize};

/// One strike's option quotes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionStrike {
    /// Strike price.
    pub strike: f64,
    /// Call open interest in contracts.
    pub call_open_interest: f64,
    /// Put open interest in contracts.
    pub put_open_interest: f64,
    /// Call traded volume in contracts.
    pub call_volume: f64,
    /// Put traded volume in contracts.
    pub put_volume: f64,
    /// Call implied volatility, when quoted.
    pub call_iv: Option<f64>,
    /// Put implied volatility, when quoted.
    pub put_iv: Option<f64>,
}

/// A chain at one expiry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionsChain {
    /// Underlying symbol.
    pub symbol: String,
    /// Expiry, unix nanoseconds.
    pub expiry_ns: i64,
    /// Current underlying price.
    pub spot: f64,
    /// Quotes by strike.
    pub strikes: Vec<OptionStrike>,
}

/// Positioning evidence from a chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositioningReport {
    /// Open-interest put/call ratio. Above 1 leans protective.
    pub open_interest_put_call: f64,
    /// Volume put/call ratio.
    pub volume_put_call: f64,
    /// Strike with the largest total open interest.
    pub max_pain_strike: Option<f64>,
    /// Put-minus-call implied volatility at the strike nearest spot.
    pub at_the_money_skew: Option<f64>,
    /// Net dealer gamma proxy in contracts, signed.
    pub dealer_gamma_proxy: f64,
    /// Estimated hedging direction from the gamma proxy.
    pub hedge_direction: f64,
    /// Explicit limitation of the proxies.
    pub limitation: String,
}

fn valid_strikes(chain: &OptionsChain) -> Vec<&OptionStrike> {
    chain
        .strikes
        .iter()
        .filter(|strike| {
            strike.strike.is_finite()
                && strike.strike > 0.0
                && strike.call_open_interest.is_finite()
                && strike.put_open_interest.is_finite()
        })
        .collect()
}

/// Derive positioning proxies from a supplied chain.
#[must_use]
pub fn analyze_positioning(chain: &OptionsChain) -> Option<PositioningReport> {
    if chain.spot <= 0.0 || !chain.spot.is_finite() {
        return None;
    }
    let strikes = valid_strikes(chain);
    if strikes.len() < 3 {
        return None;
    }
    let call_oi: f64 = strikes.iter().map(|s| s.call_open_interest.max(0.0)).sum();
    let put_oi: f64 = strikes.iter().map(|s| s.put_open_interest.max(0.0)).sum();
    let call_volume: f64 = strikes.iter().map(|s| s.call_volume.max(0.0)).sum();
    let put_volume: f64 = strikes.iter().map(|s| s.put_volume.max(0.0)).sum();
    if call_oi + put_oi <= 0.0 {
        return None;
    }
    let max_pain = strikes
        .iter()
        .max_by(|a, b| {
            let a_total = a.call_open_interest + a.put_open_interest;
            let b_total = b.call_open_interest + b.put_open_interest;
            a_total.total_cmp(&b_total)
        })
        .map(|strike| strike.strike);
    let atm = strikes.iter().min_by(|a, b| {
        (a.strike - chain.spot)
            .abs()
            .total_cmp(&(b.strike - chain.spot).abs())
    })?;
    let skew = match (atm.call_iv, atm.put_iv) {
        (Some(call), Some(put)) if call.is_finite() && put.is_finite() => Some(put - call),
        _ => None,
    };
    // Dealer gamma proxy: call open interest above spot and put open interest
    // below spot are the regions a dealer commonly hedges into. It is a shape
    // heuristic, not a real dealer book.
    let call_above: f64 = strikes
        .iter()
        .filter(|s| s.strike > chain.spot)
        .map(|s| s.call_open_interest.max(0.0))
        .sum();
    let put_below: f64 = strikes
        .iter()
        .filter(|s| s.strike < chain.spot)
        .map(|s| s.put_open_interest.max(0.0))
        .sum();
    let total = call_oi + put_oi;
    let dealer_gamma_proxy = (call_above - put_below) / total;
    let hedge_direction = (-dealer_gamma_proxy).clamp(-1.0, 1.0);

    Some(PositioningReport {
        open_interest_put_call: put_oi / call_oi.max(f64::EPSILON),
        volume_put_call: put_volume / call_volume.max(f64::EPSILON),
        max_pain_strike: max_pain,
        at_the_money_skew: skew,
        dealer_gamma_proxy,
        hedge_direction,
        limitation: "proxy from an explicitly supplied chain; no real dealer books or flow"
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain() -> OptionsChain {
        let strikes = [90.0, 95.0, 100.0, 105.0, 110.0]
            .into_iter()
            .map(|strike| OptionStrike {
                strike,
                call_open_interest: if strike > 100.0 { 120.0 } else { 40.0 },
                put_open_interest: if strike < 100.0 { 100.0 } else { 30.0 },
                call_volume: 10.0,
                put_volume: 12.0,
                call_iv: Some(0.2),
                put_iv: Some(0.25),
            })
            .collect();
        OptionsChain {
            symbol: "TEST".to_string(),
            expiry_ns: 1_700_000_000_000_000_000,
            spot: 100.0,
            strikes,
        }
    }

    #[test]
    fn a_chain_produces_bounded_proxies() {
        let report = analyze_positioning(&chain()).unwrap();
        assert!(report.open_interest_put_call > 0.0);
        assert_eq!(report.volume_put_call, 1.2);
        assert_eq!(report.max_pain_strike, Some(110.0));
        assert!((report.at_the_money_skew.unwrap() - 0.05).abs() < 1e-9);
        assert!((-1.0..=1.0).contains(&report.hedge_direction));
        assert!(report.limitation.contains("no real dealer books"));
    }

    #[test]
    fn an_empty_or_invalid_chain_is_refused() {
        let mut empty = chain();
        empty.strikes.clear();
        assert!(analyze_positioning(&empty).is_none());
        let mut invalid = chain();
        invalid.spot = 0.0;
        assert!(analyze_positioning(&invalid).is_none());
    }

    #[test]
    fn max_pain_tracks_the_largest_open_interest_strike() {
        let mut chain = chain();
        chain.strikes[2].put_open_interest = 1000.0;
        assert_eq!(
            analyze_positioning(&chain).unwrap().max_pain_strike,
            Some(100.0)
        );
    }
}
