//! Specialized agent modes.
//!
//! One agent process, several narrow responsibilities. A mode selects the
//! tools the model may call and adds a short role instruction to the system
//! prompt. The orchestrator is the only mode the UI needs to talk to; the
//! other modes are available for callers that want a specific specialist.

use serde::{Deserialize, Serialize};

use crate::llm_client::ToolSpec;

/// What this turn is responsible for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    /// Reads market state and explains what happened. No drawings, no trades.
    MarketAnalyst,
    /// Reads order flow and large-participant activity.
    WhaleFlow,
    /// Detects structure, liquidity and SMC zones.
    Smc,
    /// Writes and validates a strategy document.
    Strategy,
    /// Reads volatility and risk evidence. Proposes NO_TRADE freely.
    Risk,
    /// Default. Coordinates the other modes and writes the final thesis.
    Orchestrator,
}

impl Default for AgentMode {
    fn default() -> Self {
        Self::Orchestrator
    }
}

impl AgentMode {
    /// Stable snake_case id for logs and wire payloads.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MarketAnalyst => "market_analyst",
            Self::WhaleFlow => "whale_flow",
            Self::Smc => "smc",
            Self::Strategy => "strategy",
            Self::Risk => "risk",
            Self::Orchestrator => "orchestrator",
        }
    }

    /// Tools this mode may call. `None` means all registered tools.
    pub const fn tools(self) -> Option<&'static [&'static str]> {
        match self {
            Self::MarketAnalyst => Some(&[
                "analyze_timeframe",
                "analyze_multi_timeframe",
                "analyze_multiscale",
                "analyze_markov",
                "fuse_intelligence",
                "get_candles",
                "get_vwap",
                "get_atr",
                "get_moving_average",
                "get_rsi",
                "get_macd",
                "get_bollinger_bands",
                "compare_timeframes",
            ]),
            Self::WhaleFlow => Some(&[
                "get_footprint",
                "get_delta_by_size",
                "get_bar_delta_stats",
                "get_vpin",
                "detect_size_divergence",
                "get_delta",
                "get_cvd",
                "detect_absorption",
                "detect_imbalance",
                "analyze_markov",
                "analyze_multiscale",
                "fuse_intelligence",
            ]),
            Self::Smc => Some(&[
                "detect_liquidity",
                "detect_market_structure",
                "detect_zones",
                "detect_pattern",
                "get_candles",
                "analyze_timeframe",
                "analyze_markov",
                "analyze_multiscale",
                "fuse_intelligence",
            ]),
            Self::Strategy => Some(&[
                "create_strategy",
                "list_concepts",
                "backtest_strategy",
                "backtest_similar_setups",
                "get_candles",
            ]),
            Self::Risk => Some(&[
                "get_atr",
                "get_vwap",
                "get_bollinger_bands",
                "analyze_markov",
                "analyze_multiscale",
                "fuse_intelligence",
                "get_candles",
            ]),
            Self::Orchestrator => None,
        }
    }

    /// Short instruction appended to the system prompt.
    pub const fn instruction(self) -> &'static str {
        match self {
            Self::MarketAnalyst => "ROLE: Market Analyst. Describe market state from tool evidence. Do not draw, size, or propose a trade.",
            Self::WhaleFlow => "ROLE: Whale / Flow Analyst. Report order-flow and large-participant evidence. Headlines are leads, not facts. Do not propose a trade.",
            Self::Smc => "ROLE: Smart Money Concepts analyst. Name structure, liquidity and zones from tool evidence only. Mark NO SETUP when a condition is missing.",
            Self::Strategy => "ROLE: Strategy agent. Produce a validated strategy document or say it cannot be expressed. Never hand an unvalidated document to a bot.",
            Self::Risk => "ROLE: Risk agent. Read volatility and regime evidence, state invalidation, and prefer NO_TRADE when evidence is thin. You cannot authorize execution.",
            Self::Orchestrator => "ROLE: Orchestrator. Combine specialist evidence into one explainable answer. Do not invent numbers the tools did not return.",
        }
    }

    /// Whether this mode may call a named tool.
    pub fn allows_tool(self, name: &str) -> bool {
        match self.tools() {
            None => true,
            Some(allowed) => allowed.contains(&name),
        }
    }

    /// Narrow a tool list to this mode. `submit_thesis` is always kept: it is
    /// how every mode returns an auditable answer.
    pub fn filter_tools(self, tools: Vec<ToolSpec>) -> Vec<ToolSpec> {
        if self.tools().is_none() {
            return tools;
        }
        tools
            .into_iter()
            .filter(|tool| self.allows_tool(&tool.name) || tool.name == "submit_thesis")
            .collect()
    }
}

/// Typed output wrapper for a specialist turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeReport {
    /// Mode that produced the answer.
    pub mode: String,
    /// Instrument analysed.
    pub symbol: String,
    /// Decision timeframe.
    pub timeframe: String,
    /// Direction label from the thesis.
    pub direction: String,
    /// Confidence from the thesis, 0..100.
    pub confidence_pct: f64,
    /// The explainable narrative.
    pub narrative: String,
    /// Tools called, in order, for audit.
    pub tool_calls: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::ToolSpec;

    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
            exposed_tool: None,
        }
    }

    #[test]
    fn every_mode_name_is_unique() {
        let all = [
            AgentMode::MarketAnalyst,
            AgentMode::WhaleFlow,
            AgentMode::Smc,
            AgentMode::Strategy,
            AgentMode::Risk,
            AgentMode::Orchestrator,
        ];
        for (index, mode) in all.iter().enumerate() {
            for other in all.iter().skip(index + 1) {
                assert_ne!(mode.as_str(), other.as_str());
            }
        }
    }

    #[test]
    fn a_specialist_keeps_only_its_tools_and_the_thesis_submit() {
        let tools = vec![spec("get_cvd"), spec("get_candles"), spec("submit_thesis")];
        let filtered = AgentMode::WhaleFlow.filter_tools(tools);
        assert_eq!(
            filtered.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["get_cvd", "submit_thesis"]
        );
    }

    #[test]
    fn the_orchestrator_sees_every_tool() {
        let tools = vec![spec("get_cvd"), spec("get_candles"), spec("submit_thesis")];
        assert_eq!(AgentMode::Orchestrator.filter_tools(tools).len(), 3);
    }
}
