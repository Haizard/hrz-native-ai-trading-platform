//! The footprint ladder: bid x ask per price level, per candle.
//!
//! ## What makes this different from the volume profile
//!
//! The profile in [`crate::scene`] is one histogram for the whole window. A
//! footprint is a *grid*: every candle gets its own ladder, and the ladders share
//! one price axis so a level sits at the same height in every column. That shared
//! axis is the whole point -- it is what lets you read a seller hitting the same
//! price across five consecutive candles.
//!
//! ## The rows come from the data, not from a fixed step
//!
//! A ladder is sparse: a quiet candle has cells at three prices, a busy one at
//! thirty. So the axis is the **union** of every column's levels, and each column
//! fills only the rows it has. Rendering a fixed grid instead would draw a
//! hundred empty rows on a quiet window and hide the structure that matters.
//!
//! ## Text is formatted here, not in the shell
//!
//! `0.1 x 0.4`, `4.09 K`, `-1.25 K` -- the shell draws the string it is given.
//! That keeps the shell free of decisions about precision or magnitude, which is
//! the same rule as everywhere else in this crate. It also means the formatting
//! is unit-tested on the host rather than eyeballed in a browser.

use serde::{Deserialize, Serialize};

/// One price level of one candle, as it arrives from `/footprint`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ColumnCell {
    /// The bucket's midpoint.
    pub price: f64,
    /// Volume executed at the bid -- the seller aggressed.
    pub bid: f64,
    /// Volume executed at the ask -- the buyer aggressed.
    pub ask: f64,
    /// `ask - bid`.
    pub delta: f64,
    /// Present when this level is part of a diagonal imbalance.
    #[serde(default)]
    pub imbalance: Option<Imbalance>,
}

/// An imbalance at one level.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Imbalance {
    /// `buy` or `sell`.
    pub side: String,
    /// Dominant over opposing.
    pub ratio: f64,
    /// Length of the consecutive same-side run.
    pub stacked: usize,
}

/// One candle's ladder, as it arrives from `/footprint`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Column {
    /// Bucket open time, unix nanoseconds.
    pub open_time: i64,
    /// Open.
    pub open: f64,
    /// High.
    pub high: f64,
    /// Low.
    pub low: f64,
    /// Close.
    pub close: f64,
    /// Total volume.
    pub volume: f64,
    /// Total bid volume.
    pub bid_volume: f64,
    /// Total ask volume.
    pub ask_volume: f64,
    /// `ask - bid` for the candle.
    pub delta: f64,
    /// The level with the most volume.
    #[serde(default)]
    pub poc: Option<f64>,
    /// The ladder, ascending by price.
    pub cells: Vec<ColumnCell>,
}

/// A row of the shared price axis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    /// The bucket's midpoint.
    pub price: f64,
    /// Canvas y of the row's top edge.
    pub y: f64,
    /// Row height.
    pub h: f64,
}

/// One drawn cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellScene {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width of the whole column.
    pub w: f64,
    /// Height.
    pub h: f64,
    /// The bucket's midpoint -- the cell's identity.
    ///
    /// The shell can find it via `rows`, but a cell that does not carry its own
    /// price cannot be reasoned about: a tooltip, a highlight or a test all want
    /// to ask "what level is this".
    pub price: f64,
    /// Bid volume.
    pub bid: f64,
    /// Ask volume.
    pub ask: f64,
    /// The bid side, formatted.
    pub bid_text: String,
    /// The ask side, formatted.
    pub ask_text: String,
    /// `ask - bid`.
    pub delta: f64,
    /// `buy`, `sell`, or absent.
    pub side: Option<String>,
    /// Dominant over opposing, when imbalanced.
    pub ratio: Option<f64>,
    /// Run length, when imbalanced.
    pub stacked: Option<usize>,
    /// Whether this price is inside the window's value area.
    pub in_value_area: bool,
    /// Whether this is the column's own point of control.
    pub is_poc: bool,
}

/// The row under a column: what the candle did in total.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
    /// Total volume, formatted.
    pub volume_text: String,
    /// Delta, formatted.
    pub delta_text: String,
    /// Whether the delta was positive, so the shell knows the colour.
    pub delta_positive: bool,
    /// The close, formatted.
    pub close_text: String,
}

/// One column of the grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnScene {
    /// Left edge.
    pub x: f64,
    /// Column width.
    pub w: f64,
    /// Bucket open time.
    pub open_time: i64,
    /// The cells this candle actually has.
    pub cells: Vec<CellScene>,
    /// The row below the plot.
    pub summary: Summary,
}

/// The window's totals, for the footer.
///
/// Every number here is computed by `analytics-core` from the trades; this only
/// formats them. `trades` is the count that went into the ladders, which is the
/// figure that says whether the window has enough data to be worth reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    /// Total bid volume, formatted.
    pub bid_text: String,
    /// Total ask volume, formatted.
    pub ask_text: String,
    /// Total volume, formatted.
    pub total_text: String,
    /// Total delta, formatted.
    pub delta_text: String,
    /// Whether the total delta was positive.
    pub delta_positive: bool,
    /// The largest single-level delta, formatted.
    pub max_delta_text: String,
    /// The smallest single-level delta, formatted.
    pub min_delta_text: String,
    /// Trades that went into the window.
    pub trades: usize,
    /// Columns drawn.
    pub columns: usize,
    /// Rows on the shared axis.
    pub rows: usize,
}

/// A caveat worth showing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// The message.
    pub message: String,
}

/// How the grid came out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    /// The shared price axis, ascending by price (so descending on canvas).
    pub rows: Vec<Row>,
    /// The columns, in time order.
    pub columns: Vec<ColumnScene>,
    /// Window totals.
    pub stats: Stats,
    /// Font size the rows were sized for, so the shell does not guess.
    pub font_px: f64,
    /// Whether the shell should draw the numbers at all.
    ///
    /// Decided here rather than in the shell, because legibility is a property of
    /// the layout and the layout is this crate's. The shell used to carry its own
    /// `font_px >= 7` while this crate clamped at 6.0, and the two drifted apart:
    /// a window of the row count `footprint_routes` targets came out at 6.9px, so
    /// the ladder drew every cell as a colour and not one number.
    pub show_text: bool,
    /// The caveat that goes with a truncated axis, when it was truncated.
    ///
    /// On the grid rather than recomputed by the caller: the cap depends on the
    /// plot height, and a caller working it out a second time is a second answer
    /// waiting to disagree.
    pub note: Option<Note>,
}

/// Rows beyond this and the axis is a smear at any height.
///
/// A ceiling, not the real cap: what a window can carry depends on the plot
/// height, and [`legible_rows`] is where that is worked out. This exists because
/// a caller can hand [`layout`] a plot of any height, and four hundred rows is
/// unreadable at every one of them.
const MAX_ROWS: usize = 80;

/// The smallest font that is still a number rather than a smudge.
///
/// The same number the shell used to hold privately. It belongs to this crate
/// because this crate is what decides how many rows fit.
const MIN_FONT_PX: f64 = 7.0;

/// A row's height as a fraction of the font that sits in it.
///
/// 0.62 was too tight, and the arithmetic is the whole story: `footprint_routes`
/// sizes a window for 45 rows, 45 rows on a 500px plot is an 11.1px row, and
/// 11.1 x 0.62 is 6.9 -- a tenth of a pixel under legibility, so the shell dropped
/// every number. The reference chart in `docs/14` runs an 11px font on 12px rows.
const FONT_PER_ROW: f64 = 0.78;

/// The font floor, for a plot too short to hold a legible row at all.
///
/// Deliberately *below* [`MIN_FONT_PX`]. The cap above makes this unreachable for
/// any real plot, and a floor that could never be reached would make
/// [`Grid::show_text`] a constant -- a field that says nothing, which is the same
/// mistake as a rule over a metric nothing writes.
const FLOOR_FONT_PX: f64 = 4.0;

/// A monospace glyph's width as a fraction of the font size.
///
/// The shell draws in `ui-monospace` and this is the advance that family uses.
/// The engine cannot measure text -- there is no canvas here -- so it estimates,
/// and the shell still drops any pair that does not fit. An estimate that is
/// slightly too generous costs a number; one that is too mean costs an
/// overlapping pair, which is worse.
const GLYPH_PER_FONT: f64 = 0.62;

/// The padding a cell keeps either side of its text, in pixels.
///
/// A constant rather than a function of the font, so the shell's own fit check
/// can be strictly looser than this and never drop what the engine sized for.
const CELL_MARGIN_PX: f64 = 3.0;

/// The most rows a plot of this height can carry and still be read.
///
/// The cap and the font are two ends of one decision, so they are made together.
/// Capping at a constant meant the font fell below legibility and the text simply
/// vanished: a cap that does not do what it claims.
#[must_use]
pub fn legible_rows(plot_height: f64) -> usize {
    let fits = (plot_height / (MIN_FONT_PX / FONT_PER_ROW)).floor();
    (fits.max(1.0) as usize).min(MAX_ROWS)
}

/// The font a cell of this width can hold `pair_chars` characters in.
///
/// The second half of the same decision as [`legible_rows`], and the half that
/// was missing: a row height of 11px wants an 8.6px font, and eleven characters at
/// 8.6px need 59px of a 54px column. So the ladder drew every cell and not one
/// number -- the font was sized by the rows and nothing looked at the width.
#[must_use]
pub fn font_for_width(cell_width: f64, pair_chars: usize) -> f64 {
    let glyphs = pair_chars as f64 * GLYPH_PER_FONT;
    if glyphs <= 0.0 {
        return f64::INFINITY;
    }
    (cell_width - 2.0 * CELL_MARGIN_PX) / glyphs
}

/// Format a volume the way a ladder reads: two significant decimals, then a
/// suffix once the number stops being readable.
///
/// `0.44`, `12.5`, `4.09 K`, `1.21 M`. The reference footprint in `docs/14`
/// shows exactly this convention, and it is the reason a 3-decimal cell is
/// readable at 11px.
#[must_use]
pub fn compact(value: f64) -> String {
    let magnitude = value.abs();
    if magnitude >= 1_000_000.0 {
        format!("{:.2} M", value / 1_000_000.0)
    } else if magnitude >= 10_000.0 {
        format!("{:.1} K", value / 1_000.0)
    } else if magnitude >= 1_000.0 {
        format!("{:.2} K", value / 1_000.0)
    } else if magnitude >= 100.0 {
        format!("{value:.0}")
    } else if magnitude >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

/// Signed, for a delta column where the sign is the point.
#[must_use]
pub fn signed(value: f64) -> String {
    if value > 0.0 {
        format!("+{}", compact(value))
    } else {
        compact(value)
    }
}

/// Lay out the grid.
///
/// Returns `None` when there is nothing to draw, so the caller can decide what
/// to say rather than being handed an empty grid.
#[must_use]
pub fn layout(
    columns: &[Column],
    plot: crate::Plot,
    value_area: Option<(f64, f64)>,
    summary_height: f64,
    trades: usize,
) -> Option<Grid> {
    if columns.is_empty() || columns.iter().all(|column| column.cells.is_empty()) {
        return None;
    }

    // The shared axis: the union of every column's levels. Ascending, because
    // price grows upward and y grows downward -- the rows are then walked in
    // reverse to place them.
    let mut prices: Vec<f64> = columns
        .iter()
        .flat_map(|column| column.cells.iter().map(|cell| cell.price))
        .collect();
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    prices.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    // How many levels the window has, and how many this plot can carry.
    let levels = prices.len();
    let cap = legible_rows(plot.h);
    let truncated = levels > cap;
    if truncated {
        // Keep the middle of the range rather than the bottom: the extremes of
        // a window are usually a single wick, and the structure is where the
        // volume is.
        let drop = (levels - cap) / 2;
        prices = prices[drop..drop + cap].to_vec();
    }

    let rows_len = prices.len();
    let row_height = plot.h / rows_len as f64;
    let slot = plot.w / columns.len() as f64;

    // The widest `bid x ask` pair in the window, which is the one that has to fit.
    // Counted in characters rather than measured: this crate has no font metrics.
    let widest = columns
        .iter()
        .flat_map(|column| column.cells.iter())
        .map(|cell| compact(cell.bid).len().max(compact(cell.ask).len()))
        .max()
        .unwrap_or(0);
    // ` x ` is three characters around the two numbers.
    let pair_chars = widest * 2 + 3;

    // The font has to fit a row *and* a cell, and the smaller of the two wins.
    // The cap above is chosen so the row half lands at or above `MIN_FONT_PX`; the
    // clamp is the last resort for a plot too small for either, and it is what
    // `show_text` reports rather than a threshold the shell keeps.
    let font_px = (row_height * FONT_PER_ROW)
        .min(font_for_width(slot, pair_chars))
        .clamp(FLOOR_FONT_PX, 11.0);

    let rows: Vec<Row> = prices
        .iter()
        .rev()
        .enumerate()
        .map(|(index, price)| Row {
            price: *price,
            y: plot.y + row_height * index as f64,
            h: row_height,
        })
        .collect();

    let (value_low, value_high) = value_area.unwrap_or((f64::INFINITY, f64::NEG_INFINITY));

    let mut max_delta = f64::NEG_INFINITY;
    let mut min_delta = f64::INFINITY;
    let mut total_bid = 0.0;
    let mut total_ask = 0.0;

    let columns_scene: Vec<ColumnScene> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let x = plot.x + slot * index as f64;

            let cells: Vec<CellScene> = column
                .cells
                .iter()
                .filter_map(|cell| {
                    // A cell whose price is off the truncated axis is not drawn;
                    // drawing it at a clamped row would put it at the wrong
                    // price, which is worse than omitting it.
                    let row = rows
                        .iter()
                        .find(|row| (row.price - cell.price).abs() < 1e-9)?;
                    max_delta = max_delta.max(cell.delta);
                    min_delta = min_delta.min(cell.delta);
                    total_bid += cell.bid;
                    total_ask += cell.ask;

                    let is_poc = column
                        .poc
                        .is_some_and(|poc| (poc - cell.price).abs() < 1e-9);
                    Some(CellScene {
                        x,
                        y: row.y,
                        w: slot,
                        h: row.h,
                        price: cell.price,
                        bid: cell.bid,
                        ask: cell.ask,
                        bid_text: compact(cell.bid),
                        ask_text: compact(cell.ask),
                        delta: cell.delta,
                        side: cell.imbalance.as_ref().map(|i| i.side.clone()),
                        ratio: cell.imbalance.as_ref().map(|i| i.ratio),
                        stacked: cell.imbalance.as_ref().map(|i| i.stacked),
                        in_value_area: cell.price >= value_low && cell.price <= value_high,
                        is_poc,
                    })
                })
                .collect();

            let summary_y = plot.y + plot.h + 2.0;
            ColumnScene {
                x,
                w: slot,
                open_time: column.open_time,
                cells,
                summary: Summary {
                    x,
                    y: summary_y,
                    w: slot,
                    h: summary_height,
                    volume_text: compact(column.volume),
                    delta_text: signed(column.delta),
                    delta_positive: column.delta >= 0.0,
                    close_text: compact(column.close),
                },
            }
        })
        .collect();

    let total_delta = total_ask - total_bid;
    if max_delta == f64::NEG_INFINITY {
        max_delta = 0.0;
    }
    if min_delta == f64::INFINITY {
        min_delta = 0.0;
    }

    Some(Grid {
        rows,
        columns: columns_scene,
        stats: Stats {
            bid_text: compact(total_bid),
            ask_text: compact(total_ask),
            total_text: compact(total_bid + total_ask),
            delta_text: signed(total_delta),
            delta_positive: total_delta >= 0.0,
            max_delta_text: signed(max_delta),
            min_delta_text: signed(min_delta),
            trades,
            columns: columns.len(),
            rows: rows_len,
        },
        font_px,
        show_text: font_px >= MIN_FONT_PX,
        note: truncated.then(|| Note {
            message: format!(
                "{levels} price levels in this window, showing the middle {cap}. Ask for fewer \
                 candles or a coarser bucket to see all of them."
            ),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Plot;

    fn plot() -> Plot {
        Plot {
            x: 0.0,
            y: 0.0,
            w: 600.0,
            h: 400.0,
        }
    }

    fn cell(price: f64, bid: f64, ask: f64) -> ColumnCell {
        ColumnCell {
            price,
            bid,
            ask,
            delta: ask - bid,
            imbalance: None,
        }
    }

    fn column(open_time: i64, cells: Vec<ColumnCell>) -> Column {
        let bid: f64 = cells.iter().map(|c| c.bid).sum();
        let ask: f64 = cells.iter().map(|c| c.ask).sum();
        Column {
            open_time,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: bid + ask,
            bid_volume: bid,
            ask_volume: ask,
            delta: ask - bid,
            poc: cells
                .iter()
                .max_by(|a, b| (a.bid + a.ask).partial_cmp(&(b.bid + b.ask)).unwrap())
                .map(|c| c.price),
            cells,
        }
    }

    // --- formatting ---------------------------------------------------------

    #[test]
    fn small_volumes_keep_two_decimals() {
        // A ladder cell is often a fraction of a coin, and `0.4` hides the
        // difference between 0.44 and 0.35.
        assert_eq!(compact(0.44), "0.44");
        assert_eq!(compact(0.1), "0.10");
        assert_eq!(compact(1.5), "1.50");
        assert_eq!(compact(12.34), "12.3");
        assert_eq!(compact(123.4), "123");
    }

    #[test]
    fn large_volumes_get_a_suffix() {
        assert_eq!(compact(1_500.0), "1.50 K");
        assert_eq!(compact(14_700.0), "14.7 K");
        assert_eq!(compact(4_090_000.0), "4.09 M");
    }

    #[test]
    fn a_delta_carries_its_sign() {
        // The sign is the entire information content of a delta column.
        assert_eq!(signed(2.5), "+2.50");
        assert_eq!(signed(-2.5), "-2.50");
        assert_eq!(signed(0.0), "0.00");
    }

    // --- layout -------------------------------------------------------------

    #[test]
    fn nothing_to_draw_is_none_rather_than_an_empty_grid() {
        assert!(layout(&[], plot(), None, 20.0, 0).is_none());
        assert!(layout(&[column(0, vec![])], plot(), None, 20.0, 0).is_none());
    }

    #[test]
    fn the_axis_is_the_union_of_every_column() {
        // A quiet candle at three prices and a busy one at five must share one
        // axis, or the same price lands at a different height in each column --
        // which destroys the only thing a footprint is for.
        let columns = vec![
            column(0, vec![cell(100.0, 1.0, 1.0), cell(101.0, 1.0, 1.0)]),
            column(1, vec![cell(101.0, 1.0, 1.0), cell(102.0, 1.0, 1.0)]),
        ];
        let grid = layout(&columns, plot(), None, 20.0, 10).expect("a grid");
        assert_eq!(grid.rows.len(), 3, "100, 101, 102");

        // The shared level sits at the same y in both columns.
        let first = grid.columns[0]
            .cells
            .iter()
            .find(|c| (c.y - grid.rows[1].y).abs() < 1e-9);
        let second = grid.columns[1]
            .cells
            .iter()
            .find(|c| (c.y - grid.rows[1].y).abs() < 1e-9);
        assert!(first.is_some() && second.is_some(), "both columns have 101");
    }

    #[test]
    fn price_ascends_as_y_descends() {
        let columns = vec![column(
            0,
            vec![cell(100.0, 1.0, 1.0), cell(110.0, 1.0, 1.0)],
        )];
        let grid = layout(&columns, plot(), None, 20.0, 5).expect("a grid");
        // rows are ordered by y ascending, so price must descend across them.
        assert!(grid.rows[0].price > grid.rows[1].price);
        assert!(grid.rows[0].y < grid.rows[1].y);
    }

    #[test]
    fn a_column_only_fills_the_rows_it_has() {
        // Sparse ladders are the normal case, not an error.
        let columns = vec![
            column(0, vec![cell(100.0, 1.0, 1.0)]),
            column(1, vec![cell(105.0, 2.0, 3.0)]),
        ];
        let grid = layout(&columns, plot(), None, 20.0, 5).expect("a grid");
        assert_eq!(grid.rows.len(), 2);
        assert_eq!(grid.columns[0].cells.len(), 1);
        assert_eq!(grid.columns[1].cells.len(), 1);
    }

    #[test]
    fn a_cell_carries_the_text_the_shell_draws() {
        let columns = vec![column(0, vec![cell(100.0, 0.44, 12.5)])];
        let grid = layout(&columns, plot(), None, 20.0, 5).expect("a grid");
        let cell = &grid.columns[0].cells[0];
        assert_eq!(cell.bid_text, "0.44");
        assert_eq!(cell.ask_text, "12.5");
        assert!((cell.delta - 12.06).abs() < 1e-9);
        assert_eq!(cell.price, 100.0, "a cell knows its own level");
    }

    #[test]
    fn an_imbalance_is_carried_through_for_colouring() {
        let mut imbalanced = cell(100.0, 0.4, 2.4);
        imbalanced.imbalance = Some(Imbalance {
            side: "buy".into(),
            ratio: 6.0,
            stacked: 3,
        });
        let columns = vec![column(0, vec![imbalanced])];
        let grid = layout(&columns, plot(), None, 20.0, 5).expect("a grid");
        let cell = &grid.columns[0].cells[0];
        assert_eq!(cell.side.as_deref(), Some("buy"));
        assert_eq!(cell.ratio, Some(6.0));
        assert_eq!(cell.stacked, Some(3));
    }

    #[test]
    fn the_value_area_marks_the_levels_inside_it() {
        let columns = vec![column(
            0,
            vec![
                cell(100.0, 1.0, 1.0),
                cell(105.0, 1.0, 1.0),
                cell(110.0, 1.0, 1.0),
            ],
        )];
        let grid = layout(&columns, plot(), Some((102.0, 108.0)), 20.0, 5).expect("a grid");
        let marked: Vec<bool> = grid.columns[0]
            .cells
            .iter()
            .map(|c| c.in_value_area)
            .collect();
        assert_eq!(
            marked.iter().filter(|m| **m).count(),
            1,
            "only 105 is inside"
        );
    }

    #[test]
    fn the_point_of_control_is_marked_per_column() {
        let columns = vec![column(
            0,
            vec![cell(100.0, 1.0, 1.0), cell(105.0, 9.0, 9.0)],
        )];
        let grid = layout(&columns, plot(), None, 20.0, 5).expect("a grid");
        let pocs: Vec<f64> = grid.columns[0]
            .cells
            .iter()
            .filter(|c| c.is_poc)
            .map(|c| c.delta)
            .collect();
        assert_eq!(pocs.len(), 1, "exactly one POC per column");
    }

    #[test]
    fn every_column_gets_a_summary_row_below_the_plot() {
        let columns = vec![
            column(0, vec![cell(100.0, 5.0, 1.0)]),
            column(1, vec![cell(100.0, 1.0, 5.0)]),
        ];
        let grid = layout(&columns, plot(), None, 20.0, 5).expect("a grid");
        for (index, column) in grid.columns.iter().enumerate() {
            assert!(column.summary.y >= plot().y + plot().h);
            assert_eq!(column.summary.h, 20.0);
            assert!(
                (column.summary.x - column.x).abs() < 1e-9,
                "column {index} summary must sit under it"
            );
        }
        // And the delta sign is carried, because it decides the colour.
        assert!(!grid.columns[0].summary.delta_positive, "bid-heavy");
        assert!(grid.columns[1].summary.delta_positive, "ask-heavy");
    }

    #[test]
    fn the_stats_total_the_window() {
        let columns = vec![
            column(0, vec![cell(100.0, 5.0, 1.0)]),
            column(1, vec![cell(100.0, 1.0, 5.0)]),
        ];
        let grid = layout(&columns, plot(), None, 20.0, 42).expect("a grid");
        assert_eq!(grid.stats.trades, 42);
        assert_eq!(grid.stats.columns, 2);
        assert_eq!(grid.stats.bid_text, "6.00");
        assert_eq!(grid.stats.ask_text, "6.00");
        assert_eq!(grid.stats.total_text, "12.0");
        // 6 ask - 6 bid: balanced, so the sign is not negative.
        assert_eq!(grid.stats.delta_text, "0.00");
        assert_eq!(grid.stats.max_delta_text, "+4.00");
        assert_eq!(grid.stats.min_delta_text, "-4.00");
    }

    #[test]
    fn a_deep_window_is_truncated_to_what_the_plot_can_carry_and_says_so() {
        let cells: Vec<ColumnCell> = (0..200).map(|i| cell(100.0 + i as f64, 1.0, 1.0)).collect();
        let grid = layout(&[column(0, cells)], plot(), None, 20.0, 100).expect("a grid");
        assert_eq!(grid.rows.len(), legible_rows(plot().h));
        assert!(grid.rows.len() < 200, "200 levels do not fit a 400px plot");
        let note = grid.note.expect("a caveat");
        assert!(note.message.contains("200"), "{}", note.message);
        // The truncation exists to keep the text legible, so a truncated grid that
        // cannot draw its numbers would be the cap failing at the one thing it is
        // for.
        assert!(grid.show_text, "font {}px after truncating", grid.font_px);
    }

    #[test]
    fn a_shallow_window_is_not_truncated() {
        let grid = layout(&[column(0, vec![cell(100.0, 1.0, 1.0)])], plot(), None, 20.0, 1)
            .expect("a grid");
        assert!(grid.note.is_none());
    }

    #[test]
    fn the_row_count_the_route_targets_still_draws_its_numbers() {
        // `footprint_routes` sizes its bucket for 45 rows, so this is the row
        // count a real window has -- and at the old 0.62 font-per-row it came out
        // at 6.9px on a 500px plot, a tenth of a pixel under the shell's
        // threshold. Every cell was drawn and not one number was, which is what
        // "the footprint is not well presented" turned out to mean.
        let cells: Vec<ColumnCell> = (0..45).map(|i| cell(100.0 + i as f64, 1.0, 1.0)).collect();
        let plot = Plot { x: 0.0, y: 0.0, w: 600.0, h: 500.0 };
        let grid = layout(&[column(0, cells)], plot, None, 20.0, 1).expect("a grid");
        assert_eq!(grid.rows.len(), 45, "45 rows fit a 500px plot");
        assert!(
            grid.show_text,
            "45 rows is the row count the route targets, and {}px draws nothing",
            grid.font_px
        );
    }

    #[test]
    fn the_font_also_has_to_fit_the_width_of_a_cell() {
        // The half that was missing. 45 rows on a 500px plot wants an 8.6px font,
        // and `1.40 x 2.85` at 8.6px is 59px of a 54px column -- so the font has
        // to come down to what the cell can hold, not just what the row can.
        let cells: Vec<ColumnCell> = (0..45).map(|i| cell(100.0 + i as f64, 1.4, 2.85)).collect();
        // Twenty columns across a 1080px plot is the 54px column the shell sizes
        // for (`MIN_COLUMN_PX` in `loadFootprint`).
        let columns: Vec<Column> = (0..20).map(|i| column(i as i64, cells.clone())).collect();
        let plot = Plot { x: 0.0, y: 0.0, w: 1080.0, h: 500.0 };
        let grid = layout(&columns, plot, None, 20.0, 1).expect("a grid");

        assert_eq!(grid.rows.len(), 45);
        assert!(grid.show_text, "{}px in a 54px cell", grid.font_px);

        // And the claim is the arithmetic, not the flag: an 11-character pair at
        // this font has to fit the cell the shell will draw it in.
        let pair_chars = "1.40 x 2.85".len();
        let width = pair_chars as f64 * GLYPH_PER_FONT * grid.font_px;
        let cell = grid.columns[0].w;
        assert!(
            width <= cell - 2.0 * CELL_MARGIN_PX,
            "{pair_chars} characters at {}px is {width}px of a {cell}px cell",
            grid.font_px
        );
    }

    #[test]
    fn a_narrow_cell_says_it_cannot_draw_its_numbers() {
        // The other end: forty columns across the same plot is a 27px cell, and
        // eleven characters do not fit one at any legible size. The honest answer
        // is a heat map, and `show_text` is how it is said.
        let cells: Vec<ColumnCell> = (0..10).map(|i| cell(100.0 + i as f64, 1.4, 2.85)).collect();
        let columns: Vec<Column> = (0..40).map(|i| column(i as i64, cells.clone())).collect();
        let plot = Plot { x: 0.0, y: 0.0, w: 1080.0, h: 500.0 };
        let grid = layout(&columns, plot, None, 20.0, 1).expect("a grid");
        assert!(!grid.show_text, "{}px in a 27px cell", grid.font_px);
    }

    #[test]
    fn a_plot_too_short_for_a_row_says_it_cannot_draw_text() {
        // The one case `show_text` is for. It is true for every real plot -- the
        // cap above sees to that -- and a field that could never be false would be
        // a field that says nothing, so this pins the case where it is not.
        let plot = Plot { x: 0.0, y: 0.0, w: 600.0, h: 8.0 };
        let grid = layout(&[column(0, vec![cell(100.0, 1.0, 1.0)])], plot, None, 20.0, 1)
            .expect("a grid");
        assert_eq!(grid.rows.len(), 1, "one row, because zero rows is not a grid");
        assert!(grid.font_px < MIN_FONT_PX, "{}px in an 8px plot", grid.font_px);
        assert!(!grid.show_text, "an 8px plot cannot hold a 7px number");
    }

    #[test]
    fn the_grid_serializes_for_the_shell() {
        let columns = vec![column(0, vec![cell(100.0, 0.4, 2.4)])];
        let grid = layout(&columns, plot(), None, 20.0, 7).expect("a grid");
        let json = serde_json::to_value(&grid).expect("must serialize");
        assert!(json["rows"].is_array());
        assert!(json["columns"][0]["cells"][0]["bid_text"].is_string());
        assert_eq!(json["stats"]["trades"], 7);
        // The two fields the shell reads to decide whether to draw a number at
        // all. A rename here is not a compile error anywhere -- it is a blank
        // ladder -- which is the whole reason this test exists.
        assert!(json["font_px"].is_number());
        assert!(json["show_text"].is_boolean());
    }
}
