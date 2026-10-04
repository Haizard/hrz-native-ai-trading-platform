//! AI generation of pine-lite scripts (`docs/23` Phase 9, the studio flip).
//!
//! The studio's chat used to ask the model for a `kind: indicator` *document*;
//! it now asks for a **pine-lite script** -- imperative code with `plot()`,
//! the shape the user asked for when they asked for TradingView-style
//! generation. The pipeline is the document one's mirror:
//!
//! 1. a system prompt that teaches the language's real surface (builtins,
//!    header, grammar) and the platform's non-negotiables (the header, no
//!    unbounded loops),
//! 2. script extraction from the model's reply -- a fenced ```pine_lite
//!    block, or the whole reply when it is bare code,
//! 3. the validate-repair loop over [`pine_lite::vet`]: every refusal goes
//!    back verbatim (kind, line, col, message), and the artifact that
//!    survives is guaranteed to lex, parse, type-check and fit the limits --
//!    the same guarantee `POST /scripts/vet` gives, applied to the model.
//!
//! The loop lives here rather than in `ai-agent` because it needs no LLM
//! plumbing of its own -- just [`ai_agent::LlmClient`] -- and because the
//! gateway owns the vet pipeline.

use ai_agent::llm_client::{
    ContentBlock, LlmClient, LlmRequest, Message, Role, ToolChoice, ToolSpec,
};
use serde::Serialize;

/// One observable step of the generation pipeline (docs/36): what the
/// streaming route turns into SSE `progress` frames. The stage names are the
/// contract the shell reads, so they are tagged and stable. Not token
/// streaming, deliberately: the model produces a `submit_script` tool call,
/// not prose, so there are no answer tokens to stream -- what streams is the
/// *work* (the agent socket's precedent, `ws::agent`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum GenerationEvent {
    /// An LLM round is starting -- the long, opaque wait.
    Drafting { attempt: usize, max: usize },
    /// The draft did not survive; the complaint goes back to the model
    /// verbatim. `errors` is the vet list, or a one-line note for the shapes
    /// vet never saw (no tool call, a non-string argument).
    Repairing { attempt: usize, errors: Vec<String> },
    /// The missing-`sec` header knob was fixed deterministically.
    Autofix { detail: String },
    /// The vetted script is replaying on the stored candles for its preview.
    /// Emitted by the route, which owns the preview; declared here so the
    /// frame vocabulary has one home.
    Previewing,
}

/// Report one step, if anyone is listening. A closed receiver means the
/// client disconnected mid-generation; the pipeline continues anyway so its
/// result still persists, and the route discards the rest.
fn report(
    progress: &Option<&futures::channel::mpsc::UnboundedSender<GenerationEvent>>,
    event: GenerationEvent,
) {
    if let Some(sink) = progress {
        let _ = sink.unbounded_send(event);
    }
}

/// How many repair rounds the model gets. The document loop uses 5; scripts
/// fail on more specific, more fixable complaints (a missing import, a wrong
/// arity), so the same budget is generous.
const MAX_ATTEMPTS: usize = 5;

/// The reference script quoted verbatim in the system prompt. Every form in
/// it is implemented in `pine-lite`'s interpreter -- the prompt teaches the
/// VM's actual surface (its builtin table is copied from the dispatchers in
/// `interp.rs`), never the doc's wishlist, so the model is never baited into
/// calls that can only fail at run time.
const REFERENCE_SCRIPT: &str = "//@pine_lite version=1 overlay=true title=\"Bands + MACD signals\"\n\
    length = input.int(defval=20, title=\"Band length\")\n\
    mult = input.float(defval=2.0, title=\"Band mult\")\n\
    basis, upper, lower = ta.bb(close, length, mult)\n\
    macd_line, signal_line, hist = ta.macd(close, 12, 26, 9)\n\
    plot(basis, title=\"Basis\", color=color.gray)\n\
    plot(upper, title=\"Upper\", color=color.blue)\n\
    plot(lower, title=\"Lower\", color=color.blue)\n\
    buy = ta.crossover(macd_line, signal_line)\n\
    sell = ta.crossunder(macd_line, signal_line)\n\
    plotshape(buy, shape=\"triangleup\", color=color.green, location_value=low)\n\
    plotshape(sell, shape=\"triangledown\", color=color.red, location_value=high)";

/// The reference strategy quoted verbatim in the system prompt, next to
/// [`REFERENCE_SCRIPT`]. It exercises every taught strategy form -- the
/// `strategy(...)` header, gated entries, a `var`-ratcheted ATR trailing
/// stop re-armed each bar, the reversal `close_all`, the account read --
/// and the test suite requires it to vet AND simulate a trade, so the
/// prompt can never teach a strategy shape that draws nothing but claims
/// to trade. NOTE: built with `concat!` on purpose -- a `\n\`-continued
/// string literal strips each line's leading whitespace, which erases the
/// script's block indentation and breaks vetting.
const REFERENCE_STRATEGY: &str = concat!(
    "//@pine_lite version=1 overlay=false title=\"SMA trend strategy\" strategy(initial_capital=10000, default_qty_type=\"percent_of_equity\", default_qty_value=10, commission_value=0.04, slippage=0.02)\n",
    "fast = ta.sma(close, 10)\n",
    "slow = ta.sma(close, 30)\n",
    "atr = ta.atr(14)\n",
    "var float trail = 0.0\n",
    "long_entry = ta.crossover(fast, slow)\n",
    "short_entry = ta.crossunder(fast, slow)\n",
    "if long_entry and strategy.position_size == 0\n",
    "    strategy.entry(\"L\", direction=\"long\")\n",
    "if short_entry and strategy.position_size == 0\n",
    "    strategy.entry(\"S\", direction=\"short\")\n",
    "if short_entry and strategy.position_size > 0\n",
    "    strategy.close_all()\n",
    "if long_entry and strategy.position_size < 0\n",
    "    strategy.close_all()\n",
    "if strategy.position_size == 0\n",
    "    trail := 0.0\n",
    "if strategy.position_size > 0\n",
    "    trail := trail == 0.0 ? close - atr * 3.0 : math.max(trail, close - atr * 3.0)\n",
    "    strategy.exit(\"xl\", stop=trail)\n",
    "if strategy.position_size < 0\n",
    "    trail := trail == 0.0 ? close + atr * 3.0 : math.min(trail, close + atr * 3.0)\n",
    "    strategy.exit(\"xs\", stop=trail)\n",
    "plot(trail, title=\"Trail\", color=color.orange, style=\"linebr\", linewidth=2)\n",
    "plotshape(long_entry, shape=\"triangleup\", color=color.green, location_value=low)\n",
    "plotshape(short_entry, shape=\"triangledown\", color=color.red, location_value=high)",
);

/// The system prompt: the language's whole surface, taught compactly.
///
/// Everything listed here exists in the interpreter -- the builtin table is
/// copied from `pine-lite`'s dispatchers, so the model is never taught a
/// function that would refuse at run time. The non-negotiables (mandatory
/// header, bounded loops, one statement per line) are the platform's vetting
/// rules stated as authoring advice, which is where they cost the least.
/// Both reference scripts are pinned by tests: [`REFERENCE_SCRIPT`] must vet,
/// and [`REFERENCE_STRATEGY`] must vet AND simulate (docs/24 S3).

pub(crate) fn script_system_prompt(symbol: &str, timeframe: &str) -> String {
    format!(
        r#"You write indicators as **pine-lite** scripts: TradingView-Pine-style imperative code the platform compiles and runs bar-by-bar. The user's chart is {symbol} on the {timeframe} timeframe; write for that.

## Output contract
Reply with ONE fenced code block tagged pine_lite containing the whole script. No prose outside the block. The FIRST line must be the header:

//@pine_lite version=1 overlay=false title="My Indicator"

- `overlay=true` draws on the price chart (levels, bands drawn in price units); `overlay=false` gives the script its own pane below the chart (oscillators like RSI, stoch).
- `title` names the pane and legend.

## Syntax rules (the compiler is strict)
1. ONE statement per line. Each statement ends at the newline. Never put two statements on one line; never use `;`.
2. Indent block bodies by exactly 4 spaces under `if` / `else` / `for`.
3. Declare with `=`. Reassign with `:=`. `var x = ...` initializes once and keeps its value across bars (running counters, state).
4. Square brackets are ONLY a history offset on a named series variable or a built-in series, with an integer: `close[1]`, `myVar[3]`, `high[0]`. NOTHING else may ever be in brackets: no list literals, no `ta.sma(close, 9)[1]`, no `foo()[0]`. To compare a function result with its past, assign it first: `s = ta.sma(close, 9)` then `s[1]`. Collections exist as ARRAY FUNCTIONS, never as bracket syntax: `a = array.new()`, `array.push(a, v)`, `array.get(a, i)`. Writing `a[i] = v` is a syntax error.
5. Multi-value calls are destructured on one line: `basis, upper, lower = ta.bb(close, 20, 2)`. The names must match the callee's output count. `ta.macd` and `ta.bb` return three values, `ta.stoch` returns two (%K, %D) -- and `ta.stoch` also reads as its %K alone.
6. Numbers are floats; bools are `true`/`false`; `na` is the missing value (it propagates; comparisons with it are false). Ternary: `cond ? a : b`. Logic: `and`, `or`, `not`. Comparisons: `< <= > >= == !=`.
7. `if cond` / `else` blocks contain assignments only (no `plot` inside them); a block body must not be empty.
8. Loops: `for i = 0 to 99` (optional `by 2`), or `while cond` with a 4-space indented body. A `while` condition MUST eventually turn false (guard it with a `var` counter or state flag) -- a runaway loop is killed by the fuel budget with an error. Prefer `for` when a range is known. Keep loops small; budgets are enforced.
9. `//` starts a comment.

## Built-in series (bare names)
`open`, `high`, `low`, `close`, `volume`, `hl2`, `hlc3`, `ohlc4`, `bar_index`, `time`

## Functions (these are ALL of them; a function not listed here does not exist)
Averages and oscillators: `ta.sma(source, length)`, `ta.ema(source, length)`, `ta.rma(source, length)`, `ta.wma(source, length)`, `ta.rsi(source, length)`, `ta.highest(source, length)`, `ta.lowest(source, length)`, `ta.mom(source, length)`, `ta.roc(source, length)`, `ta.change(source)`, `ta.atr(length)`, `ta.tr()`, `ta.vwap()`, `ta.stoch(source, length)` (returns 0..100).
Multi-value (destructure them): `macd_line, signal_line, hist = ta.macd(source, fast, slow, signal)`, `basis, upper, lower = ta.bb(source, length, mult)`, `k, d = ta.stoch(source, length)`.
Crosses (return bool): `ta.crossover(a, b)`, `ta.crossunder(a, b)`, `ta.cross(a, b)`.
Math: `math.abs(x)`, `math.floor(x)`, `math.ceil(x)`, `math.round(x)`, `math.sqrt(x)`, `math.log(x)`, `math.exp(x)`, `math.sign(x)`, `math.pow(a, b)`, `math.min(a, b, ...)`, `math.max(a, b, ...)`, `math.avg(a, b, ...)`, `math.sum(source, length)`.
Checks: `na(x)`, `nz(x)`, `nz(x, fallback)`, `fixnan(x)`.
## Arrays (for zones, pivots, levels -- SMC/market-structure building blocks)
A variable holds a whole array once created with `var` (allocate ONCE, at the top):

    var swing_highs = array.new()
    var swing_bars = array.new()
    if pivot_high
        array.push(swing_highs, high[w])
        array.push(swing_bars, bar_index - w)

Readers: `array.size(a)`, `array.get(a, i)`, `array.first(a)`, `array.last(a)`, `array.min(a)`, `array.max(a)`, `array.avg(a)`, `array.includes(a, v)`. Mutators: `array.push(a, v)`, `array.pop(a)`, `array.shift(a)`, `array.clear(a)`, `array.set(a, i, v)` (only existing indexes).
Guards: out-of-bounds `array.get` returns `na` (wrap with `nz(...)` when the value feeds arithmetic); never allocate without `var` (the heap cap kills the script); pop/shift when a list could grow unbounded. A state machine over two parallel arrays (values + bar stamps) builds order blocks, FVGs, breaker blocks and session levels.

## A second instrument (cross-market math: SMT divergence, spreads, ratios)
The chart is {symbol}; a correlated second instrument rides along when the header declares it:

    //@pine_lite version=1 overlay=false title="SMT" sec="ETHUSDT"

With `sec=` set, these zero-argument calls give the pair's series, bar-aligned to the chart's own bars: `request.symbol()` (the pair's ticker, as a nonzero number), `request.open()`, `request.high()`, `request.low()`, `request.close()`, `request.volume()`. `request.close()` and `close` at the same bar are the same moment, so spreads (`close - request.close()`), ratios (`close / request.close()`) and SMT divergence (our new swing high while the pair makes a lower high) are plain arithmetic. History works as on any series -- assign first, then offset: `rc = request.close()` then `rc[1]`.
- The pair trades on its own clock: where it printed no bar, its last bar carries forward flat (same OHLC repeats), so `request.*` values stall rather than gap. The host fetches and aligns the pair's candles automatically; the script never does. `request.time()` gives the aligned pair bar's open time -- a change in it is the pair's new bar.
- The zero-argument `request.*` calls (except `request.security`) require `sec=` in the header -- the vet refuses `request.close()` without it.

## A higher timeframe (the 1h zone on the 5m chart)
`request.security("BTCUSDT", "1h", expression)` evaluates the expression over the named symbol+timeframe's candles, aligned onto this chart's bars: at each chart bar you read the HTF bar covering that moment. The host fetches and aligns the pool; symbol and timeframe must be quoted literals. Bind the reads once:

    ht = request.security("BTCUSDT", "1h", time)
    hh = request.security("BTCUSDT", "1h", high)
    hl = request.security("BTCUSDT", "1h", low)

A change in the pooled `time` IS the higher timeframe's new bar: `ht != ht[1]` marks the chart bar where one 1h bar closed and the next opened. HTF patterns need CLOSED HTF bars, so shift levels into `var`s at each boundary and test the just-closed bar BEFORE shifting:

    newbar = ht != ht[1]
    var ph1 = na
    var ph2 = na
    if newbar and not na(ph2)
        if hl[1] > ph2
            box.new_time(ht[1], hl[1], ht + 10000000000000000.0, ph2, color=color.teal)
    if newbar
        ph2 = ph1
        ph1 = hh[1]

That is a bullish 1h fair value gap: the just-closed 1h bar's low gapped above the high two closed 1h bars back. HTF zones can NOT use `box.new` -- a 1h bar's span is not a bar index on this chart. Use the time-anchored twins, which take unix-nanos timestamps (the pooled `time` values) instead of bar indexes: `box.new_time(t1, top, t2, bottom, color=...)`, `line.new_time(t1, price1, t2, price2, ...)`, `label.new_time(t, price, text, ...)`. Extend right with a large time offset (`ht + 10000000000000000.0`); the canvas clips at the plot edge. Mitigation tracking is the same array state machine as the chart-timeframe idiom, with `ht[1]` stored as the birth stamp instead of `bar_index - 2`.

## Multi-timeframe synthesis (one pattern, several timeframes, one chart)
When the request asks for confluence ("15m and 1h order blocks", "multi-timeframe FVGs"), run ONE boundary machine PER pooled timeframe -- each with its own `var` pair and its own color -- and tag every zone with its timeframe so the chart reads at a glance:

    t15 = request.security("BTCUSDT", "15m", time)
    h15 = request.security("BTCUSDT", "15m", high)
    l15 = request.security("BTCUSDT", "15m", low)
    t1h = request.security("BTCUSDT", "1h", time)
    h1h = request.security("BTCUSDT", "1h", high)
    l1h = request.security("BTCUSDT", "1h", low)
    far = 10000000000000000.0
    nb15 = t15 != t15[1]
    var a1 = na
    var a2 = na
    if nb15 and not na(a2)
        if l15[1] > a2
            box.new_time(t15[1], l15[1], t15 + far, a2, color=color.aqua)
            label.new_time(t15[1], l15[1], "15m FVG", color=color.aqua)
    if nb15
        a2 = a1
        a1 = h15[1]
    nb1h = t1h != t1h[1]
    var b1 = na
    var b2 = na
    if nb1h and not na(b2)
        if l1h[1] > b2
            box.new_time(t1h[1], l1h[1], t1h + far, b2, color=color.teal)
            label.new_time(t1h[1], l1h[1], "1h FVG", color=color.teal)
    if nb1h
        b2 = b1
        b1 = h1h[1]

The rules that keep it honest:
- Each timeframe's machine is self-contained: its own pooled reads, its own `var`s, its own boundary flag. Never share `ph1`/`ph2` across timeframes -- a 15m shift and a 1h shift happen on different chart bars.
- Higher timeframes are stronger: draw the HIGHER timeframe's zones first (they sit under), the lower's on top, and give the higher the stronger color. A zone confirmed on two timeframes is the confluence the trader asked for -- the overlapping bands ARE the synthesis; do not try to merge them into one box.
- The pool caps at 8 keys: 2-3 timeframes x (time, high, low) reads fit comfortably. Keep the chart's own timeframe out of the pool -- its bars are just `time`/`high`/`low`.
- Pick the timeframes from the request: "multi-timeframe" on a 5m chart means 15m + 1h; on a 1h chart, 4h + 1d. Two to three steps up, never sideways or down.

## Time of day (session filters)
`hour` (0..23 UTC), `minute` (0..59), `dayofweek` (1=Sunday .. 7=Saturday, Pine's convention). London open is about `hour == 7`; New York is about `hour >= 12 and hour < 21`. An Asian-session level: capture the high/low while `hour >= 0 and hour < 7` into `var` scalars and reset at `hour == 0`.

## Drawing
- `plot(series, title="...", color=color.blue, linewidth=1)` -- a line. Optional `style="histogram"` / `style="columns"` / `style="circles"` / `style="stepline"` / `style="areabr"` / `style="linebr"`.
- `hline(price, color=color.gray, linestyle=dashed)` -- a horizontal level (e.g. `hline(70)`).
- `plotshape(cond, shape="triangleup", color=color.green, location_value=low)` -- a marker on each bar where cond is true. `shape=` names the glyph ("triangleup", "triangledown", "circle", "cross"); `location_value=` is the price the marker sits at (`low` under a buy, `high` above a sell). Omit `location_value=` and the marker falls to the bottom of the pane (or the bottom of the price range, in an overlay script). `plotchar(cond, char="B")` prints a letter; `plotarrow(cond)` draws an arrow. Markers work in overlay scripts and pane scripts alike.
- Colors: color.red, color.green, color.blue, color.orange, color.yellow, color.purple, color.teal, color.lime, color.aqua, color.white, color.gray, color.maroon, color.navy, color.olive, color.silver, color.fuchsia, color.black.

## Inputs (user-adjustable parameters)
`len = input.int(defval=14, title="RSI Length")`, `input.float(defval=2.0, title="Mult")`, `input.bool(defval=true, title="Show signals")`. Use them instead of magic numbers.

## User functions
Define your own, Pine-style:

    zone_mid(a, b) =>
        (a + b) / 2

Call like any function: `mid = zone_mid(high, low)`. Trailing parameters can declare defaults -- `zone_mid(a, b, mult = 1.5) =>` -- and callers may omit them: `zone_mid(high, low)`. A required parameter may not follow one with a default. A parameter binds the caller's SERIES (Pine semantics): `f(x) => ta.sma(x, n)` averages the caller's series over its real history, and a default of `close[1]` is the caller's yesterday. No recursion; no `plot`/`hline` inside a function.

## Drawing objects (anchored lines, labels, boxes)
Statements that add to a drawing heap (cap 256 per script):

    if pivot_high
        line.new(bar_index - w, high[w], bar_index, close, color=color.blue, style="solid", width=1)
        label.new(bar_index - w, high[w], "pivot", color=color.red)
    if demand_zone
        box.new(zone_left, zone_top, zone_right, zone_bottom, color=color.green)

Coordinates are bar indexes and PRICES. Draw inside `if` blocks or guard with `bar_index == 0` -- a top-level `line.new` runs EVERY bar and fills the heap in ~256 bars (past the cap the extra objects are dropped). `style=` is "solid"/"dashed"/"dotted". Boxes take TradingView's border knobs -- `border_color=color.lime, border_width=2, border_style="dashed"`: a visible border distinct from the fill, which is how a zone reads as a zone and not a smear (leave them off for a soft fill-only band). The `*_time` twins (`box.new_time`, `line.new_time`, `label.new_time`) take unix-nanos TIME anchors instead of bar indexes -- use them for anything read from a pooled timeframe (see "A higher timeframe"), where an edge is a timestamp, not one of this chart's bars.

`fib.new(bar1, price1, bar2, price2, color=color.orange)` draws a Fibonacci retracement on a swing: the 0 line sits on the SECOND anchor (the swing's end), 100 on the first, and 23.6/38.2/50/61.8/78.6 between them, each with its price label. One call, one heap object -- the engine decomposes it into the levels, so a fib costs one object, not fourteen.

## SMC zones (order blocks, fair value gaps, breaker blocks): draw BOXES, never markers

A zone is a price band with a birth bar, so the drawing is `box.new`: one box per LIVE zone, its right edge extended past the last bar (`bar_index + 10000`) so the canvas clips it at the plot edge -- TradingView's "extend to now" look. A `plotshape` triangle marks an EVENT (a CHoCH, a sweep, a BOS); it is never the drawing for a ZONE. An SMC indicator that renders zones as triangles or lines has failed the request.

The complete fair-value-gap idiom -- detection, mitigation, and the draw pass. A bullish FVG is a 3-candle gap: today's low above the high 2 bars back; the zone is the gap itself (top = low, bottom = high[2]), born at the pattern's first candle (`bar_index - 2`). It dies when a later bar's low trades through its bottom. Bearish mirrors it (high < low[2]; the zone is low[2]..high; it dies when a later high clears its top):

    var bull_top = array.new()
    var bull_bot = array.new()
    var bull_bar = array.new()
    var bear_top = array.new()
    var bear_bot = array.new()
    var bear_bar = array.new()
    bullish_fvg = low > high[2]
    bearish_fvg = high < low[2]
    if bullish_fvg
        array.push(bull_top, low)
        array.push(bull_bot, high[2])
        array.push(bull_bar, bar_index - 2)
    if bearish_fvg
        array.push(bear_top, low[2])
        array.push(bear_bot, high)
        array.push(bear_bar, bar_index - 2)
    n_bull = array.size(bull_top)
    n_bear = array.size(bear_top)
    for i = 0 to 24
        if i < n_bull
            bb = array.get(bull_bot, i)
            if not na(bb)
                if low < bb
                    array.set(bull_bot, i, na)
        if i < n_bear
            bt = array.get(bear_top, i)
            if not na(bt)
                if high > bt
                    array.set(bear_top, i, na)
    if bar_index == last_bar_index
        for i = 0 to 24
            if i < n_bull
                t = array.get(bull_top, i)
                b = array.get(bull_bot, i)
                if not na(b)
                    box.new(array.get(bull_bar, i), t, bar_index + 10000, b, color=color.teal)
            if i < n_bear
                t2 = array.get(bear_top, i)
                b2 = array.get(bear_bot, i)
                if not na(b2)
                    box.new(array.get(bear_bar, i), t2, bar_index + 10000, b2, color=color.red)

The rules that make it work:
- Mitigation is updated EVERY bar in its own loop: `var` arrays rebuild from scratch on every run (the script re-runs over the visible window each frame), so nothing computed last frame carries.
- The draw pass lives inside `if bar_index == last_bar_index` -- one box per live zone per run keeps the 256-object heap at one box per zone, not one per bar.
- Mark a dead zone by writing `na` into its edge array (`array.set(bull_bot, i, na)`) and guard every read with `not na(...)`.
- Keep at most ~25 zones per side in the loops; the heap caps at 256 objects and older zones matter less.
- An order block is the same shape with different detection: the LAST opposing candle before a displacement (for a bullish OB, the last down candle before a strong 2-3 bar rally); top/bottom are that candle's high/low, born at that candle's bar. A breaker block is an order block that failed and flipped side -- same box, other color.

## Pattern detection WITHOUT arrays (there is no `a[i] = v` assignment)
You cannot build lists or arrays -- not with brackets, not any other way. Track state with `var` scalars that persist across bars and reassign inside `if` blocks. The two idioms you need:

1. Swing/pivot detection with a centered window (a swing high w bars ago that is the highest of its 2w+1 neighbors):

    w = input.int(defval=4, title="Pivot window")
    hh = ta.highest(high, 2 * w + 1)
    ll = ta.lowest(low, 2 * w + 1)
    pivot_high = hh[w] == high[w]
    pivot_low = ll[w] == low[w]

2. A state machine that remembers the last two pivots and where they happened:

    var ph1 = na
    var ph2 = na
    var ph1_bar = 0.0
    var ph2_bar = 0.0
    if pivot_high
        ph2 := ph1
        ph2_bar := ph1_bar
        ph1 := high[w]
        ph1_bar := bar_index - w

CRITICAL: stamp the pivot with `bar_index - w`, the bar where the swing actually happened -- a centered pivot is only confirmed w bars later. Stamping `bar_index` (the confirmation bar) anchors the old pivot value at today's bar, which displaces every projected line w bars right and makes it wobble across price instead of passing through the swing highs/lows.

Slopes come from the stamps: `slope_high = (not na(ph2) and ph1_bar != ph2_bar) ? (ph1 - ph2) / (ph1_bar - ph2_bar) : na`. A projected line through the last two pivots: `level = ph1 + slope_high * (bar_index - ph1_bar)`. Plot `level` to draw the line.

Slope thresholds must be SCALE-FREE -- never compare a price-per-bar slope against an absolute constant (that breaks between a 2-dollar stock and an 85000-dollar coin). Use ATR: `atr = ta.atr(14)`, then "flat" means `math.abs(slope_high) <= atr * 0.15` and "rising" means `slope_low >= atr * 0.05`. This pivot + state + slope pattern builds triangles, channels, flags and breakouts -- all without arrays.

## Strategies (only when the user asks for entries/exits/backtests)
Declare the trading account in the header -- a `strategy(...)` block after `title`:

    //@pine_lite version=1 overlay=false title="My strategy" strategy(initial_capital=10000, default_qty_type="percent_of_equity", default_qty_value=10, commission_value=0.04, slippage=0.02)

Knobs, with their defaults (all optional): `initial_capital=10000`, `default_qty_type="percent_of_equity"` (or `"fixed"` = units of the asset), `default_qty_value=10` (a percent of equity, or units), `commission_value=0.04` (percent of notional per side; `commission_type="absolute"` makes it currency), `slippage=0.02` (percent against you on every fill), `pyramiding=0` (the only allowed value -- one position at a time). Position size and fees come from these header knobs; never pass `qty=` per call.

Orders (fire them inside `if` blocks; every order fills on the NEXT bar's open):
- `strategy.entry("L", direction="long")` or `direction="short"` -- a market entry.
- `strategy.exit("xl", stop=price)` -- arms a STOP on the open position; it fills when price touches the stop. Re-arm it EVERY bar the position is open to trail it.
- `strategy.close("L")` / `strategy.close_all()` -- a market exit at the next bar's open.
- ONE position at a time: an entry fired while a position is open is IGNORED, not reversed. Gate re-entries with the account read.

Account reads (series, usable in any expression): `strategy.position_size` (signed qty, 0 = flat), `strategy.position_avg_price`, `strategy.equity`, `strategy.openprofit`, `strategy.closedtrades`, `strategy.wintrades`.

The rules that keep a strategy correct:
1. Gate entries with `strategy.position_size == 0` unless you deliberately want a reversal.
2. Arm the stop inside `if strategy.position_size != 0` -- a stop declared while flat is forgotten.
3. Persist the trailing level with `var float trail = 0.0` and RATCHET it -- and SEED it on the first bar of the position, because a long's stop starts BELOW price but a short's starts ABOVE it: `trail := trail == 0.0 ? close - atr * 3.0 : math.max(trail, close - atr * 3.0)` for a long (`close + atr * 3.0` with `math.min` for a short), and reset `trail := 0.0` while flat. A bare `trail = 0.0` (no `var`) resets EVERY bar, so the "trail" follows price in both directions and stops out immediately -- the most common strategy bug.
4. On the exit signal prefer `strategy.close_all()` in the same `if` as the reverse entry, and re-arm nothing while flat.

## Reference strategy (exactly this shape of code, vetted AND simulated)

```
{REFERENCE_STRATEGY}
```

## Non-negotiables
- The `//@pine_lite` header is MANDATORY on the first line.
- Keep `plot`/`hline`/`plotshape` calls at the top level (not inside `for` loops or `if` bodies).
- The script is self-contained: no imports, no network calls. The ONLY cross-symbol access is `request.*` with `sec=` in the header, and the pair always runs on the chart's own timeframe -- there is no per-call symbol or timeframe argument.
- Never use brackets except the `series[int]` history form from rule 4. Never use commas outside a call's arguments.

## Reference script (exactly this shape of code, vetted and working)

```
{REFERENCE_SCRIPT}
```
"#,
        symbol = symbol,
        timeframe = timeframe,
        REFERENCE_STRATEGY = REFERENCE_STRATEGY,
    )
}

/// Pull the script out of the model's reply.
///
/// Preference order: a fenced ```pine_lite block (what the system prompt
/// demands), then any fenced block (some models drop the tag), then the whole
/// text -- which handles bare code and lets the vet pipeline reject prose by
/// its own light. The fence scan is line-oriented rather than a global
/// regex because a script may legitimately contain backticks in strings.
#[must_use]
pub(crate) fn extract_script(reply: &str) -> Option<String> {
    let lines: Vec<&str> = reply.lines().collect();
    // First pass: a block explicitly tagged pine_lite (or pine/pinescript,
    // which models reach for out of habit and which is still code).
    for opening in ["```pine_lite", "```pine", "```pinescript"] {
        if let Some(start) = lines.iter().position(|l| l.trim().eq_ignore_ascii_case(opening)) {
            if let Some(end_rel) = lines[start + 1..]
                .iter()
                .position(|l| l.trim() == "```")
            {
                let end = start + 1 + end_rel;
                return Some(lines[start + 1..end].join("\n"));
            }
        }
    }
    // Second pass: any fence pair whose opening is bare ```.
    let mut open: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "```" {
            match open {
                Some(s) => return Some(lines[s + 1..i].join("\n")),
                None => open = Some(i),
            }
        } else if trimmed.starts_with("```") && open.is_none() {
            open = Some(i);
        }
    }
    // Fall through: the whole reply. `vet` refuses it with precise errors if
    // it is not code, and those errors are exactly what the repair loop wants.
    let whole = reply.trim();
    (!whole.is_empty()).then(|| whole.to_string())
}

/// The outcome of a generation turn.
#[derive(Debug, Clone)]
pub(crate) struct GeneratedScript {
    /// The source that passed every vetting layer.
    pub source: String,
    /// How many model round trips it took (1 = first try).
    pub attempts: usize,
    /// Every validator refusal that was repaired along the way, for the
    /// transcript's honesty fields.
    pub repaired_errors: Vec<String>,
    /// The vetted header, for display and overlay routing.
    pub header: pine_lite::Header,
}

/// One generation attempt's failure, verbatim from the vet pipeline.
fn vet_report(source: &str) -> String {
    match pine_lite::vet(source) {
        Ok(_) => String::new(),
        Err(errs) => errs
            .iter()
            .map(|e| format!("line {} col {} [{}]: {}", e.span.line, e.span.col, pine_lite::kind_name(e.kind), e.message))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// The second-instrument symbol a script wants, without asking the model:
/// an explicit `sec=SEC` line the model sometimes writes as a comment, else
/// a quoted ALL-CAPS symbol from the user request. Models forget the header
/// knob constantly; the host can derive it deterministically.
fn wanted_sec_symbol(source: &str, user_request: &str) -> Option<String> {
    for line in source.lines() {
        let t = line.trim();
        if t.starts_with("//@") && !t.contains("@pine_lite") {
            if let Some(pos) = t.to_uppercase().find("SEC=") {
                let rest = t[pos + 4..].trim().trim_start_matches('"');
                let sym: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':' || *c == '.').collect();
                if !sym.is_empty() {
                    return Some(sym.to_uppercase());
                }
            }
        }
    }
    let m = user_request
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
        .find(|w| {
            let b = w.as_bytes();
            b.len() >= 6 && b[b.len() - 4..] == *b"USDT" && b.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
    ?;
    Some(m.to_string())
}

/// When the ONLY vet problems are missing-`sec=` refusals, fix the header
/// here instead of spending a model round trip on it: append `sec="SYM"` to
/// the annotation and re-vet. Returns the corrected source on success.
fn autofix_sec(source: &str, user_request: &str) -> Option<String> {
    let errs = pine_lite::vet(source).err()?;
    let only_missing_sec = errs
        .iter()
        .all(|e| e.message.contains("needs a second instrument"));
    if !only_missing_sec {
        return None;
    }
    let sym = wanted_sec_symbol(source, user_request)?;
    let mut lines: Vec<String> = source.lines().map(String::from).collect();
    let header = lines.first_mut()?;
    if !header.contains("@pine_lite") {
        return None;
    }
    *header = format!("{} sec=\"{}\"", header.trim_end(), sym);
    let fixed = lines.join("\n");
    pine_lite::vet(&fixed).is_ok().then_some(fixed)
}

/// Generate and repair a script: the studio's code path.
///
/// Sends the system prompt with the user's request, extracts the script,
/// vets it, and on failure feeds the FULL issue list back as a tool result --
/// the same shape the document loop keeps, because the model corrects from
/// precise, complete error lists far better than from "invalid".
///
/// # Errors
/// [`AgentError`] when the LLM transport fails or every attempt is spent
/// without a script that vets; the error text carries the last report.
pub(crate) async fn generate_script(
    llm: &dyn LlmClient,
    system: String,
    user_request: &str,
    base_script: Option<&str>,
    max_tokens: u32,
    temperature: f32,
    progress: Option<&futures::channel::mpsc::UnboundedSender<GenerationEvent>>,
) -> Result<GeneratedScript, ai_agent::AgentError> {
    let description = match base_script {
        // Edit mode: the current script rides the first message, so "make the
        // bands tighter" tightens the bands instead of starting over -- the
        // document loop's same rule, in code.
        Some(base) => format!(
            "## Current script\n```pine_lite\n{base}\n```\n\n## Requested change\n{user_request}"
        ),
        None => user_request.to_string(),
    };
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text(description)],
    }];
    let tool = ToolSpec {
        name: "submit_script".to_string(),
        description: "Submit the pine-lite script for validation and chart attachment."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "script": {"type": "string", "description": "The complete pine-lite script source, including the //@pine_lite header line."}
            },
            "required": ["script"]
        }),
    };
    let tools = vec![tool];
    let mut repaired_errors = Vec::new();
    let mut last_report = String::from("no script was submitted");

    for attempt in 1..=MAX_ATTEMPTS {
        report(&progress, GenerationEvent::Drafting { attempt, max: MAX_ATTEMPTS });
        let response = llm
            .complete(LlmRequest {
                system: Some(system.clone()),
                messages: messages.clone(),
                tools: tools.clone(),
                tool_choice: Some(ToolChoice::Tool("submit_script".to_string())),
                max_tokens,
                temperature,
            })
            .await?;
        let calls = response.message.tool_calls();
        let Some(call) = calls.iter().find(|c| c.name == "submit_script") else {
            last_report = "the reply did not call submit_script".to_string();
            report(
                &progress,
                GenerationEvent::Repairing { attempt, errors: vec![last_report.clone()] },
            );
            messages.push(response.message.clone());
            messages.push(Message::user(
                "Respond by calling submit_script with the complete pine-lite script.",
            ));
            continue;
        };
        // The tool argument is the primary channel; the fenced block in any
        // accompanying text is the fallback (models do both).
        let source = call.input["script"]
            .as_str()
            .map(str::to_string)
            .or_else(|| extract_script(&response.message.text()));
        let Some(source) = source else {
            last_report = "`script` must be a string".to_string();
            report(
                &progress,
                GenerationEvent::Repairing { attempt, errors: vec![last_report.clone()] },
            );
            messages.push(response.message.clone());
            messages.push(Message::user(
                "Call submit_script with the whole script as the string value of `script`.",
            ));
            continue;
        };

        match pine_lite::vet(&source) {
            Ok((header, _script)) => {
                return Ok(GeneratedScript {
                    source,
                    attempts: attempt,
                    repaired_errors,
                    header,
                });
            }
            Err(errs) => {
                // The one failure with a deterministic fix is a missing
                // header knob: if every refusal is "needs a second
                // instrument" and the request names a pair, append `sec=`
                // here rather than burning a model round trip teaching
                // header syntax the model keeps getting wrong.
                if errs
                    .iter()
                    .all(|e| e.message.contains("needs a second instrument"))
                {
                    if let Some(fixed) = autofix_sec(&source, user_request) {
                        let (header, _script) =
                            pine_lite::vet(&fixed).expect("autofix re-vet");
                        let detail = format!(
                            "appended sec=\"{}\" to the header",
                            header.sec.clone().unwrap_or_default()
                        );
                        report(&progress, GenerationEvent::Autofix { detail: detail.clone() });
                        repaired_errors.push(format!("autofix: {detail}"));
                        return Ok(GeneratedScript {
                            source: fixed,
                            attempts: attempt,
                            repaired_errors,
                            header,
                        });
                    }
                }
                last_report = vet_report(&source);
                repaired_errors.extend(errs.iter().map(|e| e.message.clone()));
                report(
                    &progress,
                    GenerationEvent::Repairing {
                        attempt,
                        errors: errs.iter().map(|e| e.message.clone()).collect(),
                    },
                );
                messages.push(response.message.clone());
                messages.push(Message::tool_results(vec![ai_agent::llm_client::ToolResult {
                    tool_use_id: call.id.clone(),
                    // Errors name what broke and where; the cheat sheet riding
                    // next to them names what the compiler accepts instead, so
                    // the model repairs from the language's real surface
                    // rather than re-deriving it from the system prompt.
                    content: crate::pine_cheatsheet::repair_content(errs.iter().map(|e| serde_json::json!({
                        "kind": pine_lite::kind_name(e.kind),
                        "line": e.span.line,
                        "col": e.span.col,
                        "message": e.message,
                    })).collect::<Vec<_>>()),
                    is_error: true,
                }]));
            }
        }
    }
    Err(ai_agent::AgentError::InvalidStrategyDocument(
        strategy_dsl::DslError::Validation {
            issues: vec![strategy_dsl::ValidationIssue::new(
                "script",
                format!(
                    "the script did not pass validation after {MAX_ATTEMPTS} attempts. Last errors: {last_report}"
                ),
            )],
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSI: &str = concat!(
        "```pine_lite\n",
        "//@pine_lite version=1 overlay=false title=\"RSI\"\n",
        "len = input.int(defval=14, title=\"Length\")\n",
        "r = ta.rsi(close, len)\n",
        "plot(r, title=\"RSI\", color=color.purple)\n",
        "hline(70)\n",
        "hline(30)\n",
        "```\n",
    );

    #[test]
    fn the_system_prompt_teaches_only_vettable_forms() {
        // Every form the prompt's prose demonstrates, as one script: if the
        // prompt ever teaches a construct the language refuses, the model is
        // baited into a refusal the repair loop must undo.
        let src = concat!(
            "//@pine_lite version=1 overlay=false title=\"Prompt forms\"\n",
            "len = input.int(defval=4, title=\"Pivot window\")\n",
            "mult = input.float(defval=2.0, title=\"Mult\")\n",
            "show = input.bool(defval=true, title=\"Show\")\n",
            "hh = ta.highest(high, 2 * len + 1)\n",
            "pivot_high = hh[len] == high[len]\n",
            "var ph1 = na\n",
            "var ph2 = na\n",
            "if pivot_high\n",
            "    ph2 := ph1\n",
            "    ph1 := high[len]\n",
            "var count = 0\n",
            "while count < 3\n",
            "    count := count + 1\n",
            "zone_mid(a, b, m = 1.5) =>\n",
            "    (a + b) / 2 * m\n",
            "mid = zone_mid(high, low)\n",
            "atr = ta.atr(14)\n",
            "flat = math.abs(mid - close) <= atr * 0.15\n",
            "plot(mid)\n",
            "plotshape(flat and show, shape=\"triangledown\", color=color.red, location_value=high)\n",
        );
        let errs = pine_lite::vet(src);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
    }

    #[test]
    fn extracts_a_tagged_block() {
        let reply = format!("Here is your indicator:\n{RSI}Let me know if you want changes.");
        let script = extract_script(&reply).expect("script");
        assert!(script.starts_with("//@pine_lite"));
        assert!(script.contains("ta.rsi(close, len)"));
        assert!(!script.contains("```"));
    }

    #[test]
    fn extracts_an_untagged_block() {
        let reply = "Sure:\n```\n//@pine_lite version=1\nplot(close)\n```\ndone";
        let script = extract_script(reply).expect("script");
        assert!(script.contains("plot(close)"));
    }

    #[test]
    fn prose_alone_yields_itself_for_vetting() {
        let script = extract_script("no code here").expect("whole reply");
        assert_eq!(script, "no code here");
    }

    #[test]
    fn vet_reports_accept_the_good_script() {
        let script = extract_script(RSI).expect("script");
        assert!(pine_lite::vet(&script).is_ok(), "{}", vet_report(&script));
    }

    #[test]
    fn vet_reports_carry_positions_for_repair() {
        let bad = "//@pine_lite version=1\nx = close + true\nplot(x)\n";
        let report = vet_report(bad);
        assert!(report.contains("line 2"), "{report}");
    }

    #[test]
    fn the_reference_script_vets_clean() {
        // The prompt quotes this script as the shape to imitate; if it ever
        // stops vetting, the prompt is teaching failure.
        assert!(pine_lite::vet(REFERENCE_SCRIPT).is_ok(), "{}", vet_report(REFERENCE_SCRIPT));
    }

    #[test]
    fn a_multi_value_call_read_as_a_scalar_names_the_destructuring_fix() {
        // The old prompt's mistake, now caught by the language itself: a MACD
        // line is three values and the error says how to take them apart.
        let macd = "//@pine_lite version=1\nm = ta.macd(close, 12, 26, 9)\nplot(m)\n";
        let errs = pine_lite::vet(macd).expect_err("macd needs destructuring");
        assert!(errs.iter().any(|e| e.span.line == 2), "{errs:?}");
        assert!(
            errs.iter().any(|e| e.message.contains("a, b, c = ta.macd")),
            "{errs:?}"
        );
    }

    #[test]
    fn the_builtins_list_matches_the_interpreter() {
        // Every call shape the prompt teaches must vet. Keep this list in sync
        // with `script_system_prompt` -- a drifting table baits the model into
        // calls the repair loop can only bounce.
        let src = concat!(
            "//@pine_lite version=1 overlay=false title=\"t\"\n",
            "a = ta.sma(close, 9)\n",
            "b = ta.ema(close, 9)\n",
            "c = ta.rma(close, 9)\n",
            "d = ta.rsi(close, 14)\n",
            "e = ta.highest(high, 20)\n",
            "f = ta.lowest(low, 20)\n",
            "g = ta.mom(close, 10)\n",
            "h = ta.roc(close, 10)\n",
            "i = ta.change(close)\n",
            "j = ta.atr(14)\n",
            "k = ta.tr()\n",
            "l = ta.vwap()\n",
            "m = ta.stoch(close, 14)\n",
            "m2, d2 = ta.stoch(close, 14)\n",
            "w = ta.wma(close, 9)\n",
            "b1, b2, b3 = ta.bb(close, 20, 2)\n",
            "g1, g2, g3 = ta.macd(close, 12, 26, 9)\n",
            "n = ta.crossover(a, b)\n",
            "o = ta.crossunder(a, b)\n",
            "p = ta.cross(a, b)\n",
            "q = math.abs(a)\n",
            "r = math.pow(a, 2.0)\n",
            "s = math.min(a, b)\n",
            "t = math.max(a, b)\n",
            "u = math.avg(a, b)\n",
            "v = math.sum(close, 9)\n",
            "w = math.round(a) + math.floor(a) + math.ceil(a)\n",
            "x = math.sqrt(math.abs(a)) + math.log(a + 1.0) + math.exp(-a) + math.sign(a)\n",
            "y = na(a) ? 0.0 : nz(a, 1.0)\n",
            "plot(a)\n",
        );
        assert!(pine_lite::vet(src).is_ok(), "{}", vet_report(src));
    }

    #[test]
    fn history_off_a_call_is_refused_at_vet_time() {
        // `ta.sma(close, 9)[1]` -- the model's favorite shortcut; the VM only
        // indexes variables and builtin series, so vet must catch it.
        let src = "//@pine_lite version=1\nx = ta.sma(close, 9)[1]\nplot(x)\n";
        let errs = pine_lite::vet(src).expect_err("history needs a variable");
        assert!(!errs.is_empty(), "{errs:?}");
    }

    /// The idioms section of the system prompt, verbatim, wrapped in a
    /// runnable script. If the prompt teaches a form the VM refuses, this
    /// catches it before any model does.
    const IDIOM_SCRIPT: &str = concat!(
        "//@pine_lite version=1 overlay=false title=\"Pivots\"\n",
        "w = input.int(defval=4, title=\"Pivot window\")\n",
        "hh = ta.highest(high, 2 * w + 1)\n",
        "ll = ta.lowest(low, 2 * w + 1)\n",
        "pivot_high = hh[w] == high[w]\n",
        "pivot_low = ll[w] == low[w]\n",
        "var ph1 = na\n",
        "var ph2 = na\n",
        "var ph1_bar = 0.0\n",
        "var ph2_bar = 0.0\n",
        "if pivot_high\n",
        "    ph2 := ph1\n",
        "    ph2_bar := ph1_bar\n",
        "    ph1 := high[w]\n",
        "    ph1_bar := bar_index - w\n",
        "slope_high = (not na(ph2) and ph1_bar != ph2_bar) ? (ph1 - ph2) / (ph1_bar - ph2_bar) : na\n",
        "atr = ta.atr(14)\n",
        "flat = math.abs(slope_high) <= atr * 0.15\n",
        "level = ph1 + slope_high * (bar_index - ph1_bar)\n",
        "plot(level)\n",
    );

    #[test]
    fn the_taught_idioms_vet_and_run() {
        let errs = pine_lite::vet(IDIOM_SCRIPT);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
        let (_, parsed) = pine_lite::vet(IDIOM_SCRIPT).ok().unwrap();
        let candles: Vec<analytics_core::types::Candle> = (0..60)
            .map(|i| {
                let close = 100.0 + (i as f64) * 0.5 + (i as f64) * 0.25 % 3.0;
                analytics_core::types::Candle {
                    symbol: "TEST".into(),
                    timeframe: analytics_core::types::Timeframe::M1,
                    open_time: 0,
                    open: close,
                    high: close + 1.0,
                    low: close - 1.0,
                    close,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                }
            })
            .collect();
        let inputs = pine_lite::Inputs::default();
        let output = pine_lite::run(&parsed, &candles, &inputs).expect("the idioms run");
        assert!(!output.plots.is_empty(), "the projected line plots");
    }

    /// The multi-symbol idiom the prompt teaches, verbatim: an SMT divergence
    /// sweep between the chart and a declared `sec=` pair. If the prompt ever
    /// teaches a `request.*` form the language refuses, this catches it.
    const SMT_IDIOM_SCRIPT: &str = concat!(
        "//@pine_lite version=1 overlay=false title=\"SMT divergence\" sec=\"ETHUSDT\"\n",
        "w = input.int(defval=3, title=\"Pivot window\")\n",
        "hh = ta.highest(high, 2 * w + 1)\n",
        "ll = ta.lowest(low, 2 * w + 1)\n",
        "pivot_high = hh[w] == high[w]\n",
        "pivot_low = ll[w] == low[w]\n",
        "var ph1 = na\n",
        "var ph2 = na\n",
        "var ph1_bar = 0.0\n",
        "var ph2_bar = 0.0\n",
        "var ph1_sec = na\n",
        "if pivot_high\n",
        "    ph2 := ph1\n",
        "    ph2_bar := ph1_bar\n",
        "    ph1 := high[w]\n",
        "    ph1_bar := bar_index - w\n",
        "    ph1_sec := request.high()\n",
        "var pl1_sec = na\n",
        "if pivot_low\n",
        "    pl1_sec := request.low()\n",
        "smt_bear = not na(ph2) and request.high() < ph1_sec and ph1 > ph2\n",
        "smt_bull = not na(pl1_sec) and request.low() > pl1_sec\n",
        "spread = close - request.close()\n",
        "plot(smt_bear ? 1.0 : 0.0, title=\"SMT bear\", style=\"histogram\")\n",
        "plot(smt_bull ? 1.0 : 0.0, title=\"SMT bull\", style=\"histogram\")\n",
        "plot(spread, title=\"Spread\")\n",
    );

    #[test]
    fn the_taught_smt_idiom_vets_and_runs() {
        let errs = pine_lite::vet(SMT_IDIOM_SCRIPT);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
        let (_, parsed) = pine_lite::vet(SMT_IDIOM_SCRIPT).ok().unwrap();
        // Ours rises while the pair falls -- the divergence genuinely fires.
        let candles: Vec<analytics_core::types::Candle> = (0..60)
            .map(|i| {
                let close = 100.0 + (i as f64) * 0.5 + (i as f64) * 0.25 % 3.0;
                analytics_core::types::Candle {
                    symbol: "TEST".into(),
                    timeframe: analytics_core::types::Timeframe::M1,
                    open_time: 0,
                    open: close,
                    high: close + 1.0,
                    low: close - 1.0,
                    close,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                }
            })
            .collect();
        let security: Vec<analytics_core::types::Candle> = candles
            .iter()
            .enumerate()
            .map(|(i, c)| analytics_core::types::Candle {
                symbol: "ETHUSDT".into(),
                timeframe: analytics_core::types::Timeframe::M1,
                open_time: c.open_time,
                open: 50.0,
                high: 51.0 - (i as f64) * 0.2,
                low: 49.0,
                close: 50.0,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            })
            .collect();
        let inputs = pine_lite::Inputs { security, ..pine_lite::Inputs::default() };
        let output = pine_lite::run(&parsed, &candles, &inputs).expect("the SMT idiom runs");
        assert_eq!(output.plots.len(), 3, "two histograms + the spread");
    }

    #[test]
    fn autofix_adds_sec_when_the_body_needs_it() {
        // The live failure: correct `request.*` body, header without the
        // knob, request names the pair. One deterministic pass, no model.
        let src = "//@pine_lite version=1 overlay=true title=\"SMT\"\nrc = request.close()\nplot(close - rc)\n";
        let fixed = autofix_sec(src, "SMT divergence against ETHUSDT").expect("autofix");
        assert!(fixed.contains("sec=\"ETHUSDT\""), "{fixed}");
        assert!(pine_lite::vet(&fixed).is_ok());
    }

    #[test]
    fn autofix_prefers_the_comment_declared_symbol() {
        let src = "//@pine_lite version=1\n//@sec=BNBUSDT\nrc = request.close()\nplot(rc)\n";
        let fixed = autofix_sec(src, "SMT vs SOLUSDT").expect("autofix");
        assert!(fixed.contains("sec=\"BNBUSDT\""), "{fixed}");
    }

    #[test]
    fn autofix_leaves_other_errors_to_the_model() {
        // A real syntax error is NOT auto-fixable: no sec is appended.
        let src = "//@pine_lite version=1\nrc = request.close(\n";
        assert!(autofix_sec(src, "vs ETHUSDT").is_none());
    }

    #[test]
    fn indexed_assignment_is_refused() {
        // The exact construct from the live failure: `arr[i] = v`. It parses
        // as a call expression followed by `=`, so vet must refuse it with a
        // position the repair loop can name.
        let src = "//@pine_lite version=1\nvar arr = 0.0\narr[3] = close\nplot(arr)\n";
        let errs = pine_lite::vet(src).expect_err("no indexed assignment");
        assert!(errs.iter().any(|e| e.span.line == 3), "{errs:?}");
    }

    /// The array + session idioms the prompt teaches, runnable end to end:
    /// collect pivot highs into arrays, project the line through the last
    /// two, and gate the setup by hour. The SMC/liquidity shape, minus the
    /// second instrument SMT needs.
    const ARRAY_IDIOM_SCRIPT: &str = "//@pine_lite version=1 overlay=true title=\"Swing structure\"\n\
        w = input.int(defval=4, title=\"Pivot window\")\n\
        var ph = array.new()\n\
        var phb = array.new()\n\
        hh = ta.highest(high, 2 * w + 1)\n\
        pivot_high = hh[w] == high[w]\n\
        if pivot_high\n\
            array.push(ph, high[w])\n\
            array.push(phb, bar_index - w)\n\
        n = array.size(ph)\n\
        res = n >= 2 ? array.get(ph, n - 1) + (array.get(ph, n - 1) - array.get(ph, n - 2)) / (array.get(phb, n - 1) - array.get(phb, n - 2)) * (bar_index - array.get(phb, n - 1)) : na\n\
        in_session = hour >= 7 and hour < 20\n\
        plot(res, title=\"Resistance\", color=color.orange)\n\
        plotshape(close > res and in_session, shape=\"triangledown\", color=color.red, location_value=high)\n";

    #[test]
    fn the_taught_array_idioms_vet_and_run() {
        let errs = pine_lite::vet(ARRAY_IDIOM_SCRIPT);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
        let (_, parsed) = pine_lite::vet(ARRAY_IDIOM_SCRIPT).ok().unwrap();
        let candles: Vec<analytics_core::types::Candle> = (0..80)
            .map(|i| {
                let close = 100.0 + (i as f64) * 0.5 + ((i % 9) as f64) * 0.6;
                analytics_core::types::Candle {
                    symbol: "TEST".into(),
                    timeframe: analytics_core::types::Timeframe::M1,
                    open_time: 1_790_553_600_000_000_000i64 + (i as i64) * 60_000_000_000,
                    open: close,
                    high: close + 1.2,
                    low: close - 1.2,
                    close,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                }
            })
            .collect();
        let inputs = pine_lite::Inputs::default();
        let output = pine_lite::run(&parsed, &candles, &inputs).expect("the array idioms run");
        assert!(!output.plots.is_empty());
        // The resistance ray through the last two pivots is finite by the end.
        let res = &output.plots[0].values;
        assert!(res[79].is_finite(), "resistance projected: {}", res[79]);
    }

    /// The SMC zone idiom the prompt teaches, verbatim: the fair-value-gap
    /// detector that draws BOXES. The live failure this pins: a generation
    /// asked for Smart Money Concepts rendered zones as `plotshape` triangles
    /// and `plot` lines -- the shapes the reference scripts demonstrate --
    /// because no taught form showed a zone as a box. If the language ever
    /// strands this idiom, the prompt is teaching failure and this test
    /// fails before a model does.
    const SMC_IDIOM_SCRIPT: &str = concat!(
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
    fn the_taught_smc_idiom_vets_runs_and_draws_boxes() {
        let errs = pine_lite::vet(SMC_IDIOM_SCRIPT);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
        let (_, parsed) = pine_lite::vet(SMC_IDIOM_SCRIPT).ok().unwrap();
        // A bullish gap at bars 0..2 (mitigated at bar 4), another at bars
        // 6..8 left live: exactly one box survives to the last bar.
        let ohlc: [(f64, f64, f64, f64); 12] = [
            (100.0, 101.0, 99.0, 100.0),
            (102.0, 105.0, 102.0, 104.0),
            (105.0, 107.0, 104.5, 106.0),
            (105.0, 106.0, 103.0, 105.0),
            (104.0, 105.0, 100.0, 101.0),
            (101.0, 104.5, 99.5, 102.0),
            (102.0, 103.5, 101.5, 103.0),
            (104.0, 107.0, 104.0, 106.0),
            (107.0, 109.0, 106.5, 108.0),
            (108.0, 110.0, 107.0, 109.0),
            (109.0, 111.0, 108.0, 110.0),
            (110.0, 112.0, 109.0, 111.0),
        ];
        let candles: Vec<analytics_core::types::Candle> = ohlc
            .iter()
            .enumerate()
            .map(|(i, &(o, h, l, c))| analytics_core::types::Candle {
                symbol: "TEST".into(),
                timeframe: analytics_core::types::Timeframe::M1,
                open_time: (i as i64) * 60_000_000_000,
                open: o,
                high: h,
                low: l,
                close: c,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            })
            .collect();
        let output = pine_lite::run(&parsed, &candles, &pine_lite::Inputs::default()).expect("the SMC idiom runs");
        let boxes = output
            .objects
            .iter()
            .filter(|o| matches!(o, pine_lite::interp::ScriptObject::Box { .. }))
            .count();
        assert_eq!(boxes, 1, "the live gap draws exactly one box: {:?}", output.objects);
    }

    /// The strategy idiom the prompt teaches, verbatim (docs/24 S3): the
    /// `strategy(...)` header, gated entries, a `var`-ratcheted ATR trailing
    /// stop re-armed each bar, and the account read. This is the shape the
    /// generated "ATR Trail" script got wrong (`trail = 0.0`, no `var`, no
    /// gate) -- the prompt now teaches the fix and this test pins it.
    const STRATEGY_IDIOM_SCRIPT: &str = REFERENCE_STRATEGY;

    /// An up-trend, a sharp reversal, and a recovery: the SMA(10)/SMA(30)
    /// cross fires early (long), the reversal's `close_all` (or the trail
    /// stop) closes it, and the recovery arms the long again -- at least
    /// one closed trade either way.
    fn strategy_candles() -> Vec<analytics_core::types::Candle> {
        (0..140)
            .map(|i| {
                let close = match i {
                    0..=19 => 100.0 + (i as f64) * 0.05,
                    20..=59 => 101.0 + ((i - 20) as f64) * 0.72,
                    60..=89 => 130.0 - ((i - 59) as f64) * 0.85,
                    _ => 104.0 + ((i - 89) as f64) * 0.5,
                };
                analytics_core::types::Candle {
                    symbol: "TEST".into(),
                    timeframe: analytics_core::types::Timeframe::M1,
                    open_time: 1_790_553_600_000_000_000i64 + (i as i64) * 60_000_000_000,
                    open: close - 0.15,
                    high: close + 1.0,
                    low: close - 1.0,
                    close,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                }
            })
            .collect()
    }

    #[test]
    fn the_taught_strategy_idiom_vets_and_runs() {
        // docs/24 S3: the reference strategy must VET...
        let errs = pine_lite::vet(STRATEGY_IDIOM_SCRIPT);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
        let (_, parsed) = pine_lite::vet(STRATEGY_IDIOM_SCRIPT).ok().unwrap();
        let candles = strategy_candles();
        let output = pine_lite::run(&parsed, &candles, &pine_lite::Inputs::default()).expect("the strategy runs");
        // ...and SIMULATE: a strategy that draws but trades nothing has no
        // business being the prompt's model answer.
        let sim = output.simulation.as_ref().expect("the reference strategy simulates");
        assert!(
            sim.report.total_trades >= 1.0,
            "the taught idiom must close at least one trade: {:?}",
            sim.report
        );
        assert!(output.strategy_used);
        // The trail is armed and real: the reference draws positive levels
        // while the position is open. (The monotone-ratchet property is
        // pinned separately by the pullback test below.)
        let trail = &output.plots[0].values;
        assert!(
            trail.iter().copied().filter(|v| *v > 0.0).count() > 10,
            "the trail level is drawn while the position is open"
        );
    }

    /// The prompt's rule 3 lesson as a differential test: on a rise, a
    /// shallow pullback, then a recovery, the bare `trail = close - atr * 3`
    /// shape (no `var`) resets every bar and follows price DOWN through the
    /// pullback; the taught `var` + `math.max` ratchet holds the high-water
    /// level. This is the exact bug the "ATR Trail" generation shipped.
    #[test]
    fn the_taught_trail_ratchet_survives_a_pullback() {
        fn trail_candles() -> Vec<analytics_core::types::Candle> {
            (0..96)
                .map(|i| {
                    let close = match i {
                        0..=39 => 100.0 + (i as f64) * 0.7,
                        40..=54 => 127.3 - ((i - 39) as f64) * 0.25,
                        _ => 123.55 + ((i - 54) as f64) * 0.4,
                    };
                    analytics_core::types::Candle {
                        symbol: "TEST".into(),
                        timeframe: analytics_core::types::Timeframe::M1,
                        open_time: (i as i64) * 60_000_000_000,
                        open: close - 0.15,
                        high: close + 1.0,
                        low: close - 1.0,
                        close,
                        volume: 1.0,
                        buy_volume: 0.5,
                        sell_volume: 0.5,
                    }
                })
                .collect()
        }
        fn trail_of(body: &str) -> Vec<f64> {
            let head = concat!(
                "//@pine_lite version=1 overlay=false title=\"Ratchet check\" strategy(initial_capital=10000)\n",
                "atr = ta.atr(14)\n",
                "if bar_index == 1\n",
                "    strategy.entry(\"L\", direction=\"long\")\n",
            );
            let src = format!("{head}{body}plot(trail, title=\"Trail\")\n");
            let (_, parsed) = pine_lite::vet(&src).ok().expect("the ratchet fixture vets");
            let output =
                pine_lite::run(&parsed, &trail_candles(), &pine_lite::Inputs::default()).expect("the ratchet fixture runs");
            output.plots[0].values.iter().copied().filter(|v| v.is_finite()).collect()
        }
        let taught = trail_of(concat!(
            "var float trail = 0.0\n",
            "if strategy.position_size > 0 and not na(atr)\n",
            "    trail := math.max(trail, close - atr * 3.0)\n",
            "    strategy.exit(\"xl\", stop=trail)\n",
        ));
        let buggy = trail_of(concat!(
            "trail = close - atr * 3.0\n",
            "if not na(trail)\n",
            "    strategy.exit(\"xl\", stop=trail)\n",
        ));
        assert!(
            buggy.windows(2).any(|w| w[1] < w[0] - 1e-9),
            "the no-var shape resets each bar and follows price down through the pullback"
        );
        assert!(
            !taught.is_empty() && taught.windows(2).all(|w| w[1] >= w[0] - 1e-9),
            "the var ratchet holds the high-water level through the pullback: {taught:?}"
        );
    }

    #[test]
    fn a_zero_fill_strategy_warns_instead_of_failing() {
        // The model's condition never fires here (the cross never happens on
        // a flat line) -- the run must still succeed, but its note must name
        // the zero fills so the repair loop and the user see the truth.
        let src = concat!(
            "//@pine_lite version=1 overlay=false title=\"Never fires\" strategy(initial_capital=10000)\n",
            "fast = ta.sma(close, 10)\n",
            "slow = ta.sma(close, 30)\n",
            "if ta.crossover(fast, slow) and strategy.position_size == 0\n",
            "    strategy.entry(\"L\", direction=\"long\")\n",
            "if strategy.position_size > 0\n",
            "    strategy.exit(\"xl\", stop=close - 1.0)\n",
            "plot(close)\n",
        );
        let (_, parsed) = pine_lite::vet(src).ok().expect("vets");
        let candles: Vec<analytics_core::types::Candle> = (0..80)
            .map(|i| {
                let close = 100.0;
                analytics_core::types::Candle {
                    symbol: "TEST".into(),
                    timeframe: analytics_core::types::Timeframe::M1,
                    open_time: (i as i64) * 60_000_000_000,
                    open: close,
                    high: close + 0.5,
                    low: close - 0.5,
                    close,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                }
            })
            .collect();
        let output = pine_lite::run(&parsed, &candles, &pine_lite::Inputs::default()).expect("runs");
        let sim = output.simulation.as_ref().expect("a strategy run simulates");
        assert_eq!(sim.report.total_trades as usize, 0, "the entry condition never fires");
        assert!(
            output.notes.iter().any(|n| n.contains("0 fills")),
            "the zero-fill note must ride the output: {:?}",
            output.notes
        );
    }

    #[test]
    fn the_strategy_header_teaches_only_real_knobs() {
        // The header the prompt teaches must vet as-is: every knob named,
        // every quoted value in the language's accepted spelling.
        let header = REFERENCE_STRATEGY.lines().next().expect("header").to_string();
        let src = format!("{header}\nplot(close)\n");
        let errs = pine_lite::vet(&src);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line 1 col {}: {}", e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
    }

    #[test]
    fn the_prompt_carries_both_reference_scripts() {
        // The system prompt embeds both references verbatim; a formatting
        // regression that drops one is exactly what this catches.
        let prompt = script_system_prompt("BTCUSDT", "15m");
        assert!(prompt.contains(REFERENCE_SCRIPT), "the indicator reference rides the prompt");
        assert!(prompt.contains(REFERENCE_STRATEGY), "the strategy reference rides the prompt");
        assert!(prompt.contains("## Strategies"), "the strategy section exists");
    }

    /// The MTF idiom the prompt teaches (docs/28), assembled as the complete
    /// script the model is expected to write from it.
    const MTF_IDIOM_SCRIPT: &str = concat!(
        "//@pine_lite version=1 overlay=true title=\"HTF FVG\"\n",
        "ht = request.security(\"BTCUSDT\", \"1h\", time)\n",
        "hh = request.security(\"BTCUSDT\", \"1h\", high)\n",
        "hl = request.security(\"BTCUSDT\", \"1h\", low)\n",
        "newbar = ht != ht[1]\n",
        "var ph1 = na\n",
        "var ph2 = na\n",
        "if newbar and not na(ph2)\n",
        "    if hl[1] > ph2\n",
        "        box.new_time(ht[1], hl[1], ht + 10000000000000000.0, ph2, color=color.teal)\n",
        "if newbar\n",
        "    ph2 = ph1\n",
        "    ph1 = hh[1]\n",
        "plot(close)\n",
    );

    #[test]
    fn the_taught_mtf_idiom_vets_runs_and_draws_the_gap() {
        let errs = pine_lite::vet(MTF_IDIOM_SCRIPT);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
        let (_, parsed) = pine_lite::vet(MTF_IDIOM_SCRIPT).ok().unwrap();
        // The fixture mirrors the pine-lite integration test: 1h bars of 10
        // chart bars each; B2's low (105) gaps above B0's high (100).
        let mk = |i: i64| analytics_core::types::Candle {
            symbol: "TEST".into(),
            timeframe: analytics_core::types::Timeframe::M1,
            open_time: i * 60_000_000_000,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        };
        let candles: Vec<_> = (0..40).map(mk).collect();
        let pool_levels: [(f64, f64); 4] = [(100.0, 90.0), (110.0, 95.0), (115.0, 105.0), (120.0, 110.0)];
        let pool: Vec<_> = (0..40)
            .map(|i| {
                let b = (i / 10) as usize;
                let mut c = mk(i);
                c.open_time = (b as i64) * 10 * 60_000_000_000;
                c.high = pool_levels[b].0;
                c.low = pool_levels[b].1;
                c
            })
            .collect();
        let mut series_pool = std::collections::HashMap::new();
        series_pool.insert("BTCUSDT@1H".to_string(), pool);
        let inputs = pine_lite::Inputs { series_pool, ..pine_lite::Inputs::default() };
        let output = pine_lite::run(&parsed, &candles, &inputs).expect("the MTF idiom runs");
        let boxes = output
            .objects
            .iter()
            .filter(|o| matches!(o, pine_lite::interp::ScriptObject::BoxTime { .. }))
            .count();
        assert_eq!(boxes, 1, "one HTF gap, one time-anchored box: {:?}", output.objects);
    }

    #[test]
    fn the_prompt_teaches_the_mtf_section_and_time_anchored_drawing() {
        let prompt = script_system_prompt("BTCUSDT", "15m");
        assert!(prompt.contains("## A higher timeframe"), "the MTF section rides the prompt");
        assert!(prompt.contains("box.new_time"), "the time-anchored twins ride the prompt");
        assert!(prompt.contains("request.security"), "pooled reads ride the prompt");
        assert!(prompt.contains("request.time()"), "the aligned-time read rides the prompt");
    }

    /// The multi-timeframe idiom the prompt teaches (docs/28), assembled as
    /// the complete script the model is expected to write from it.
    const MTF_SYNTHESIS_SCRIPT: &str = concat!(
        "//@pine_lite version=1 overlay=true title=\"MTF FVG\"\n",
        "t15 = request.security(\"BTCUSDT\", \"15m\", time)\n",
        "h15 = request.security(\"BTCUSDT\", \"15m\", high)\n",
        "l15 = request.security(\"BTCUSDT\", \"15m\", low)\n",
        "t1h = request.security(\"BTCUSDT\", \"1h\", time)\n",
        "h1h = request.security(\"BTCUSDT\", \"1h\", high)\n",
        "l1h = request.security(\"BTCUSDT\", \"1h\", low)\n",
        "far = 10000000000000000.0\n",
        "nb15 = t15 != t15[1]\n",
        "var a1 = na\n",
        "var a2 = na\n",
        "if nb15 and not na(a2)\n",
        "    if l15[1] > a2\n",
        "        box.new_time(t15[1], l15[1], t15 + far, a2, color=color.aqua)\n",
        "        label.new_time(t15[1], l15[1], \"15m FVG\", color=color.aqua)\n",
        "if nb15\n",
        "    a2 = a1\n",
        "    a1 = h15[1]\n",
        "nb1h = t1h != t1h[1]\n",
        "var b1 = na\n",
        "var b2 = na\n",
        "if nb1h and not na(b2)\n",
        "    if l1h[1] > b2\n",
        "        box.new_time(t1h[1], l1h[1], t1h + far, b2, color=color.teal)\n",
        "        label.new_time(t1h[1], l1h[1], \"1h FVG\", color=color.teal)\n",
        "if nb1h\n",
        "    b2 = b1\n",
        "    b1 = h1h[1]\n",
        "plot(close)\n",
    );

    #[test]
    fn the_taught_multi_timeframe_idiom_vets_and_runs_over_two_pools() {
        let errs = pine_lite::vet(MTF_SYNTHESIS_SCRIPT);
        assert!(errs.is_ok(), "{}", errs.expect_err("vet").iter().map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message)).collect::<Vec<_>>().join("\n"));
        let (_, parsed) = pine_lite::vet(MTF_SYNTHESIS_SCRIPT).ok().unwrap();
        // The same fixture as the pine-lite integration test: 15m bars 4
        // chart bars wide, 1h bars 10 wide, one qualifying gap each.
        let mk = |i: i64| analytics_core::types::Candle {
            symbol: "TEST".into(),
            timeframe: analytics_core::types::Timeframe::M1,
            open_time: i * 60_000_000_000,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        };
        let candles: Vec<_> = (0..40).map(mk).collect();
        let p15: [(f64, f64); 10] = [
            (100.0, 90.0), (108.0, 94.0), (112.0, 104.0), (110.0, 103.0), (111.0, 102.0),
            (112.0, 103.0), (113.0, 104.0), (114.0, 105.0), (115.0, 106.0), (116.0, 107.0),
        ];
        let pool15: Vec<_> = (0..40)
            .map(|i| {
                let b = (i / 4) as usize;
                let mut c = mk(i);
                c.open_time = (b as i64) * 4 * 60_000_000_000;
                c.high = p15[b].0;
                c.low = p15[b].1;
                c
            })
            .collect();
        let p1h: [(f64, f64); 4] = [(100.0, 90.0), (110.0, 95.0), (115.0, 105.0), (120.0, 110.0)];
        let pool1h: Vec<_> = (0..40)
            .map(|i| {
                let b = (i / 10) as usize;
                let mut c = mk(i);
                c.open_time = (b as i64) * 10 * 60_000_000_000;
                c.high = p1h[b].0;
                c.low = p1h[b].1;
                c
            })
            .collect();
        let mut series_pool = std::collections::HashMap::new();
        series_pool.insert("BTCUSDT@15M".to_string(), pool15);
        series_pool.insert("BTCUSDT@1H".to_string(), pool1h);
        let inputs = pine_lite::Inputs { series_pool, ..pine_lite::Inputs::default() };
        let output = pine_lite::run(&parsed, &candles, &inputs).expect("the multi-TF idiom runs");
        let boxes = output
            .objects
            .iter()
            .filter(|o| matches!(o, pine_lite::interp::ScriptObject::BoxTime { .. }))
            .count();
        assert_eq!(boxes, 2, "one zone per timeframe: {:?}", output.objects);
        let labels: Vec<_> = output
            .objects
            .iter()
            .filter_map(|o| match o {
                pine_lite::interp::ScriptObject::LabelTime { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["15m FVG", "1h FVG"], "each zone names its timeframe");
    }

    #[test]
    fn the_prompt_teaches_multi_timeframe_synthesis() {
        let prompt = script_system_prompt("BTCUSDT", "5m");
        assert!(prompt.contains("## Multi-timeframe synthesis"), "the synthesis section rides the prompt");
        assert!(prompt.contains("15m FVG"), "the tf-tagged zone labels ride the prompt");
    }

    /// A scripted LLM: each completion pops the next canned tool call, so a
    /// test can run the whole attempt loop without a provider.
    struct ScriptedLlm {
        scripts: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl LlmClient for ScriptedLlm {
        async fn complete(
            &self,
            _request: LlmRequest,
        ) -> Result<ai_agent::llm_client::LlmResponse, ai_agent::AgentError> {
            let script = self.scripts.lock().expect("scripts").remove(0);
            Ok(ai_agent::llm_client::LlmResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "call-1".into(),
                        name: "submit_script".into(),
                        input: serde_json::json!({ "script": script }),
                    }],
                },
                stop_reason: ai_agent::llm_client::StopReason::ToolUse,
                usage: ai_agent::llm_client::Usage {
                    input_tokens: None,
                    output_tokens: None,
                },
            })
        }
        fn name(&self) -> &str {
            "scripted"
        }
    }

    #[tokio::test]
    async fn the_progress_stream_reports_each_attempt_and_repair() {
        // docs/36: a failing first draft then a passing one must stream
        // drafting, the verbatim vet complaint, drafting again -- real events,
        // no fake progress.
        let bad = "//@pine_lite version=1\nplot(close"; // unparseable
        let good = "//@pine_lite version=1\nplot(close)\n";
        let llm = ScriptedLlm {
            scripts: std::sync::Mutex::new(vec![bad.to_string(), good.to_string()]),
        };
        let (tx, rx) = futures::channel::mpsc::unbounded::<GenerationEvent>();
        let generated = generate_script(&llm, "system".into(), "a plot", None, 4096, 0.2, Some(&tx))
            .await
            .expect("the second attempt passes");
        assert_eq!(generated.attempts, 2);
        drop(tx);
        let events: Vec<GenerationEvent> = futures::StreamExt::collect(rx).await;
        let stages: Vec<String> = events
            .iter()
            .map(|event| serde_json::to_value(event).expect("serializes")["stage"].to_string())
            .collect();
        assert_eq!(
            stages,
            vec![
                "\"drafting\"".to_string(),
                "\"repairing\"".to_string(),
                "\"drafting\"".to_string()
            ],
            "draft, complaint, draft: {stages:?}"
        );
        // The repair frame carries the vet's own words.
        let repair = serde_json::to_value(&events[1]).expect("serializes");
        let errors = repair["errors"].as_array().expect("errors array");
        assert!(!errors.is_empty(), "the vet complaint rides the frame");
    }

    #[tokio::test]
    async fn no_listener_means_no_events_and_no_change() {
        // The non-streaming route passes None: the pipeline must behave
        // exactly as it did before the stream existed.
        let good = "//@pine_lite version=1\nplot(close)\n";
        let llm = ScriptedLlm {
            scripts: std::sync::Mutex::new(vec![good.to_string()]),
        };
        let generated = generate_script(&llm, "system".into(), "a plot", None, 4096, 0.2, None)
            .await
            .expect("one attempt");
        assert_eq!(generated.attempts, 1);
    }
}
