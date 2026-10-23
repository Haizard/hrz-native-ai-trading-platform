//! The multi-broker taxonomy: venue-**products**, not venues; **capabilities**,
//! not assumptions.
//!
//! The platform hosts several brokers and several kinds of market data, and
//! the honest fact of the business is that no two products serve the same
//! data. Order flow is the standing example: footprint and delta need real
//! trades with aggressor side, which a synthetic index (price quotes from a
//! random engine) does not have and will never have. This module is the
//! single place that says what each product *can* do, so features and agents
//! ask the registry instead of guessing — and unavailable features refuse
//! with a named reason instead of computing a confident lie.
//!
//! Three levels:
//!
//! - [`BrokerPlugin`] — one per broker (binance, deriv, ...).
//! - [`Product`] — one per market shape under a broker (spot, usdt-m,
//!   synthetic, forex-cfd). Data channels, execution model and attribution
//!   live here.
//! - [`VenueSymbol`] — one global `VENUE:SYMBOL` namespace every symbol
//!   travels in (URLs, DB rows, pine-lite, agent text).

use serde::{Deserialize, Serialize};

/// The data channels a product can serve, as a bitset. Features declare the
/// channels they require; availability is set intersection, never a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataChannels(pub u32);

impl DataChannels {
    /// OHLCV candles (any construction: venue klines, tick aggregation).
    pub const CANDLES: Self = Self(1 << 0);
    /// Individual trade prints.
    pub const TRADES: Self = Self(1 << 1);
    /// Trade prints WITH aggressor side (order flow's hard requirement).
    pub const TRADES_AGGRESSOR: Self = Self(1 << 2);
    /// Order-book depth.
    pub const DEPTH: Self = Self(1 << 3);
    /// Funding rate (futures products).
    pub const FUNDING: Self = Self(1 << 4);
    /// Open interest (futures products).
    pub const OPEN_INTEREST: Self = Self(1 << 5);
    /// Liquidation prints (futures products).
    pub const LIQUIDATIONS: Self = Self(1 << 6);
    /// Price quotes without a real trade behind them (tick streams).
    pub const TICK_QUOTES: Self = Self(1 << 7);
    /// Macroeconomic context (economic releases as market catalysts). A
    /// synthetic index's price is algorithmic -- macro is UNDEFINED for it,
    /// which is the taxonomy's whole point.
    pub const MACRO: Self = Self(1 << 8);

    /// The union of two sets.
    #[must_use]
    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether `need` is fully contained in this set.
    #[must_use]
    pub fn supports(self, need: Self) -> bool {
        self.0 & need.0 == need.0
    }

    /// The channels present, as readable names (for the capability catalog).
    #[must_use]
    pub fn names(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for (bit, name) in [
            (Self::CANDLES, "candles"),
            (Self::TRADES, "trades"),
            (Self::TRADES_AGGRESSOR, "trades_aggressor"),
            (Self::DEPTH, "depth"),
            (Self::FUNDING, "funding"),
            (Self::OPEN_INTEREST, "open_interest"),
            (Self::LIQUIDATIONS, "liquidations"),
            (Self::TICK_QUOTES, "tick_quotes"),
        ] {
            if self.supports(bit) {
                out.push(name);
            }
        }
        out
    }
}

/// How orders work on a product. Not one fake unified model: a Deriv
/// contract is not a spot order wearing a costume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionModel {
    /// Spot-style market/limit orders on a position (crypto spot).
    SpotOrders,
    /// Futures orders with margin semantics (usdt-m, linear).
    FuturesOrders,
    /// Buy/sell contracts (multipliers, rise/fall): open, hold, sell or
    /// expire. Payout semantics, no bracket orders.
    Contracts,
}

/// How trading volume on a product earns the platform commission. First-class
/// because it is the business: an untagged order leaving the router is money
/// lost, and the plugin must make that impossible, not unlikely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionModel {
    /// No attribution (data-only or self-funded venue).
    None,
    /// New users sign up through the platform's referral link and are tagged
    /// for life (Deriv Partners track 1).
    ReferralLink,
    /// Orders carry the platform's registered app id (Deriv Partners track 2,
    /// OAuth-connected accounts).
    AppTag,
    /// Both tracks (referral for new accounts, app tag for existing ones).
    ReferralAndAppTag,
}

/// One market shape under a broker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Product {
    /// Stable id, e.g. `"spot"`, `"usdt-m"`, `"synthetic"`, `"forex-cfd"`.
    pub id: &'static str,
    /// What it can serve.
    pub channels: DataChannels,
    /// How orders work on it.
    pub execution: ExecutionModel,
    /// How its volume earns commission.
    pub attribution: AttributionModel,
}

/// One broker, as a plugin: its products and how they behave.
pub trait BrokerPlugin: Send + Sync + std::fmt::Debug {
    /// Lowercase broker name, e.g. `"binance"`, `"deriv"`.
    fn name(&self) -> &'static str;
    /// The products this broker offers, with their capabilities.
    fn products(&self) -> &[Product];
}

/// Binance, as plugin #1: the venue the platform already speaks.
#[derive(Debug)]
pub struct BinancePlugin;

const BINANCE_PRODUCTS: &[Product] = &[
            Product {
                id: "spot",
                // Crypto trades macro through USD, yields, and risk appetite.
                channels: DataChannels::CANDLES
                    .or(DataChannels::TRADES)
                    .or(DataChannels::TRADES_AGGRESSOR)
                    .or(DataChannels::DEPTH)
                    .or(DataChannels::MACRO),
                execution: ExecutionModel::SpotOrders,
                attribution: AttributionModel::None,
            },
            Product {
                id: "usdt-m",
                channels: DataChannels::CANDLES
                    .or(DataChannels::TRADES)
                    .or(DataChannels::TRADES_AGGRESSOR)
                    .or(DataChannels::DEPTH)
                    .or(DataChannels::FUNDING)
                    .or(DataChannels::OPEN_INTEREST)
                    .or(DataChannels::LIQUIDATIONS)
                    .or(DataChannels::MACRO),
                execution: ExecutionModel::FuturesOrders,
                attribution: AttributionModel::None,
            },
];

impl BrokerPlugin for BinancePlugin {
    fn name(&self) -> &'static str {
        "binance"
    }

    fn products(&self) -> &[Product] {
        BINANCE_PRODUCTS
    }
}

/// Deriv, declared: data plane lands in phase 2, contracts in phase 4. The
/// declaration itself is the point -- the taxonomy only means something when
/// it can say "synthetics have no real trade flow" on day one.
#[derive(Debug)]
pub struct DerivPlugin;

const DERIV_PRODUCTS: &[Product] = &[
            Product {
                id: "synthetic",
                // Synthetic indices are price quotes from a random engine:
                // candles derivable from ticks, but no real trades, no
                // aggressor side, no book, and NO MACRO -- a random engine
                // does not read the BLS, and pretending otherwise is the
                // category error the taxonomy exists to prevent.
                channels: DataChannels::CANDLES.or(DataChannels::TICK_QUOTES),
                execution: ExecutionModel::Contracts,
                attribution: AttributionModel::ReferralAndAppTag,
            },
            Product {
                id: "forex-cfd",
                // Forex CFDs trade the dollar's own world: macro applies.
                channels: DataChannels::CANDLES.or(DataChannels::TICK_QUOTES).or(DataChannels::MACRO),
                execution: ExecutionModel::Contracts,
                attribution: AttributionModel::ReferralAndAppTag,
            },
];

impl BrokerPlugin for DerivPlugin {
    fn name(&self) -> &'static str {
        "deriv"
    }

    fn products(&self) -> &[Product] {
        DERIV_PRODUCTS
    }
}

/// The built-in registry. Every feature and every agent asks HERE what an
/// instrument can do; nothing else is authoritative.
#[must_use]
pub fn broker_registry() -> Vec<Box<dyn BrokerPlugin>> {
    vec![Box::new(BinancePlugin), Box::new(DerivPlugin)]
}

/// Look up one product by broker and product id.
#[must_use]
pub fn find_product(broker: &str, product: &str) -> Option<&'static Product> {
    // The registry is two plugins; a static lookup keeps the borrow simple.
    static BINANCE: BinancePlugin = BinancePlugin;
    static DERIV: DerivPlugin = DerivPlugin;
    let plugin: &dyn BrokerPlugin = match broker {
        "binance" => &BINANCE,
        "deriv" => &DERIV,
        _ => return None,
    };
    plugin.products().iter().find(|p| p.id == product)
}

/// A symbol in the global namespace: `VENUE:SYMBOL` or
/// `VENUE-PRODUCT:SYMBOL` when the broker has several products (`BINANCE-F:`
/// is the usdt-m spelling; bare `BINANCE:` is spot).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VenueSymbol {
    /// The broker, lowercase (`binance`, `deriv`).
    pub broker: String,
    /// The product id within the broker (`spot`, `synthetic`, ...).
    pub product: String,
    /// The instrument symbol as the broker spells it (`BTCUSDT`,
    /// `Volatility 75`, `frxEURUSD`).
    pub symbol: String,
}

/// The default venue a legacy bare symbol (`BTCUSDT`) resolves to.
pub const DEFAULT_VENUE: (&str, &str) = ("binance", "spot");

impl VenueSymbol {
    /// `DERIV:Volatility 75`, `BINANCE:BTCUSDT`, `BINANCE-F:BTCUSDT`.
    #[must_use]
    pub fn id(&self) -> String {
        if self.broker == "binance" && self.product == "usdt-m" {
            format!("BINANCE-F:{}", self.symbol)
        } else {
            format!("{}:{}", self.broker.to_uppercase(), self.symbol)
        }
    }

    /// Parse a namespaced symbol; a legacy bare symbol resolves to
    /// [`DEFAULT_VENUE`].
    #[must_use]
    pub fn parse(id: &str) -> Self {
        if let Some((venue, symbol)) = id.split_once(':') {
            let v = venue.to_ascii_lowercase();
            let (broker, product) = match v.as_str() {
                "binance-f" => ("binance", "usdt-m"),
                "binance" => ("binance", "spot"),
                "deriv" => ("deriv", "synthetic"),
                "deriv-fx" => ("deriv", "forex-cfd"),
                other => (other, "spot"),
            };
            Self {
                broker: broker.to_string(),
                product: product.to_string(),
                symbol: symbol.to_string(),
            }
        } else {
            Self {
                broker: DEFAULT_VENUE.0.to_string(),
                product: DEFAULT_VENUE.1.to_string(),
                symbol: id.to_string(),
            }
        }
    }

    /// The product's declared channels, when the broker+product are known.
    #[must_use]
    pub fn channels(&self) -> Option<DataChannels> {
        find_product(&self.broker, &self.product).map(|p| p.channels)
    }

    /// Whether the product can serve `need`.
    #[must_use]
    pub fn supports(&self, need: DataChannels) -> bool {
        self.channels().is_some_and(|c| c.supports(need))
    }
}

/// A platform feature's hard channel requirements, for the capability
/// catalog. `None` = the feature works on candles alone (nearly everything).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureRequirement {
    /// Stable feature id (`order_flow`, `volume_profile`, ...).
    pub id: &'static str,
    /// The channels it cannot run without.
    pub requires: DataChannels,
    /// The refusal text when a product lacks them.
    pub reason: &'static str,
}

/// The feature requirements the platform knows today.
pub const FEATURE_REQUIREMENTS: &[FeatureRequirement] = &[
    FeatureRequirement {
        id: "order_flow",
        requires: DataChannels::TRADES_AGGRESSOR,
        reason: "order flow needs real trades with aggressor side; this product serves quotes only",
    },
    FeatureRequirement {
        id: "depth_map",
        requires: DataChannels::DEPTH,
        reason: "the depth map needs order-book data this product does not serve",
    },
    FeatureRequirement {
        id: "funding_dashboard",
        requires: DataChannels::FUNDING,
        reason: "funding data exists only on futures products",
    },
    FeatureRequirement {
        id: "open_interest",
        requires: DataChannels::OPEN_INTEREST,
        reason: "open interest exists only on futures products",
    },
    FeatureRequirement {
        id: "liquidation_feed",
        requires: DataChannels::LIQUIDATIONS,
        reason: "liquidation prints exist only on futures products",
    },
];

/// The availability of every known feature on one symbol: the catalog the
/// frontend grays features out from, and the agent reads before promising.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureAvailability {
    /// The feature id.
    pub feature: String,
    /// Whether the symbol's product supports it.
    pub available: bool,
    /// The named refusal when not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Feature availability on one namespaced symbol.
#[must_use]
pub fn availability_for(symbol: &VenueSymbol) -> Vec<FeatureAvailability> {
    FEATURE_REQUIREMENTS
        .iter()
        .map(|f| {
            let available = symbol.supports(f.requires);
            FeatureAvailability {
                feature: f.id.to_string(),
                available,
                reason: (!available).then(|| f.reason.to_string()),
            }
        })
        .collect()
}

/// The whole catalog: brokers, their products and channels, and the feature
/// requirements. The agent and the frontend consume this verbatim.
#[must_use]
pub fn capability_catalog() -> serde_json::Value {
    let brokers: Vec<serde_json::Value> = broker_registry()
        .iter()
        .map(|b| {
            serde_json::json!({
                "broker": b.name(),
                "products": b.products().iter().map(|p| {
                    serde_json::json!({
                        "id": p.id,
                        "channels": p.channels.names(),
                        "execution": p.execution,
                        "attribution": p.attribution,
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "brokers": brokers,
        "features": FEATURE_REQUIREMENTS.iter().map(|f| {
            serde_json::json!({
                "id": f.id,
                "requires": f.requires.names(),
                "reason": f.reason,
            })
        }).collect::<Vec<_>>(),
        "default_venue": DEFAULT_VENUE.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_flow_is_unavailable_on_synthetics_and_available_on_spot() {
        let syn = VenueSymbol::parse("DERIV:Volatility 75");
        assert_eq!(syn.broker, "deriv");
        assert_eq!(syn.product, "synthetic");
        assert!(!syn.supports(DataChannels::TRADES_AGGRESSOR));
        let avail = availability_for(&syn);
        let of = avail.iter().find(|a| a.feature == "order_flow").expect("row");
        assert!(!of.available);
        assert!(of.reason.as_deref().unwrap_or_default().contains("quotes only"));

        let btc = VenueSymbol::parse("BINANCE:BTCUSDT");
        assert!(btc.supports(DataChannels::TRADES_AGGRESSOR));
        let avail = availability_for(&btc);
        // Spot has order flow and the depth map; the futures features are
        // correctly unavailable (funding/OI/liquidations).
        for f in ["order_flow", "depth_map"] {
            assert!(avail.iter().find(|a| a.feature == f).expect("row").available, "{f}");
        }
        for f in ["funding_dashboard", "open_interest", "liquidation_feed"] {
            assert!(!avail.iter().find(|a| a.feature == f).expect("row").available, "{f}");
        }
    }

    #[test]
    fn futures_channels_set_them_apart_from_spot() {
        let f = VenueSymbol::parse("BINANCE-F:BTCUSDT");
        assert_eq!(f.product, "usdt-m");
        assert_eq!(f.id(), "BINANCE-F:BTCUSDT");
        assert!(f.supports(DataChannels::FUNDING));
        assert!(f.supports(DataChannels::OPEN_INTEREST));
        let spot = VenueSymbol::parse("BTCUSDT");
        assert_eq!(spot.broker, "binance");
        assert_eq!(spot.product, "spot");
        assert_eq!(spot.id(), "BINANCE:BTCUSDT");
        assert!(!spot.supports(DataChannels::FUNDING));
    }

    #[test]
    fn deriv_forex_ticks_have_no_volume_channels() {
        let fx = VenueSymbol::parse("DERIV-FX:frxEURUSD");
        assert_eq!(fx.product, "forex-cfd");
        assert_eq!(fx.broker, "deriv");
        assert!(fx.supports(DataChannels::TICK_QUOTES));
        assert!(!fx.supports(DataChannels::TRADES));
        assert!(!fx.supports(DataChannels::DEPTH));
    }

    #[test]
    fn the_catalog_lists_both_brokers_and_the_feature_rules() {
        let cat = capability_catalog();
        let brokers: Vec<&str> = cat["brokers"]
            .as_array()
            .expect("brokers")
            .iter()
            .filter_map(|b| b["broker"].as_str())
            .collect();
        assert_eq!(brokers, ["binance", "deriv"]);
        let syn = &cat["brokers"][1]["products"][0];
        assert_eq!(syn["channels"].as_array().expect("channels").len(), 2);
        assert_eq!(syn["execution"], "contracts");
        assert_eq!(syn["attribution"], "referral_and_app_tag");
    }
}
