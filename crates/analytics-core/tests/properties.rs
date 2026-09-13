//! Property tests for `analytics-core`.
//!
//! `docs/05-ANALYTICS-ENGINE.md` names three invariants that must hold for
//! *any* input, not just the fixtures in the unit tests:
//!
//! 1. CVD of a full session equals the sum of the per-candle deltas.
//! 2. `VAH >= POC >= VAL` always holds.
//! 3. Footprint cell volumes sum to the candle's total volume.
//!
//! The rest of the file extends the same idea to the invariants that would
//! otherwise silently corrupt the Strategy DSL or the AI agent's reasoning:
//! no `NaN`/infinity leaking into a `MarketState`, and RSI staying inside its
//! defined range.

use analytics_core::footprint::{build_footprint, build_footprint_from_candle};
use analytics_core::imbalance::{detect_imbalances, ImbalanceConfig};
use analytics_core::indicators::{atr, ema, rsi, sma};
use analytics_core::prelude::*;
use analytics_core::volume_profile::DEFAULT_VALUE_AREA_PCT;
use proptest::prelude::*;

/// One minute apart, so every generated candle sits in the same UTC session.
const MINUTE_NS: i64 = 60 * 1_000_000_000;

fn candle_strategy() -> impl Strategy<Value = Candle> {
    (0i64..1_000, 1.0f64..10_000.0, 0.0f64..500.0, 0.0f64..500.0).prop_map(
        |(i, price, buy_volume, sell_volume)| Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time: i * MINUTE_NS,
            open: price,
            // Keep the OHLC consistent: the close sits inside the range.
            high: price * 1.001,
            low: price * 0.999,
            close: price,
            volume: buy_volume + sell_volume,
            buy_volume,
            sell_volume,
        },
    )
}

fn trade_strategy() -> impl Strategy<Value = Trade> {
    (1.0f64..10_000.0, 0.0001f64..100.0, any::<bool>()).prop_map(
        |(price, quantity, is_buyer_maker)| Trade {
            symbol: "BTCUSDT".into(),
            trade_id: 0,
            price,
            quantity,
            is_buyer_maker,
            timestamp: 0,
        },
    )
}

proptest! {
    /// Invariant 1: the running sum and the sum of deltas must agree.
    #[test]
    fn cvd_equals_the_sum_of_per_candle_deltas(
        candles in prop::collection::vec(candle_strategy(), 1..200)
    ) {
        let series = calculate_cvd(&candles, false);
        prop_assert_eq!(series.len(), candles.len());

        let expected: f64 = candles.iter().map(Candle::delta).sum();
        let actual = series.last().copied().unwrap_or(0.0);
        prop_assert!(
            (actual - expected).abs() <= 1e-6 * expected.abs().max(1.0),
            "cvd {} != sum of deltas {}",
            actual,
            expected
        );
    }

    /// Invariant 2: the value area brackets the POC.
    #[test]
    fn value_area_brackets_the_poc(
        trades in prop::collection::vec(trade_strategy(), 1..500)
    ) {
        let profile = calculate_volume_profile(&trades, 1.0);
        prop_assert!(!profile.is_empty());
        prop_assert!(profile.val <= profile.poc, "val {} > poc {}", profile.val, profile.poc);
        prop_assert!(profile.poc <= profile.vah, "poc {} > vah {}", profile.poc, profile.vah);
        prop_assert!(profile.in_value_area(profile.poc));
    }

    /// The value area must actually cover its target share of the volume.
    #[test]
    fn value_area_covers_the_target_share(
        trades in prop::collection::vec(trade_strategy(), 1..300)
    ) {
        let profile = calculate_volume_profile(&trades, 1.0);
        prop_assert!(!profile.is_empty());

        let in_area: f64 = profile
            .histogram
            .iter()
            .filter(|node| node.price_level >= profile.val && node.price_level <= profile.vah)
            .map(|node| node.volume)
            .sum();

        let share = in_area / profile.total_volume;
        prop_assert!(
            share >= DEFAULT_VALUE_AREA_PCT - 1e-9,
            "value area covered only {}",
            share
        );
    }

    /// Invariant 3: no volume is lost or invented when bucketing into cells.
    #[test]
    fn footprint_cell_volumes_sum_to_the_traded_volume(
        trades in prop::collection::vec(trade_strategy(), 1..300)
    ) {
        let footprint = build_footprint(&trades, 1.0);
        let cell_total: f64 = footprint.cells.iter().map(FootprintCell::total_volume).sum();
        let traded: f64 = trades.iter().map(|t| t.quantity).sum();

        prop_assert!(
            (cell_total - traded).abs() <= 1e-6 * traded.max(1.0),
            "cells {} != traded {}",
            cell_total,
            traded
        );
    }

    /// The bid/ask split survives bucketing, and delta is derived from it.
    #[test]
    fn footprint_split_matches_the_aggressor_sides(
        trades in prop::collection::vec(trade_strategy(), 1..300)
    ) {
        let footprint = build_footprint(&trades, 1.0);

        let bid: f64 = footprint.cells.iter().map(|c| c.bid_volume).sum();
        let ask: f64 = footprint.cells.iter().map(|c| c.ask_volume).sum();
        let expected_bid: f64 = trades.iter().filter(|t| t.is_buyer_maker).map(|t| t.quantity).sum();
        let expected_ask: f64 = trades.iter().filter(|t| !t.is_buyer_maker).map(|t| t.quantity).sum();

        prop_assert!((bid - expected_bid).abs() <= 1e-6 * expected_bid.max(1.0));
        prop_assert!((ask - expected_ask).abs() <= 1e-6 * expected_ask.max(1.0));

        for cell in &footprint.cells {
            prop_assert!((cell.delta - (cell.ask_volume - cell.bid_volume)).abs() < 1e-9);
        }
    }

    /// A candle-derived footprint must never manufacture a signal.
    #[test]
    fn candle_derived_footprints_never_report_imbalances(
        candles in prop::collection::vec(candle_strategy(), 1..50)
    ) {
        for candle in &candles {
            let footprint = build_footprint_from_candle(candle, 1.0);
            prop_assert!(footprint.imbalances.is_empty());
            // ...even though the raw cells might well trip the detector.
            let _ = detect_imbalances(&footprint, ImbalanceConfig::default().ratio_threshold);
        }
    }

    /// Imbalance ratios are always finite and at or above the threshold, so the
    /// serialized `MarketState` can never contain a `null` where a number
    /// belongs.
    #[test]
    fn imbalance_ratios_are_finite(
        trades in prop::collection::vec(trade_strategy(), 1..200)
    ) {
        let footprint = build_footprint(&trades, 1.0);
        for event in detect_imbalances(&footprint, 3.0) {
            prop_assert!(event.ratio.is_finite());
            prop_assert!(event.ratio >= 3.0);
            prop_assert!(event.volume > 0.0);
            prop_assert!(event.opposing_volume > 0.0);
            prop_assert!(event.stacked >= 1);
        }
    }

    /// Nothing in a `MarketState` may be `NaN` or infinite.
    #[test]
    fn market_state_is_always_finite(
        candles in prop::collection::vec(candle_strategy(), 1..60),
        trades in prop::collection::vec(trade_strategy(), 0..60),
    ) {
        let config = MarketStateConfig {
            bucket_size: 1.0,
            ..MarketStateConfig::default()
        };

        let Some(state) = build_market_state(&candles, &trades, &config) else {
            return Ok(());
        };

        prop_assert!(state.price.is_finite());
        prop_assert!(state.delta.is_finite());
        prop_assert!(state.cvd.is_finite());
        prop_assert!(state.poc.is_finite());
        prop_assert!(state.vah.is_finite());
        prop_assert!(state.val.is_finite());
        prop_assert!(state.volume.is_finite());
        prop_assert!(state.buy_volume.is_finite());
        prop_assert!(state.sell_volume.is_finite());
        prop_assert!(state.net_imbalance_volume().is_finite());
        prop_assert!(state.vwap.is_none_or(f64::is_finite));
        prop_assert!(state.poc_deviation().is_none_or(f64::is_finite));

        for level in &state.liquidity {
            prop_assert!(level.price.is_finite());
        }
        for event in &state.absorption {
            prop_assert!(event.price_level.is_finite());
            prop_assert!(event.strength.is_finite());
        }

        // And it must round-trip through JSON, which is how the AI agent's
        // tools hand it over.
        let json = serde_json::to_string(&state).expect("state must serialize");
        let parsed: MarketState = serde_json::from_str(&json).expect("state must deserialize");
        prop_assert_eq!(parsed.symbol, state.symbol);
    }

    /// RSI stays inside its defined range, or is `None` during warm-up.
    #[test]
    fn rsi_stays_within_zero_and_one_hundred(
        values in prop::collection::vec(1.0f64..10_000.0, 1..200)
    ) {
        for value in rsi(&values, 14).into_iter().flatten() {
            prop_assert!((0.0..=100.0).contains(&value), "rsi out of range: {}", value);
            prop_assert!(value.is_finite());
        }
    }

    /// Indicators return exactly one entry per input, never a short slice.
    #[test]
    fn indicators_are_length_preserving(
        values in prop::collection::vec(1.0f64..10_000.0, 0..200),
        candles in prop::collection::vec(candle_strategy(), 0..200),
    ) {
        prop_assert_eq!(rsi(&values, 14).len(), values.len());
        prop_assert_eq!(sma(&values, 14).len(), values.len());
        prop_assert_eq!(ema(&values, 14).len(), values.len());
        prop_assert_eq!(atr(&candles, 14).len(), candles.len());
    }
}
