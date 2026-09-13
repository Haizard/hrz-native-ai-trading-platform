//! Imbalance -- where one side of the book overwhelmed the other.
//!
//! An imbalance marks a price where aggressive orders massively outnumbered
//! the passive ones on the neighbouring level. Those prices are the ones that
//! got *rejected*: the market moved through them fast, or refused to trade
//! there at all. Traders read them as the footprint of institutional flow, and
//! a **stack** of them (several consecutive levels, same side) as a directional
//! wall.
//!
//! ## The diagonal convention
//!
//! With `diagonal = true` (the default, and the one footprint software uses):
//!
//! * **Buy imbalance** at level `i`: `ask[i] >= ratio * bid[i-1]` -- the
//!   aggression at this price is measured against the *passive* side one level
//!   **below**.
//! * **Sell imbalance** at level `i`: `bid[i] >= ratio * ask[i+1]` -- measured
//!   against the passive side one level **above**.
//!
//! That asymmetry is deliberate: it compares the side that *pushed* with the
//! side that *failed to appear* in the direction of travel.
//!
//! With `diagonal = false` the comparison is vertical -- ask vs bid at the
//! **same** level.
//!
//! ## Why both sides must have volume
//!
//! Cells include empty levels (see [`footprint`](crate::footprint)), so a
//! populated cell is always adjacent to an empty one somewhere. If a zero
//! opposing volume counted as an imbalance, every edge of every candle would
//! register one. Requiring `opposing > 0` means "there was a two-sided auction
//! here and one side won decisively" -- which is the actual signal, and it also
//! keeps `ratio` finite so the result serializes cleanly for the AI tools.

use serde::{Deserialize, Serialize};

use crate::footprint::FootprintCandle;
use crate::types::Side;

/// The classic footprint threshold: the dominant side must be 3x the other.
pub const DEFAULT_IMBALANCE_RATIO: f64 = 3.0;

/// Tuning for [`detect_imbalances_with`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ImbalanceConfig {
    /// Multiple of the opposing volume required to call it an imbalance.
    pub ratio_threshold: f64,
    /// Compare against the diagonal neighbour rather than the same level.
    pub diagonal: bool,
    /// Only report events that are part of a run of at least this many
    /// consecutive same-side imbalances.
    pub min_stack: usize,
}

impl Default for ImbalanceConfig {
    fn default() -> Self {
        Self {
            ratio_threshold: DEFAULT_IMBALANCE_RATIO,
            diagonal: true,
            min_stack: 1,
        }
    }
}

/// One detected imbalance at one price level.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ImbalanceEvent {
    /// Index into [`FootprintCandle::cells`].
    pub index: usize,
    /// Price level of the imbalanced cell.
    pub price_level: f64,
    /// Which side dominated. `Buy` means aggressive buying (ask side) won.
    pub side: Side,
    /// `dominant / opposing`; always finite and `>= ratio_threshold`.
    pub ratio: f64,
    /// Volume on the dominant side.
    pub volume: f64,
    /// Volume on the opposing side.
    pub opposing_volume: f64,
    /// Length of the consecutive same-side run this event belongs to (`>= 1`).
    pub stacked: usize,
}

impl ImbalanceEvent {
    /// Whether buyers dominated at this level.
    #[must_use]
    pub const fn is_buy(&self) -> bool {
        matches!(self.side, Side::Buy)
    }

    /// Whether sellers dominated at this level.
    #[must_use]
    pub const fn is_sell(&self) -> bool {
        matches!(self.side, Side::Sell)
    }

    /// Whether this event is part of a stack of at least `levels` events.
    #[must_use]
    pub const fn is_stacked(&self, levels: usize) -> bool {
        self.stacked >= levels
    }
}

/// Detected imbalances using the default diagonal convention and no stack
/// filter.
///
/// `ratio_threshold` is the dominant/opposing multiple, e.g. `3.0` for 300%.
#[must_use]
pub fn detect_imbalances(footprint: &FootprintCandle, ratio_threshold: f64) -> Vec<ImbalanceEvent> {
    detect_imbalances_with(
        footprint,
        &ImbalanceConfig {
            ratio_threshold,
            ..ImbalanceConfig::default()
        },
    )
}

/// Detected imbalances with explicit tuning.
#[must_use]
pub fn detect_imbalances_with(
    footprint: &FootprintCandle,
    config: &ImbalanceConfig,
) -> Vec<ImbalanceEvent> {
    let cells = &footprint.cells;
    // A diagonal comparison needs a neighbour; a vertical one does not.
    if cells.is_empty() || (config.diagonal && cells.len() < 2) {
        return Vec::new();
    }

    // A non-positive or non-finite threshold would flag everything or nothing;
    // fall back to the documented default rather than silently misbehaving.
    let ratio = if config.ratio_threshold.is_finite() && config.ratio_threshold > 0.0 {
        config.ratio_threshold
    } else {
        DEFAULT_IMBALANCE_RATIO
    };

    let mut raw: Vec<RawImbalance> = Vec::new();

    for (i, cell) in cells.iter().enumerate() {
        let opposing_bid = if config.diagonal {
            i.checked_sub(1)
                .and_then(|j| cells.get(j))
                .map_or(0.0, |c| c.bid_volume)
        } else {
            cell.bid_volume
        };
        if cell.ask_volume > 0.0 && opposing_bid > 0.0 && cell.ask_volume >= ratio * opposing_bid {
            raw.push(RawImbalance {
                index: i,
                side: Side::Buy,
                ratio: cell.ask_volume / opposing_bid,
                volume: cell.ask_volume,
                opposing_volume: opposing_bid,
            });
        }

        let opposing_ask = if config.diagonal {
            cells.get(i + 1).map_or(0.0, |c| c.ask_volume)
        } else {
            cell.ask_volume
        };
        if cell.bid_volume > 0.0 && opposing_ask > 0.0 && cell.bid_volume >= ratio * opposing_ask {
            raw.push(RawImbalance {
                index: i,
                side: Side::Sell,
                ratio: cell.bid_volume / opposing_ask,
                volume: cell.bid_volume,
                opposing_volume: opposing_ask,
            });
        }
    }

    let stacked = stack_lengths(&raw);
    let min_stack = config.min_stack.max(1);

    let mut events = Vec::with_capacity(raw.len());
    for (entry, run) in raw.iter().zip(stacked) {
        if run < min_stack {
            continue;
        }
        events.push(ImbalanceEvent {
            index: entry.index,
            price_level: cells[entry.index].price_level,
            side: entry.side,
            ratio: entry.ratio,
            volume: entry.volume,
            opposing_volume: entry.opposing_volume,
            stacked: run,
        });
    }

    events
}

/// Whether any run of `levels` consecutive same-side imbalances exists.
#[must_use]
pub fn has_stacked_imbalance(events: &[ImbalanceEvent], levels: usize) -> bool {
    events.iter().any(|e| e.is_stacked(levels))
}

/// A raw hit before its price level and stack size are attached.
struct RawImbalance {
    index: usize,
    side: Side,
    ratio: f64,
    volume: f64,
    opposing_volume: f64,
}

/// For each raw hit, the length of the maximal run of consecutive levels with
/// the same side that contains it.
fn stack_lengths(raw: &[RawImbalance]) -> Vec<usize> {
    let mut lengths = vec![1usize; raw.len()];
    let mut start = 0usize;

    while start < raw.len() {
        let mut end = start + 1;
        while end < raw.len()
            && raw[end].side == raw[start].side
            && raw[end].index == raw[end - 1].index + 1
        {
            end += 1;
        }

        let run = end - start;
        for slot in lengths.iter_mut().take(end).skip(start) {
            *slot = run;
        }
        start = end;
    }

    lengths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Candle, FootprintCell, Timeframe};

    fn candle() -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time: 0,
            open: 100.0,
            high: 104.0,
            low: 100.0,
            close: 104.0,
            volume: 0.0,
            buy_volume: 0.0,
            sell_volume: 0.0,
        }
    }

    /// Build a footprint straight from `(bid, ask)` pairs at ascending levels.
    fn footprint(levels: &[(f64, f64)]) -> FootprintCandle {
        let cells = levels
            .iter()
            .enumerate()
            .map(|(i, (bid, ask))| {
                #[allow(clippy::cast_precision_loss)]
                let price_level = 100.5 + i as f64;
                FootprintCell {
                    price_level,
                    bid_volume: *bid,
                    ask_volume: *ask,
                    delta: ask - bid,
                }
            })
            .collect();
        FootprintCandle {
            candle: candle(),
            cells,
            imbalances: Vec::new(),
        }
    }

    #[test]
    fn diagonal_buy_imbalance_is_ask_here_versus_bid_below() {
        // Level 1: ask 30 vs level 0's bid 5 -> 6x, above the 3x default.
        let fp = footprint(&[(5.0, 0.0), (5.0, 30.0)]);
        let events = detect_imbalances(&fp, DEFAULT_IMBALANCE_RATIO);
        assert_eq!(events.len(), 1);
        assert!(events[0].is_buy());
        assert_eq!(events[0].index, 1);
        assert!((events[0].ratio - 6.0).abs() < 1e-9);
        assert!((events[0].price_level - 101.5).abs() < 1e-9);
    }

    #[test]
    fn diagonal_sell_imbalance_is_bid_here_versus_ask_above() {
        // Level 0: bid 40 vs level 1's ask 5 -> 8x.
        let fp = footprint(&[(40.0, 0.0), (0.0, 5.0)]);
        let events = detect_imbalances(&fp, DEFAULT_IMBALANCE_RATIO);
        assert_eq!(events.len(), 1);
        assert!(events[0].is_sell());
        assert_eq!(events[0].index, 0);
        assert!((events[0].ratio - 8.0).abs() < 1e-9);
    }

    #[test]
    fn a_level_with_no_opposing_volume_is_not_an_imbalance() {
        // The bottom cell trades, the one above is empty: with a diagonal rule
        // the populated cell has no opposing side, so nothing fires.
        let fp = footprint(&[(100.0, 100.0), (0.0, 0.0)]);
        assert!(detect_imbalances(&fp, DEFAULT_IMBALANCE_RATIO).is_empty());
    }

    #[test]
    fn balanced_levels_are_not_imbalances() {
        let fp = footprint(&[(10.0, 10.0), (10.0, 10.0), (10.0, 10.0)]);
        assert!(detect_imbalances(&fp, DEFAULT_IMBALANCE_RATIO).is_empty());
    }

    #[test]
    fn threshold_is_configurable() {
        // 2x: below the 3x default, above an explicit 1.5x.
        let fp = footprint(&[(10.0, 0.0), (0.0, 20.0)]);
        assert!(detect_imbalances(&fp, DEFAULT_IMBALANCE_RATIO).is_empty());
        assert_eq!(detect_imbalances(&fp, 1.5).len(), 1);
    }

    #[test]
    fn vertical_mode_compares_within_the_same_cell() {
        let fp = footprint(&[(5.0, 50.0)]);
        // Diagonal needs a neighbour, so a single cell yields nothing...
        assert!(detect_imbalances(&fp, DEFAULT_IMBALANCE_RATIO).is_empty());
        // ...but vertical mode sees 50 vs 5.
        let config = ImbalanceConfig {
            diagonal: false,
            ..ImbalanceConfig::default()
        };
        let events = detect_imbalances_with(&fp, &config);
        assert_eq!(events.len(), 1);
        assert!(events[0].is_buy());
    }

    #[test]
    fn consecutive_same_side_levels_form_a_stack() {
        // Level 1: ask 50 vs bid[0] 1. Level 2: ask 60 vs bid[1] 5. Both buys.
        let fp = footprint(&[(1.0, 0.0), (5.0, 50.0), (6.0, 60.0)]);
        let events = detect_imbalances(&fp, DEFAULT_IMBALANCE_RATIO);
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(ImbalanceEvent::is_buy));
        assert_eq!(events[0].stacked, 2);
        assert_eq!(events[1].stacked, 2);
        assert!(has_stacked_imbalance(&events, 2));
    }

    #[test]
    fn min_stack_filters_out_lone_imbalances() {
        let fp = footprint(&[(5.0, 0.0), (5.0, 50.0)]);
        let config = ImbalanceConfig {
            min_stack: 2,
            ..ImbalanceConfig::default()
        };
        assert!(detect_imbalances_with(&fp, &config).is_empty());
        assert!(!detect_imbalances(&fp, DEFAULT_IMBALANCE_RATIO).is_empty());
    }

    #[test]
    fn a_single_imbalance_reports_a_stack_of_one() {
        let fp = footprint(&[(5.0, 0.0), (5.0, 50.0)]);
        let events = detect_imbalances(&fp, DEFAULT_IMBALANCE_RATIO);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].stacked, 1);
        assert!(!has_stacked_imbalance(&events, 2));
    }

    #[test]
    fn opposite_sides_on_adjacent_levels_do_not_stack() {
        // Level 1: ask 10 vs bid[0] 1 -> buy. Level 2: bid 10 vs ask[3] 1 -> sell.
        // Adjacent indices, opposite sides: two runs of one, not a stack of two.
        let fp = footprint(&[(1.0, 0.0), (0.0, 10.0), (10.0, 0.0), (0.0, 1.0)]);
        let events = detect_imbalances(&fp, DEFAULT_IMBALANCE_RATIO);
        assert_eq!(events.len(), 2);
        assert!(events[0].is_buy());
        assert!(events[1].is_sell());
        assert!(events.iter().all(|e| e.stacked == 1));
        assert!(!has_stacked_imbalance(&events, 2));
    }

    #[test]
    fn too_few_cells_yields_nothing() {
        assert!(detect_imbalances(&footprint(&[]), DEFAULT_IMBALANCE_RATIO).is_empty());
        assert!(detect_imbalances(&footprint(&[(10.0, 10.0)]), DEFAULT_IMBALANCE_RATIO).is_empty());
    }

    #[test]
    fn a_non_positive_threshold_falls_back_to_the_default() {
        let fp = footprint(&[(10.0, 0.0), (0.0, 20.0)]);
        let config = ImbalanceConfig {
            ratio_threshold: 0.0,
            ..ImbalanceConfig::default()
        };
        // 2x does not clear the 3x default.
        assert!(detect_imbalances_with(&fp, &config).is_empty());
    }

    #[test]
    fn ratios_are_finite_and_above_the_threshold() {
        // Level 1: bid 1 (thin). Level 2: ask 9 vs that bid -> 9x buy.
        //                    Level 2: bid 9 vs level 3's ask 1 -> 9x sell.
        let fp = footprint(&[(0.0, 0.0), (1.0, 0.0), (9.0, 9.0), (0.0, 1.0), (0.0, 0.0)]);
        let events = detect_imbalances(&fp, 2.0);
        assert_eq!(events.len(), 2);
        assert!(events[0].is_buy());
        assert!(events[1].is_sell());
        for event in &events {
            assert!(event.ratio.is_finite());
            assert!(event.ratio >= 2.0);
            assert!(event.volume > 0.0);
            assert!(event.opposing_volume > 0.0);
        }
    }
}
