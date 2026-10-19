//! The interpreter: per-bar execution over a host-provided window.
//!
//! One [`Vm`] owns the whole script run: it walks bars 0..N and, for each bar,
//! walks the AST. Variables are ring buffers (one value per bar) so `x[3]`
//! and `ta.sma(x, 9)` read the same history the chart would draw. `na` is
//! `f64::NAN` and propagates arithmetically; comparisons with `na` are false;
//! plots skip `na` rather than drawing it.
//!
//! The VM is deterministic and side-effect-free: it reads candles and input
//! values, and writes [`Output`] (plots, hlines, shapes, strategy intents).
//! Everything time-dependent is supplied by the host, which is what makes the
//! native run and the sandboxed run byte-identical.

use analytics_core::types::Candle;

use crate::parse::{Arg, BinOp, BlockKind, CmpOp, Expr, ExprKind, Item, Script, UnOp, VarMode};
use crate::ta::{self, NA, Series};

/// Host-supplied input values, by variable name. Anything the script declares
/// with `input.*` but the host does not supply runs on its declared default.
#[derive(Debug, Default, Clone)]
pub struct Inputs {
    /// Numeric inputs (`input.int` / `input.float`).
    pub numbers: std::collections::HashMap<String, f64>,
    /// Boolean inputs.
    pub bools: std::collections::HashMap<String, bool>,
    /// String inputs (`title`s live in the script; these are values).
    pub strings: std::collections::HashMap<String, String>,
    /// The second instrument (`sec="..."` in the header), already fetched
    /// and time-aligned by the host: candle i covers the SAME bar window as
    /// the script's own candle i. When the second market did not trade a
    /// bar the host still supplies a slot (carry-forward close, flat OHLC),
    /// so indexes never drift -- cross-market math stays bar-aligned.
    /// Empty when the header declares no `sec`.
    pub security: Vec<Candle>,
    /// The chart instrument's tick size, from the host's instrument
    /// metadata (exchange info). `syminfo.mintick` and
    /// `math.round_to_mintick` read it; None when the host did not supply
    /// one, and both are loud about it rather than guessing a tick.
    pub mintick: Option<f64>,
    /// Every named pair of `request.security("SYM", "tf", ...)` in the
    /// script, keyed `SYM@tf`, uppercased, already fetched and aligned onto
    /// the chart's bars by the host -- the same contract as `security`.
    /// Phase 11 (docs/23): multi-pair, multi-timeframe cross-market math.
    /// The chart's own symbol/timeframe may be named too (a script reading
    /// its primary at a coarser tf); the host serves it from the same pool.
    /// Empty when the script names none. Cap: 8 keys (see `check_with_header`).
    pub series_pool: std::collections::HashMap<String, Vec<Candle>>,
    /// Host-supplied data series for `request.data("name")` (docs/23 Phase
    /// 14): platform-native feeds (ticker fields, funding, OI) as per-bar
    /// values aligned onto the chart's bars. A name the host did not fill
    /// reads as the VM's data-missing error -- the truth, not a zero.
    pub data_series: std::collections::HashMap<String, Vec<f64>>,
}

/// One collected plot series.
#[derive(Debug, Clone, PartialEq)]
pub struct Plot {
    /// Stable id, in source order: `p0`, `p1`, ...
    pub id: String,
    /// `title=`, or `plot 3`.
    pub title: String,
    /// The per-bar values; `na` bars are skipped by the renderer.
    pub values: Series,
    /// How the renderer draws it.
    pub style: PlotStyle,
    /// Packed RGBA from `color=`.
    pub color: u32,
    /// `linewidth=`.
    pub linewidth: f64,
    /// Whether the series was produced by a histogram-style plot.
    pub kind: PlotKind,
    /// `plotcandle`/`plotbar` carry four series here; None for line plots.
    pub candles: Option<PlotCandles>,
    /// `offset=`: the renderer shifts the plot this many bars (positive
    /// right, negative left). The values array stays in bar order; the
    /// shift is pure presentation, like Pine.
    pub offset: f64,
}

/// One `table.new` panel: cells update in place per bar (the dashboard
/// pattern), and the LAST bar's contents serialize.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OutputTable {
    /// "top_right" | "top_left" | "bottom_right" | "bottom_left" |
    /// "top_center" | "bottom_center" | "middle_right" | "middle_left" |
    /// "middle_center".
    pub position: String,
    /// Grid width, in cells.
    pub columns: usize,
    /// Grid height, in cells.
    pub rows: usize,
    /// Only the cells a `table.cell` wrote, keyed (column, row).
    pub cells: std::collections::HashMap<(usize, usize), OutputTableCell>,
}

/// One written table cell.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputTableCell {
    /// The cell's text (a string expression, so str.format works).
    pub text: String,
    /// `bgcolor=` override.
    pub bg: Option<u32>,
    /// `text_color=` override.
    pub text_color: Option<u32>,
}

/// One `fill(a, b, color=...)` band, replayed against completed buffers.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlotFill {
    /// First boundary series.
    pub a: Series,
    /// Second boundary series.
    pub b: Series,
    /// Per-bar color (None = na, that bar's segment draws nothing).
    pub color: Vec<Option<u32>>,
}

/// The four series a `plotcandle`/`plotbar` draws, plus per-bar colors.
/// `Plot.values` mirrors the close for the legend's last-value readout.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlotCandles {
    /// Open series.
    pub open: Series,
    /// High series.
    pub high: Series,
    /// Low series.
    pub low: Series,
    /// Close series.
    pub close: Series,
    /// Per-bar body (and border) color; absent color= defaults to
    /// TradingView's up/down green/red.
    pub body: Vec<u32>,
    /// Per-bar wick color; absent wickcolor= follows the body.
    pub wick: Vec<u32>,
    /// true = `plotbar` (OHLC ticks), false = `plotcandle` (bodies).
    pub bars: bool,
}

/// Visual style, from `style=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlotStyle {
    /// A line, skipping na.
    #[default]
    Line,
    /// A line that breaks at na instead of bridging it.
    LineBr,
    /// Histogram from the pane's zero line.
    Histogram,
    /// Columns (histogram that always renders, even at zero).
    Columns,
    /// Circles at each point.
    Circles,
    /// Step line.
    StepLine,
    /// Area fill to the pane bottom.
    AreaBr,
}

/// Plot family, which decides the renderer's primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlotKind {
    /// A continuous series.
    #[default]
    Line,
    /// A point marker (plotshape/plotchar).
    Shape,
    /// A per-bar arrow (plotarrow).
    Arrow,
    /// `plotcandle`/`plotbar` -- excluded from line rendering; the chart
    /// engine decomposes the OHLC series into wick lines and body boxes.
    Candles,
}

/// A horizontal reference level from `hline()`.
#[derive(Debug, Clone, PartialEq)]
pub struct HLine {
    /// The price/value it sits at.
    pub value: f64,
    /// `title=`.
    pub title: String,
    /// Packed RGBA.
    pub color: u32,
    /// Dashed/dotted; the renderer maps linestyle to dash patterns.
    pub dashed: bool,
}

/// A point marker from `plotshape`/`plotchar`.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    /// Bar index.
    pub bar: usize,
    /// The value to place it at (`location.absolute`) or the bar extreme.
    pub value: f64,
    /// Text/shape name, for the renderer's glyph table.
    pub glyph: String,
    /// Packed RGBA.
    pub color: u32,
}

/// One strategy intent recorded during the run.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// `strategy.entry(id, direction, qty=)`.
    Entry {
        /// Order id.
        id: String,
        /// `long` or `short`.
        long: bool,
        /// Units of the asset when the call passed `qty=`; None sizes from
        /// the header's `default_qty_*` knobs (docs/24 S1).
        qty: Option<f64>,
    },
    #[allow(dead_code)]
    /// `strategy.exit(id, stop=, limit=, qty_percent=, ...)`.
    Exit {
        /// Order id.
        id: String,
        /// Optional stop price.
        stop: Option<f64>,
        /// Optional limit price.
        limit: Option<f64>,
        /// Fraction of the position this order closes when it triggers
        /// (Pine's qty_percent, 0..100). None = the whole position. The
        /// bracket stays armed on the remainder -- scale-out ladders arm
        /// the next leg with a fresh exit call.
        qty_percent: Option<f64>,
    },
    /// `strategy.close(id)` / `strategy.close_all()`.
    Close {
        /// The entry id, or all when empty.
        id: Option<String>,
        /// Fraction to close at market (Pine's qty_percent); None = all.
        qty_percent: Option<f64>,
    },
}

/// What one run produced.
#[derive(Debug, Default, Clone)]
pub struct Output {
    /// Plots in source order.
    pub plots: Vec<Plot>,
    /// Horizontal levels.
    pub hlines: Vec<HLine>,
    /// Point markers.
    pub shapes: Vec<Shape>,
    /// Drawing objects from `line.new` / `label.new` / `box.new`
    /// (docs/23 Phase 13): anchored to bar-index + price coordinates the
    /// script chose, in creation order. Capped like the array heap: past
    /// the cap the draw calls become no-ops and `objects_truncated` reports
    /// it, the way TradingView stops drawing instead of failing.
    pub objects: Vec<ScriptObject>,
    /// `fill(a, b, color=...)` bands: the two series + per-bar colors
    /// (None = na segment, nothing drawn).
    pub fills: Vec<PlotFill>,
    /// `bgcolor(...)`: per-bar chart tints, one array per call site.
    pub tints: Vec<Vec<Option<u32>>>,
    /// `barcolor(...)`: per-bar recolors of the MAIN candles.
    pub barcolors: Vec<Vec<Option<u32>>>,
    /// `table.new` panels, in creation order.
    pub tables: Vec<OutputTable>,
    /// The strategy simulation (docs/24 S1): orders, equity curve, account
    /// snapshots and the report. None for indicator scripts; Some for every
    /// run that recorded `strategy.*` intents or (with the header block)
    /// read account state.
    pub simulation: Option<crate::sim::Simulation>,
    /// Non-fatal notes: run warnings that must reach the user (the S1
    /// pass-mismatch note, S3's simulation_note) without failing the run.
    pub notes: Vec<String>,
    /// True when a script tried to draw past [`MAX_OBJECTS`]: the scene
    /// note says so, and the first [`MAX_OBJECTS`] objects still render.
    pub objects_truncated: bool,
    /// Strategy intents, in bar order.
    pub intents: Vec<(usize, Intent)>,
    /// Strategy state reads the script made, for the report.
    pub strategy_used: bool,
}

/// One drawing object. Bar coordinates are INDexes into the run window
/// (negative counts back from the last bar, Pine's convention for
/// `bar_index - n`); the engine maps them to time/canvas, never the script.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptObject {
    /// `line.new(bar1, price1, bar2, price2, ..., extend=extend.right)`.
    Line {
        /// First anchor's bar index.
        bar1: f64,
        /// First anchor's price.
        price1: f64,
        /// Second anchor's bar index.
        bar2: f64,
        /// Second anchor's price.
        price2: f64,
        /// Packed RGBA.
        color: u32,
        /// "solid" | "dashed" | "dotted".
        style: String,
        /// Line width.
        width: f64,
        /// "" | "right" | "left" | "both": the renderer projects the
        /// segment to the plot edge(s) along its slope. Pine's
        /// extend.none is the empty string.
        extend: String,
    },
    /// `label.new(bar, price, text, ...)`.
    Label {
        /// Anchor bar index.
        bar: f64,
        /// Anchor price.
        price: f64,
        /// The label text.
        text: String,
        /// Packed RGBA.
        color: u32,
    },
    /// `box.new(left, top, right, bottom, ...)`.
    Box {
        /// Left bar index.
        left: f64,
        /// Top price.
        top: f64,
        /// Right bar index.
        right: f64,
        /// Bottom price.
        bottom: f64,
        /// Packed RGBA (fill).
        color: u32,
        /// `border_color=` — packed RGBA when set; `None` draws fill only
        /// (docs/31).
        border_color: Option<u32>,
        /// `border_width=` (1 when unset).
        border_width: f64,
        /// `border_style=` — "solid" | "dashed" | "dotted".
        border_style: String,
    },
    /// `line.new_time(t1, price1, t2, price2, ...)` — the time-anchored twin
    /// (docs/28): the anchors are unix-nanos timestamps, so a script reading
    /// a pooled higher timeframe can draw what it found there onto this
    /// chart's time axis. Nanos ride in f64 like the VM's `time` series; the
    /// ~256ns rounding is invisible inside a bar slot.
    LineTime {
        /// First anchor's time, unix nanos as f64.
        t1: f64,
        /// First anchor's price.
        price1: f64,
        /// Second anchor's time, unix nanos as f64.
        t2: f64,
        /// Second anchor's price.
        price2: f64,
        /// Packed RGBA.
        color: u32,
        /// "solid" | "dashed" | "dotted".
        style: String,
        /// Line width.
        width: f64,
        /// "" | "right" | "left" | "both" (see Line).
        extend: String,
    },
    /// `label.new_time(t, price, text, ...)` — time-anchored (docs/28).
    LabelTime {
        /// Anchor time, unix nanos as f64.
        nanos: f64,
        /// Anchor price.
        price: f64,
        /// The label text.
        text: String,
        /// Packed RGBA.
        color: u32,
    },
    /// `box.new_time(t1, top, t2, bottom, ...)` — time-anchored (docs/28).
    /// The MTF zone shape: a higher-timeframe order block or gap spans its
    /// birth bar's start to "now" in absolute time, which is not a bar index
    /// on this chart.
    BoxTime {
        /// Left edge's time, unix nanos as f64.
        left_nanos: f64,
        /// Top price.
        top: f64,
        /// Right edge's time, unix nanos as f64.
        right_nanos: f64,
        /// Bottom price.
        bottom: f64,
        /// Packed RGBA (fill).
        color: u32,
        /// `border_color=` — packed RGBA when set; `None` draws fill only
        /// (docs/31).
        border_color: Option<u32>,
        /// `border_width=` (1 when unset).
        border_width: f64,
        /// `border_style=` — "solid" | "dashed" | "dotted".
        border_style: String,
    },
    /// `fib.new(bar1, price1, bar2, price2, color=...)` (docs/37): a
    /// Fibonacci retracement anchored on two bars. One registry primitive --
    /// the engine decomposes it into the level lines and labels the shell
    /// already paints, so the shell never learns what a fib IS.
    Fib {
        /// First anchor's bar index (the swing's start).
        bar1: f64,
        /// First anchor's price.
        price1: f64,
        /// Second anchor's bar index (the swing's end).
        bar2: f64,
        /// Second anchor's price.
        price2: f64,
        /// Packed RGBA.
        color: u32,
    },
}

/// The drawing-object heap cap, per run (docs/23 Phase 13): the array cap's
/// sibling. A script that draws on every bar hits it and is refused, not
/// allowed to leak.
pub const MAX_OBJECTS: usize = 256;

/// Run a vetted script over a window, producing its output.
///
/// # Errors
/// Returns a [`ScriptError`](crate::ScriptError) when the script reads a
/// variable before writing it, calls a function it did not define, or exceeds
/// a dynamic budget -- all things static checking cannot see (values that
/// only appear at runtime, like a loop bound computed from `close`).
pub fn run(script: &Script, candles: &[Candle], inputs: &Inputs) -> Result<Output, crate::ScriptError> {
    let mut vm = Vm::new(script, candles, inputs);
    vm.run()
}

/// The virtual machine.
pub struct Vm<'a> {
    script: &'a Script,
    candles: &'a [Candle],
    inputs: &'a Inputs,
    /// Per-variable ring buffers.
    vars: std::collections::HashMap<String, Vec<f64>>,
    /// Parallel to `vars`: which slots an assignment actually RAN on. The
    /// buffer alone cannot tell "no assignment this bar" (Pine carries the
    /// value forward) from "the assignment ran and produced na" (the value
    /// IS na) -- and confusing them either kills sticky `var` state or
    /// resurrects stale values past na computations (the zone-scan bug).
    vars_written: std::collections::HashMap<String, Vec<bool>>,
    /// Variables that carry state across bars (`var`).
    var_modes: std::collections::HashMap<String, VarMode>,
    /// User functions.
    funcs: std::collections::HashMap<String, &'a [Item]>,
    /// Call depth of user functions; recursion is refused at [`MAX_CALL_DEPTH`].
    call_depth: usize,
    /// Loop variable values, scoped like call frames.
    loop_scopes: Vec<(String, f64)>,
    /// Call frames for user functions (locals shadow nothing; Pine has one
    /// namespace, and the checker refuses reads-before-writes).
    scopes: Vec<std::collections::HashMap<String, f64>>,
    /// Per-call-frame parameter SERIES bindings: param name -> the caller's
    /// argument expression, re-evaluated at whichever bar the body reads it.
    /// This is Pine's series-passing model (`f(close)` passes close, not a
    /// snapshot) and what lets a function body run `ta.*` over a parameter.
    series_args: Vec<std::collections::HashMap<String, Expr>>,
    /// Plot statements deferred to after the bar loop: replaying a plot's
    /// expression needs *completed* buffers, and one AST call site is one
    /// plot no matter how many bars its branch fires on.
    deferred: Vec<Expr>,
    deferred_seen: std::collections::HashSet<usize>,
    /// Pass 2's per-bar account snapshots (docs/24 S1); None in pass 1, so
    /// the strategy state builtins answer zero until the simulation exists.
    account: Option<Vec<crate::sim::SimSnapshot>>,
    /// True while the per-bar loop runs; plot calls defer instead of acting.
    collecting: bool,
    /// Cached whole-window series for ta calls that read only builtins
    /// (`ta.sma(close, 9)`): computed once, keyed by the argument expression's
    /// address. Var-dependent series recompute -- their buffers change.
    series_cache: std::collections::HashMap<usize, Series>,
    /// `request.security` results, memoized per call site (`SYM@TF#site` ->
    /// one value per bar). The pooled series is index-aligned to the chart and
    /// the third argument is backward-looking series logic, so evaluating it
    /// once over the whole pool -- instead of re-running a sub-VM on every
    /// parent bar -- is semantically identical and turns the per-bar cost from
    /// O(pool) to O(1). The site key is the third argument's AST address,
    /// stable for the VM's lifetime.
    security_value_cache: std::collections::HashMap<String, Vec<f64>>,
    /// The array heap. A variable holds a *handle* (the heap index, as a
    /// plain f64 in its ring buffer), so `var a = array.new()` allocates once
    /// at bar 0 and every later bar reads the same object -- Pine's model,
    /// with `array.push`/`array.get` instead of indexed assignment. Capped:
    /// a script that allocates a fresh array every bar is refused, not
    /// allowed to leak.
    heap: Vec<Vec<f64>>,
    /// The variable name an `input.*` call is binding, set by the assignment
    /// evaluator just before the call runs -- Pine's `x = input.int(...)`
    /// names the input by its target.
    current_input_name: String,
    /// `tf = input.timeframe("4h")` registrations: var name -> timeframe
    /// string. `request.security("SYM", tf, ...)` resolves its second
    /// argument through this map (strings are expression-level; no float
    /// buffer holds one).
    input_timeframes: std::collections::HashMap<String, String>,
    /// Side channel from the `input.timeframe` call to its Assign: the
    /// string the assignment registers, then clears.
    last_input_tf: Option<String>,
    /// Live `table.new` panels: the handle is the index, `table.cell`
    /// updates in place. Serialized into out.tables at the end of the run.
    tables_live: Vec<OutputTable>,
    /// The live drawing-object arena: `l = line.new(...)` binds the ARENA
    /// INDEX as a float handle, setters mutate through it, deletes clear
    /// the slot. `out.objects` fills from this at the end of the run
    /// (creation order, deleted slots skipped).
    objects_live: Vec<Option<ScriptObject>>,
    /// The header's `sec=` symbol, when the script declared one. `request.*`
    /// reads are real only when this is Some -- an empty `security` vec on a
    /// sec-declaring script means the HOST failed to fetch, which must read
    /// as data, not as a missing-feature error.
    security_ticker: Option<String>,
    out: Output,
    bar: usize,
    steps: u64,
    fuel: u64,
}

/// The per-bar step budget per 1000 bars, from `docs/23`. Calibrated against
/// real generated scripts: a full strategy (session tracking + sweep + SMT +
/// drawing objects) burns ~200 steps/bar, so the meter must allow that with
/// headroom while still stopping runaway loops within a window.
pub const FUEL_PER_1000_BARS: u64 = 1_000_000;

/// How deep user-function calls may nest. Pine allows recursion; this
/// platform refuses it -- a recursive script cannot have a per-bar cost
/// bound, and the stack depth is the one thing the fuel counter cannot
/// meter before it blows.
pub const MAX_CALL_DEPTH: usize = 16;

/// The array heap's cap, per run. A script may keep a working set of pivots,
/// zones and session levels comfortably under this; a script that allocates
/// fresh arrays every bar hits the cap and is killed, not leaked.
pub const MAX_ARRAYS: usize = 64;

/// The cap on one array's length. Enough for a full session's FVGs or a
/// chart's worth of pivots at any sane lookback.
pub const MAX_ARRAY_LEN: usize = 4096;

/// The body-less script a `request.security` sub-VM runs: it has no
/// statements, only the caller's cloned expression, so one `'static` anchor
/// serves every sub-run and the lifetime stays honest.
static EMPTY_SCRIPT: Script = Script { items: Vec::new(), header: crate::Header::empty() };

impl<'a> Vm<'a> {
    fn new(script: &'a Script, candles: &'a [Candle], inputs: &'a Inputs) -> Self {
        let fuel = (candles.len() as u64).max(1) * FUEL_PER_1000_BARS / 1000;
        Self::with_fuel(script, candles, inputs, fuel)
    }

    /// The pooled-series key for a `request.security` call: `SYM@TF` from
    /// its two literal arguments. The host fills `Inputs::series_pool` with
    /// exactly the keys the checker collected, so a key miss here is a host
    /// bug and reads as data, not as a missing feature.
    fn security_key(&self, args: &[Arg]) -> Result<String, crate::ScriptError> {
        let sym = match args.first().map(|a| &a.value.kind) {
            Some(ExprKind::Str(s)) => s.to_uppercase(),
            _ => {
                return Err(crate::ScriptError {
                    kind: crate::ErrorKind::Type,
                    span: crate::Span::new(1, 1),
                    message: "`request.security` argument 1 must be a quoted symbol".into(),
                })
            }
        };
        let tf = match args.get(1).map(|a| &a.value.kind) {
            Some(ExprKind::Str(s)) => s.to_uppercase(),
            // `request.security("BTCUSDT", tf, ...)` where tf is an
            // input.timeframe var: resolve through the registry.
            Some(ExprKind::Ident(v)) => match self.input_timeframes.get(v) {
                Some(s) => s.to_uppercase(),
                None => {
                    return Err(crate::ScriptError {
                        kind: crate::ErrorKind::Type,
                        span: crate::Span::new(1, 1),
                        message: format!("`{v}` is not an input.timeframe variable; `request.security` argument 2 must be a quoted timeframe or one").into(),
                    })
                }
            },
            _ => {
                return Err(crate::ScriptError {
                    kind: crate::ErrorKind::Type,
                    span: crate::Span::new(1, 1),
                    message: "`request.security` argument 2 must be a quoted timeframe".into(),
                })
            }
        };
        Ok(format!("{sym}@{tf}"))
    }

    fn with_fuel(script: &'a Script, candles: &'a [Candle], inputs: &'a Inputs, fuel: u64) -> Self {
        Self {
            script,
            candles,
            inputs,
            vars: std::collections::HashMap::new(),
            vars_written: std::collections::HashMap::new(),
            var_modes: std::collections::HashMap::new(),
            funcs: std::collections::HashMap::new(),
            call_depth: 0,
            loop_scopes: Vec::new(),
            scopes: Vec::new(),
            series_args: Vec::new(),
            deferred: Vec::new(),
            deferred_seen: std::collections::HashSet::new(),
            heap: Vec::new(),
            collecting: false,
            series_cache: std::collections::HashMap::new(),
            security_value_cache: std::collections::HashMap::new(),
            current_input_name: String::new(),
            input_timeframes: std::collections::HashMap::new(),
            last_input_tf: None,
            objects_live: Vec::new(),
            tables_live: Vec::new(),
            security_ticker: script.header.sec.clone(),
            account: None,
            out: Output::default(),
            bar: 0,
            steps: 0,
            fuel,
        }
    }

    /// A one-expression VM over a pooled series: `request.security`'s third
    /// argument, evaluated per bar as if the PAIR were the chart. Fuel is a
    /// per-call slice of the parent's remaining budget (a sub-run must never
    /// outlive its parent's meter), every ring starts empty, and `ta.*` over
    /// the pooled candles is plain composition -- smoothed pair closes need
    /// no special case. The sub-VM's own `request.*` surface is empty: nested
    /// security calls are refused at vet time (the pool key would need a
    /// per-call context the flat pool cannot express).
    fn new_for_pool(
        expr: &Expr,
        candles: &'a [Candle],
        inputs: &'a Inputs,
        fuel: u64,
    ) -> Self {
        // An empty script: the sub-run has no statements of its own, only
        // the caller's cloned expression (parked in `deferred`). No
        // `'static` dance needed because `with_fuel` only ever READS the
        // script within the VM's own lifetime... which is this call, so the
        // script must outlive the sub-VM. The caller keeps it alive: the
        // static EMPTY_SCRIPT below is that caller's anchor.
        let mut sub = Self::with_fuel(&EMPTY_SCRIPT, candles, inputs, fuel);
        sub.deferred.push(expr.clone());
        sub
    }

    fn run(&mut self) -> Result<Output, crate::ScriptError> {
        // Pass 1: hoist function definitions (Pine allows calling a function
        // defined later in the file).
        for item in &self.script.items {
            if let Item::FuncDef { name, body, .. } = item {
                self.funcs.insert(name.clone(), body);
            }
        }
        // Per-bar execution. Plot calls defer: their series are replayed
        // after the loop, against completed buffers.
        self.collecting = true;
        for bar in 0..self.candles.len() {
            self.bar = bar;
            for item in &self.script.items {
                self.item(item)?;
            }
        }
        self.collecting = false;
        // Deferred plots, replayed against completed buffers.
        let deferred = std::mem::take(&mut self.deferred);
        self.deferred_seen.clear();
        for expr in &deferred {
            if let ExprKind::Call { callee, args } = &expr.kind {
                match callee.as_str() {
                    "fill" => self.fill_series(args)?,
                    "bgcolor" => self.tint_series(args, true)?,
                    "barcolor" => self.tint_series(args, false)?,
                    _ => {
                        let series = self.plot_series(callee, args)?;
                        self.out.plots.push(series);
                    }
                }
            }
        }
        // Strategy execution (docs/24 S1): the intents this run recorded go
        // through the shared simulator. A script that READS account state
        // (`strategy.equity`, ...) gets a second pass: the simulation built
        // from pass 1's own decisions becomes the account, and the bar loop
        // reruns with the state builtins answering from it. Pine's broker
        // emulator has the same shape -- the script calculates against the
        // PREVIOUS state, one bar late, by construction.
        let wants_state = crate::reads_strategy_state(self.script);
        if self.out.strategy_used || (wants_state && self.script.header.strategy.is_some()) {
            let knobs = self.script.header.strategy.clone().unwrap_or_default();
            self.out.simulation = Some(crate::sim::simulate(&self.out.intents, self.candles, &knobs));
            if wants_state {
                // Fixpoint over state reads: pass 2 decides against pass 1's
                // account, but ITERATED dependence (leg 2 arms only after leg
                // 1's partial fill shrank the position) needs pass 3 to
                // decide against pass 2's fills. Rerun until the intent list
                // stops changing -- a scale-out ladder converges in three.
                // The cap guards scripts whose gates flip-flop (those ship
                // the last converging pass, never a hang).
                for _ in 0..5 {
                    let before = self.out.intents.clone();
                    self.rerun_with_account()?;
                    if self.out.intents == before {
                        break;
                    }
                }
            }
            // docs/24 S3: a strategy whose entry never fired still simulates
            // (a flat equity line at initial_capital), but a silent "net 0"
            // reads as a broken platform. The note is non-fatal and rides the
            // preview into the chat row, where the repair loop's next round
            // can act on it. Pushed AFTER the possible second pass: pass 2
            // resets `self.out`, which would drop any earlier note.
            if self
                .out
                .simulation
                .as_ref()
                .is_some_and(|s| s.report.total_trades == 0.0)
            {
                self.out.notes.push(format!(
                    "simulation produced 0 fills in {} bars -- every entry was skipped or the entry condition never fired; loosen the entry condition or gate it on strategy.position_size == 0",
                    self.candles.len()
                ));
            }
        }
        // Serialize the arena: creation order, deleted slots gone. One
        // drain serves every pass -- a rerun cleared and refilled the
        // arena, so the LAST pass's arena is the one that ships.
        self.out.objects = self.objects_live.iter().filter_map(|o| o.clone()).collect();
        self.out.tables = self.tables_live.clone();
        Ok(std::mem::take(&mut self.out))
    }

    /// Pass 2: the simulated account exists, so rerun the bar loop with the
    /// strategy state builtins reading the per-bar snapshots. The script is
    /// deterministic and the account came from pass 1's decisions, so the
    /// intents MUST match; on the mismatch that would mean otherwise, pass
    /// 2's artifacts stand (they align with the state the user reads) and a
    /// note rides the output instead of a silently wrong trade list.
    fn rerun_with_account(&mut self) -> Result<(), crate::ScriptError> {
        let Some(sim) = self.out.simulation.take() else {
            return Ok(());
        };
        let pass1_intents = self.out.intents.clone();
        self.account = Some(sim.snapshots.clone());
        // A fresh run: state, series caches and the object/array heaps reset;
        // fuel restarts (the two passes meter separately, per docs/24).
        self.out = Output::default();
        self.vars.clear();
        self.vars_written.clear();
        self.var_modes.clear();
        self.scopes.clear();
        self.series_cache.clear();
        self.security_value_cache.clear();
        self.heap.clear();
        self.objects_live.clear();
        self.tables_live.clear();
        self.deferred.clear();
        self.deferred_seen.clear();
        self.steps = 0;
        self.collecting = true;
        for bar in 0..self.candles.len() {
            self.bar = bar;
            for item in &self.script.items {
                self.item(item)?;
            }
        }
        self.collecting = false;
        let deferred = std::mem::take(&mut self.deferred);
        self.deferred_seen.clear();
        for expr in &deferred {
            if let ExprKind::Call { callee, args } = &expr.kind {
                match callee.as_str() {
                    "fill" => self.fill_series(args)?,
                    "bgcolor" => self.tint_series(args, true)?,
                    "barcolor" => self.tint_series(args, false)?,
                    _ => {
                        let series = self.plot_series(callee, args)?;
                        self.out.plots.push(series);
                    }
                }
            }
        }
        if self.out.intents != pass1_intents {
            // docs/24 S3: pass 1 reads the account as zero (it does not exist
            // yet), so any order GATED on account state -- `strategy.exit`
            // under `if strategy.position_size > 0` above all -- is missing
            // from pass 1's decisions. The state-aware pass is the script's
            // real intent: rebuild the simulation from it instead of
            // shipping a gate-blind trade list. Gated entries need no special
            // case: pass 1 may push them where the position is open, but the
            // simulator refuses them (pyramiding 0), so the state-aware
            // rebuild produces the same trades.
            let knobs = self.script.header.strategy.clone().unwrap_or_default();
            let state_sim = crate::sim::simulate(&self.out.intents, self.candles, &knobs);
            self.out.notes.push(format!(
                "strategy state reads changed the script's decisions between passes; the trades come from the state-aware pass ({} order(s))",
                state_sim.orders.len()
            ));
            self.out.simulation = Some(state_sim);
        } else {
            self.out.simulation = Some(sim);
        }
        self.account = None;
        Ok(())
    }

    /// A strategy account-state read (docs/24 S1). Pass 1 has no account yet
    /// (its own intents are what build it), so the read answers zero -- the
    /// same one-bar-late shape Pine documents for its broker emulator. Pass
    /// 2 parked the simulated snapshots in `account`; this bar's row answers.
    fn strategy_read(&self, name: &str) -> f64 {
        let Some(snaps) = &self.account else { return 0.0 };
        let Some(s) = snaps.get(self.bar) else { return 0.0 };
        match name {
            "strategy.position_size" => s.position_size,
            "strategy.position_avg_price" => s.avg_price,
            "strategy.equity" => s.equity,
            "strategy.openprofit" => s.openprofit,
            "strategy.closedtrades" => s.closedtrades,
            "strategy.wintrades" => s.wintrades,
            _ => 0.0,
        }
    }

    fn tick(&mut self) -> Result<(), crate::ScriptError> {
        self.steps += 1;
        // A small tolerance beyond the budget: a real strategy script is
        // dozens of statements per bar, and the meter counts every eval, so
        // the round number lands mid-run for scripts a few percent over.
        // Overflow is still bounded -- anything genuinely runaway blows
        // through the grace and stops.
        const FUEL_GRACE: u64 = FUEL_PER_1000_BARS / 4;
        if self.steps > self.fuel + FUEL_GRACE {
            return Err(crate::ScriptError {
                kind: crate::ErrorKind::Limit,
                span: crate::Span::new(1, 1),
                message: format!(
                    "script exceeded its step budget ({}) for this window; simplify the loop or lower max_bars_back",
                    self.fuel
                ),
            });
        }
        Ok(())
    }

    fn item(&mut self, item: &Item) -> Result<(), crate::ScriptError> {
        self.tick()?;
        match item {
            Item::Assign { name, mode, expr, op, .. } => {
                // `x = input.int(...)` -- the input reads its name from the
                // assignment target.
                if let ExprKind::Call { callee, .. } = &expr.kind {
                    if callee.starts_with("input.") {
                        self.current_input_name = name.clone();
                    }
                    // A timeframe input binds NO float: register the string
                    // under the var name and skip the buffer write.
                    if callee == "input.timeframe" {
                        self.eval_f(expr)?;
                        if let Some(tf) = self.last_input_tf.take() {
                            self.input_timeframes.insert(name.clone(), tf);
                        }
                        return Ok(());
                    }
                }
                // Compound assignment (`i += e`): desugars to `i := i <op> e`
                // -- the current value comes through the normal name read,
                // so an unreadable name errors exactly like `i := i + e`.
                if let Some(op) = op.as_deref() {
                    if matches!(op, "+=" | "-=" | "*=" | "/=" | "%=") {
                        let cur = self.read_name(name)?;
                        let rhs = self.eval_f(expr)?;
                        let value = match &op[..1] {
                            "+" => cur + rhs,
                            "-" => cur - rhs,
                            "*" => cur * rhs,
                            "%" => cur % rhs,
                            _ => cur / rhs,
                        };
                        let buf = self
                            .vars
                            .entry(name.clone())
                            .or_insert_with(|| vec![NA; self.candles.len()]);
                        buf[self.bar] = value;
                        self.vars_written
                            .entry(name.clone())
                            .or_insert_with(|| vec![false; self.candles.len()])[self.bar] = true;
                        return Ok(());
                    }
                }
                // `var` semantics: initialized once, at bar 0. On later bars
                // the initializer does not run at all -- Pine's rule, which is
                // what makes `var count = 0` a counter and not a zeroing.
                if *mode != VarMode::Auto && self.bar > 0 && self.vars.contains_key(name) {
                    return Ok(());
                }
                let value = self.eval_f(expr)?;
                // Ring buffer: one slot per bar, written at the current bar.
                // `var`/`varip` initialize **once** (bar 0) and then carry:
                // their earlier slots stay frozen so `count := count + 1`
                // accumulates and any `count[k]` reads real history.
                let buf = self
                    .vars
                    .entry(name.clone())
                    .or_insert_with(|| vec![NA; self.candles.len()]);
                buf[self.bar] = value;
                self.vars_written
                    .entry(name.clone())
                    .or_insert_with(|| vec![false; self.candles.len()])[self.bar] = true;
                self.var_modes.insert(name.clone(), *mode);
            }
            Item::Destructure { names, call, .. } => {
                // One call, several outputs: `basis, upper, lower = ta.bb(...)`.
                // The callee's outputs are whole-window series; each target
                // takes this bar's slot, so the names carry real history.
                let ExprKind::Call { callee, args } = &call.kind else {
                    return Err(crate::ScriptError {
                        kind: crate::ErrorKind::Type,
                        span: crate::Span::new(1, 1),
                        message: "a multi-value assignment needs a function call".into(),
                    });
                };
                let outputs = self.call_multi(callee, args)?;
                for (index, name) in names.iter().enumerate() {
                    let value = outputs.get(index).map_or(NA, |series| {
                        series.get(self.bar).copied().unwrap_or(NA)
                    });
                    let buf = self
                        .vars
                        .entry(name.clone())
                        .or_insert_with(|| vec![NA; self.candles.len()]);
                    buf[self.bar] = value;
                    self.vars_written
                        .entry(name.clone())
                        .or_insert_with(|| vec![false; self.candles.len()])[self.bar] = true;
                    self.var_modes.insert(name.clone(), VarMode::Auto);
                }
            }
            Item::Expr { expr, .. } => {
                self.eval_stmt(expr)?;
            }
            Item::Block { kind, exprs, loop_var, body, els, .. } => match kind {
                BlockKind::If => {
                    let cond = self.eval_f(&exprs[0])?;
                    if self.truthy(cond) {
                        self.body(body)?;
                    } else if let Some(els) = els {
                        self.body(els)?;
                    }
                }
                BlockKind::For => {
                    let from = self.eval_f(&exprs[0])?;
                    let to = self.eval_f(&exprs[1])?;
                    let step = self.eval_f(&exprs[2])?;
                    let mut i = from;
                    let positive = step >= 0.0;
                    // A zero step would spin forever; Pine errors, here the
                    // loop simply does not run. Fuel is the backstop.
                    if step == 0.0 {
                        return Ok(());
                    }
                    let name = loop_var.clone().unwrap_or_default();
                    while if positive { i <= to } else { i >= to } {
                        self.tick()?;
                        self.loop_scopes.push((name.clone(), i));
                        self.body(body)?;
                        self.loop_scopes.pop();
                        i += step;
                    }
                }
                BlockKind::While => {
                    // `while` (v1.1): each iteration re-evaluates the
                    // condition and burns fuel like a `for` iteration, so a
                    // condition that never turns false dies on the step
                    // budget with the same message any runaway loop gets --
                    // not a hang, not a silent skip.
                    loop {
                        let cond = self.eval_f(&exprs[0])?;
                        if !self.truthy(cond) {
                            break;
                        }
                        self.tick()?;
                        self.body(body)?;
                    }
                }
            },
            Item::FuncDef { .. } => {} // hoisted in run()
        }
        Ok(())
    }

    fn body(&mut self, items: &[Item]) -> Result<(), crate::ScriptError> {
        for item in items {
            self.item(item)?;
        }
        Ok(())
    }

    fn truthy(&self, value: f64) -> bool {
        // Pine's bools are numbers under the hood: 1.0 true, 0.0 false, na
        // false. This is the one place the unified model pays its way.
        value.is_finite() && value != 0.0
    }

    /// Evaluate a statement expression: calls whose values are discarded
    /// (plots, strategy calls) return nothing meaningful.
    fn eval_stmt(&mut self, expr: &Expr) -> Result<(), crate::ScriptError> {
        self.tick()?;
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                self.call_stmt(callee, args)
            }
            ExprKind::Bin { left, op: BinOp::Cmp(_), right } => {
                // `flag and plot(...)`-style mixing is refused by the type
                // checker; a bare comparison as a statement does nothing, but
                // evaluating it keeps `x == y` from erroring.
                self.eval_f(left)?;
                self.eval_f(right)?;
                Ok(())
            }
            _ => {
                self.eval_f(expr)?;
                Ok(())
            }
        }
    }

    /// The `color.*` constructors: `color.new(base, transparency)` repacks a
    /// base with its own alpha (transparency 0..100 -> alpha 255..0),
    /// `color.rgb(r, g, b[, transparency])` builds from channels, and
    /// `color.from_gradient(v, bottom, top, c_bottom, c_top)` interpolates
    /// every channel including alpha. All pack 0xAARRGGBB.
    fn color_call(&mut self, callee: &str, args: &[Arg]) -> Result<Option<f64>, crate::ScriptError> {
        let bad = |what: &str| crate::ScriptError {
            kind: crate::ErrorKind::Type,
            span: crate::Span::new(1, 1),
            message: what.to_string(),
        };
        match callee {
            "color.new" => {
                self.tick()?;
                let base = self.arg_color_pos(args, 0)
                    .or_else(|| self.arg_color_named(args, "base_color"))
                    .or_else(|| self.arg_color_named(args, "color"))
                    .ok_or_else(|| bad("`color.new` argument 1 is a base color: a color.* name, a #rrggbb literal, or a color variable"))?;
                let transp = self
                    .arg_f(args, 1)?
                    .or_else(|| self.arg_named_f(args, "transparency"))
                    .unwrap_or(0.0)
                    .clamp(0.0, 100.0);
                let alpha = (255.0 * (100.0 - transp) / 100.0).round() as u32;
                Ok(Some((((alpha << 24) | (base & 0x00FF_FFFF)) as f64)))
            }
            "color.rgb" => {
                self.tick()?;
                let mut chan = |i: usize| -> Result<u32, crate::ScriptError> {
                    Ok(self
                        .arg_f(args, i)?
                        .ok_or_else(|| bad("`color.rgb` needs red, green and blue channels (0-255)"))?
                        .clamp(0.0, 255.0)
                        .round() as u32)
                };
                let (r, g, b) = (chan(0)?, chan(1)?, chan(2)?);
                let transp = self.arg_f(args, 3)?.unwrap_or(0.0).clamp(0.0, 100.0);
                let alpha = (255.0 * (100.0 - transp) / 100.0).round() as u32;
                Ok(Some((((alpha << 24) | (r << 16) | (g << 8) | b) as f64)))
            }
            "color.from_gradient" => {
                self.tick()?;
                let v = self.arg_f(args, 0)?.unwrap_or(NA);
                let bottom = self.arg_f(args, 1)?.unwrap_or(NA);
                let top = self.arg_f(args, 2)?.unwrap_or(NA);
                let c_bottom = self.arg_color_pos(args, 3)
                    .ok_or_else(|| bad("`color.from_gradient` needs (value, bottom, top, bottom_color, top_color)"))?;
                let c_top = self.arg_color_pos(args, 4)
                    .ok_or_else(|| bad("`color.from_gradient` needs (value, bottom, top, bottom_color, top_color)"))?;
                if !v.is_finite() || !bottom.is_finite() || !top.is_finite() || top == bottom {
                    return Ok(Some(NA));
                }
                let t = ((v - bottom) / (top - bottom)).clamp(0.0, 1.0);
                let lerp = |shift: u32| -> u32 {
                    let a = ((c_bottom >> shift) & 0xFF) as f64;
                    let b = ((c_top >> shift) & 0xFF) as f64;
                    (a + (b - a) * t).round() as u32
                };
                let packed = (lerp(24) << 24) | (lerp(16) << 16) | (lerp(8) << 8) | lerp(0);
                Ok(Some((packed as f64)))
            }
            _ => Ok(None),
        }
    }

    /// `table.new(position.top_right, columns, rows, ...)`: create a panel,
    /// hand back its handle. `None` when the callee is not table.new.
    fn table_handle(&mut self, callee: &str, args: &[Arg]) -> Result<Option<f64>, crate::ScriptError> {
        if callee != "table.new" {
            return Ok(None);
        }
        self.tick()?;
        let position = match args.first().map(|a| &a.value.kind) {
            Some(ExprKind::Member { path }) => path.strip_prefix("position.").unwrap_or("").to_string(),
            Some(ExprKind::Ident(name)) => name.strip_prefix("position.").unwrap_or("").to_string(),
            _ => String::new(),
        };
        let known = [
            "top_right", "top_left", "top_center",
            "bottom_right", "bottom_left", "bottom_center",
            "middle_right", "middle_left", "middle_center",
        ];
        if !known.contains(&position.as_str()) {
            return Err(crate::ScriptError {
                kind: crate::ErrorKind::Type,
                span: crate::Span::new(1, 1),
                message: "`table.new` argument 1 is a position: position.top_right, position.top_left, position.bottom_right, position.bottom_left (or the center/middle variants)".into(),
            });
        }
        let columns = self.arg_f(args, 1)?.unwrap_or(1.0).max(1.0) as usize;
        let rows = self.arg_f(args, 2)?.unwrap_or(1.0).max(1.0) as usize;
        let handle = self.tables_live.len() as f64;
        self.tables_live.push(OutputTable {
            position,
            columns,
            rows,
            cells: std::collections::HashMap::new(),
        });
        Ok(Some(handle))
    }

    /// `table.cell(t, col, row, text, bgcolor=..., text_color=...)`: write
    /// one cell of a live table. `true` when the callee was table.cell.
    fn table_stmt(&mut self, callee: &str, args: &[Arg]) -> Result<bool, crate::ScriptError> {
        if callee != "table.cell" {
            return Ok(false);
        }
        self.tick()?;
        let h = self.arg_f(args, 0)?.unwrap_or(NA);
        let idx = if h.is_finite() && h >= 0.0 { h as usize } else { usize::MAX };
        if idx >= self.tables_live.len() {
            return Err(crate::ScriptError {
                kind: crate::ErrorKind::Type,
                span: crate::Span::new(1, 1),
                message: "`table.cell` needs a live table handle from table.new".into(),
            });
        }
        let text_arg = args.get(3).ok_or_else(|| crate::ScriptError {
            kind: crate::ErrorKind::Type,
            span: crate::Span::new(1, 1),
            message: "`table.cell` needs (table, column, row, text)".to_string(),
        })?;
        let text = self.eval_s(&text_arg.value)?;
        let bg = self.arg_color_named(args, "bgcolor");
        let text_color = self.arg_color_named(args, "text_color");
        let col = self.arg_f(args, 1)?.unwrap_or(0.0) as usize;
        let row = self.arg_f(args, 2)?.unwrap_or(0.0) as usize;
        let table = &mut self.tables_live[idx];
        if col >= table.columns || row >= table.rows {
            return Err(crate::ScriptError {
                kind: crate::ErrorKind::Type,
                span: crate::Span::new(1, 1),
                message: format!(
                    "`table.cell` ({col}, {row}) is outside the {}x{} grid the table declared",
                    table.columns, table.rows
                ),
            });
        }
        table.cells.insert((col, row), OutputTableCell { text, bg, text_color });
        Ok(true)
    }

    /// Create one drawing object in the arena and hand back its float
    /// handle (the arena index). `None` when the callee is not a drawing
    /// constructor. The cap STOPS drawing rather than killing the run
    /// (TradingView's behavior): past MAX_OBJECTS the call is a no-op and
    /// the handle is na.
    fn draw_object(&mut self, callee: &str, args: &[Arg]) -> Result<Option<f64>, crate::ScriptError> {
        let object = match callee {
            "line.new" => {
                self.tick()?;
                let bar1 = self.arg_f(args, 0)?.unwrap_or(NA);
                let price1 = self.arg_f(args, 1)?.unwrap_or(NA);
                let bar2 = self.arg_f(args, 2)?.unwrap_or(NA);
                let price2 = self.arg_f(args, 3)?.unwrap_or(NA);
                let color = self.arg_color(args).unwrap_or(0xFF_94_A3_B8_u32);
                let style = self.arg_str(args, "style").unwrap_or_else(|| "solid".into());
                let width = self.arg_named_f(args, "width").unwrap_or(1.0);
                let extend = self.arg_extend(args)?;
                ScriptObject::Line { bar1, price1, bar2, price2, color, style, width, extend }
            }
            "label.new" => {
                self.tick()?;
                let bar = self.arg_f(args, 0)?.unwrap_or(NA);
                let price = self.arg_f(args, 1)?.unwrap_or(NA);
                let text = self.arg_text(args, "text", 2)?.unwrap_or_default();
                let color = self.arg_color(args).unwrap_or(0xFF_94_A3_B8_u32);
                ScriptObject::Label { bar, price, text, color }
            }
            "box.new" => {
                self.tick()?;
                let left = self.arg_f(args, 0)?.unwrap_or(NA);
                let top = self.arg_f(args, 1)?.unwrap_or(NA);
                let right = self.arg_f(args, 2)?.unwrap_or(NA);
                let bottom = self.arg_f(args, 3)?.unwrap_or(NA);
                let color = self.arg_color(args).unwrap_or(0x33_94_A3_B8_u32);
                // docs/31: the TradingView box look -- a visible border
                // distinct from the fill, off when unasked.
                let border_color = self.arg_color_named(args, "border_color");
                let border_width = self.arg_named_f(args, "border_width").unwrap_or(1.0);
                let border_style = self.arg_str(args, "border_style").unwrap_or_else(|| "solid".into());
                ScriptObject::Box { left, top, right, bottom, color, border_color, border_width, border_style }
            }
            // ---- time-anchored drawing objects (docs/28): the same three
            // shapes with unix-nanos anchors instead of bar indexes, so a
            // script reading a pooled higher timeframe can draw what it found
            // there onto this chart's time axis. Same heap, same cap.
            "line.new_time" => {
                self.tick()?;
                let t1 = self.arg_f(args, 0)?.unwrap_or(NA);
                let price1 = self.arg_f(args, 1)?.unwrap_or(NA);
                let t2 = self.arg_f(args, 2)?.unwrap_or(NA);
                let price2 = self.arg_f(args, 3)?.unwrap_or(NA);
                let color = self.arg_color(args).unwrap_or(0xFF_94_A3_B8_u32);
                let style = self.arg_str(args, "style").unwrap_or_else(|| "solid".into());
                let width = self.arg_named_f(args, "width").unwrap_or(1.0);
                let extend = self.arg_extend(args)?;
                ScriptObject::LineTime { t1, price1, t2, price2, color, style, width, extend }
            }
            "label.new_time" => {
                self.tick()?;
                let nanos = self.arg_f(args, 0)?.unwrap_or(NA);
                let price = self.arg_f(args, 1)?.unwrap_or(NA);
                let text = self.arg_text(args, "text", 2)?.unwrap_or_default();
                let color = self.arg_color(args).unwrap_or(0xFF_94_A3_B8_u32);
                ScriptObject::LabelTime { nanos, price, text, color }
            }
            "box.new_time" => {
                self.tick()?;
                let left_nanos = self.arg_f(args, 0)?.unwrap_or(NA);
                let top = self.arg_f(args, 1)?.unwrap_or(NA);
                let right_nanos = self.arg_f(args, 2)?.unwrap_or(NA);
                let bottom = self.arg_f(args, 3)?.unwrap_or(NA);
                let color = self.arg_color(args).unwrap_or(0x33_94_A3_B8_u32);
                // docs/31: the same border knobs as the bar-index box.
                let border_color = self.arg_color_named(args, "border_color");
                let border_width = self.arg_named_f(args, "border_width").unwrap_or(1.0);
                let border_style = self.arg_str(args, "border_style").unwrap_or_else(|| "solid".into());
                ScriptObject::BoxTime { left_nanos, top, right_nanos, bottom, color, border_color, border_width, border_style }
            }
            _ => return Ok(None),
        };
        if self.objects_live.len() < MAX_OBJECTS {
            let handle = self.objects_live.len() as f64;
            self.objects_live.push(Some(object));
            Ok(Some(handle))
        } else {
            self.out.objects_truncated = true;
            Ok(Some(NA))
        }
    }

    /// Resolve a setter/delete handle to a live arena slot. na, out of
    /// range, or a deleted object is a clear error -- a handle the script
    /// never created is a bug the user should see, not a silent no-op.
    fn object_index(&mut self, args: &[Arg], callee: &str) -> Result<usize, crate::ScriptError> {
        let h = self.arg_f(args, 0)?.unwrap_or(NA);
        let err = |reason: &str| crate::ScriptError {
            kind: crate::ErrorKind::Type,
            span: crate::Span::new(1, 1),
            message: format!("`{callee}` needs a live drawing handle from line.new/label.new/box.new: {reason}"),
        };
        if !h.is_finite() || h < 0.0 {
            return Err(err("got na or a negative handle"));
        }
        let idx = h as usize;
        if idx >= self.objects_live.len() {
            return Err(err("no drawing was ever created with that handle"));
        }
        if self.objects_live[idx].is_none() {
            return Err(err("that drawing was deleted"));
        }
        Ok(idx)
    }

    /// Mutate or delete a live drawing through its handle. `true` when the
    /// callee was a setter/delete (handled), `false` otherwise.
    fn set_object(&mut self, callee: &str, args: &[Arg]) -> Result<bool, crate::ScriptError> {
        let kind_err = |callee: &str, want: &str| crate::ScriptError {
            kind: crate::ErrorKind::Type,
            span: crate::Span::new(1, 1),
            message: format!("`{callee}` needs a {want} handle"),
        };
        match callee {
            "line.set_x1" | "line.set_x2" | "line.set_y1" | "line.set_y2" => {
                let idx = self.object_index(args, callee)?;
                let v = self.arg_f(args, 1)?.unwrap_or(NA);
                match &mut self.objects_live[idx] {
                    Some(ScriptObject::Line { bar1, bar2, price1, price2, .. }) => {
                        match callee {
                            "line.set_x1" => *bar1 = v,
                            "line.set_x2" => *bar2 = v,
                            "line.set_y1" => *price1 = v,
                            _ => *price2 = v,
                        }
                    }
                    Some(ScriptObject::LineTime { t1, t2, price1, price2, .. }) => {
                        match callee {
                            "line.set_x1" => *t1 = v,
                            "line.set_x2" => *t2 = v,
                            "line.set_y1" => *price1 = v,
                            _ => *price2 = v,
                        }
                    }
                    _ => return Err(kind_err(callee, "line")),
                }
                Ok(true)
            }
            "line.set_color" => {
                let idx = self.object_index(args, callee)?;
                let c = self.arg_color_pos(args, 1).unwrap_or(0xFF_94_A3_B8_u32);
                match &mut self.objects_live[idx] {
                    Some(ScriptObject::Line { color, .. }) | Some(ScriptObject::LineTime { color, .. }) => *color = c,
                    _ => return Err(kind_err(callee, "line")),
                }
                Ok(true)
            }
            "line.set_extend" => {
                let idx = self.object_index(args, callee)?;
                let e = self.extend_from_pos(args, 1)?;
                match &mut self.objects_live[idx] {
                    Some(ScriptObject::Line { extend, .. }) | Some(ScriptObject::LineTime { extend, .. }) => *extend = e,
                    _ => return Err(kind_err(callee, "line")),
                }
                Ok(true)
            }
            "box.set_left" | "box.set_right" | "box.set_top" | "box.set_bottom" => {
                let idx = self.object_index(args, callee)?;
                let v = self.arg_f(args, 1)?.unwrap_or(NA);
                match &mut self.objects_live[idx] {
                    Some(ScriptObject::Box { left, right, top, bottom, .. }) => {
                        match callee {
                            "box.set_left" => *left = v,
                            "box.set_right" => *right = v,
                            "box.set_top" => *top = v,
                            _ => *bottom = v,
                        }
                    }
                    Some(ScriptObject::BoxTime { left_nanos, right_nanos, top, bottom, .. }) => {
                        match callee {
                            "box.set_left" => *left_nanos = v,
                            "box.set_right" => *right_nanos = v,
                            "box.set_top" => *top = v,
                            _ => *bottom = v,
                        }
                    }
                    _ => return Err(kind_err(callee, "box")),
                }
                Ok(true)
            }
            "box.set_border_color" => {
                let idx = self.object_index(args, callee)?;
                let c = self.arg_color_pos(args, 1).unwrap_or(0xFF_94_A3_B8_u32);
                match &mut self.objects_live[idx] {
                    Some(ScriptObject::Box { border_color, .. }) | Some(ScriptObject::BoxTime { border_color, .. }) => *border_color = Some(c),
                    _ => return Err(kind_err(callee, "box")),
                }
                Ok(true)
            }
            "box.set_border_width" => {
                let idx = self.object_index(args, callee)?;
                let v = self.arg_f(args, 1)?.unwrap_or(1.0);
                match &mut self.objects_live[idx] {
                    Some(ScriptObject::Box { border_width, .. }) | Some(ScriptObject::BoxTime { border_width, .. }) => *border_width = v,
                    _ => return Err(kind_err(callee, "box")),
                }
                Ok(true)
            }
            "box.set_bgcolor" => {
                let idx = self.object_index(args, callee)?;
                let c = self.arg_color_pos(args, 1).unwrap_or(0x33_94_A3_B8_u32);
                match &mut self.objects_live[idx] {
                    Some(ScriptObject::Box { color, .. }) | Some(ScriptObject::BoxTime { color, .. }) => *color = c,
                    _ => return Err(kind_err(callee, "box")),
                }
                Ok(true)
            }
            "label.set_x" | "label.set_y" => {
                let idx = self.object_index(args, callee)?;
                let v = self.arg_f(args, 1)?.unwrap_or(NA);
                match &mut self.objects_live[idx] {
                    Some(ScriptObject::Label { bar, price, .. }) => {
                        if callee == "label.set_x" { *bar = v } else { *price = v }
                    }
                    Some(ScriptObject::LabelTime { nanos, price, .. }) => {
                        if callee == "label.set_x" { *nanos = v } else { *price = v }
                    }
                    _ => return Err(kind_err(callee, "label")),
                }
                Ok(true)
            }
            "label.set_text" => {
                let idx = self.object_index(args, callee)?;
                let arg = args.get(1).ok_or_else(|| crate::ScriptError {
                    kind: crate::ErrorKind::Type,
                    span: crate::Span::new(1, 1),
                    message: "`label.set_text` needs (id, text)".to_string(),
                })?;
                let text = self.eval_s(&arg.value)?;
                match &mut self.objects_live[idx] {
                    Some(ScriptObject::Label { text: t, .. }) | Some(ScriptObject::LabelTime { text: t, .. }) => *t = text,
                    _ => return Err(kind_err(callee, "label")),
                }
                Ok(true)
            }
            "label.set_color" => {
                let idx = self.object_index(args, callee)?;
                let c = self.arg_color_pos(args, 1).unwrap_or(0xFF_94_A3_B8_u32);
                match &mut self.objects_live[idx] {
                    Some(ScriptObject::Label { color, .. }) | Some(ScriptObject::LabelTime { color, .. }) => *color = c,
                    _ => return Err(kind_err(callee, "label")),
                }
                Ok(true)
            }
            "line.delete" | "label.delete" | "box.delete" => {
                let idx = self.object_index(args, callee)?;
                let matches_kind = match (&self.objects_live[idx], callee) {
                    (Some(ScriptObject::Line { .. }) | Some(ScriptObject::LineTime { .. }), "line.delete") => true,
                    (Some(ScriptObject::Label { .. }) | Some(ScriptObject::LabelTime { .. }), "label.delete") => true,
                    (Some(ScriptObject::Box { .. }) | Some(ScriptObject::BoxTime { .. }), "box.delete") => true,
                    _ => false,
                };
                if !matches_kind {
                    let want = callee.strip_suffix(".delete").unwrap_or(callee);
                    return Err(kind_err(callee, want));
                }
                self.objects_live[idx] = None;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn call_stmt(&mut self, callee: &str, args: &[Arg]) -> Result<(), crate::ScriptError> {
        self.tick()?;
        match callee {
            "plot" | "plotshape" | "plotchar" | "plotarrow" | "plotcandle" | "plotbar"
            | "fill" | "bgcolor" | "barcolor" => {
                if self.collecting {
                    // Defer by call site: one AST node, one plot, however many
                    // bars (or branch firings) reach it. The key is the callee
                    // plus the first argument's source position, which is
                    // stable per call site and needs no pointer tricks.
                    let key = callee.len()
                        ^ args
                            .first()
                            .map_or(0, |a| a.value.span.col ^ a.value.span.line);
                    if self.deferred_seen.insert(key) {
                        // Rebuild the full expression for replay.
                        self.deferred.push(Expr {
                            span: crate::Span::new(1, 1),
                            kind: ExprKind::Call {
                                callee: callee.to_string(),
                                args: args.to_vec(),
                            },
                        });
                    }
                    return Ok(());
                }
                let series = self.plot_series(callee, args)?;
                self.out.plots.push(series);
                Ok(())
            }
            "hline" => {
                // hlines have no series -- they are a level and nothing else,
                // so they act once, at bar 0, not once per bar.
                if self.bar > 0 {
                    return Ok(());
                }
                let value = self.arg_f(args, 0)?.unwrap_or(NA);
                let title = self.arg_str(args, "title").unwrap_or_default();
                let color = self.arg_color(args).unwrap_or(0xFF_94_A3_B8_u32);
                let dashed = self.arg_str(args, "linestyle").is_some_and(|s| s != "solid");
                self.out.hlines.push(HLine { value, title, color, dashed });
                Ok(())
            }
            "fill" | "bgcolor" | "barcolor" => {
                // Handled by the deferred replay above (the tint/band needs
                // completed buffers, exactly like plots). Reaching here means
                // the replay itself ran a statement-position copy, which is
                // a no-op.
                Ok(())
            }
            "strategy.entry" => {
                let id = self.arg_str(args, "id").or_else(|| self.arg_str_pos(args, 0)).unwrap_or_default();
                let dir = self.arg_str(args, "direction").or_else(|| self.arg_str_pos(args, 1)).unwrap_or_default();
                let qty = self.arg_named_f(args, "qty");
                self.out.strategy_used = true;
                self.out
                    .intents
                    .push((self.bar, Intent::Entry { id, long: dir != "short", qty }));
                Ok(())
            }
            "strategy.exit" => {
                let id = self.arg_str(args, "id").or_else(|| self.arg_str_pos(args, 0)).unwrap_or_default();
                let stop = self.arg_named_f(args, "stop");
                let limit = self.arg_named_f(args, "limit");
                let qty_percent = self.arg_named_f(args, "qty_percent");
                self.out.strategy_used = true;
                self.out.intents.push((self.bar, Intent::Exit { id, stop, limit, qty_percent }));
                Ok(())
            }
            "strategy.close" => {
                let id = self.arg_str(args, "id").or_else(|| self.arg_str_pos(args, 0));
                let qty_percent = self.arg_named_f(args, "qty_percent");
                self.out.strategy_used = true;
                self.out.intents.push((self.bar, Intent::Close { id, qty_percent }));
                Ok(())
            }
            "strategy.close_all" => {
                self.out.strategy_used = true;
                self.out.intents.push((self.bar, Intent::Close { id: None, qty_percent: None }));
                Ok(())
            }
            "strategy.cancel" => Ok(()),
            // ---- drawing objects (docs/23 Phase 13): statements that push
            // to the object heap, capped like the array heap. Coordinates are
            // plain f64 bar indexes/prices; the engine maps them to the
            // canvas, never the script.
            // ---- drawing constructors + mutators (Tier 1 D): the six
            // `*.new` shapes live behind handles now -- `l = line.new(...)`
            // binds the arena index, `line.set_*(l, ...)` mutates through
            // it, `*.delete(l)` clears the slot. The construction itself is
            // in `draw_object`, shared with the expression path.
            _ if self.draw_object(callee, args)?.is_some() => Ok(()),
            _ if self.set_object(callee, args)? => Ok(()),
            _ if self.table_handle(callee, args)?.is_some() => Ok(()),
            _ if self.table_stmt(callee, args)? => Ok(()),
            // ---- the registry primitive (docs/37): the VM stores the raw
            // anchors; the decomposition into levels and labels is the
            // engine's, because positioning is the engine's.
            "fib.new" => {
                self.tick()?;
                if self.out.objects.len() < MAX_OBJECTS {
                    let bar1 = self.arg_f(args, 0)?.unwrap_or(NA);
                    let price1 = self.arg_f(args, 1)?.unwrap_or(NA);
                    let bar2 = self.arg_f(args, 2)?.unwrap_or(NA);
                    let price2 = self.arg_f(args, 3)?.unwrap_or(NA);
                    // Gold, the TradingView fib color, in the VM's packing.
                    let color = self.arg_color(args).unwrap_or(0xFF_D4_A0_17_u32);
                    self.out.objects.push(ScriptObject::Fib { bar1, price1, bar2, price2, color });
                } else {
                    self.out.objects_truncated = true;
                }
                Ok(())
            }
            // ---- array mutators: statements, because they act on the heap
            // and their value (if any) is rarely used. `array.push` grows
            // with a cap; `array.pop`/`shift` shrink; `array.set` writes.
            "array.push" => {
                self.tick()?;
                let handle = self.arg_f(args, 0)?.unwrap_or(NA);
                let value = self.arg_f(args, 1)?.unwrap_or(NA);
                let list = self.array_ref(handle)?;
                if list.len() >= MAX_ARRAY_LEN {
                    return Err(crate::ScriptError {
                        kind: crate::ErrorKind::Limit,
                        span: crate::Span::new(1, 1),
                        message: format!("an array grew past {MAX_ARRAY_LEN} entries; trim it with array.pop/shift or bound the loop"),
                    });
                }
                list.push(value);
                Ok(())
            }
            "array.pop" => {
                let handle = self.arg_f(args, 0)?.unwrap_or(NA);
                if let Some(v) = self.array_ref(handle)?.pop() {
                    let _ = v;
                }
                Ok(())
            }
            "array.shift" => {
                let handle = self.arg_f(args, 0)?.unwrap_or(NA);
                if !self.array_ref(handle)?.is_empty() {
                    self.array_ref(handle)?.remove(0);
                }
                Ok(())
            }
            "array.clear" => {
                let handle = self.arg_f(args, 0)?.unwrap_or(NA);
                self.array_ref(handle)?.clear();
                Ok(())
            }
            "array.fill" => {
                let handle = self.arg_f(args, 0)?.unwrap_or(NA);
                let value = self.arg_f(args, 1)?.unwrap_or(NA);
                let from = self.arg_f(args, 2)?.unwrap_or(0.0);
                let to = self.arg_f(args, 3)?.unwrap_or(NA);
                let list = self.array_ref(handle)?;
                let len = list.len();
                let lo = if from.is_finite() { from.max(0.0) as usize } else { 0 };
                let hi = if to.is_finite() { (to.max(0.0) as usize).min(len) } else { len };
                for v in list.iter_mut().take(hi).skip(lo) {
                    *v = value;
                }
                Ok(())
            }
            "array.set" => {
                self.tick()?;
                let handle = self.arg_f(args, 0)?.unwrap_or(NA);
                let index = self.arg_f(args, 1)?.unwrap_or(NA);
                let value = self.arg_f(args, 2)?.unwrap_or(NA);
                let i = if index.is_finite() && index >= 0.0 { index as usize } else {
                    return Err(crate::ScriptError {
                        kind: crate::ErrorKind::Type,
                        span: crate::Span::new(1, 1),
                        message: "array.set index must be a whole number >= 0".into(),
                    });
                };
                let list = self.array_ref(handle)?;
                if i >= list.len() {
                    return Err(crate::ScriptError {
                        kind: crate::ErrorKind::Type,
                        span: crate::Span::new(1, 1),
                        message: format!("array.set index {i} is past the array's length {}", list.len()),
                    });
                }
                list[i] = value;
                Ok(())
            }
            _ => {
                // User function called as a statement: run for side effects.
                if self.funcs.contains_key(callee) {
                    self.call_user(callee, args)?;
                    Ok(())
                } else {
                    Err(crate::ScriptError {
                        kind: crate::ErrorKind::Type,
                        span: crate::Span::new(1, 1),
                        message: format!("`{callee}` is not a statement here"),
                    })
                }
            }
        }
    }

    /// Build a [`Plot`] from a plot call. The series argument is evaluated
    /// into a full-window buffer by re-evaluating the expression over the
    /// run's own variable history -- the current bar's value plus the ring
    /// buffers the VM already maintains.
    /// Evaluate a color EXPRESSION per bar: a `color.red` literal, a named
    /// color, an input.color var, or a ternary of any of those
    /// (`close >= open ? color.green : color.red`).
    fn eval_color_expr(&mut self, expr: &Expr) -> Result<u32, crate::ScriptError> {
        match &expr.kind {
            ExprKind::Color(c) => Ok(*c),
            ExprKind::Ident(name) if named_color(name).is_some() => {
                Ok(named_color(name).expect("checked"))
            }
            ExprKind::Ternary { cond, then, els } => {
                let c = self.eval_f(cond)?;
                if self.truthy(c) {
                    self.eval_color_expr(then)
                } else {
                    self.eval_color_expr(els)
                }
            }
            _ => {
                let v = self.eval_f(expr)?;
                Ok(if v.is_finite() { v as u32 } else { 0xFF_94_A3_B8 })
            }
        }
    }

    /// Like eval_color_expr but na-aware: a conditional tint's `na` branch
    /// means "nothing here", not a gray bar.
    fn eval_color_opt(&mut self, expr: &Expr) -> Result<Option<u32>, crate::ScriptError> {
        match &expr.kind {
            ExprKind::Color(c) => Ok(Some(*c)),
            ExprKind::Ident(name) if named_color(name).is_some() => {
                Ok(Some(named_color(name).expect("checked")))
            }
            ExprKind::Ternary { cond, then, els } => {
                let c = self.eval_f(cond)?;
                if self.truthy(c) {
                    self.eval_color_opt(then)
                } else {
                    self.eval_color_opt(els)
                }
            }
            _ => {
                let v = self.eval_f(expr)?;
                Ok(if v.is_finite() { Some(v as u32) } else { None })
            }
        }
    }

    /// Replay a `fill(a, b, color=...)` against completed buffers.
    fn fill_series(&mut self, args: &[Arg]) -> Result<(), crate::ScriptError> {
        let n = self.candles.len();
        let (mut a, mut b) = (vec![NA; n], vec![NA; n]);
        let mut color = vec![None; n];
        let color_arg = args.iter().find(|x| x.name.as_deref() == Some("color")).map(|x| x.value.clone());
        let saved_bar = self.bar;
        for bar in 0..n {
            self.bar = bar;
            if let Some(x) = args.first() { a[bar] = self.eval_f(&x.value)?; }
            if let Some(x) = args.get(1) { b[bar] = self.eval_f(&x.value)?; }
            color[bar] = match &color_arg {
                Some(e) => self.eval_color_opt(e)?,
                None => Some(0xFF_60_A5_FA),
            };
        }
        self.bar = saved_bar;
        self.out.fills.push(crate::interp::PlotFill { a, b, color });
        Ok(())
    }

    /// Replay a `bgcolor(...)`/`barcolor(...)` per-bar color expression.
    fn tint_series(&mut self, args: &[Arg], is_bg: bool) -> Result<(), crate::ScriptError> {
        let n = self.candles.len();
        let mut colors = vec![None; n];
        let saved_bar = self.bar;
        for bar in 0..n {
            self.bar = bar;
            if let Some(x) = args.first() {
                colors[bar] = self.eval_color_opt(&x.value)?;
            }
        }
        self.bar = saved_bar;
        if is_bg {
            self.out.tints.push(colors);
        } else {
            self.out.barcolors.push(colors);
        }
        Ok(())
    }

    fn plot_series(&mut self, callee: &str, args: &[Arg]) -> Result<Plot, crate::ScriptError> {
        let n = self.candles.len();
        let mut values = vec![NA; n];
        if callee == "plotcandle" || callee == "plotbar" {
            // Four series + per-bar colors, replayed against frozen history
            // exactly like the one-series plot path.
            let (mut o, mut h, mut l, mut c) = (vec![NA; n], vec![NA; n], vec![NA; n], vec![NA; n]);
            let mut body = vec![0u32; n];
            let mut wick = vec![0u32; n];
            let color_arg = args.iter().find(|a| a.name.as_deref() == Some("color")).map(|a| a.value.clone());
            let wick_arg = args.iter().find(|a| a.name.as_deref() == Some("wickcolor")).map(|a| a.value.clone());
            let saved_bar = self.bar;
            for bar in 0..n {
                self.bar = bar;
                if let Some(a) = args.first() { o[bar] = self.eval_f(&a.value)?; }
                if let Some(a) = args.get(1) { h[bar] = self.eval_f(&a.value)?; }
                if let Some(a) = args.get(2) { l[bar] = self.eval_f(&a.value)?; }
                if let Some(a) = args.get(3) { c[bar] = self.eval_f(&a.value)?; }
                let up = !c[bar].is_finite() || !o[bar].is_finite() || c[bar] >= o[bar];
                body[bar] = match &color_arg {
                    Some(expr) => self.eval_color_expr(expr)?,
                    // TradingView's up green / down red.
                    None => if up { 0xFF_26_A6_9A } else { 0xFF_EF_53_50 },
                };
                wick[bar] = match &wick_arg {
                    Some(expr) => self.eval_color_expr(expr)?,
                    None => body[bar],
                };
            }
            self.bar = saved_bar;
            return Ok(Plot {
                id: format!("p{}", self.out.plots.len()),
                title: self.arg_str(args, "title").unwrap_or_else(|| "candles".into()),
                values: c.clone(),
                style: crate::PlotStyle::Line,
                color: 0,
                linewidth: 1.0,
                kind: crate::PlotKind::Candles,
                candles: Some(crate::interp::PlotCandles {
                    open: o,
                    high: h,
                    low: l,
                    close: c,
                    body,
                    wick,
                    bars: callee == "plotbar",
                }),
                offset: self.arg_named_f(args, "offset").unwrap_or(0.0),
            });
        }
        if callee == "plot" {
            if let Some(first) = args.first() {
                // Evaluate the plot's expression at every bar by replaying the
                // VM's variable history: assignments wrote one slot per bar,
                // so re-reading the expression against frozen history gives
                // the same series the script saw -- without re-running.
                let saved_bar = self.bar;
                for (bar, slot) in values.iter_mut().enumerate() {
                    self.bar = bar;
                    *slot = self.eval_f(&first.value)?;
                }
                self.bar = saved_bar;
            }
        } else {
            // plotshape/plotchar/plotarrow: a bool or numeric condition; the
            // bars where it is truthy become shapes.
            let series = if let Some(first) = args.first() {
                let saved_bar = self.bar;
                let mut flags = vec![NA; n];
                for (bar, slot) in flags.iter_mut().enumerate() {
                    self.bar = bar;
                    *slot = self.eval_f(&first.value)?;
                }
                self.bar = saved_bar;
                flags
            } else {
                vec![NA; n]
            };
            let glyph = self
                .arg_str(args, "shape")
                .or_else(|| self.arg_str(args, "char"))
                .unwrap_or_else(|| "circle".to_string());
            let color = self.arg_color(args).unwrap_or(0xFF_60_A5_FAu32);
            // `location_value=` is a SERIES, evaluated per bar like the
            // condition: a marker sits at the price of ITS bar. Evaluating
            // it once -- here, on the last bar, where the deferred plot
            // replays -- stamped every marker with the final bar's extreme,
            // so the whole marker field rode live price (the user's
            // "triangles move with the price" bug).
            let location_series = match args
                .iter()
                .find(|a| a.name.as_deref() == Some("location_value"))
            {
                Some(arg) => {
                    let saved_bar = self.bar;
                    let mut vals = vec![NA; n];
                    for (bar, slot) in vals.iter_mut().enumerate() {
                        self.bar = bar;
                        *slot = self.eval_f(&arg.value)?;
                    }
                    self.bar = saved_bar;
                    vals
                }
                None => vec![NA; n],
            };
            for (bar, v) in series.iter().enumerate() {
                if self.truthy(*v) {
                    self.out.shapes.push(Shape {
                        bar,
                        value: location_series[bar],
                        glyph: glyph.clone(),
                        color,
                    });
                }
            }
            return Ok(Plot {
                id: format!("p{}", self.out.plots.len()),
                title: self.arg_str(args, "title").unwrap_or_else(|| "plot".into()),
                values,
                style: crate::PlotStyle::Line,
                color,
                linewidth: 1.0,
                kind: crate::PlotKind::Shape,
                candles: None,
                offset: 0.0,
            });
        }
        let style = match self.arg_str(args, "style").as_deref() {
            Some("linebr") => crate::PlotStyle::LineBr,
            Some("histogram") => crate::PlotStyle::Histogram,
            Some("columns") => crate::PlotStyle::Columns,
            Some("circles") => crate::PlotStyle::Circles,
            Some("stepline") => crate::PlotStyle::StepLine,
            Some("areabr") => crate::PlotStyle::AreaBr,
            _ => crate::PlotStyle::Line,
        };
        Ok(Plot {
            id: format!("p{}", self.out.plots.len()),
            title: self.arg_str(args, "title").unwrap_or_else(|| "plot".into()),
            values,
            style,
            color: self.arg_color(args).unwrap_or(0xFF_60_A5_FAu32),
            linewidth: self.arg_named_f(args, "linewidth").unwrap_or(1.0),
            kind: crate::PlotKind::Line,
            candles: None,
            offset: self.arg_named_f(args, "offset").unwrap_or(0.0),
        })
    }

    // ---- expression evaluation ----

    fn eval_f(&mut self, expr: &Expr) -> Result<f64, crate::ScriptError> {
        self.tick()?;
        Ok(match &expr.kind {
            ExprKind::Num(n) => *n,
            ExprKind::Bool(b) => f64::from(*b),
            ExprKind::Color(_) | ExprKind::Str(_) => NA, // colors/strings cannot be numeric
            ExprKind::Na => NA,
            ExprKind::NaChecked { value } => f64::from(!self.eval_f(value)?.is_finite()),
            ExprKind::Ident(name) => self.read_name(name)?,
            ExprKind::Member { path } => self.read_member(path)?,
            ExprKind::History { base, offset } => {
                let off = self.eval_f(offset)?;
                let off = if off.is_finite() { off.max(0.0) as usize } else { 0 };
                self.read_history(base, off)?
            }
            ExprKind::Ternary { cond, then, els } => {
                let c = self.eval_f(cond)?;
                if self.truthy(c) {
                    self.eval_f(then)?
                } else {
                    self.eval_f(els)?
                }
            }
            ExprKind::Un { op, expr } => {
                let v = self.eval_f(expr)?;
                match op {
                    UnOp::Neg => -v,
                    UnOp::Pos => v,
                    UnOp::Not => f64::from(!self.truthy(v)),
                }
            }
            ExprKind::Bin { left, op, right } => {
                let l = self.eval_f(left)?;
                let r = self.eval_f(right)?;
                self.bin(*op, l, r)
            }
            ExprKind::Call { callee, args } => self.call_f(callee, args)?,
        })
    }

    fn bin(&self, op: BinOp, l: f64, r: f64) -> f64 {
        // Pine: comparisons with na are false; arithmetic with na is na.
        match op {
            BinOp::Or => f64::from(self.truthy(l) || self.truthy(r)),
            BinOp::And => f64::from(self.truthy(l) && self.truthy(r)),
            BinOp::Cmp(op) => {
                if !(l.is_finite() && r.is_finite()) {
                    return 0.0;
                }
                let hit = match op {
                    CmpOp::Eq => l == r,
                    CmpOp::Ne => l != r,
                    CmpOp::Lt => l < r,
                    CmpOp::Le => l <= r,
                    CmpOp::Gt => l > r,
                    CmpOp::Ge => l >= r,
                };
                f64::from(hit)
            }
            BinOp::Add(add) => match add {
                crate::parse::AddOp::Add => l + r,
                crate::parse::AddOp::Sub => l - r,
            },
            BinOp::Mul(mul) => match mul {
                crate::parse::MulOp::Mul => l * r,
                crate::parse::MulOp::Div => {
                    if r == 0.0 {
                        NA
                    } else {
                        l / r
                    }
                }
                crate::parse::MulOp::Rem => {
                    if r == 0.0 {
                        NA
                    } else {
                        l % r
                    }
                }
            },
            BinOp::Pow => l.powf(r),
        }
    }

    fn read_name(&mut self, name: &str) -> Result<f64, crate::ScriptError> {
        // Loop variables and function locals shadow globals.
        for (n, v) in self.loop_scopes.iter().rev() {
            if n == name {
                return Ok(*v);
            }
        }
        // A function PARAMETER bound to a caller series: re-evaluate the
        // caller's expression AT THIS BAR (Pine's series-passing model).
        // Checked before the scalar scope frame, which holds the bind-time
        // snapshot only as a fallback.
        for frame in self.series_args.iter().rev() {
            if let Some(expr) = frame.get(name) {
                let expr = expr.clone();
                return self.eval_f(&expr);
            }
        }
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Ok(*v);
            }
        }
        if let Some(buf) = self.vars.get(name) {
            // Pine carry-forward: a variable read before its (re)assignment
            // this bar has the value it ended the previous bar with -- this
            // is what makes `var count := count + 1` a counter. `var`
            // declarations write their slot only once (bar 0), so the carry
            // walks back to the LAST written slot, not just one bar: reading
            // a `var a = array.new()` handle at bar 5 must still find bar 0's
            // allocation. The carry applies to STICKY (`var`) state ONLY:
            // an auto var (`b2 = expr`) is recomputed every bar, and a bar
            // whose expression is na IS na -- carrying a stale value forward
            // would make `not na(b2)` guards pass on ghosts (the zone-scan
            // corruption).
            let current = buf[self.bar];
            if current.is_finite() {
                return Ok(current);
            }
            // A slot the assignment RAN on is authoritative, na included;
            // carry forward only across slots no assignment touched.
            if let Some(wrote) = self.vars_written.get(name) {
                if wrote[self.bar] {
                    return Ok(current);
                }
                for slot in (0..self.bar).rev() {
                    if wrote[slot] {
                        return Ok(buf[slot]);
                    }
                }
            }
            return Ok(current);
        }
        // Builtin series.
        self.read_member(name)
    }

    fn read_member(&self, path: &str) -> Result<f64, crate::ScriptError> {
        let c = &self.candles[self.bar];
        Ok(match path {
            "open" => c.open,
            "high" => c.high,
            "low" => c.low,
            "close" => c.close,
            "volume" => c.volume,
            "hl2" => (c.high + c.low) / 2.0,
            "hlc3" => (c.high + c.low + c.close) / 3.0,
            "ohlc4" => (c.open + c.high + c.low + c.close) / 4.0,
            "bar_index" => self.bar as f64,
            // ---- time-of-day words: UTC hours/minutes of the current bar's
            // open, so session filters (`hour >= 7 and hour < 16`) are plain
            // comparisons. `dayofweek` is Pine's 1=Sunday..7=Saturday.
            "hour" => {
                let t = self.candles.get(self.bar).map_or(0, |c| c.open_time);
                (t / 3_600_000_000_000).rem_euclid(24) as f64
            }
            "minute" => {
                let t = self.candles.get(self.bar).map_or(0, |c| c.open_time);
                (t / 60_000_000_000).rem_euclid(60) as f64
            }
            "dayofweek" => {
                let t = self.candles.get(self.bar).map_or(0, |c| c.open_time);
                let days = t.div_euclid(86_400_000_000_000);
                // 1970-01-01 was a Thursday (4).
                (days + 4).rem_euclid(7) as f64 + 1.0
            }
            "year" => {
                let t = self.candles.get(self.bar).map_or(0, |c| c.open_time);
                civil_from_nanos(t).0 as f64
            }
            "month" => {
                let t = self.candles.get(self.bar).map_or(0, |c| c.open_time);
                civil_from_nanos(t).1 as f64
            }
            "dayofmonth" => {
                let t = self.candles.get(self.bar).map_or(0, |c| c.open_time);
                civil_from_nanos(t).2 as f64
            }
            "last_bar_index" => (self.candles.len().saturating_sub(1)) as f64,
            "time" => c.open_time as f64,
            "time_close" => (c.open_time + c.timeframe.nanos()) as f64,
            "barstate.isconfirmed" => 1.0, // the host only runs closed bars
            "syminfo.mintick" => match self.inputs.mintick {
                Some(t) if t > 0.0 => t,
                _ => {
                    return Err(crate::ScriptError {
                        kind: crate::ErrorKind::Type,
                        span: crate::Span::new(1, 1),
                        message: "`syminfo.mintick` needs the host's instrument metadata (the tick size); the host did not supply one for this chart".into(),
                    })
                }
            },
            "math.pi" => std::f64::consts::PI,
            "strategy.position_size" | "strategy.position_avg_price" | "strategy.equity"
            | "strategy.openprofit" | "strategy.closedtrades" | "strategy.wintrades" => {
                self.strategy_read(path)
            }
            _ => {
                return Err(crate::ScriptError {
                    kind: crate::ErrorKind::Type,
                    span: crate::Span::new(1, 1),
                    message: format!("`{path}` is not readable here"),
                })
            }
        })
    }

    fn read_history(&mut self, base: &Expr, off: usize) -> Result<f64, crate::ScriptError> {
        // The base must be a variable or a builtin series: expressions get a
        // value only for the current bar, and Pine's `f(x)[3]` sugar is v2.
        // Beyond the beginning of the window the read is `na`, not the first
        // value -- saturating at index 0 would fabricate history.
        match &base.kind {
            ExprKind::Ident(name) => {
                if let Some(buf) = self.vars.get(name) {
                    if off > self.bar {
                        return Ok(NA);
                    }
                    // The var buffer is SPARSE (a slot holds a value only
                    // where an assignment ran); a history read gets Pine's
                    // carried value -- the most recent assignment at or
                    // before the indexed bar, the same rule `read_name`
                    // applies to the current bar.
                    let idx = self.bar - off;
                    let mut value = buf[idx];
                    if !value.is_finite() {
                        // A slot the assignment RAN on is authoritative, na
                        // included; carry across untouched slots only.
                        if let Some(wrote) = self.vars_written.get(name) {
                            for slot in (0..=idx).rev() {
                                if wrote[slot] {
                                    value = buf[slot];
                                    break;
                                }
                            }
                        }
                    }
                    return Ok(value);
                }
                let series = self.builtin_series(name)?;
                Ok(if off <= self.bar { series[self.bar - off] } else { NA })
            }
            ExprKind::Member { path } => {
                let series = self.builtin_series(path)?;
                Ok(if off <= self.bar { series[self.bar - off] } else { NA })
            }
            _ => Err(crate::ScriptError {
                kind: crate::ErrorKind::Type,
                span: crate::Span::new(1, 1),
                message: "history indexing needs a variable or a builtin series".into(),
            }),
        }
    }

    fn builtin_series(&self, name: &str) -> Result<Series, crate::ScriptError> {
        Ok(match name {
            "open" => self.candles.iter().map(|c| c.open).collect(),
            "high" => self.candles.iter().map(|c| c.high).collect(),
            "low" => self.candles.iter().map(|c| c.low).collect(),
            "close" => self.candles.iter().map(|c| c.close).collect(),
            "volume" => self.candles.iter().map(|c| c.volume).collect(),
            // Time-of-day words as whole series: the per-bar scalar path
            // (`read_member`) has always had them; history reads (`hour[1]`,
            // session-boundary tests like `hour < hour[1]`), ta consumers and
            // plot replay all come through here and used to die with
            // "not a series". Same integer math as the scalar path.
            "hour" => self
                .candles
                .iter()
                .map(|c| (c.open_time / 3_600_000_000_000).rem_euclid(24) as f64)
                .collect(),
            "minute" => self
                .candles
                .iter()
                .map(|c| (c.open_time / 60_000_000_000).rem_euclid(60) as f64)
                .collect(),
            "dayofweek" => self
                .candles
                .iter()
                .map(|c| {
                    let days = c.open_time.div_euclid(86_400_000_000_000);
                    (days + 4).rem_euclid(7) as f64 + 1.0
                })
                .collect(),
            "year" => self.candles.iter().map(|c| civil_from_nanos(c.open_time).0 as f64).collect(),
            "month" => self.candles.iter().map(|c| civil_from_nanos(c.open_time).1 as f64).collect(),
            "dayofmonth" => {
                self.candles.iter().map(|c| civil_from_nanos(c.open_time).2 as f64).collect()
            }
            "hl2" => self.candles.iter().map(|c| (c.high + c.low) / 2.0).collect(),
            "hlc3" => self
                .candles
                .iter()
                .map(|c| (c.high + c.low + c.close) / 3.0)
                .collect(),
            "ohlc4" => self
                .candles
                .iter()
                .map(|c| (c.open + c.high + c.low + c.close) / 4.0)
                .collect(),
            // The clock words are series too: `time[1]` is Pine-idiomatic and
            // a model writes it without hesitation (session resets key on it).
            "time" => self.candles.iter().map(|c| c.open_time as f64).collect(),
            "time_close" => self
                .candles
                .iter()
                .map(|c| (c.open_time + c.timeframe.nanos()) as f64)
                .collect(),
            _ => {
                return Err(crate::ScriptError {
                    kind: crate::ErrorKind::Type,
                    span: crate::Span::new(1, 1),
                    message: format!("`{name}` is not a series"),
                })
            }
        })
    }

    #[allow(clippy::too_many_lines)]
/// Borrow a heap array by handle. The handle rides a variable's ring
    /// buffer as a plain float; anything else (na, negative, out of range) is
    /// an error naming the real cause.
    fn array_ref(&mut self, handle: f64) -> Result<&mut Vec<f64>, crate::ScriptError> {
        if std::env::var("PINE_ARRAY_DEBUG").is_ok() {
            eprintln!("array_ref handle={handle} heap_len={} bar={}", self.heap.len(), self.bar);
        }
        if !handle.is_finite() || handle < 0.0 || handle >= self.heap.len() as f64 {
            return Err(crate::ScriptError {
                kind: crate::ErrorKind::Type,
                span: crate::Span::new(1, 1),
                message: "that variable does not hold an array; create one with `var a = array.new()`".into(),
            });
        }
        Ok(&mut self.heap[handle as usize])
    }

    /// `array.get(a, i)`, bounds-checked.
    fn array_read(&mut self, handle: f64, index: f64) -> Result<f64, crate::ScriptError> {
        let list = self.array_ref(handle)?;
        let i = if index.is_finite() && index >= 0.0 { index as usize } else {
            return Ok(NA);
        };
        Ok(list.get(i).copied().unwrap_or(NA))
    }

    /// A multi-output builtin's whole-window series, in output order.
    ///
    /// Reachable only from `Item::Destructure`; the scalar path refuses these
    /// callees (a MACD line read as one number is always a mistake).
    fn call_multi(
        &mut self,
        callee: &str,
        args: &[Arg],
    ) -> Result<Vec<Series>, crate::ScriptError> {
        self.tick()?;
        macro_rules! a {
            ($i:expr, $default:expr) => {
                match args.get($i) {
                    Some(arg) => {
                        let v = self.eval_f(&arg.value)?;
                        if v.is_finite() { v } else { $default }
                    }
                    None => $default,
                }
            };
        }
        let out = match callee {
            "ta.macd" => {
                let src = self.series_arg(args, 0)?;
                let (macd, signal, hist) = ta::ta_macd(
                    &src,
                    a!(1, 12.0),
                    a!(2, 26.0),
                    a!(3, 9.0),
                );
                vec![macd, signal, hist]
            }
            "ta.bb" => {
                let src = self.series_arg(args, 0)?;
                let (basis, upper, lower) = ta::ta_bb(&src, a!(1, 20.0), a!(2, 2.0));
                vec![basis, upper, lower]
            }
            "ta.dmi" => {
                let (p, m, a) = ta::ta_dmi(self.candles, a!(0, 14.0));
                vec![p, m, a]
            }
            "ta.kc" => {
                let (b, u, l) = ta::ta_kc(self.candles, a!(0, 20.0), a!(1, 2.0));
                vec![b, u, l]
            }
            "ta.supertrend" => {
                let (line, dirn) = ta::ta_supertrend(self.candles, a!(0, 3.0), a!(1, 10.0));
                vec![line, dirn]
            }
            "ta.stoch" => {
                let src = self.series_arg(args, 0)?;
                let k = ta::ta_stoch(
                    &src,
                    &self.builtin_series("high")?,
                    &self.builtin_series("low")?,
                    a!(1, 14.0),
                );
                let d = ta::ta_sma(&k, 3.0);
                vec![k, d]
            }
            other => {
                return Err(crate::ScriptError {
                    kind: crate::ErrorKind::Type,
                    span: crate::Span::new(1, 1),
                    message: format!("`{other}` does not return several values"),
                })
            }
        };
        Ok(out)
    }

    fn call_f(&mut self, callee: &str, args: &[Arg]) -> Result<f64, crate::ScriptError> {
        self.tick()?;
        // Inputs first: they are host-supplied or defaults, never computed.
        // The bool/int/string distinction rides on the call's own kind.
        // `l = line.new(...)` -- a drawing constructor in expression
        // position binds the arena handle (a statement discards it).
        if let Some(handle) = self.draw_object(callee, args)? {
            return Ok(handle);
        }
        if let Some(handle) = self.table_handle(callee, args)? {
            return Ok(handle);
        }
        // The color constructors evaluate to a packed 0xAARRGGBB float that
        // every color= argument accepts (vars, ternaries, inline calls).
        if let Some(v) = self.color_call(callee, args)? {
            return Ok(v);
        }
        if let Some(kind) = callee.strip_prefix("input.") {
            let name = self.current_input_name.clone();
            let default = self.arg_named_f(args, "defval").unwrap_or(0.0);
            return Ok(match kind {
                "bool" => {
                    f64::from(self.inputs.bools.get(&name).copied().unwrap_or(default != 0.0))
                }
                "int" | "float" => self.inputs.numbers.get(&name).copied().unwrap_or(default),
                // `src = input.source(defval=close)` -- the var BECOMES the
                // named series (a dense every-bar value, so src[i] history
                // works). The host overrides by series name.
                "source" => {
                    let chosen = self.inputs.strings.get(&name).cloned().unwrap_or_else(|| {
                        args.iter()
                            .find(|a| a.name.as_deref() == Some("defval"))
                            .or_else(|| args.first())
                            .map(|a| match &a.value.kind {
                                ExprKind::Ident(s) => s.clone(),
                                ExprKind::Member { path } => path.clone(),
                                ExprKind::Str(s) => s.clone(),
                                _ => "close".to_string(),
                            })
                            .unwrap_or_else(|| "close".to_string())
                    });
                    self.read_member(&chosen)?
                }
                // `c = input.color(defval=color.red)` -- colors ride the
                // float buffer as their u32 value; drawing color= args
                // accept the numeric form (see arg_color_named).
                "color" => {
                    let def = args
                        .iter()
                        .find(|a| a.name.as_deref() == Some("defval"))
                        .or_else(|| args.first())
                        .and_then(|a| match &a.value.kind {
                            ExprKind::Color(c) => Some(*c),
                            ExprKind::Ident(n) => named_color(n),
                            _ => None,
                        })
                        .unwrap_or(0xFF_94_A3_B8);
                    self.inputs
                        .numbers
                        .get(&name)
                        .copied()
                        .unwrap_or(def as f64)
                }
                // `tf = input.timeframe(defval="4h")` -- the string never
                // touches a float buffer: the assignment registers the var
                // name in `input_timeframes` (the Assign arm skips the
                // write), and `request.security` resolves the var there.
                "timeframe" => {
                    self.last_input_tf = Some(
                        self.inputs
                            .strings
                            .get(&name)
                            .cloned()
                            .unwrap_or_else(|| {
                                self.arg_str(args, "defval")
                                    .or_else(|| self.arg_str_pos(args, 0))
                                    .unwrap_or_else(|| "1h".to_string())
                            }),
                    );
                    0.0
                }
                _ => default,
            });
        }
        macro_rules! a {
            ($i:expr, $default:expr) => {
                match args.get($i) {
                    Some(arg) => {
                        let v = self.eval_f(&arg.value)?;
                        if v.is_finite() { v } else { $default }
                    }
                    None => $default,
                }
            };
            ($i:expr) => {
                match args.get($i) {
                    Some(arg) => self.eval_f(&arg.value),
                    None => Ok(NA),
                }
            };
        }
        match callee {
            // `time("0800-1200")` -- Pine's session filter: the bar's unix
            // nanos when the bar falls inside the session, na outside.
            // Spec: "HHMM-HHMM" or "HH:MM-HH:MM" UTC, overnight specs wrap
            // midnight ("2200-0600"), an optional ":days" suffix restricts
            // weekdays in Pine dayofweek codes (1=Sunday..7=Saturday;
            // ":23456" = Mon-Fri). `not na(time("0800-1200"))` is the gate.
            "time" => {
                let spec = match args.first().map(|a| &a.value.kind) {
                    Some(ExprKind::Str(s)) => s.clone(),
                    _ => {
                        return Err(crate::ScriptError {
                            kind: crate::ErrorKind::Type,
                            span: crate::Span::new(1, 1),
                            message: "`time(session)` needs a quoted session spec like \"0800-1200\"".into(),
                        })
                    }
                };
                let nanos = self.candles.get(self.bar).map_or(0, |c| c.open_time) as f64;
                Ok(if session_hit(&spec, nanos as i64) { nanos } else { crate::ta::NA })
            }
            // The call form of the time-of-day reads: `hour(time)` is the
            // current bar's UTC hour exactly like the bare `hour` series.
            // The argument is evaluated (it may be `time` or a timestamp)
            // and otherwise ignored -- the platform runs closed bars of one
            // series, so there is nothing else the clock could be of.
            "hour" | "minute" | "dayofweek" | "year" | "month" | "dayofmonth" => {
                // The argument is usually `time`; read it as a series so the
                // idiomatic `hour(time[1])` -- a session-boundary test --
                // computes over the referenced bar, not the current one.
                let t = a!(0)?;
                let nanos = if t.is_finite() && t.abs() > 1e15 { t } else {
                    self.candles.get(self.bar).map_or(0, |c| c.open_time) as f64
                };
                Ok(match callee {
                    "hour" => (nanos / 3_600_000_000_000.0).floor().rem_euclid(24.0),
                    "minute" => (nanos / 60_000_000_000.0).floor().rem_euclid(60.0),
                    "year" => civil_from_nanos(nanos as i64).0 as f64,
                    "month" => civil_from_nanos(nanos as i64).1 as f64,
                    "dayofmonth" => civil_from_nanos(nanos as i64).2 as f64,
                    _ => {
                        let days = (nanos / 86_400_000_000_000.0).floor();
                        (days + 4.0).rem_euclid(7.0) + 1.0
                    }
                })
            }
            // `timestamp(year, month, day, hour, minute)` builds unix nanos:
            // the anchor for drawings pinned to a date ("mark March 15").
            "timestamp" => {
                let y = a!(0)? as i64;
                let mo = a!(1)? as i64;
                let d = a!(2)? as i64;
                let h = a!(3)? as i64;
                let mi = a!(4)? as i64;
                let days = days_from_civil(y, mo, d);
                Ok((days * 86_400 + h * 3_600 + mi * 60) as f64 * 1_000_000_000.0)
            }
            "math.round_to_mintick" => {
                let v = a!(0)?;
                // na in, na out -- the warmup bars of a windowed source are
                // not a missing-tick error.
                if !v.is_finite() {
                    return Ok(crate::ta::NA);
                }
                match self.inputs.mintick {
                    Some(t) if t > 0.0 => Ok((v / t).round() * t),
                    _ => Err(crate::ScriptError {
                        kind: crate::ErrorKind::Type,
                        span: crate::Span::new(1, 1),
                        message: "`math.round_to_mintick` needs the host's instrument metadata (the tick size)".into(),
                    }),
                }
            }
            "na" => Ok(f64::from(!a!(0)?.is_finite())),
            "nz" => {
                let v = a!(0)?;
                if v.is_finite() {
                    return Ok(v);
                }
                let fallback = match args.get(1) {
                    Some(arg) => self.eval_f(&arg.value)?,
                    None => 0.0,
                };
                Ok(fallback)
            }
            "fixnan" => {
                // Carry the last finite value forward; na before the first.
                let v = a!(0)?;
                Ok(v) // full-window form handled by ta::fixnan_series in v2
            }
            "ta.sma" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_sma(&src, a!(1)?)[self.bar])
            }
            "ta.ema" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_ema(&src, a!(1)?)[self.bar])
            }
            "ta.rma" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_rma(&src, a!(1)?)[self.bar])
            }
            "ta.wma" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_wma(&src, a!(1)?)[self.bar])
            }
            "ta.rsi" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_rsi(&src, a!(1)?)[self.bar])
            }
            // Multi-output builtins: the scalar path refuses them so an
            // author learns the destructuring form instead of plotting half a
            // Bollinger band by accident.
            "ta.macd" | "ta.bb" | "ta.dmi" | "ta.kc" | "ta.supertrend" => Err(crate::ScriptError {
                kind: crate::ErrorKind::Type,
                span: crate::Span::new(1, 1),
                message: format!(
                    "`{callee}` returns 3 values; write `a, b, c = {callee}(...)`"
                ),
            }),
            "ta.atr" => Ok(ta::ta_atr(self.candles, a!(0)?)[self.bar]),
            "ta.stdev" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_stdev(&src, a!(1)?)[self.bar])
            }
            "ta.variance" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_variance(&src, a!(1)?)[self.bar])
            }
            "ta.correlation" | "ta.covariance" => {
                let x = self.series_arg(args, 0)?;
                let y = self.series_arg(args, 1)?;
                let len = a!(2)?;
                Ok(if callee == "ta.correlation" {
                    ta::ta_correlation(&x, &y, len)
                } else {
                    ta::ta_covariance(&x, &y, len)
                }[self.bar])
            }
            "ta.deviation" | "ta.median" | "ta.mode" | "ta.percentrank" | "ta.range" => {
                let src = self.series_arg(args, 0)?;
                let len = a!(1)?;
                Ok(match callee {
                    "ta.deviation" => ta::ta_deviation(&src, len),
                    "ta.median" => ta::ta_median(&src, len),
                    "ta.mode" => ta::ta_mode(&src, len),
                    "ta.percentrank" => ta::ta_percentrank(&src, len),
                    _ => ta::ta_range(&src, len),
                }[self.bar])
            }
            "ta.percentile_linear_interpolation" | "ta.percentile_nearest_rank" => {
                let src = self.series_arg(args, 0)?;
                let (len, pct) = (a!(1)?, a!(2)?);
                Ok(if callee == "ta.percentile_linear_interpolation" {
                    ta::ta_percentile_linear_interpolation(&src, len, pct)
                } else {
                    ta::ta_percentile_nearest_rank(&src, len, pct)
                }[self.bar])
            }
            "ta.vwma" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_vwma(&src, self.candles, a!(1)?)[self.bar])
            }
            "ta.swma" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_swma(&src)[self.bar])
            }
            "ta.alma" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_alma(&src, a!(1)?, a!(2, 0.85), a!(3, 6.0))[self.bar])
            }
            "ta.lsma" | "ta.linreg" => {
                let src = self.series_arg(args, 0)?;
                let f = if callee == "ta.lsma" { ta::ta_lsma } else { ta::ta_linreg };
                Ok(f(&src, a!(1)?, a!(2, 0.0))[self.bar])
            }
            "ta.tema" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_tema(&src, a!(1)?)[self.bar])
            }
            "ta.hma" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_hma(&src, a!(1)?)[self.bar])
            }
            "ta.cci" => Ok(ta::ta_cci(self.candles, a!(0)?)[self.bar]),
            "ta.mfi" => Ok(ta::ta_mfi(self.candles, a!(0)?)[self.bar]),
            "ta.willr" => Ok(ta::ta_willr(self.candles, a!(0)?)[self.bar]),
            "ta.barssince" => {
                let cond = self.series_arg(args, 0)?;
                Ok(ta::ta_barssince(&cond)[self.bar])
            }
            "ta.valuewhen" => {
                let cond = self.series_arg(args, 0)?;
                let src = self.series_arg(args, 1)?;
                let occ = a!(2)?;
                Ok(ta::ta_valuewhen(&cond, &src, if occ.is_finite() { occ } else { 0.0 })
                    [self.bar])
            }
            "ta.highestbars" => Ok(ta::ta_highestbars(self.candles, a!(0)?)[self.bar]),
            "ta.lowestbars" => Ok(ta::ta_lowestbars(self.candles, a!(0)?)[self.bar]),
            "ta.cum" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_cum(&src)[self.bar])
            }
            "ta.pivothigh" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_pivothigh(&src, a!(1)?, a!(2)?)[self.bar])
            }
            "ta.pivotlow" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_pivotlow(&src, a!(1)?, a!(2)?)[self.bar])
            }
            "ta.rising" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_rising(&src, a!(1)?)[self.bar])
            }
            "ta.falling" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_falling(&src, a!(1)?)[self.bar])
            }
            "ta.tr" => Ok(ta::ta_tr(self.candles)[self.bar]),
            "ta.highest" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_highest(&src, a!(1)?)[self.bar])
            }
            "ta.lowest" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_lowest(&src, a!(1)?)[self.bar])
            }
            "ta.change" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_change(&src)[self.bar])
            }
            "ta.mom" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_mom(&src, a!(1)?)[self.bar])
            }
            "ta.roc" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_roc(&src, a!(1)?)[self.bar])
            }
            "ta.stoch" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::ta_stoch(&src, &self.builtin_series("high")?, &self.builtin_series("low")?, a!(1)?)[self.bar])
            }
            "ta.vwap" => Ok(ta::ta_vwap(self.candles)[self.bar]),
            // ---- request.security("SYM", "tf", expr) (Phase 11, docs/23):
            // any pair, any timeframe the host pooled, per call. The first
            // two arguments are string literals (vet-checked); the third is
            // evaluated per bar over that pair's candles, as if it were the
            // script's own series. History on the result works through the
            // ordinary assign-then-offset rule, because the value lands in a
            // variable like any other.
            "request.security" => {
                let key = self.security_key(args)?;
                // Memoize per call site: the pool is index-aligned to the
                // chart and the third argument is backward-looking series
                // logic, so its whole per-bar value vector is computed once
                // on first touch (a sub-VM sweep, the same thing the per-bar
                // path did, minus the O(pool) repetition). Later bars index
                // the cached vector -- semantically identical, O(1) per bar.
                // The site key folds the AST address of the third argument
                // into the pool key, so two calls on one pool stay separate.
                let site = format!("{key}#{:p}", &args[2].value);
                if let Some(values) = self.security_value_cache.get(&site) {
                    return Ok(values.get(self.bar).copied().unwrap_or(NA));
                }
                let candles = self
                    .inputs
                    .series_pool
                    .get(&key)
                    .ok_or_else(|| crate::ScriptError {
                        kind: crate::ErrorKind::Type,
                        span: crate::Span::new(1, 1),
                        message: format!(
                            "`request.security` has no pooled series for {key}; the host must fetch and align it before the run"
                        ),
                    })?;
                // The pooled series replaces the script's own candles for the
                // third argument's evaluation: a sub-VM over that pair sees
                // `close` as the PAIR's close. One allocation per call per
                // script is bounded by the fuel meter like every other step.
                let expr = args[2].value.clone();
                // Bare `request.*` INSIDE the third argument reads the pair:
                // the sub-VM's security vec IS the pooled series (the key was
                // vetted as literals, so no recursion is expressible).
                let sub_inputs = Inputs {
                    security: candles.clone(),
                    series_pool: std::collections::HashMap::new(),
                    ..Inputs::default()
                };
                // A per-call slice of the parent's remaining fuel: a nested
                // expression cannot spend what its parent has not earned.
                let slice = self.fuel.saturating_sub(self.steps).max(1);
                let mut sub = Vm::new_for_pool(&expr, candles, &sub_inputs, slice);
                let len = candles.len();
                let mut values = Vec::with_capacity(len);
                for bar in 0..len {
                    sub.bar = bar;
                    values.push(sub.eval_f(&expr)?);
                }
                self.steps += sub.steps;
                self.security_value_cache.insert(site, values.clone());
                Ok(values.get(self.bar).copied().unwrap_or(NA))
            }
            // ---- request.data("name") (Phase 14, docs/23): a host-filled
            // platform-native series (ticker fields, funding, OI), aligned
            // onto the chart's bars. A name the host did not supply is a
            // data-missing error naming the key -- the truth, not a zero.
            "request.data" => {
                let key = match args.first().map(|a| &a.value.kind) {
                    Some(ExprKind::Str(s)) => s.clone(),
                    _ => {
                        return Err(crate::ScriptError {
                            kind: crate::ErrorKind::Type,
                            span: crate::Span::new(1, 1),
                            message: "`request.data` takes a quoted series name, e.g. request.data(\"BTCUSDT.change_pct\")".into(),
                        })
                    }
                };
                let series = self.inputs.data_series.get(&key).ok_or_else(|| crate::ScriptError {
                    kind: crate::ErrorKind::Type,
                    span: crate::Span::new(1, 1),
                    message: format!(
                        "`request.data` has no series named \"{key}\"; the host supplies platform feeds (ticker fields today, funding/OI next)"
                    ),
                })?;
                Ok(series.get(self.bar).copied().unwrap_or(NA))
            }
            // ---- request.*: the second instrument's series, bar-aligned by
            // the host. A call with no `sec=` in the header is refused here
            // with the fix named -- the same honesty the vet layer gives.
            "request.symbol" => Ok(self
                .security_ticker
                .clone()
                .map(|_| 1.0)
                .unwrap_or(NA)),
            "request.open" | "request.high" | "request.low" | "request.close"
            | "request.volume" | "request.time" => {
                let slot = self
                    .inputs
                    .security
                    .get(self.bar)
                    .ok_or_else(|| crate::ScriptError {
                        kind: crate::ErrorKind::Type,
                        span: crate::Span::new(1, 1),
                        message: format!(
                            "`{callee}` needs a second instrument: add sec=\"SYMBOL\" to the //@pine_lite header"
                        ),
                    })?;
                Ok(match callee {
                    "request.open" => slot.open,
                    "request.high" => slot.high,
                    "request.low" => slot.low,
                    "request.close" => slot.close,
                    // docs/28: the aligned bar's open time, so a script can
                    // see the second instrument's bar boundaries (a change in
                    // this series IS the other market's new bar) and anchor
                    // time-based drawing objects at them.
                    "request.time" => slot.open_time as f64,
                    _ => slot.volume,
                })
            }
            "ta.crossover" | "ta.crossunder" | "ta.cross" => {
                let l = self.series_arg(args, 0)?;
                let r = self.series_arg(args, 1)?;
                Ok(if callee == "ta.crossover" {
                    ta::ta_crossover(&l, &r)[self.bar]
                } else {
                    ta::ta_crossunder(&l, &r)[self.bar]
                })
            }
            "math.abs" => Ok(a!(0)?.abs()),
            // The trig pack: radians in and out, exactly like Pine; the
            // inverse functions return na outside their domains rather than
            // erroring (asin 2.0 is a data bug a script survives, not a vet).
            "math.sin" => Ok(a!(0)?.sin()),
            "math.cos" => Ok(a!(0)?.cos()),
            "math.tan" => Ok(a!(0)?.tan()),
            "math.asin" => {
                let v = a!(0)?;
                Ok(if (-1.0..=1.0).contains(&v) { v.asin() } else { NA })
            }
            "math.acos" => {
                let v = a!(0)?;
                Ok(if (-1.0..=1.0).contains(&v) { v.acos() } else { NA })
            }
            "math.atan" => Ok(a!(0)?.atan()),
            "math.atan2" => Ok(a!(0)?.atan2(a!(1)?)),
            "math.toradians" => Ok(a!(0)?.to_radians()),
            "math.todegrees" => Ok(a!(0)?.to_degrees()),
            "math.min" | "math.max" => {
                let mut acc = a!(0)?;
                for i in 1..args.len() {
                    let v = a!(i)?;
                    acc = if callee == "math.min" { acc.min(v) } else { acc.max(v) };
                }
                Ok(acc)
            }
            "math.floor" => Ok(a!(0)?.floor()),
            "math.ceil" => Ok(a!(0)?.ceil()),
            "math.round" => Ok(a!(0)?.round()),
            "math.sqrt" => Ok(a!(0)?.sqrt()),
            "math.log" => Ok(a!(0)?.ln()),
            "math.exp" => Ok(a!(0)?.exp()),
            "math.sign" => Ok(a!(0)?.signum()),
            "math.pow" => Ok(a!(0)?.powf(a!(1)?)),
            "math.avg" => {
                let mut sum = 0.0;
                let mut count = 0.0;
                for i in 0..args.len() {
                    let v = a!(i)?;
                    if v.is_finite() {
                        sum += v;
                        count += 1.0;
                    }
                }
                Ok(if count > 0.0 { sum / count } else { NA })
            }
            "math.sum" => {
                let src = self.series_arg(args, 0)?;
                Ok(ta::math_sum(&src, a!(1)?)[self.bar])
            }
            // ---- arrays: a variable holds a handle (heap index); the heap
            // lives on the Vm, so `var a = array.new()` is one object for the
            // whole run and `array.push(a, v)` mutates it in place.
            "array.new" => {
                if self.heap.len() >= MAX_ARRAYS {
                    return Err(crate::ScriptError {
                        kind: crate::ErrorKind::Limit,
                        span: crate::Span::new(1, 1),
                        message: format!("more than {MAX_ARRAYS} arrays; allocate them once with `var`"),
                    });
                }
                self.heap.push(Vec::new());
                Ok((self.heap.len() - 1) as f64)
            }
            "array.get" => {
                let handle = a!(0)?;
                let index = a!(1)?;
                Ok(self.array_read(handle, index)?)
            }
            "array.size" => {
                let handle = a!(0)?;
                Ok(self.array_ref(handle)?.len() as f64)
            }
            "array.first" => {
                let handle = a!(0)?;
                Ok(self.array_ref(handle)?.first().copied().unwrap_or(NA))
            }
            "array.last" => {
                let handle = a!(0)?;
                Ok(self.array_ref(handle)?.last().copied().unwrap_or(NA))
            }
            "array.min" => {
                let handle = a!(0)?;
                Ok(self
                    .array_ref(handle)?
                    .iter()
                    .copied()
                    .filter(|v| v.is_finite())
                    .fold(f64::INFINITY, f64::min))
            }
            "array.max" => {
                let handle = a!(0)?;
                Ok(self
                    .array_ref(handle)?
                    .iter()
                    .copied()
                    .filter(|v| v.is_finite())
                    .fold(f64::NEG_INFINITY, f64::max))
            }
            "array.avg" => {
                let handle = a!(0)?;
                let list = self.array_ref(handle)?;
                let finite: Vec<f64> = list.iter().copied().filter(|v| v.is_finite()).collect();
                Ok(if finite.is_empty() {
                    NA
                } else {
                    finite.iter().sum::<f64>() / finite.len() as f64
                })
            }
            "array.includes" => {
                let handle = a!(0)?;
                let needle = a!(1)?;
                Ok(f64::from(
                    self.array_ref(handle)?.iter().any(|v| (*v - needle).abs() < f64::EPSILON),
                ))
            }
            "array.from" => {
                if self.heap.len() >= MAX_ARRAYS {
                    return Err(crate::ScriptError {
                        kind: crate::ErrorKind::Limit,
                        span: crate::Span::new(1, 1),
                        message: format!("more than {MAX_ARRAYS} arrays; allocate them once with `var`"),
                    });
                }
                let mut list = Vec::with_capacity(args.len());
                for arg in args {
                    list.push(self.eval_f(&arg.value)?);
                }
                self.heap.push(list);
                Ok((self.heap.len() - 1) as f64)
            }
            "array.slice" => {
                let handle = a!(0)?;
                let (from, to) = (a!(1)?, a!(2)?);
                if self.heap.len() >= MAX_ARRAYS {
                    return Err(crate::ScriptError {
                        kind: crate::ErrorKind::Limit,
                        span: crate::Span::new(1, 1),
                        message: format!("more than {MAX_ARRAYS} arrays; allocate them once with `var`"),
                    });
                }
                let slice = {
                    let list = self.array_ref(handle)?;
                    let len = list.len();
                    let lo = if from.is_finite() { from.max(0.0) as usize } else { 0 };
                    // Pine's end index is exclusive.
                    let hi = if to.is_finite() { (to.max(0.0) as usize).min(len) } else { len };
                    if lo < hi { list[lo..hi].to_vec() } else { Vec::new() }
                };
                self.heap.push(slice);
                Ok((self.heap.len() - 1) as f64)
            }
            "array.indexof" => {
                let handle = a!(0)?;
                let needle = a!(1)?;
                Ok(self
                    .array_ref(handle)?
                    .iter()
                    .position(|v| (*v - needle).abs() < f64::EPSILON)
                    .map(|i| i as f64)
                    .unwrap_or(-1.0))
            }
            "array.concat" => {
                let (h1, h2) = (a!(0)?, a!(1)?);
                if self.heap.len() >= MAX_ARRAYS {
                    return Err(crate::ScriptError {
                        kind: crate::ErrorKind::Limit,
                        span: crate::Span::new(1, 1),
                        message: format!("more than {MAX_ARRAYS} arrays; allocate them once with `var`"),
                    });
                }
                let mut joined = { self.array_ref(h1)?.clone() };
                let second = { self.array_ref(h2)?.clone() };
                joined.extend(second);
                self.heap.push(joined);
                Ok((self.heap.len() - 1) as f64)
            }
            _ => self.call_user(callee, args),
        }
    }

    /// Evaluate an argument into a whole-window series, for the ta functions
    /// that must see history: the plot/ta split means `ta.sma(rsi(close, 14),
    /// 9)` needs rsi's *past bars*, which the VM has only as ring buffers.
    ///
    /// The rule: a series argument is either (a) a builtin series, (b) a
    /// variable (ring buffer, already per-bar), or (c) a call -- in which case
    /// the call is re-evaluated for every bar against frozen buffers. (c) is
    /// the expensive path and why fuel exists.
    fn series_arg(&mut self, args: &[Arg], index: usize) -> Result<Series, crate::ScriptError> {
        let Some(arg) = args.get(index) else {
            return Ok(vec![NA; self.candles.len()]);
        };
        let n = self.candles.len();
        match &arg.value.kind {
            ExprKind::Ident(name) => {
                if let Some(buf) = self.vars.get(name) {
                    // The var buffer is SPARSE: a slot holds a value only on
                    // the bar its assignment ran (`var x = ...` writes bar
                    // 0, `x := ...` writes the bars it fires on). Pine's
                    // semantics everywhere else are carry-forward, which the
                    // scalar path applies in `read_name`; a `ta.*` consumer
                    // needs the same rule as a whole series -- a sparse
                    // buffer is NA-riddled and `ta.crossover` against a
                    // tracked swing level silently never fires (the BOS bug).
                    let wrote = self.vars_written.get(name);
                    let mut out = Vec::with_capacity(buf.len());
                    let mut last = NA;
                    for (i, &slot) in buf.iter().enumerate() {
                        let ran = wrote.is_some_and(|w| w[i]);
                        if ran {
                            if slot.is_finite() {
                                last = slot;
                            }
                            out.push(slot);
                        } else {
                            out.push(last);
                        }
                    }
                    return Ok(out);
                }
                // A function PARAMETER as a series argument (`f(x) =>
                // ta.sma(x, n)` with `f(close)`): Pine passes the SERIES, so
                // the parameter's history is the caller expression evaluated
                // per bar. Replay it across the window (the cache rule below
                // keeps var-free caller expressions to one pass).
                for frame in self.series_args.iter().rev() {
                    if let Some(expr) = frame.get(name) {
                        let expr = expr.clone();
                        let key = std::ptr::from_ref(&expr).addr();
                        if !Self::expr_depends_on_vars(&expr) {
                            if let Some(cached) = self.series_cache.get(&key) {
                                return Ok(cached.clone());
                            }
                        }
                        let saved_bar = self.bar;
                        let mut out = vec![NA; n];
                        for (bar, slot) in out.iter_mut().enumerate() {
                            self.bar = bar;
                            *slot = self.eval_f(&expr)?;
                        }
                        self.bar = saved_bar;
                        if !Self::expr_depends_on_vars(&expr) {
                            self.series_cache.insert(key, out.clone());
                        }
                        return Ok(out);
                    }
                }
                self.builtin_series(name)
            }
            ExprKind::Member { path } => self.builtin_series(path),
            _ => {
                // Pure-builtin ta calls (`ta.sma(close, 9)`) never change:
                // compute once and cache. Var-dependent ones recompute, which
                // is the real cost the fuel budget meters.
                let key = std::ptr::from_ref(&arg.value).addr();
                if !Self::expr_depends_on_vars(&arg.value) {
                    if let Some(cached) = self.series_cache.get(&key) {
                        return Ok(cached.clone());
                    }
                }
                let saved_bar = self.bar;
                let mut out = vec![NA; n];
                for (bar, slot) in out.iter_mut().enumerate() {
                    self.bar = bar;
                    *slot = self.eval_f(&arg.value)?;
                }
                self.bar = saved_bar;
                if !Self::expr_depends_on_vars(&arg.value) {
                    self.series_cache.insert(key, out.clone());
                }
                Ok(out)
            }
        }
    }

    /// Whether an expression reads a script variable (anything that is not a
    /// builtin series name or a namespaced read). Used to decide whether a
    /// ta call's series can be cached for the whole run.
    fn expr_depends_on_vars(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Ident(name) => !matches!(
                name.as_str(),
                "open" | "high" | "low" | "close" | "volume" | "hl2" | "hlc3" | "ohlc4"
            ) && !name.contains('.'),
            ExprKind::Member { .. } | ExprKind::Num(_) | ExprKind::Bool(_) | ExprKind::Str(_)
            | ExprKind::Color(_) | ExprKind::Na => false,
            ExprKind::NaChecked { value } => Self::expr_depends_on_vars(value),
            ExprKind::Bin { left, right, .. } => {
                Self::expr_depends_on_vars(left) || Self::expr_depends_on_vars(right)
            }
            ExprKind::Un { expr, .. } => Self::expr_depends_on_vars(expr),
            ExprKind::Ternary { cond, then, els } => {
                Self::expr_depends_on_vars(cond)
                    || Self::expr_depends_on_vars(then)
                    || Self::expr_depends_on_vars(els)
            }
            ExprKind::History { base, offset } => {
                Self::expr_depends_on_vars(base) || Self::expr_depends_on_vars(offset)
            }
            ExprKind::Call { args, .. } => {
                args.iter().any(|a| Self::expr_depends_on_vars(&a.value))
            }
        }
    }

    /// Substitute parameter names in a caller argument with their bound
    /// expressions (one level per existing frame, walking outward): this is
    /// what keeps a recursive or forwarding call's series binding from
    /// becoming a self-referential frame that reads itself forever.
    fn resolve_param(&self, expr: &Expr) -> Expr {
        if let ExprKind::Ident(name) = &expr.kind {
            for frame in self.series_args.iter().rev() {
                if let Some(bound) = frame.get(name) {
                    return bound.clone();
                }
            }
        }
        expr.clone()
    }

    fn call_user(&mut self, callee: &str, args: &[Arg]) -> Result<f64, crate::ScriptError> {
        // Depth first: a recursive body must name RECURSION as its refusal
        // (the fuel message would be true but misleading), and each frame
        // binds caller expressions, so the cap also bounds that growth.
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(crate::ScriptError {
                kind: crate::ErrorKind::Limit,
                span: crate::Span::new(1, 1),
                message: format!(
                    "call depth exceeds {MAX_CALL_DEPTH}; recursion is refused"
                ),
            });
        }
        self.tick()?;
        let body = *self
            .funcs
            .get(callee)
            .ok_or_else(|| crate::ScriptError {
                kind: crate::ErrorKind::Type,
                span: crate::Span::new(1, 1),
                message: format!("`{callee}` is not defined"),
            })?;
        // Bind arguments by the signature's parameter names -- the checker
        // already guaranteed the arity matches. PINE SEMANTICS: a parameter
        // binds the caller's EXPRESSION as a series, not its value snapshot
        // -- `f(close)` means every read of `x` inside the body sees close
        // AT THE READING BAR, which is what makes `f(x) => ta.sma(x, n)`
        // the sma of the caller's series over its real history. Call-site
        // expressions are kept in `series_args` (keyed by param name); a
        // read of a param consults that map first, so plain arithmetic
        // (`f(high - low)`) replays the expression per bar too.
        let def = self.script.items.iter().find_map(|item| match item {
            Item::FuncDef { name: n, params, defaults, .. } if n == callee => {
                Some((params.clone(), defaults.clone()))
            }
            _ => None,
        });
        let (def_params, def_defaults) = def.unwrap_or_else(|| (Vec::new(), Vec::new()));
        let mut locals = std::collections::HashMap::new();
        let mut series_args: std::collections::HashMap<String, Expr> = std::collections::HashMap::new();
        for (i, arg) in args.iter().enumerate() {
            let param = def_params.get(i).cloned().unwrap_or_else(|| format!("arg{i}"));
            // Transitive substitution: binding `n -> Ident("n")` would make a
            // parameter read resolve against ITS OWN frame -- an eval loop
            // the depth cap can never see (no call frame is pushed). Resolve
            // the argument through existing frames first; a recursion
            // `f(f(n-1))` therefore binds a real expression, and a bare
            // self-name binds nothing (the scalar local rules).
            let resolved = self.resolve_param(&arg.value);
            series_args.insert(param.clone(), resolved);
            let v = self.eval_f(&arg.value)?;
            locals.insert(param, v);
        }
        // Omitted trailing parameters (v1.1 defaults): each default is
        // evaluated fresh per call, in the CALLER's scope, and bound exactly
        // like an explicit argument -- series semantics included, so a
        // default of `close[1]` is the caller's yesterday and `b = a * 2`
        // tracks the bound `a`. The checker already refused a call that
        // omits a REQUIRED parameter.
        for i in args.len()..def_params.len() {
            let Some(default_expr) = def_defaults.get(i).and_then(|d| d.as_ref()) else {
                continue;
            };
            let param = def_params[i].clone();
            let resolved = self.resolve_param(default_expr);
            series_args.insert(param.clone(), resolved);
            let v = self.eval_f(default_expr)?;
            locals.insert(param, v);
        }
        // Named parameters by position: the checker knows the signature; the
        // VM binds positionally (docs/23: positional order matches the table).
        self.series_args.push(series_args);
        self.scopes.push(locals);
        self.call_depth += 1;
        let mut result = NA;
        let last = body.len().saturating_sub(1);
        for (i, item) in body.iter().enumerate() {
            if i == last {
                if let Item::Expr { expr, .. } = item {
                    result = self.eval_f(expr)?;
                    continue;
                }
            }
            self.item(item)?;
        }
        self.call_depth -= 1;
        self.scopes.pop();
        self.series_args.pop();
        Ok(result)
    }

    // ---- argument helpers ----

    fn arg_f(&mut self, args: &[Arg], index: usize) -> Result<Option<f64>, crate::ScriptError> {
        match args.get(index) {
            Some(arg) => Ok(Some(self.eval_f(&arg.value)?)),
            None => Ok(None),
        }
    }

    fn arg_named_f(&mut self, args: &[Arg], name: &str) -> Option<f64> {
        let arg = args.iter().find(|a| a.name.as_deref() == Some(name))?;
        self.eval_f(&arg.value).ok()
    }

    /// The string-expression evaluator: labels take TEXT, and text is a
    /// real expression now -- a quoted literal, `"..." + ...` concatenation,
    /// `str.tostring(value)` / `str.tostring(value, "#.##")`, or
    /// `str.format("{0} {1}", a, b)`. Strings are expression-level only in
    /// v1: no variable holds one (the var buffers are float rings), so an
    /// Ident here is a vet-time shape error, not a runtime one. A purely
    /// numeric expression in a text slot is coerced with the plain tostring
    /// -- convenient and unambiguous.
    fn eval_s(&mut self, e: &Expr) -> Result<String, crate::ScriptError> {
        match &e.kind {
            ExprKind::Str(s) => Ok(s.clone()),
            ExprKind::Bin { op: crate::parse::BinOp::Add(crate::parse::AddOp::Add), left, right } => {
                Ok(format!("{}{}", self.eval_s(left)?, self.eval_s(right)?))
            }
            ExprKind::Call { callee, args } if callee == "str.tostring" => {
                let v = self
                    .arg_f(args, 0)?
                    .ok_or_else(|| crate::ScriptError {
                        kind: crate::ErrorKind::Type,
                        span: crate::Span::new(1, 1),
                        message: "`str.tostring` needs a value".into(),
                    })?;
                let fmt = self.arg_str(args, "format").or_else(|| self.arg_str_pos(args, 1));
                Ok(format_number(v, fmt.as_deref()))
            }
            ExprKind::Call { callee, args } if callee == "str.format" => {
                let mut out = match args.first().map(|a| &a.value.kind) {
                    Some(ExprKind::Str(t)) => t.clone(),
                    _ => {
                        return Err(crate::ScriptError {
                            kind: crate::ErrorKind::Type,
                            span: crate::Span::new(1, 1),
                            message: "`str.format` argument 1 must be a quoted template".into(),
                        })
                    }
                };
                for (i, a) in args.iter().enumerate().skip(1) {
                    let idx = i - 1;
                    // {2} plain, or {2,number,#.##} with the tostring spec.
                    let mut head = format!("{{{idx}}}");
                    if out.contains(&head) {
                        let text = self.eval_s(&a.value)?;
                        out = out.replace(&head, &text);
                    }
                    // Brace-scan for the decorated form: {idx,anything}.
                    let mut scan = 0usize;
                    while let Some(open) = out[scan..].find(&format!("{{{idx},")) {
                        let start = scan + open;
                        let Some(close) = out[start..].find('}') else { break };
                        let spec = &out[start..=start + close];
                        let parts: Vec<&str> = spec
                            .trim_start_matches('{')
                            .trim_end_matches('}')
                            .split(',')
                            .collect();
                        let fmt = parts.get(2).map(|s| s.trim().to_string());
                        let v = self.eval_f(&a.value)?;
                        let text = format_number(v, fmt.as_deref());
                        out = out.replacen(spec, &text, 1);
                        scan = start + text.len();
                        head.clear();
                    }
                }
                Ok(out)
            }
            ExprKind::Call { callee, args } if callee == "array.join" => {
                let handle = self
                    .arg_f(args, 0)?
                    .ok_or_else(|| crate::ScriptError {
                        kind: crate::ErrorKind::Type,
                        span: crate::Span::new(1, 1),
                        message: "`array.join` needs an array".into(),
                    })?;
                let sep = match args.get(1) {
                    Some(a) => self.eval_s(&a.value)?,
                    None => ",".to_string(),
                };
                Ok(self
                    .array_ref(handle)?
                    .iter()
                    .map(|v| if v.is_finite() { format_number(*v, None) } else { "na".to_string() })
                    .collect::<Vec<_>>()
                    .join(&sep))
            }
            ExprKind::Member { path } if path == "syminfo.ticker" => Ok(self
                .candles
                .first()
                .map(|c| c.symbol.clone())
                .unwrap_or_default()),
            // Dotted paths parse as Ident in some positions; the chart
            // symbol must read as text in all of them.
            ExprKind::Ident(name) if name == "syminfo.ticker" => Ok(self
                .candles
                .first()
                .map(|c| c.symbol.clone())
                .unwrap_or_default()),
            ExprKind::Ident(name) if self.vars.contains_key(name) => Err(crate::ScriptError {
                kind: crate::ErrorKind::Type,
                span: crate::Span::new(1, 1),
                message: format!(
                    "`{name}` can't be read as text: v1 strings are expression-level (no variable holds one) -- build the text inline with str.tostring/str.format"
                ),
            }),
            _ => {
                // Numeric coercion in a text slot (builtin series like
                // bar_index land here too -- they are not vars).
                let v = self.eval_f(e)?;
                Ok(format_number(v, None))
            }
        }
    }

    /// The label text args, through eval_s (fallible: a bad text expression
    /// is a script error, not an empty label).
    fn arg_text(&mut self, args: &[Arg], name: &str, pos: usize) -> Result<Option<String>, crate::ScriptError> {
        let arg = args
            .iter()
            .find(|a| a.name.as_deref() == Some(name))
            .or_else(|| args.get(pos).filter(|a| a.name.is_none()));
        match arg {
            Some(a) => Ok(Some(self.eval_s(&a.value)?)),
            None => Ok(None),
        }
    }

    fn arg_str_pos(&self, args: &[Arg], index: usize) -> Option<String> {
        match args.get(index) {
            Some(Arg { value: Expr { kind: ExprKind::Str(s), .. }, .. }) => Some(s.clone()),
            _ => None,
        }
    }

    /// `extend=extend.right` on a line: the value is a dotted constant,
    /// not a string literal. Maps to "" | "right" | "left" | "both";
    /// anything else is a clear error (Pine has exactly these four).
    fn arg_extend(&self, args: &[Arg]) -> Result<String, crate::ScriptError> {
        let Some(arg) = args.iter().find(|a| a.name.as_deref() == Some("extend")) else {
            return Ok(String::new());
        };
        let path = match &arg.value.kind {
            ExprKind::Member { path } => path.as_str(),
            ExprKind::Ident(name) => name.as_str(),
            _ => "",
        };
        match path {
            "extend.none" => Ok(String::new()),
            "extend.right" => Ok("right".to_string()),
            "extend.left" => Ok("left".to_string()),
            "extend.both" => Ok("both".to_string()),
            _ => Err(crate::ScriptError {
                kind: crate::ErrorKind::Type,
                span: crate::Span::new(1, 1),
                message: "extend= takes one of extend.none, extend.right, extend.left, extend.both".into(),
            }),
        }
    }

    /// A color read by POSITION (the setters take `line.set_color(l,
    /// color.red)`): constants, named colors, or a numeric input.color var.
    fn arg_color_pos(&mut self, args: &[Arg], index: usize) -> Option<u32> {
        let arg = args.get(index)?;
        match &arg.value.kind {
            ExprKind::Color(c) => Some(*c),
            ExprKind::Ident(name) => named_color(name).or_else(|| {
                if self.vars.contains_key(name) {
                    self.eval_f(&arg.value).ok().filter(|v| v.is_finite()).map(|v| v as u32)
                } else {
                    None
                }
            }),
            _ => self.eval_f(&arg.value).ok().filter(|v| v.is_finite()).map(|v| v as u32),
        }
    }

    /// The extend constant by POSITION (`line.set_extend(l, extend.right)`).
    fn extend_from_pos(&self, args: &[Arg], index: usize) -> Result<String, crate::ScriptError> {
        let path = match args.get(index).map(|a| &a.value.kind) {
            Some(ExprKind::Member { path }) => path.as_str(),
            Some(ExprKind::Ident(name)) => name.as_str(),
            _ => "",
        };
        match path {
            "extend.none" => Ok(String::new()),
            "extend.right" => Ok("right".to_string()),
            "extend.left" => Ok("left".to_string()),
            "extend.both" => Ok("both".to_string()),
            _ => Err(crate::ScriptError {
                kind: crate::ErrorKind::Type,
                span: crate::Span::new(1, 1),
                message: "set_extend takes one of extend.none, extend.right, extend.left, extend.both".into(),
            }),
        }
    }

    fn arg_str(&self, args: &[Arg], name: &str) -> Option<String> {
        let arg = args.iter().find(|a| a.name.as_deref() == Some(name))?;
        match &arg.value.kind {
            ExprKind::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    fn arg_color(&mut self, args: &[Arg]) -> Option<u32> {
        self.arg_color_named(args, "color")
    }

    /// A named color argument (`border_color=color.red`): the same accepted
    /// spellings as `color=` -- a `#rrggbb` literal or a `color.*` name.
    fn arg_color_named(&mut self, args: &[Arg], name: &str) -> Option<u32> {
        let arg = args.iter().find(|a| a.name.as_deref() == Some(name))?;
        match &arg.value.kind {
            ExprKind::Color(c) => Some(*c),
            ExprKind::Ident(name) => named_color(name).or_else(|| {
                // An input.color var (or any numeric) holds the u32 as a
                // float; read it.
                if self.vars.contains_key(name) {
                    self.eval_f(&arg.value).ok().filter(|v| v.is_finite()).map(|v| v as u32)
                } else {
                    None
                }
            }),
            _ => self.eval_f(&arg.value).ok().filter(|v| v.is_finite()).map(|v| v as u32),
        }
    }
}

/// The 17 named colors Pine has; the handful a chart legend needs.
fn named_color(name: &str) -> Option<u32> {
    Some(match name {
        "color.red" => 0xFF_EF_44_44,
        "color.green" => 0xFF_22_C5_5E,
        "color.blue" => 0xFF_3B_82_F6,
        "color.orange" => 0xFF_F9_73_16,
        "color.yellow" => 0xFF_EA_B3_08,
        "color.purple" => 0xFF_A8_55_F7,
        "color.white" => 0xFF_F8_FA_FC,
        "color.gray" => 0xFF_94_A3_B8,
        "color.teal" => 0xFF_2D_D4_BF,
        "color.lime" => 0xFF_84_CC_16,
        "color.aqua" => 0xFF_22_D3_EE,
        "color.maroon" => 0xFF_7F_1D_1D,
        "color.navy" => 0xFF_1E_3A_8A,
        "color.olive" => 0xFF_80_80_00,
        "color.silver" => 0xFF_C0_C0_C0,
        "color.fuchsia" => 0xFF_D9_46_EF,
        "color.black" => 0xFF_13_17_22,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lex, parse, typecheck, limits};
    use analytics_core::types::Timeframe;

    #[test]
    fn two_plotshapes_both_fire() {
        // Mirrors the live probe: a state plotshape plus a crossover
        // plotshape on consecutive lines. Both must produce shapes.
        let candles: Vec<Candle> = (0..10).map(|i| candle(i, 100.0 + i as f64)).collect();
        let out = run_src(
            "//@pine_lite version=1 overlay=true title=\"t\"\n\
             plotshape(high > 105.3, shape=\"circle\", location_value=high)\n\
             plotshape(ta.crossover(high, 105.3), shape=\"cross\", location_value=high)",
            &candles,
        );
        assert_eq!(out.shapes.len(), 6, "5 state bars (5-9) + 1 cross bar (5)");
    }

    #[test]
    fn crossover_against_a_sparse_var_fires() {
        // Regression: a `var` that only reassigns on pivot bars has a sparse
        // write buffer; `ta.crossover(high, ph1)` against it used to see
        // NA-riddled data and never fire. Pine's carry-forward is the law:
        // the var's value at every bar is its most recent assignment.
        // (concat! with real newlines: the backslash-newline continuation
        // strips the if-body's indentation and flattens the statement to
        // top level.)
        let candles: Vec<Candle> = (0..20).map(|i| candle(i, 100.0 + i as f64)).collect();
        let src = concat!(
            "//@pine_lite version=1 overlay=true title=\"t\"
",
            "var ph1 = na
",
            "if bar_index == 2
",
            "    ph1 := 104.0
",
            "cross = ta.crossover(high, ph1)
",
            "plotshape(cross, shape=\"cross\", location_value=high)
",
        );
        let out = run_src(src, &candles);
        // high = close + 1 = 101+i; crosses 104 at bar 4 (104 <= 104, 105 > 104)
        assert_eq!(out.shapes.len(), 1, "the crossover fires once, at the cross bar");
        assert_eq!(out.shapes[0].bar, 4);
    }

    #[test]
    fn var_history_read_carries_forward() {
        // ph1[1] on a bar with no assignment must read the carried value,
        // not the sparse slot's NA.
        let candles: Vec<Candle> = (0..10).map(|i| candle(i, 100.0 + i as f64)).collect();
        let src = concat!(
            "//@pine_lite version=1 overlay=true title=\"t\"
",
            "var ph1 = na
",
            "if bar_index == 2
",
            "    ph1 := 42.0
",
            "plot(ph1[1], title=\"h\")
",
        );
        let out = run_src(src, &candles);
        let values = &out.plots[0].values;
        assert!(values[0].is_nan() && values[1].is_nan() && values[2].is_nan(),
            "before the first assignment the history is na: {values:?}");
        assert_eq!(values[3], 42.0, "bar 3 reads bar 2's assignment");
        assert_eq!(values[9], 42.0, "bar 9 still reads the carried assignment");
    }

    #[test]
    fn plotshape_crossover_inline() {
        // candle(i, 100+i): high = 101+i; crosses 105.3 at bar 5.
        let candles: Vec<Candle> = (0..10).map(|i| candle(i, 100.0 + i as f64)).collect();
        let out = run_src(
            "//@pine_lite version=1 overlay=true title=\"t\"\n\
             plotshape(ta.crossover(high, 105.3), shape=\"cross\", location_value=high)",
            &candles,
        );
        assert_eq!(out.shapes.len(), 1);
        assert_eq!(out.shapes[0].bar, 5);
    }

    #[test]
    fn plotshape_location_value_is_the_markers_own_bar() {
        // Regression: `location_value=` used to be evaluated once, on the
        // final bar (where the deferred plot replays), stamping every marker
        // with the last bar's extreme -- the marker field rode live price.
        // Each marker must sit at the high of the bar its condition fired on.
        let candles: Vec<Candle> = (0..10).map(|i| candle(i, 100.0 + i as f64)).collect();
        let out = run_src(
            "//@pine_lite version=1 overlay=true title=\"t\"\n\
             plotshape(close > 0.0, shape=\"circle\", location_value=high)",
            &candles,
        );
        assert_eq!(out.shapes.len(), 10);
        for shape in &out.shapes {
            // candle(i, c) has high = c + 1 = 101 + i
            assert_eq!(
                shape.value,
                101.0 + shape.bar as f64,
                "marker at bar {} must carry that bar's high",
                shape.bar
            );
        }
    }

    fn candle(i: usize, close: f64) -> Candle {
        Candle {
            symbol: "T".into(),
            timeframe: Timeframe::M1,
            open_time: i as i64 * 60_000_000_000,
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume: 10.0,
            buy_volume: 5.0,
            sell_volume: 5.0,
        }
    }

    fn run_src(src: &str, candles: &[Candle]) -> Output {
        let (_header, tokens) = lex::lex(src).expect("lex");
        let script = parse::parse(tokens).expect("parse");
        assert!(typecheck::check(&script).is_empty(), "type errors");
        assert!(limits::check(&script).is_empty(), "limit errors");
        run(&script, candles, &Inputs::default()).expect("run")
    }

    #[test]
    fn rsi_script_plots_a_series_matching_the_crate() {
        let candles: Vec<Candle> = (0..40)
            .map(|i| candle(i, 100.0 + ((i % 7) as f64)))
            .collect();
        let src = "//@pine_lite version=1 overlay=false title=\"RSI\"\n\
                   r = ta.rsi(close, 14)\n\
                   plot(r, title=\"RSI\", color=color.purple)\n";
        let out = run_src(src, &candles);
        assert_eq!(out.plots.len(), 1);
        let expected = ta::ta_rsi(&ta::closes(&candles), 14.0);
        for (got, want) in out.plots[0].values.iter().zip(&expected) {
            if want.is_finite() {
                assert!((got - want).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn history_indexing_reads_the_ring_buffer() {
        let candles: Vec<Candle> = (0..10).map(|i| candle(i, i as f64)).collect();
        let src = "//@pine_lite version=1\n\
                   x = close\n\
                   d = close[1] - close[2]\n";
        let out = run_src(src, &candles);
        assert!(out.plots.is_empty());
        // No crash is the contract here; value checks come via plots below.
    }

    #[test]
    fn history_of_a_variable_is_visible_through_a_plot() {
        let candles: Vec<Candle> = (0..12).map(|i| candle(i, i as f64)).collect();
        let src = "//@pine_lite version=1\n\
                   x = close\n\
                   lag = x[3]\n\
                   plot(lag, title=\"lag\")\n";
        let out = run_src(src, &candles);
        let values = &out.plots[0].values;
        assert!((values[5] - 2.0).abs() < 1e-9, "{}", values[5]);
        assert!(values[2].is_nan(), "before enough history the lag is na");
    }

    #[test]
    fn var_state_carries_across_bars() {
        let candles: Vec<Candle> = (0..20).map(|i| candle(i, i as f64)).collect();
        let src = "//@pine_lite version=1\n\
                   var count = 0\n\
                   count := count + 1\n\
                   plot(count, title=\"n\")\n";
        let out = run_src(src, &candles);
        let values = &out.plots[0].values;
        assert!((values[0] - 1.0).abs() < 1e-9);
        assert!((values[19] - 20.0).abs() < 1e-9);
    }

    #[test]
    fn ternary_and_bools_behave_like_pine() {
        let candles: Vec<Candle> = (0..6).map(|i| candle(i, i as f64)).collect();
        let src = "//@pine_lite version=1\n\
                   up = close > close[1]\n\
                   plot(up ? 1 : 0, title=\"u\")\n";
        let out = run_src(src, &candles);
        let values = &out.plots[0].values;
        assert_eq!(values[0], 0.0, "close[1] on bar 0 is na; the comparison is false");
        assert_eq!(values[3], 1.0);
    }

    #[test]
    fn strategy_intents_are_recorded() {
        let candles: Vec<Candle> = (0..30).map(|i| candle(i, 100.0 + ((i % 7) as f64))).collect();
        // Real newlines with real indentation: Rust's `\n\` continuation
        // strips the next line's leading whitespace, which would flatten the
        // if-body out of the block.
        let src = concat!(
            "//@pine_lite version=1\n",
            "cross_up = ta.crossover(close, ta.sma(close, 5))\n",
            "if cross_up\n",
            "    strategy.entry(\"long\", direction=\"long\")\n",
            "plot(close)\n",
        );
        let out = run_src(src, &candles);
        assert!(out.strategy_used);
        assert!(!out.intents.is_empty());
        assert!(out.intents.iter().all(|(_, i)| matches!(i, Intent::Entry { long: true, .. })));
    }

    #[test]
    fn crossover_over_a_ta_call_works() {
        let candles: Vec<Candle> = (0..60).map(|i| candle(i, 100.0 + ((i % 9) as f64))).collect();
        let src = "//@pine_lite version=1\n\
                   up = ta.crossover(close, ta.sma(close, 9))\n\
                   plot(up ? 1 : 0, title=\"x\")\n";
        let out = run_src(src, &candles);
        assert!(out.plots[0].values.contains(&1.0));
    }

    #[test]
    fn na_propagates_and_comparisons_are_false() {
        let candles: Vec<Candle> = (0..20).map(|i| candle(i, i as f64)).collect();
        let src = "//@pine_lite version=1\n\
                   m = ta.sma(close, 20)\n\
                   plot(m, title=\"m\")\n";
        let out = run_src(src, &candles);
        assert!(out.plots[0].values[0].is_nan());
    }

    #[test]
    fn for_loop_variable_is_bound_inside_the_body() {
        let candles: Vec<Candle> = (0..5).map(|i| candle(i, 100.0)).collect();
        // Indentation must survive: see the note on `concat!` above.
        let src = concat!(
            "//@pine_lite version=1\n",
            "s = 0.0\n",
            "for i = 1 to 4\n",
            "    s := s + i\n",
            "plot(s, title=\"s\")\n",
        );
        let out = run_src(src, &candles);
        assert!((out.plots[0].values[0] - 10.0).abs() < 1e-9, "1+2+3+4=10, got {}", out.plots[0].values[0]);
    }

    #[test]
    fn for_loop_sums_a_window_of_history() {
        let candles: Vec<Candle> = (0..10).map(|i| candle(i, i as f64)).collect();
        let src = concat!(
            "//@pine_lite version=1\n",
            "s = 0.0\n",
            "for k = 0 to 3\n",
            "    s := s + close[k]\n",
            "plot(s, title=\"sum4\")\n",
        );
        let out = run_src(src, &candles);
        // Bar 9: close[0..3] = 9+8+7+6 = 30.
        assert!((out.plots[0].values[9] - 30.0).abs() < 1e-9, "got {}", out.plots[0].values[9]);
    }

    #[test]
    fn a_zero_step_loop_never_runs() {
        let candles: Vec<Candle> = (0..5).map(|i| candle(i, 100.0)).collect();
        let src = concat!(
            "//@pine_lite version=1\n",
            "s = 0.0\n",
            "for i = 1 to 4 by 0\n",
            "    s := s + 1\n",
            "plot(s, title=\"s\")\n",
        );
        let out = run_src(src, &candles);
        assert_eq!(out.plots[0].values[0], 0.0);
    }

    #[test]
    fn recursion_is_refused_at_the_depth_cap() {
        let candles: Vec<Candle> = (0..5).map(|i| candle(i, 100.0)).collect();
        let src = concat!(
            "//@pine_lite version=1\n",
            "f(n) =>\n",
            "    f(n)\n",
            "x = f(1)\n",
            "plot(close)\n",
        );
        let result = run_src_safe(src, &candles);
        let err = result.expect_err("recursion refused");
        assert!(err.message.contains("recursion is refused"), "{err:?}");
    }

    #[test]
    fn a_huge_loop_is_killed_by_fuel() {
        let candles: Vec<Candle> = (0..5).map(|i| candle(i, 100.0)).collect();
        // 5 candles -> fuel = 1000 steps. Ten million iterations cannot fit;
        // the loop is killed mid-run and reported, never half-drawn.
        let src = concat!(
            "//@pine_lite version=1\n",
            "s = 0.0\n",
            "for i = 1 to 10000000\n",
            "    s := s + 1\n",
            "plot(close)\n",
        );
        let result = run_src_safe(src, &candles);
        let err = result.expect_err("fuel killed");
        assert!(err.message.contains("step budget"), "{err:?}");
    }

    /// Like [`run_src`] but returns the error instead of panicking.
    fn run_src_safe(src: &str, candles: &[Candle]) -> Result<Output, crate::ScriptError> {
        let (_header, tokens) = lex::lex(src).expect("lex");
        let script = parse::parse(tokens).expect("parse");
        assert!(typecheck::check(&script).is_empty(), "type errors");
        assert!(limits::check(&script).is_empty(), "limit errors");
        run(&script, candles, &Inputs::default())
    }

    /// The SMC zone idiom the studio's system prompt teaches, verbatim: zones
    /// live in parallel arrays, mitigation is marked per bar, and each live
    /// zone is drawn ONCE per run on the last bar as a `box.new` whose right
    /// edge extends into the future (the canvas clips it at the plot edge --
    /// TradingView's "extend to now" look). A zone is a band, so it is a box;
    /// a `plotshape` triangle marks an event and is never the drawing for a
    /// zone. If a language change strands this script, the prompt is teaching
    /// failure -- this test fails before a model does.
    const SMC_ZONE_IDIOM: &str = concat!(
        "//@pine_lite version=1 overlay=true title=\"SMC: Fair Value Gaps\"\n",
        "var bull_top = array.new()\n",
        "var bull_bot = array.new()\n",
        "var bull_bar = array.new()\n",
        "var bear_top = array.new()\n",
        "var bear_bot = array.new()\n",
        "var bear_bar = array.new()\n",
        "bullish_fvg = low > high[2]\n",
        "bearish_fvg = high < low[2]\n",
        "if bullish_fvg\n",
        "    array.push(bull_top, low)\n",
        "    array.push(bull_bot, high[2])\n",
        "    array.push(bull_bar, bar_index - 2)\n",
        "if bearish_fvg\n",
        "    array.push(bear_top, low[2])\n",
        "    array.push(bear_bot, high)\n",
        "    array.push(bear_bar, bar_index - 2)\n",
        "n_bull = array.size(bull_top)\n",
        "n_bear = array.size(bear_top)\n",
        "for i = 0 to 24\n",
        "    if i < n_bull\n",
        "        bb = array.get(bull_bot, i)\n",
        "        if not na(bb)\n",
        "            if low < bb\n",
        "                array.set(bull_bot, i, na)\n",
        "    if i < n_bear\n",
        "        bt = array.get(bear_top, i)\n",
        "        if not na(bt)\n",
        "            if high > bt\n",
        "                array.set(bear_top, i, na)\n",
        "if bar_index == last_bar_index\n",
        "    for i = 0 to 24\n",
        "        if i < n_bull\n",
        "            t = array.get(bull_top, i)\n",
        "            b = array.get(bull_bot, i)\n",
        "            if not na(b)\n",
        "                box.new(array.get(bull_bar, i), t, bar_index + 10000, b, color=color.teal)\n",
        "        if i < n_bear\n",
        "            t2 = array.get(bear_top, i)\n",
        "            b2 = array.get(bear_bot, i)\n",
        "            if not na(b2)\n",
        "                box.new(array.get(bear_bar, i), t2, bar_index + 10000, b2, color=color.red)\n",
    );

    #[test]
    fn the_smc_zone_idiom_vets_runs_and_draws_boxes() {
        // Bar 0..2: a bullish FVG (bar 2's low clears bar 0's high), zone
        // 101..104.5 born at bar 0. Bar 4 dips through it -- mitigated.
        // Bar 6..8: a second gap, 103.5..106.5, never revisited -- live.
        let ohlc: [(f64, f64, f64, f64); 12] = [
            (100.0, 101.0, 99.0, 100.0),
            (102.0, 105.0, 102.0, 104.0),
            (105.0, 107.0, 104.5, 106.0),
            (105.0, 106.0, 103.0, 105.0),
            (104.0, 105.0, 100.0, 101.0),
            // Bar 5's high must reach 104.5: any lower and bar 7's low
            // (104) gaps over it, and the fixture grows a third FVG.
            (101.0, 104.5, 99.5, 102.0),
            (102.0, 103.5, 101.5, 103.0),
            (104.0, 107.0, 104.0, 106.0),
            (107.0, 109.0, 106.5, 108.0),
            (108.0, 110.0, 107.0, 109.0),
            (109.0, 111.0, 108.0, 110.0),
            (110.0, 112.0, 109.0, 111.0),
        ];
        let candles: Vec<Candle> = ohlc
            .iter()
            .enumerate()
            .map(|(i, &(o, h, l, c))| Candle {
                symbol: "T".into(),
                timeframe: Timeframe::M1,
                open_time: i as i64 * 60_000_000_000,
                open: o,
                high: h,
                low: l,
                close: c,
                volume: 10.0,
                buy_volume: 5.0,
                sell_volume: 5.0,
            })
            .collect();
        let out = run_src(SMC_ZONE_IDIOM, &candles);
        let boxes: Vec<_> = out
            .objects
            .iter()
            .filter_map(|o| match o {
                ScriptObject::Box { left, top, right, bottom, .. } => {
                    Some((*left, *top, *right, *bottom))
                }
                _ => None,
            })
            .collect();
        // The mitigated gap is gone; the live gap is the one box drawn.
        assert_eq!(boxes.len(), 1, "{boxes:?}");
        let (left, top, right, bottom) = boxes[0];
        assert_eq!(left, 6.0, "the box anchors at the pattern's first candle");
        assert_eq!(top, 106.5);
        assert_eq!(bottom, 103.5);
        assert!(right >= 10_000.0, "the right edge extends into the future: {right}");
    }
}

/// Number-to-text for the string builtins. Plain form: integers print
/// without a decimal point, others with trailing zeros trimmed ("103.6",
/// not "103.60000001"). The "#.##" spec fixes decimal places (round-half-up);
/// "#" pins to integers. na prints as "na".
fn format_number(v: f64, fmt: Option<&str>) -> String {
    if !v.is_finite() {
        return "na".to_string();
    }
    match fmt {
        Some(spec) => {
            let decimals = spec
                .split_once('.')
                .map(|(_, tail)| tail.chars().take_while(|c| *c == '#').count())
                .unwrap_or(0);
            let scale = 10f64.powi(decimals as i32);
            let rounded = (v * scale).round() / scale;
            format!("{rounded:.decimals$}")
        }
        None => {
            if v == v.trunc() && v.abs() < 1e15 {
                format!("{}", v as i64)
            } else {
                let s = format!("{v:.10}");
                let s = s.trim_end_matches('0').trim_end_matches('.');
                s.to_string()
            }
        }
    }
}

/// Unix nanos -> (year, month, day) in UTC, Howard Hinnant's
/// civil-from-days (no chrono in the VM).
/// Parse a Pine session spec ("HHMM-HHMM" / "HH:MM-HH:MM" with an
/// optional ":days" weekday suffix) and test one bar against it. UTC --
/// the platform's candles are UTC; exchange timezones are a Tier-2 knob.
fn session_hit(spec: &str, nanos: i64) -> bool {
    let (range, days) = match spec.split_once(':') {
        Some((r, d)) if d.chars().all(|c| c.is_ascii_digit()) && !d.is_empty() => (r, Some(d)),
        _ => (spec, None),
    };
    let Some((start_s, end_s)) = range.split_once('-') else {
        return false;
    };
    let to_min = |s: &str| -> Option<i64> {
        let d: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
        if d.len() != 4 {
            return None;
        }
        let h: i64 = d.get(..2)?.parse().ok()?;
        let m: i64 = d.get(2..4)?.parse().ok()?;
        if h > 23 || m > 59 {
            return None;
        }
        Some(h * 60 + m)
    };
    let (Some(start), Some(end)) = (to_min(start_s), to_min(end_s)) else {
        return false;
    };
    if let Some(days) = days {
        // Pine dayofweek codes: 1 = Sunday .. 7 = Saturday.
        let dow = ((nanos / 86_400_000_000_000) + 4).rem_euclid(7) + 1;
        if !days.chars().any(|c| c.to_digit(10) == Some(dow as u32)) {
            return false;
        }
    }
    let mins = (nanos / 60_000_000_000).rem_euclid(24 * 60);
    if start == end {
        true
    } else if start < end {
        mins >= start && mins < end
    } else {
        // Overnight: "2200-0600" wraps midnight.
        mins >= start || mins < end
    }
}

fn civil_from_nanos(nanos: i64) -> (i64, i64, i64) {
    let z = nanos.div_euclid(86_400_000_000_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (year, month, day) in UTC -> days since the epoch: the inverse of
/// `civil_from_nanos`, for `timestamp(...)`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
