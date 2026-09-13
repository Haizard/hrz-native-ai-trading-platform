//! The sandboxed half of the interpreter: everything that runs **inside** the
//! WASM module.
//!
//! This module is deliberately free of any FFI. It talks to the outside world
//! only through [`MarketSource`], which the ABI shim implements by calling the
//! host function the sandbox allowlist grants. Keeping the two apart means the
//! interesting logic -- parse, validate, build a context, interpret one candle
//! -- is ordinary Rust that can be unit-tested on the host, and the untestable
//! part is a few lines of pointer marshalling.
//!
//! ## What the guest is *not* allowed to do
//!
//! It cannot read a file, open a socket, spawn a process, read an environment
//! variable or reach another strategy's state -- not because it declines to,
//! but because the module imports nothing that could. The sandbox host checks
//! the module's import section against a four-name allowlist before it
//! instantiates anything; see `sandbox::allowlist`.
//!
//! ## Why the document is re-validated here
//!
//! The host validates before it sends the document (`docs/08-SANDBOX-WASM.md`:
//! "reject early, reject cheaply"), so this second pass is redundant on every
//! path we control. It stays because the sandbox must not *depend* on the host
//! having done it: `strategy-runtime` only accepts a
//! [`ValidatedStrategy`](strategy_dsl::ValidatedStrategy), so a document that
//! skipped validation could not be executed even if the host were wrong.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use strategy_dsl::StrategyDocument;
use strategy_runtime::context::{MarketContext, PositionView, TimeframeView};
use strategy_runtime::engine::Strategy as _;
use strategy_runtime::signal::Signal;
use strategy_runtime::{RuntimeConfig, StrategyEngine};

/// What the host tells the guest about the instant being evaluated.
///
/// Note what is *absent*: the market data. The guest has to ask for each
/// timeframe by name through [`MarketSource`], so it can only ever receive a
/// timeframe the document declared. Handing the whole context over in the call
/// would be one fewer round trip and one more thing the guest could poke at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    /// Symbol being evaluated, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Close time of the newest decision candle, unix nanoseconds.
    pub now: i64,
    /// Account equity, used for position sizing.
    pub equity: f64,
    /// The open position, if there is one.
    pub position: Option<PositionView>,
}

/// The one thing the interpreter needs from outside the sandbox.
pub trait MarketSource {
    /// The view for a declared timeframe name.
    ///
    /// Three outcomes, and the difference between the first two is the whole
    /// reason this returns an `Option`:
    ///
    /// * `Ok(Some(view))` -- here it is.
    /// * `Ok(None)` -- the timeframe is declared, but no candle of it has closed
    ///   yet. A 4h context timeframe has nothing to say for the first 48 bars of
    ///   a 5m series, and the native path treats that as warm-up rather than as
    ///   an error. Reporting it as one would make the sandbox disagree with the
    ///   native run on every candle before the first 4h close.
    /// * `Err(_)` -- the host refused. Either the name was never declared, or the
    ///   capability is not granted. The guest cannot tell the two apart, which
    ///   is intentional.
    fn view(&mut self, name: &str) -> Result<Option<TimeframeView>, String>;
}

/// The payload of `sbx_init`: what to run, and how.
///
/// The runtime configuration travels with the document rather than being
/// defaulted inside the guest. If the guest picked its own `max_history` while
/// the native run used another, `new_low(n)` could be true in one and false in
/// the other -- an equivalence failure with no visible cause.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitPayload {
    /// The strategy document, already validated by the host.
    pub document: StrategyDocument,
    /// The runtime tuning the native path would have used.
    pub runtime: RuntimeConfig,
}

/// The interpreter, as it exists inside the sandbox.
#[derive(Debug, Default)]
pub struct Interpreter {
    engine: Option<StrategyEngine>,
}

impl Interpreter {
    /// A guest that has not been given a document yet.
    #[must_use]
    pub fn new() -> Self {
        Self { engine: None }
    }

    /// Validate the document and build the engine.
    ///
    /// Returns the validation error verbatim when the document is refused, so
    /// the host can surface the same field-level message a native run would.
    pub fn init(&mut self, payload_json: &str) -> Result<(), String> {
        let payload: InitPayload =
            serde_json::from_str(payload_json).map_err(|e| format!("bad init payload: {e}"))?;

        let validated = strategy_dsl::ValidatedStrategy::with_defaults(payload.document)
            .map_err(|e| format!("the document did not pass validation: {e}"))?;
        let engine = StrategyEngine::new(&validated, payload.runtime)
            .map_err(|e| format!("the document is not tradable: {e}"))?;

        self.engine = Some(engine);
        Ok(())
    }

    /// The decision timeframe the engine settled on.
    #[must_use]
    pub fn decision_timeframe(&self) -> Option<&str> {
        self.engine.as_ref().map(StrategyEngine::decision_timeframe)
    }

    /// Evaluate one candle.
    ///
    /// Pulls exactly the timeframes the document declared -- no more, because
    /// it never asks for a name it did not find in its own copy of the document.
    pub fn eval(
        &mut self,
        header: &Header,
        market: &mut dyn MarketSource,
    ) -> Result<Option<Signal>, String> {
        let engine = self
            .engine
            .as_mut()
            .ok_or_else(|| "the sandbox was evaluated before it was initialised".to_string())?;

        let names: Vec<String> = engine.document().timeframes.keys().cloned().collect();

        let mut timeframes = BTreeMap::new();
        for name in names {
            // A declared timeframe with nothing to show yet simply does not make
            // it into the context, exactly as on the native path -- where the
            // replay only inserts a view for a cache that has a closed candle.
            if let Some(view) = market.view(&name)? {
                timeframes.insert(name, view);
            }
        }

        let context = MarketContext {
            symbol: header.symbol.clone(),
            now: header.now,
            // Taken from the engine rather than from the header: the engine
            // derived it from the document, and a second copy sent over the
            // boundary could disagree with it.
            decision_timeframe: engine.decision_timeframe().to_string(),
            timeframes,
            position: header.position.clone(),
            equity: header.equity,
        };

        Ok(engine.on_candle(&context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::types::Timeframe;
    use strategy_runtime::context::TimeframeView;

    /// A market source that behaves like the host, without a WASM boundary in
    /// the way: it knows which names were declared, and which of those have a
    /// closed candle yet.
    #[derive(Default)]
    struct FakeMarket {
        declared: std::collections::BTreeSet<String>,
        views: BTreeMap<String, TimeframeView>,
        asked: Vec<String>,
    }

    impl FakeMarket {
        fn declaring(names: &[&str]) -> Self {
            Self {
                declared: names.iter().map(|name| (*name).to_string()).collect(),
                ..Self::default()
            }
        }
    }

    impl MarketSource for FakeMarket {
        fn view(&mut self, name: &str) -> Result<Option<TimeframeView>, String> {
            self.asked.push(name.to_string());
            if !self.declared.contains(name) {
                return Err(format!("`{name}` was not declared"));
            }
            Ok(self.views.get(name).cloned())
        }
    }

    fn payload(json_document: &str) -> String {
        format!(
            r#"{{"document": {json_document}, "runtime": {}}}"#,
            serde_json::to_string(&RuntimeConfig::default()).unwrap()
        )
    }

    const DOCUMENT: &str = r#"{
        "name": "Guest test",
        "version": "1",
        "kind": "strategy",
        "market": "BTCUSDT",
        "timeframes": {"entry": "5m"},
        "entry": {"all_of": [
            {"timeframe": "entry", "condition": "close > threshold(0)"}
        ]},
        "risk": {"max_risk_pct": 1.0, "stop": {"kind": "below_recent_low", "bars": 2}},
        "invalidation": [{"timeframe": "entry", "condition": "close_below(vwap)"}]
    }"#;

    fn view(name: &str, close: f64) -> TimeframeView {
        use analytics_core::types::Candle;
        use analytics_core::{build_market_state, MarketStateConfig};

        let candles: Vec<Candle> = (0..40)
            .map(|i| {
                let price = 100.0 + f64::from(i);
                Candle {
                    symbol: "BTCUSDT".into(),
                    timeframe: Timeframe::M5,
                    open_time: i64::from(i) * Timeframe::M5.nanos(),
                    open: price,
                    high: price + 1.0,
                    low: price - 1.0,
                    close: price,
                    volume: 10.0,
                    buy_volume: 6.0,
                    sell_volume: 4.0,
                }
            })
            .collect();

        let mut last = candles[candles.len() - 1].clone();
        last.close = close;
        let mut history = candles;
        *history.last_mut().unwrap() = last.clone();

        let state = build_market_state(&history, &[], &MarketStateConfig::default()).unwrap();
        TimeframeView {
            name: name.to_string(),
            timeframe: Timeframe::M5,
            candle: last,
            state,
            previous: None,
            history,
        }
    }

    #[test]
    fn an_uninitialised_guest_refuses_to_evaluate() {
        let mut guest = Interpreter::new();
        let err = guest
            .eval(
                &Header {
                    symbol: "BTCUSDT".into(),
                    now: 0,
                    equity: 10_000.0,
                    position: None,
                },
                &mut FakeMarket::default(),
            )
            .unwrap_err();
        assert!(err.contains("before it was initialised"), "{err}");
    }

    #[test]
    fn the_guest_asks_only_for_declared_timeframes() {
        let mut guest = Interpreter::new();
        guest.init(&payload(DOCUMENT)).expect("valid document");

        // `entry` is declared and has data. `context` is a decoy the host knows
        // about but the document never named -- the guest must not ask for it,
        // because asking is how a document could probe for data it did not
        // declare.
        let mut market = FakeMarket::declaring(&["entry", "context"]);
        market.views.insert("entry".into(), view("entry", 200.0));

        let signal = guest
            .eval(
                &Header {
                    symbol: "BTCUSDT".into(),
                    now: 1_000,
                    equity: 10_000.0,
                    position: None,
                },
                &mut market,
            )
            .expect("evaluation succeeds");

        assert_eq!(
            market.asked,
            vec!["entry".to_string()],
            "the guest asked for something the document never declared"
        );
        assert!(
            signal.is_some(),
            "close > threshold(0) is true on this series"
        );
    }

    #[test]
    fn a_refused_view_is_an_error_not_a_silent_absence() {
        let mut guest = Interpreter::new();
        guest.init(&payload(DOCUMENT)).expect("valid document");

        // The document declares `entry`; the host declines to supply it.
        let err = guest
            .eval(
                &Header {
                    symbol: "BTCUSDT".into(),
                    now: 1_000,
                    equity: 10_000.0,
                    position: None,
                },
                &mut FakeMarket::default(),
            )
            .unwrap_err();
        assert!(err.contains("not declared"), "{err}");
    }

    /// The other half of the pair above, and the one that actually bit us.
    ///
    /// A declared timeframe with no closed candle yet -- a 1h context during the
    /// first two days of a 5m series -- is warm-up, not refusal. An earlier
    /// version of the host could not tell the two apart and reported a refusal
    /// on every early candle, which failed the whole run with
    /// `the host declined the context view`. The native path treats absence as
    /// absence, so the sandbox must too, or the two disagree exactly where the
    /// data is thinnest.
    #[test]
    fn a_declared_timeframe_with_no_closed_candle_is_warm_up_not_an_error() {
        let mut guest = Interpreter::new();
        guest.init(&payload(DOCUMENT)).expect("valid document");

        // Declared, but nothing to show.
        let mut market = FakeMarket::declaring(&["entry"]);
        assert!(market.views.is_empty(), "the fixture should hold no views");

        let signal = guest
            .eval(
                &Header {
                    symbol: "BTCUSDT".into(),
                    now: 1_000,
                    equity: 10_000.0,
                    position: None,
                },
                &mut market,
            )
            .expect("warm-up is not an error");

        assert_eq!(
            market.asked,
            vec!["entry".to_string()],
            "the guest should still have asked for the declared name"
        );
        assert!(signal.is_none(), "nothing can fire with no data to read");
    }

    #[test]
    fn a_document_that_fails_validation_is_refused_by_the_guest_too() {
        let mut guest = Interpreter::new();
        // `max_risk_pct` above the ceiling the document cannot raise.
        let bad = DOCUMENT.replace("\"max_risk_pct\": 1.0", "\"max_risk_pct\": 80.0");
        let err = guest.init(&payload(&bad)).unwrap_err();
        assert!(err.contains("did not pass validation"), "{err}");
    }

    #[test]
    fn a_malformed_init_payload_is_a_message_not_a_panic() {
        let mut guest = Interpreter::new();
        assert!(guest.init("not json at all").is_err());
        assert!(guest.init(r#"{"document": {}}"#).is_err());
    }
}
