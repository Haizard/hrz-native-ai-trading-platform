//! The subscription matrix: plans, prices, limits and features (`docs/52`).
//!
//! ## The matrix is code, not data
//!
//! A plan is a product decision with a review. Putting it in the database
//! would let a deployment grant a plan whose limits the code that enforces them
//! has never heard of; putting it here makes every pricing change a compile-
//! checked one. [`catalog`] is the single answer to "what does Core include",
//! and both the API and the enforcement read it.
//!
//! ## Unlimited is a negative limit
//!
//! `-1` means unlimited throughout. An `Option<i64>` would be more idiomatic in
//! isolation, but this crosses a JSON boundary into a UI that renders a number,
//! and every extra shape is another place a client can read `null` as `0`. The
//! sign is the whole convention: negative is unlimited, zero is none.
//!
//! ## Prices are cents per the period
//!
//! `annual_cents` is the amount charged once a year, not twelve monthly
//! charges. `annual_effective_monthly_cents` is what marketing may compare
//! against the monthly price; the two are kept separate so no client has to
//! divide to find out what a subscription actually costs.

use serde::{Deserialize, Serialize};

/// Days in the card-free trial.
pub const TRIAL_DAYS: i64 = 14;

/// Credits an ordinary chart question costs.
pub const CREDIT_CHART_ANALYSIS: i64 = 3;
/// Credits a short, non-chart answer costs.
pub const CREDIT_QUICK_ANSWER: i64 = 1;
/// Credits a backtest interpretation costs.
pub const CREDIT_BACKTEST_READ: i64 = 5;
/// Credits generating a strategy costs.
pub const CREDIT_STRATEGY_GENERATION: i64 = 8;
/// Credits one zone-watcher run costs.
pub const CREDIT_WATCHER_RUN: i64 = 3;
/// Credits one indicator-workspace turn costs.
pub const CREDIT_WORKSPACE_TURN: i64 = 3;
/// Credits reviewing an indicator revision costs.
pub const CREDIT_REVIEW: i64 = 3;
/// Credits an author-mode socket turn costs.
pub const CREDIT_AUTHOR_TURN: i64 = 2;

/// The five rungs of the matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanId {
    /// Anonymous-equivalent access behind a login: charts, paper trading, a
    /// small agent allowance.
    Free,
    /// Live order flow and enough of a workflow to use the platform daily.
    Core,
    /// The full analytical desk, including advanced order flow and one live bot.
    Pro,
    /// More of everything, plus a bring-your-own model key.
    Elite,
    /// Multiple live books and the highest allowances.
    Desk,
}

impl PlanId {
    /// Every plan, cheapest first.
    pub const ALL: [Self; 5] = [Self::Free, Self::Core, Self::Pro, Self::Elite, Self::Desk];

    /// The wire name, which is also the database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Core => "core",
            Self::Pro => "pro",
            Self::Elite => "elite",
            Self::Desk => "desk",
        }
    }

    /// Parse a wire name, case-insensitively.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "free" => Some(Self::Free),
            "core" => Some(Self::Core),
            "pro" => Some(Self::Pro),
            "elite" => Some(Self::Elite),
            "desk" => Some(Self::Desk),
            _ => None,
        }
    }

    /// Whether this plan is a paid rung.
    #[must_use]
    pub const fn is_paid(self) -> bool {
        !matches!(self, Self::Free)
    }
}

impl std::fmt::Display for PlanId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How often a plan is billed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BillingCycle {
    /// Charged every month.
    Monthly,
    /// Charged once a year.
    Annual,
}

impl BillingCycle {
    /// The wire name, which is also the database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Annual => "annual",
        }
    }

    /// Parse a wire name, case-insensitively.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "monthly" | "month" => Some(Self::Monthly),
            "annual" | "annually" | "yearly" | "year" => Some(Self::Annual),
            _ => None,
        }
    }
}

impl std::fmt::Display for BillingCycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a plan costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Pricing {
    /// Monthly price, in cents.
    pub monthly_cents: i64,
    /// Annual price charged once, in cents.
    pub annual_cents: i64,
    /// Annual price divided by twelve, in cents, for comparison copy.
    pub annual_effective_monthly_cents: i64,
}

/// The numeric allowances of a plan. `-1` means unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Limits {
    /// Panes in one chart layout.
    pub max_panes: i64,
    /// Saved layouts (chart sessions).
    pub max_layouts: i64,
    /// Saved price alerts.
    pub max_alerts: i64,
    /// Enabled zone watchers.
    pub max_watchers: i64,
    /// AI credits consumed per billing period.
    pub max_ai_credits: i64,
    /// Backtests consumed per billing period.
    pub max_backtests: i64,
    /// Chart history depth in days.
    pub history_days: i64,
    /// Concurrent live market streams.
    ///
    /// The shell keeps one stream per pane, so this matches `max_panes` on
    /// every rung and the pane cap is the enforceable bound. The market socket
    /// itself is public (`docs/12`), so counting sockets would gate data that
    /// anyone can read without an account -- the pane a signed-in user can
    /// open is the part a plan can honestly limit.
    pub max_concurrent: i64,
    /// Running or paused paper bots.
    pub max_paper_bots: i64,
    /// Running or paused live bots.
    pub max_live_bots: i64,
}

/// The feature flags a plan carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Features {
    /// Live footprint, bar delta and delta-by-size.
    pub orderflow: bool,
    /// Profile memory, icebergs, VPIN and absorption reads.
    pub orderflow_advanced: bool,
    /// A user's own AI provider key.
    pub byo_model_key: bool,
    /// Priority support.
    pub priority_support: bool,
    /// A dedicated onboarding session.
    pub dedicated_onboarding: bool,
    /// What the community indicator library allows.
    ///
    /// Browsing and publishing are separate gates: `CommunityIndicators` is
    /// the entitlement, and [`Feature::CommunityBrowse`] /
    /// [`Feature::CommunityPublish`] are what a route enforces.
    pub community_indicators: CommunityIndicators,
}

/// Access to the community indicator library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CommunityIndicators {
    /// No access.
    None,
    /// Browse and install public indicators.
    Browse,
    /// Browse and publish.
    Publish,
    /// Browse, publish, and get review priority.
    Priority,
}

/// One plan's complete definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// The identifier.
    pub id: PlanId,
    /// Display name.
    pub name: &'static str,
    /// One line describing who it is for.
    pub tagline: &'static str,
    /// The prices.
    pub pricing: Pricing,
    /// The numeric allowances.
    pub limits: Limits,
    /// The feature flags.
    pub features: Features,
}

/// The complete matrix, cheapest first.
pub const PLANS: [Plan; 5] = [
    Plan {
        id: PlanId::Free,
        name: "Free",
        tagline: "Chart, paper trade, and try the agent.",
        pricing: Pricing {
            monthly_cents: 0,
            annual_cents: 0,
            annual_effective_monthly_cents: 0,
        },
        limits: Limits {
            max_panes: 4,
            max_layouts: 3,
            max_alerts: 5,
            max_watchers: 1,
            max_ai_credits: 25,
            max_backtests: 5,
            history_days: 31,
            max_concurrent: 4,
            max_paper_bots: 1,
            max_live_bots: 0,
        },
        features: Features {
            orderflow: false,
            orderflow_advanced: false,
            byo_model_key: false,
            priority_support: false,
            dedicated_onboarding: false,
            community_indicators: CommunityIndicators::None,
        },
    },
    Plan {
        id: PlanId::Core,
        name: "Core",
        tagline: "Live order flow and a daily workflow.",
        pricing: Pricing {
            monthly_cents: 1_200,
            annual_cents: 10_800,
            annual_effective_monthly_cents: 900,
        },
        limits: Limits {
            max_panes: 16,
            max_layouts: 25,
            max_alerts: 50,
            max_watchers: 5,
            max_ai_credits: 250,
            max_backtests: 50,
            history_days: 365,
            max_concurrent: 16,
            max_paper_bots: 3,
            max_live_bots: 0,
        },
        features: Features {
            orderflow: true,
            orderflow_advanced: false,
            byo_model_key: false,
            priority_support: false,
            dedicated_onboarding: false,
            community_indicators: CommunityIndicators::Browse,
        },
    },
    Plan {
        id: PlanId::Pro,
        name: "Pro",
        tagline: "The full analytical desk, with one live bot.",
        pricing: Pricing {
            monthly_cents: 2_400,
            annual_cents: 22_800,
            annual_effective_monthly_cents: 1_900,
        },
        limits: Limits {
            max_panes: 50,
            max_layouts: 100,
            max_alerts: 200,
            max_watchers: 20,
            max_ai_credits: 1_200,
            max_backtests: 300,
            history_days: 1_095,
            max_concurrent: 50,
            max_paper_bots: 10,
            max_live_bots: 1,
        },
        features: Features {
            orderflow: true,
            orderflow_advanced: true,
            byo_model_key: false,
            priority_support: true,
            dedicated_onboarding: false,
            community_indicators: CommunityIndicators::Browse,
        },
    },
    Plan {
        id: PlanId::Elite,
        name: "Elite",
        tagline: "More of everything, plus your own model key.",
        pricing: Pricing {
            monthly_cents: 3_600,
            annual_cents: 34_800,
            annual_effective_monthly_cents: 2_900,
        },
        limits: Limits {
            max_panes: 100,
            max_layouts: -1,
            max_alerts: 500,
            max_watchers: 50,
            max_ai_credits: 2_500,
            max_backtests: -1,
            history_days: -1,
            max_concurrent: 100,
            max_paper_bots: 25,
            max_live_bots: 3,
        },
        features: Features {
            orderflow: true,
            orderflow_advanced: true,
            byo_model_key: true,
            priority_support: true,
            dedicated_onboarding: false,
            community_indicators: CommunityIndicators::Publish,
        },
    },
    Plan {
        id: PlanId::Desk,
        name: "Desk",
        tagline: "For teams running several live books.",
        pricing: Pricing {
            monthly_cents: 5_500,
            annual_cents: 52_800,
            annual_effective_monthly_cents: 4_400,
        },
        limits: Limits {
            max_panes: 100,
            max_layouts: -1,
            max_alerts: 1_000,
            max_watchers: 100,
            max_ai_credits: 5_000,
            max_backtests: -1,
            history_days: -1,
            max_concurrent: 100,
            max_paper_bots: 50,
            max_live_bots: 5,
        },
        features: Features {
            orderflow: true,
            orderflow_advanced: true,
            byo_model_key: true,
            priority_support: true,
            dedicated_onboarding: true,
            community_indicators: CommunityIndicators::Priority,
        },
    },
];

/// The complete matrix.
#[must_use]
pub const fn catalog() -> &'static [Plan; 5] {
    &PLANS
}

/// One plan by id.
#[must_use]
pub fn plan(id: PlanId) -> &'static Plan {
    match id {
        PlanId::Free => &PLANS[0],
        PlanId::Core => &PLANS[1],
        PlanId::Pro => &PLANS[2],
        PlanId::Elite => &PLANS[3],
        PlanId::Desk => &PLANS[4],
    }
}

/// A gated capability, for errors that name what to upgrade for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    /// Live footprint and delta.
    Orderflow,
    /// Profile memory, icebergs and VPIN.
    AdvancedOrderflow,
    /// A user's own model key.
    ByoModelKey,
    /// Any live bot at all.
    LiveBots,
    /// Browsing and installing from the community library.
    CommunityBrowse,
    /// Publishing to the community library.
    CommunityPublish,
}

impl Feature {
    /// The stable error `details.feature` value.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Orderflow => "orderflow",
            Self::AdvancedOrderflow => "orderflow_advanced",
            Self::ByoModelKey => "byo_model_key",
            Self::LiveBots => "live_bots",
            Self::CommunityBrowse => "community_browse",
            Self::CommunityPublish => "community_publish",
        }
    }

    /// The plan that would unlock it, for `details.required_plan`.
    #[must_use]
    pub const fn minimum_plan(self) -> PlanId {
        match self {
            Self::Orderflow => PlanId::Core,
            Self::AdvancedOrderflow | Self::ByoModelKey | Self::LiveBots => PlanId::Pro,
            Self::CommunityBrowse => PlanId::Core,
            Self::CommunityPublish => PlanId::Elite,
        }
    }

    /// Whether a feature set carries this capability.
    ///
    /// Live bots are a counted limit, not a flag; they are checked through
    /// [`Resource`].
    #[must_use]
    pub const fn allowed_by(self, features: &Features) -> bool {
        match self {
            Self::Orderflow => features.orderflow,
            Self::AdvancedOrderflow => features.orderflow_advanced,
            Self::ByoModelKey => features.byo_model_key,
            Self::LiveBots => false,
            Self::CommunityBrowse => matches!(
                features.community_indicators,
                CommunityIndicators::Browse
                    | CommunityIndicators::Publish
                    | CommunityIndicators::Priority
            ),
            Self::CommunityPublish => matches!(
                features.community_indicators,
                CommunityIndicators::Publish | CommunityIndicators::Priority
            ),
        }
    }
}

/// A capped standing resource. `-1` limits mean unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// Panes in one layout.
    Panes,
    /// Saved layouts.
    Layouts,
    /// Saved alerts.
    Alerts,
    /// Enabled watchers.
    Watchers,
    /// Running or paused paper bots.
    PaperBots,
    /// Running or paused live bots.
    LiveBots,
    /// Concurrent market streams.
    ConcurrentStreams,
}

impl Resource {
    /// The stable error `details.resource` value.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Panes => "panes",
            Self::Layouts => "layouts",
            Self::Alerts => "alerts",
            Self::Watchers => "watchers",
            Self::PaperBots => "paper_bots",
            Self::LiveBots => "live_bots",
            Self::ConcurrentStreams => "concurrent_streams",
        }
    }

    /// The limit this resource has under a plan.
    #[must_use]
    pub const fn limit(self, limits: &Limits) -> i64 {
        match self {
            Self::Panes => limits.max_panes,
            Self::Layouts => limits.max_layouts,
            Self::Alerts => limits.max_alerts,
            Self::Watchers => limits.max_watchers,
            Self::PaperBots => limits.max_paper_bots,
            Self::LiveBots => limits.max_live_bots,
            Self::ConcurrentStreams => limits.max_concurrent,
        }
    }

    /// The human name used in an error message.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Panes => "panes",
            Self::Layouts => "saved layouts",
            Self::Alerts => "alerts",
            Self::Watchers => "zone watchers",
            Self::PaperBots => "paper bots",
            Self::LiveBots => "live bots",
            Self::ConcurrentStreams => "concurrent streams",
        }
    }
}

/// Per-period usage metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageMetric {
    /// Weighted agent work.
    AiCredit,
    /// One backtest or parameter sweep.
    Backtest,
}

impl UsageMetric {
    /// The database counter name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AiCredit => "ai_credit",
            Self::Backtest => "backtest",
        }
    }

    /// The plan limit for this metric.
    #[must_use]
    pub const fn limit(self, limits: &Limits) -> i64 {
        match self {
            Self::AiCredit => limits.max_ai_credits,
            Self::Backtest => limits.max_backtests,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_are_ordered_cheapest_first() {
        let mut previous = -1;
        for plan in catalog() {
            assert!(
                plan.pricing.monthly_cents >= previous,
                "{} is cheaper than the plan before it",
                plan.name
            );
            previous = plan.pricing.monthly_cents;
        }
    }

    #[test]
    fn annual_is_cheaper_than_twelve_months() {
        for plan in catalog()
            .iter()
            .filter(|plan| plan.pricing.monthly_cents > 0)
        {
            assert!(
                plan.pricing.annual_cents < plan.pricing.monthly_cents * 12,
                "{} annual price is not a discount",
                plan.name
            );
            assert_eq!(
                plan.pricing.annual_cents / 12,
                plan.pricing.annual_effective_monthly_cents,
                "{} effective monthly price is not the annual price divided by 12",
                plan.name
            );
        }
    }

    #[test]
    fn free_has_no_live_bots_and_no_orderflow() {
        let free = plan(PlanId::Free);
        assert_eq!(free.limits.max_live_bots, 0);
        assert!(!free.features.orderflow);
        assert!(!free.features.orderflow_advanced);
    }

    #[test]
    fn every_paid_plan_increases_or_keeps_the_free_pane_cap() {
        let free = plan(PlanId::Free);
        for plan in catalog().iter().filter(|plan| plan.id.is_paid()) {
            assert!(plan.limits.max_panes >= free.limits.max_panes);
            assert!(plan.limits.max_ai_credits > free.limits.max_ai_credits);
        }
    }

    #[test]
    fn advanced_orderflow_starts_at_pro() {
        assert!(!plan(PlanId::Core).features.orderflow_advanced);
        assert!(plan(PlanId::Pro).features.orderflow_advanced);
    }

    #[test]
    fn community_browsing_starts_at_core_and_publishing_at_elite() {
        assert!(!Feature::CommunityBrowse.allowed_by(&plan(PlanId::Free).features));
        assert!(Feature::CommunityBrowse.allowed_by(&plan(PlanId::Core).features));
        assert!(!Feature::CommunityPublish.allowed_by(&plan(PlanId::Core).features));
        assert!(Feature::CommunityPublish.allowed_by(&plan(PlanId::Elite).features));
        assert_eq!(Feature::CommunityBrowse.minimum_plan(), PlanId::Core);
        assert_eq!(Feature::CommunityPublish.minimum_plan(), PlanId::Elite);
        assert_eq!(
            plan(PlanId::Desk).features.community_indicators,
            CommunityIndicators::Priority
        );
    }

    #[test]
    fn parse_round_trips_wire_names() {
        for plan in PlanId::ALL {
            assert_eq!(PlanId::parse(plan.as_str()), Some(plan));
        }
        assert_eq!(BillingCycle::parse("ANNUAL"), Some(BillingCycle::Annual));
        assert_eq!(PlanId::parse("platinum"), None);
    }
}
