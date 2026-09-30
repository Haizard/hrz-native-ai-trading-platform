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
    /// `strategy.entry(id, direction)`.
    Entry {
        /// Order id.
        id: String,
        /// `long` or `short`.
        long: bool,
    },
    /// `strategy.exit(id, stop=, limit=, ...)`.
    Exit {
        /// Order id.
        id: String,
        /// Optional stop price.
        stop: Option<f64>,
        /// Optional limit price.
        limit: Option<f64>,
    },
    /// `strategy.close(id)` / `strategy.close_all()`.
    Close {
        /// The entry id, or all when empty.
        id: Option<String>,
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
    /// True when a script tried to draw past [`MAX_OBJECTS`]: the scene
    /// note says so, and the first 64 objects still render.
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
    /// `line.new(bar1, price1, bar2, price2, ...)`.
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
    },
}

/// The drawing-object heap cap, per run (docs/23 Phase 13): the array cap's
/// sibling. A script that draws on every bar hits it and is refused, not
/// allowed to leak.
pub const MAX_OBJECTS: usize = 64;

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
            security_ticker: script.header.sec.clone(),
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
                let series = self.plot_series(callee, args)?;
                self.out.plots.push(series);
            }
        }
        Ok(std::mem::take(&mut self.out))
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
            Item::Assign { name, mode, expr, .. } => {
                // `x = input.int(...)` -- the input reads its name from the
                // assignment target.
                if let ExprKind::Call { callee, .. } = &expr.kind {
                    if callee.starts_with("input.") {
                        self.current_input_name = name.clone();
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
                    // Refused statically; unreachable here, and refusing again
                    // costs nothing.
                    return Err(crate::ScriptError {
                        kind: crate::ErrorKind::Limit,
                        span: crate::Span::new(1, 1),
                        message: "`while` is refused in v1".into(),
                    });
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

    fn call_stmt(&mut self, callee: &str, args: &[Arg]) -> Result<(), crate::ScriptError> {
        self.tick()?;
        match callee {
            "plot" | "plotshape" | "plotchar" | "plotarrow" => {
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
                // v1 collects but does not render fills/tints; the plots they
                // reference are kept by id for the renderer to pair.
                Ok(())
            }
            "strategy.entry" => {
                let id = self.arg_str(args, "id").or_else(|| self.arg_str_pos(args, 0)).unwrap_or_default();
                let dir = self.arg_str(args, "direction").or_else(|| self.arg_str_pos(args, 1)).unwrap_or_default();
                self.out.strategy_used = true;
                self.out
                    .intents
                    .push((self.bar, Intent::Entry { id, long: dir != "short" }));
                Ok(())
            }
            "strategy.exit" => {
                let id = self.arg_str(args, "id").or_else(|| self.arg_str_pos(args, 0)).unwrap_or_default();
                let stop = self.arg_named_f(args, "stop");
                let limit = self.arg_named_f(args, "limit");
                self.out.strategy_used = true;
                self.out.intents.push((self.bar, Intent::Exit { id, stop, limit }));
                Ok(())
            }
            "strategy.close" => {
                let id = self.arg_str(args, "id").or_else(|| self.arg_str_pos(args, 0));
                self.out.strategy_used = true;
                self.out.intents.push((self.bar, Intent::Close { id }));
                Ok(())
            }
            "strategy.close_all" => {
                self.out.strategy_used = true;
                self.out.intents.push((self.bar, Intent::Close { id: None }));
                Ok(())
            }
            "strategy.cancel" => Ok(()),
            // ---- drawing objects (docs/23 Phase 13): statements that push
            // to the object heap, capped like the array heap. Coordinates are
            // plain f64 bar indexes/prices; the engine maps them to the
            // canvas, never the script.
            "line.new" => {
                self.tick()?;
                // The heap cap STOPS drawing rather than killing the run
                // (TradingView's behavior): a script that conditions fired
                // 100 times on a long window still renders its first 64
                // objects, with the truncation reported in the scene note.
                if self.out.objects.len() < MAX_OBJECTS {
                    let bar1 = self.arg_f(args, 0)?.unwrap_or(NA);
                    let price1 = self.arg_f(args, 1)?.unwrap_or(NA);
                    let bar2 = self.arg_f(args, 2)?.unwrap_or(NA);
                    let price2 = self.arg_f(args, 3)?.unwrap_or(NA);
                    let color = self.arg_color(args).unwrap_or(0xFF_94_A3_B8_u32);
                    let style = self.arg_str(args, "style").unwrap_or_else(|| "solid".into());
                    let width = self.arg_named_f(args, "width").unwrap_or(1.0);
                    self.out.objects.push(ScriptObject::Line { bar1, price1, bar2, price2, color, style, width });
                } else {
                    self.out.objects_truncated = true;
                }
                Ok(())
            }
            "label.new" => {
                self.tick()?;
                if self.out.objects.len() < MAX_OBJECTS {
                    let bar = self.arg_f(args, 0)?.unwrap_or(NA);
                    let price = self.arg_f(args, 1)?.unwrap_or(NA);
                    let text = self.arg_str(args, "text").or_else(|| self.arg_str_pos(args, 2)).unwrap_or_default();
                    let color = self.arg_color(args).unwrap_or(0xFF_94_A3_B8_u32);
                    self.out.objects.push(ScriptObject::Label { bar, price, text, color });
                } else {
                    self.out.objects_truncated = true;
                }
                Ok(())
            }
            "box.new" => {
                self.tick()?;
                if self.out.objects.len() < MAX_OBJECTS {
                    let left = self.arg_f(args, 0)?.unwrap_or(NA);
                    let top = self.arg_f(args, 1)?.unwrap_or(NA);
                    let right = self.arg_f(args, 2)?.unwrap_or(NA);
                    let bottom = self.arg_f(args, 3)?.unwrap_or(NA);
                    let color = self.arg_color(args).unwrap_or(0x33_94_A3_B8_u32);
                    self.out.objects.push(ScriptObject::Box { left, top, right, bottom, color });
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
    fn plot_series(&mut self, callee: &str, args: &[Arg]) -> Result<Plot, crate::ScriptError> {
        let n = self.candles.len();
        let mut values = vec![NA; n];
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
            let location_value = self.arg_named_f(args, "location_value").unwrap_or(NA);
            for (bar, v) in series.iter().enumerate() {
                if self.truthy(*v) {
                    self.out.shapes.push(Shape {
                        bar,
                        value: location_value,
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
            // allocation.
            let current = buf[self.bar];
            if current.is_finite() {
                return Ok(current);
            }
            for slot in (0..self.bar).rev() {
                if buf[slot].is_finite() {
                    return Ok(buf[slot]);
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
            "last_bar_index" => (self.candles.len().saturating_sub(1)) as f64,
            "time" => c.open_time as f64,
            "time_close" => (c.open_time + c.timeframe.nanos()) as f64,
            "barstate.isconfirmed" => 1.0, // the host only runs closed bars
            "strategy.position_size" | "strategy.position_avg_price" | "strategy.equity"
            | "strategy.openprofit" | "strategy.closedtrades" | "strategy.wintrades" => 0.0,
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
                    return Ok(if off <= self.bar { buf[self.bar - off] } else { NA });
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
        if let Some(kind) = callee.strip_prefix("input.") {
            let name = self.current_input_name.clone();
            let default = self.arg_named_f(args, "defval").unwrap_or(0.0);
            return Ok(match kind {
                "bool" => {
                    f64::from(self.inputs.bools.get(&name).copied().unwrap_or(default != 0.0))
                }
                "int" | "float" => self.inputs.numbers.get(&name).copied().unwrap_or(default),
                _ => default,
            });
        }
        macro_rules! a {
            ($i:expr) => {
                match args.get($i) {
                    Some(arg) => self.eval_f(&arg.value),
                    None => Ok(NA),
                }
            };
        }
        match callee {
            // The call form of the time-of-day reads: `hour(time)` is the
            // current bar's UTC hour exactly like the bare `hour` series.
            // The argument is evaluated (it may be `time` or a timestamp)
            // and otherwise ignored -- the platform runs closed bars of one
            // series, so there is nothing else the clock could be of.
            "hour" | "minute" | "dayofweek" => {
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
                    _ => {
                        let days = (nanos / 86_400_000_000_000.0).floor();
                        (days + 4.0).rem_euclid(7.0) + 1.0
                    }
                })
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
            "ta.macd" | "ta.bb" => Err(crate::ScriptError {
                kind: crate::ErrorKind::Type,
                span: crate::Span::new(1, 1),
                message: format!(
                    "`{callee}` returns 3 values; write `a, b, c = {callee}(...)`"
                ),
            }),
            "ta.atr" => Ok(ta::ta_atr(self.candles, a!(0)?)[self.bar]),
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
            | "request.volume" => {
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
                    return Ok(buf.clone());
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
        let mut locals = std::collections::HashMap::new();
        let mut series_args: std::collections::HashMap<String, Expr> = std::collections::HashMap::new();
        for (i, arg) in args.iter().enumerate() {
            let param = self
                .script
                .items
                .iter()
                .find_map(|item| match item {
                    Item::FuncDef { name: n, params, .. } if n == callee => {
                        params.get(i).cloned()
                    }
                    _ => None,
                })
                .unwrap_or_else(|| format!("arg{i}"));
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

    fn arg_str_pos(&self, args: &[Arg], index: usize) -> Option<String> {
        match args.get(index) {
            Some(Arg { value: Expr { kind: ExprKind::Str(s), .. }, .. }) => Some(s.clone()),
            _ => None,
        }
    }

    fn arg_str(&self, args: &[Arg], name: &str) -> Option<String> {
        let arg = args.iter().find(|a| a.name.as_deref() == Some(name))?;
        match &arg.value.kind {
            ExprKind::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    fn arg_color(&self, args: &[Arg]) -> Option<u32> {
        let arg = args.iter().find(|a| a.name.as_deref() == Some("color"))?;
        match &arg.value.kind {
            ExprKind::Color(c) => Some(*c),
            ExprKind::Ident(name) => named_color(name),
            _ => None,
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
}
