//! The script -> strategy -> execution boundary (`docs/55` Phase 5).
//!
//! A vetted Pine-lite script is a *measurement* program. The only thing it can
//! hand the platform is a `strategy_runtime::signal::Signal`, and the only
//! consumer of that signal on the script path is the deterministic backtester.
//! The live order path (`trading-engine`) does not depend on `pine-lite`, so a
//! script cannot name a venue, reach a credential, or place an order even if
//! its author asks for one.
//!
//! These tests pin the three properties the boundary rests on:
//!
//! 1. the language has no broker/account vocabulary to call;
//! 2. a script's declared size is not carried into the signal;
//! 3. the risk figure that reaches the engine is the platform's, not the
//!    script's -- and `strategy.risk.*` calls cannot change it.
//!
//! The live side of the boundary is pinned separately: a live bot cannot be
//! constructed without a gate-minted authorization
//! (`trading-engine::gate::LiveAuthorization`).

use analytics_core::types::{Candle, Timeframe};
use backtester::script_strategy::intents_to_signal;
use strategy_runtime::context::MarketContext;
use strategy_runtime::signal::Signal;
use strategy_runtime::Strategy;

fn candles(count: usize) -> Vec<Candle> {
    (0..count)
        .map(|i| Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M5,
            open_time: i as i64 * 300_000_000_000,
            open: 100.0 + i as f64,
            high: 101.0 + i as f64,
            low: 99.0 + i as f64,
            close: 100.5 + i as f64,
            volume: 10.0,
            buy_volume: 5.0,
            sell_volume: 5.0,
        })
        .collect()
}

fn signal_for(source: &str) -> Option<Signal> {
    let (_, parsed) = pine_lite::vet(source).expect("the fixture vets");
    let series = candles(40);
    let output =
        pine_lite::run(&parsed, &series, &pine_lite::Inputs::default()).expect("the fixture runs");
    let bar = series.len() - 1;
    let mut last_stop = None;
    intents_to_signal(&output, bar, &series, false, &mut last_stop)
}

#[test]
fn a_script_has_no_broker_or_account_vocabulary() {
    // The names an author would reach for to trade directly. None is in the
    // builtin catalog, and vetting is the gate that keeps it that way.
    for name in [
        "broker.order",
        "order.send",
        "order.place",
        "account.withdraw",
        "binance.place_order",
        "strategy.live",
    ] {
        let source = format!("//@pine_lite version=1\nx = {name}(close)\nplot(x)\n");
        assert!(
            pine_lite::vet(&source).is_err(),
            "`{name}` must not be callable from a script"
        );
    }
}

#[test]
fn a_script_declared_quantity_is_not_carried_to_the_signal() {
    let signal = signal_for(concat!(
        "//@pine_lite version=1\n",
        "strategy.entry(\"long\", direction=\"long\", qty=1000000000)\n",
        "plot(close)\n",
    ))
    .expect("the entry lowers to a signal");

    match signal {
        Signal::Enter(enter) => {
            assert_eq!(
                enter.max_risk_pct, 1.0,
                "sizing is the platform's own figure, not the script's `qty`"
            );
            assert!(
                enter.stop_price < enter.reference_price,
                "the stop still brackets the trade: {:?}",
                enter
            );
        }
        other => panic!("expected an entry, got {other:?}"),
    }
}

#[test]
fn a_script_risk_override_cannot_change_the_risk_figure() {
    // `strategy.risk.*` is recorded as an intent; the replay adapter ignores
    // it, and the risk engine is the only authority on limits. A script that
    // asks for an enormous intraday loss budget therefore still produces the
    // platform's 1% risk figure.
    let signal = signal_for(concat!(
        "//@pine_lite version=1\n",
        "strategy.risk.max_intraday_loss(1000000000)\n",
        "strategy.risk.max_position_size(1000000000)\n",
        "strategy.entry(\"long\", direction=\"long\")\n",
        "plot(close)\n",
    ))
    .expect("the entry still lowers");

    match signal {
        Signal::Enter(enter) => assert_eq!(enter.max_risk_pct, 1.0),
        other => panic!("expected an entry, got {other:?}"),
    }
}

#[test]
fn the_strategy_trait_gives_a_script_nothing_to_return_but_a_signal() {
    // A compile-time pin: `on_candle` returns `Option<Signal>` and there is no
    // other channel on the trait. If the boundary ever grows an order-shaped
    // return type, this assignment stops compiling.
    let _on_candle: fn(
        &mut backtester::script_strategy::ScriptStrategy,
        &MarketContext,
    ) -> Option<Signal> = Strategy::on_candle;
}
