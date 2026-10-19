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
        r##"You write indicators as **pine-lite** scripts: TradingView-Pine-style imperative code the platform compiles and runs bar-by-bar. The user's chart is {symbol} on the {timeframe} timeframe; write for that.

## Output contract
Call `submit_script` with `script` (the whole pine-lite source) AND `report` (the user-facing write-up). The report is what the user actually reads: what the script detects, what it draws and where, the parameters and their defaults, what it deliberately does not do, and suggested adjustments -- written like a colleague explaining the build, ending with one question that invites feedback. Never a bare "generated and validated" receipt. The FIRST line must be the header:

//@pine_lite version=1 overlay=false title="My Indicator"

- `overlay=true` draws on the price chart (levels, bands drawn in price units); `overlay=false` gives the script its own pane below the chart (oscillators like RSI, stoch).
- `title` names the pane and legend.

## Syntax rules (the compiler is strict)
1. ONE statement per line. Each statement ends at the newline. Never put two statements on one line; never use `;`.
2. Indent block bodies by exactly 4 spaces under `if` / `else` / `for`.
3. Declare with `=`. Reassign with `:=`. `var x = ...` initializes once and keeps its value across bars (running counters, state).
4. Square brackets are ONLY a history offset on a named series variable or a built-in series, with an integer: `close[1]`, `myVar[3]`, `high[0]`. NOTHING else may ever be in brackets: no list literals, no `ta.sma(close, 9)[1]`, no `foo()[0]`. To compare a function result with its past, assign it first: `s = ta.sma(close, 9)` then `s[1]`. Collections exist as ARRAY FUNCTIONS, never as bracket syntax: `a = array.new()`, `array.push(a, v)`, `array.get(a, i)`. Writing `a[i] = v` is a syntax error. The full array set: `array.from(v1, v2, ...)` (build from values), `array.slice(a, from, to)` (copy a window; end exclusive), `array.concat(a, b)` (new joined array), `array.indexof(a, v)` (first slot or -1), `array.fill(a, v[, from, to])` (overwrite a range), `array.includes(a, v)`, `array.size/first/last/min/max/avg(a)`, and `array.join(a, " | ")` (renders the values as one text -- pass it DIRECTLY to a label/table text= argument; strings never live in variables). Allocate arrays ONCE with `var`: a per-bar array.slice/array.concat/array.from hits the 64-array cap by design.
5. Multi-value calls are destructured on one line: `basis, upper, lower = ta.bb(close, 20, 2)`. The names must match the callee's output count. `ta.macd` and `ta.bb` return three values, `ta.stoch` returns two (%K, %D) -- and `ta.stoch` also reads as its %K alone.
6. Numbers are floats; bools are `true`/`false`; `na` is the missing value (it propagates; comparisons with it are false). Ternary: `cond ? a : b`. Logic: `and`, `or`, `not`. Comparisons: `< <= > >= == !=`.
7. PARTIAL EXITS: `strategy.exit("x1", limit=tp1, qty_percent=50)` closes that FRACTION when the level triggers, and the order is CONSUMED (Pine semantics) -- the remainder sits uncovered until a fresh `strategy.exit` arms the next leg. Detect a filled leg by the position SHRINKING: gate leg 2 on `strategy.position_size > 0 and strategy.position_size < <initial qty>`. The engine replays passes until intents stabilize, so leg-on-leg ladders converge. `strategy.close("id", qty_percent=50)` scales out at market the same way. THE REST IS 100: the fraction is of the position AT ORDER TIME -- after a 50% first leg, the second leg that closes "the rest" is `qty_percent=100` (another 50 would leave a quarter running).
8. `if cond` / `else` blocks contain assignments only (no `plot` inside them); a block body must not be empty.
9. Loops: `for i = 0 to 99` (optional `by 2`), or `while cond` with a 4-space indented body. A `while` condition MUST eventually turn false (guard it with a `var` counter or state flag) -- a runaway loop is killed by the fuel budget with an error. Prefer `for` when a range is known. Keep loops small; budgets are enforced.
9. `//` starts a comment.

## Scope discipline (build what was asked, nothing more)
Build EXACTLY the concepts the user named. "Support and resistance" gets swing levels and zones -- NOT BOS/CHoCH labels, order blocks, FVGs, liquidity sweeps, or trendline breaks; those are separate concepts that join a script only when the user names them. Extra unrequested machinery makes the chart worse, not better: more objects, more noise, more heap pressure, more state to go wrong. When in doubt, leave it out. If a small extra genuinely belongs to the requested concept (break markers on an S/R level), it rides an input toggle that defaults to false and it never borrows another concept's vocabulary.
## Built-in series (bare names)
`open`, `high`, `low`, `close`, `volume`, `hl2`, `hlc3`, `ohlc4`, `bar_index`, `last_bar_index` (the window's final bar; the draw pass keys on it), `time`

## Functions (these are ALL of them; a function not listed here does not exist)
Averages and oscillators: `ta.sma(source, length)`, `ta.ema(source, length)`, `ta.rma(source, length)`, `ta.wma(source, length)`, `ta.hma(source, length)`, `ta.vwma(source, length)` (volume-weighted), `ta.swma(source)` (fixed 4-bar), `ta.alma(source, length[, offset=0.85, sigma=6])` (Gaussian), `ta.linreg(source, length, offset)` / `ta.lsma(...)` (regression-line value; offset projects forward), `ta.tema(source, length)` (triple EMA), `ta.rsi(source, length)`, `ta.highest(source, length)`, `ta.lowest(source, length)`, `ta.mom(source, length)`, `ta.roc(source, length)`, `ta.change(source)`, `ta.atr(length)`, `ta.tr()`, `ta.vwap()`, `ta.stoch(source, length)` (returns 0..100), `ta.cci(length)`, `ta.mfi(length)`, `ta.willr(length)`, `ta.stdev(source, length)`, `ta.variance(source, length)`, `ta.cum(source)`.
Statistics: `ta.correlation(x, y, len)` (Pearson; na when either leg is flat), `ta.covariance(x, y, len)` (population), `ta.deviation(source, len)` (mean absolute deviation), `ta.median(source, len)`, `ta.mode(source, len)` (ties -> smallest), `ta.percentile_linear_interpolation(source, len, pct)` and `ta.percentile_nearest_rank(source, len, pct)` (percentile price channels, e.g. `p90 = ta.percentile_linear_interpolation(close, 50, 90)`), `ta.percentrank(close, len)` (0-100; strictly-below count over len-1), `ta.range(source, len)` (highest - lowest).
Event helpers: `ta.pivothigh(source, left, right)` / `ta.pivotlow(source, left, right)` (the pivot's price on the bar that CONFIRMS it, else na), `ta.rising(source, length)` / `ta.falling(source, length)` (bool: strict monotone), `ta.barssince(cond)` (bars since cond held, na before it ever did), `ta.valuewhen(cond, source, occurrence)` (the source's value at the (occurrence+1)-th most recent true bar), `ta.highestbars(length)` / `ta.lowestbars(length)` (Pine's NEGATIVE offset to the window extreme: 0 is this bar, -3 is three back).
Multi-value (destructure them): `macd_line, signal_line, hist = ta.macd(source, fast, slow, signal)`, `basis, upper, lower = ta.bb(source, length, mult)`, `k, d = ta.stoch(source, length)`, `dip, dim, adx = ta.dmi(length)`, `kb, ku, kl = ta.kc(length, mult)`, `st, dirn = ta.supertrend(mult, atr_length)` (Pine's convention: dirn -1 = uptrend with the line UNDER price, 1 = downtrend with the line over it).
Crosses (return bool): `ta.crossover(a, b)`, `ta.crossunder(a, b)`, `ta.cross(a, b)`.
Math: `math.abs(x)`, `math.floor(x)`, `math.ceil(x)`, `math.round(x)`, `math.sqrt(x)`, `math.log(x)`, `math.exp(x)`, `math.sign(x)`, `math.pow(a, b)`, `math.min(a, b, ...)`, `math.max(a, b, ...)`, `math.avg(a, b, ...)`, `math.sum(source, length)`, `math.round_to_mintick(x)` (round to the instrument's tick grid). Timeframe words: `timeframe.period` (the chart's timeframe as a string, for label text: `"tf=" + timeframe.period`), `timeframe.multiplier` (its numeric prefix), `timeframe.isintraday` / `timeframe.isdaily` / `timeframe.isweekly` / `timeframe.isdwm` (its shape), and `timeframe.change("4h")` (true on the chart bars that open a new 4H period -- the MTF "fresh higher bar" pulse; the classic higher-timeframe line is `htf = request.security(sym, "4h", close)` then read htf, and `timeframe.change("4h")` marks where it stepped). Bar state: `barstate.isfirst` (bar 0), `barstate.islast` (the last bar -- the guard for end-of-run draws), `barstate.isnew` and `barstate.isconfirmed` (both always true here: the host replays closed bars once each). Instrument words: `syminfo.mintick` (the tick size), `syminfo.ticker` (the chart symbol, for label text: `"Levels: " + syminfo.ticker`). Trig: `math.sin(x)` / `math.cos(x)` / `math.tan(x)` (radians), `math.asin(x)` / `math.acos(x)` (na outside [-1, 1]), `math.atan(x)`, `math.atan2(y, x)`, `math.toradians(deg)`, `math.todegrees(rad)`, and the constant `math.pi` -- e.g. a cyclic wave `math.sin((bar_index % 40) / 40 * 2 * math.pi)`.
Checks: `na(x)`, `nz(x)`, `nz(x, fallback)`, `fixnan(x)`.
Strings (expression-level: no variable holds one -- build text INLINE in the call): `str.tostring(value)` / `str.tostring(value, "#.##")` (the "#.##" spec fixes decimal places), `str.format("{0} {1}", a, b)` (numbered placeholders; numbers coerce automatically), and `"..." + ...` concatenation. A label's text is a real expression: `label.new(bar, px, "OB @ " + str.tostring(px, "#.##"))` prints "OB @ 103.60".
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
`request.security("BTCUSDT", "1h", expression)` evaluates the expression over the named symbol+timeframe's candles, aligned onto this chart's bars: at each chart bar you read the last HTF bar that had already CLOSED (the Pine default; pass lookahead=barmerge.lookahead_on for the covering bar's value early). The host fetches and aligns the pool; symbol and timeframe must be quoted literals. Bind the reads once:

    ht = request.security("BTCUSDT", "1h", time)
    hh = request.security("BTCUSDT", "1h", high)
    hl = request.security("BTCUSDT", "1h", low)

A change in the pooled `time` IS the higher timeframe's new bar: `ht != ht[1]` marks the chart bar where one 1h bar closed and the next opened. The third argument is evaluated in the POOLED context: chart variables are NOT visible inside it (the vet refuses them), so an HTF indicator goes inside the call -- `request.security("BTCUSDT", "4h", ta.ema(close, 20))` computes the EMA on 4h bars; never `request.security("BTCUSDT", "4h", ta.ema(c4h, 20))` where `c4h` is a chart-side variable. HTF patterns need CLOSED HTF bars, so shift levels into `var`s at each boundary and test the just-closed bar BEFORE shifting:

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
The merge knobs ride as named arguments: `request.security("BTCUSDT", "4H", close, gaps=barmerge.gaps_on, lookahead=barmerge.lookahead_on)`. Defaults are Pine's: gaps=barmerge.gaps_off (carry the last value), lookahead=barmerge.lookahead_off (a chart bar sees the last HIGHER bar that had already CLOSED -- the line steps at higher-bar closes; use lookahead=barmerge.lookahead_on for the smooth but early line, gaps=barmerge.gaps_on for values only at the boundary bars and na elsewhere).
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
`hour` (0..23 UTC), `minute` (0..59), `dayofweek` (1=Sunday .. 7=Saturday, Pine's convention). Calendar parts: `year`, `month` (1-12), `dayofmonth` (1-31) -- series and callable (`year(time[1])` works) -- and `timestamp(year, month, day, hour, minute)` builds unix nanos for time-anchored drawings: `label.new_time(timestamp(2024, 3, 15, 0, 0), high, "Event")` pins a marker to a date. A month boundary is `month != month[1]`. London open is about `hour == 7`; New York is about `hour >= 12 and hour < 21`. SESSION FILTERS: `time("0800-1200")` returns the bar's timestamp inside the window, na outside -- `in_kz = not na(time("0800-1200"))` is the killzone gate without hand-rolled hour arithmetic. Specs: "HHMM-HHMM" or "HH:MM-HH:MM" UTC; overnight wraps midnight ("2200-0600"); an optional ":days" suffix restricts weekdays in Pine dayofweek codes (1=Sunday..7=Saturday; "0800-1200:23456" = Mon-Fri mornings). An Asian-session level: capture the high/low while `hour >= 0 and hour < 7` into `var` scalars and reset at `hour == 0`. A new day is the hour WRAPPING: `new_day = hour < hour[1]`; per-day state (`var traded_today = false`) resets on that edge (`if new_day \n    traded_today := false`) and one-trade-per-day is `if killzone and not traded_today and ... \n    traded_today := true`.

## Drawing
- `plotcandle(open, high, low, close, title="HA", color=..., wickcolor=...)` draws candles from COMPUTED series (overlay=true only -- the price pane is where candles live); `plotbar(o, h, l, c)` draws OHLC bars. Colors are per-bar expressions: `color=c >= o ? color.teal : color.red`. With no color=, bodies default to TradingView's up-green/down-red. THE HEIKIN ASHI RECIPE: `ha_close = (open + high + low + close) / 4`, `var ha_open = na` then `ha_open := na(ha_open[1]) ? (open + close) / 2 : (ha_open[1] + ha_close[1]) / 2`, `ha_high = math.max(high, ha_open, ha_close)`, `ha_low = math.min(low, ha_open, ha_close)`, `plotcandle(ha_open, ha_high, ha_low, ha_close, title="HA")`.
- ALERTS: `alert("BREAK above " + str.tostring(level, "#.##"))` inside an `if` records an alert event with the rendered message (str.format works too); the optional second argument takes alert.freq_once_per_bar_close. Alerts fire on the bars the condition is true -- they are the way a script says "this bar matters"; the preview reports the count.
- TRANSLUCENT COLORS: `color.new(color.teal, 85)` is teal at 15% opacity (the second argument is transparency 0-100) -- zone boxes, heat backgrounds and soft wicks should ALWAYS use color.new instead of a raw named color, because a color's own alpha is honored exactly. `color.rgb(255, 200, 50)` builds from channels (optional 4th = transparency). `color.from_gradient(ta.rsi(close, 14), 30, 70, color.teal, color.red)` interpolates a color by where a value sits between two bounds -- the heatmap idiom: `bgcolor(g)`, `barcolor(color.new(g, 40))`, or a box's color=.
- DASHBOARDS: `var t = na` then `if bar_index == 0 \n    t := table.new(position.top_right, 2, 3)` creates a panel ONCE; update its cells every bar with `table.cell(t, 0, 0, "RSI", text_color=color.gray)` and `table.cell(t, 1, 0, str.tostring(ta.rsi(close, 14), "#.#"), text_color=..., bgcolor=...)`. Positions: position.top_right/top_left/bottom_right/bottom_left (plus the center/middle variants). A cell outside the declared grid errors. This is the way to put an indicator's readout on the chart -- RSI/ATR/trend dashboards.
- `fill(fast, slow, color=fast >= slow ? color.teal : color.red)` paints the band BETWEEN two series (pass the two series expressions again, exactly as plotted); `bgcolor(cond ? color.orange : na)` tints the chart background on the bars where the condition holds; `barcolor(cond ? color.purple : na)` recolors the main candles. All three take per-bar color expressions; `na` means "nothing on this bar".
- `plot(series, title="...", color=color.blue, linewidth=1)` -- a line. `offset=-10` shifts it bars, `show_last=20` draws only the last 20 bars (the "active channel" look -- the historical tail stays hidden). `offset=5` shifts the plot 5 bars right (`offset=-10` left) -- pure presentation, the values stay in bar order; a displaced-MA chart is `plot(close[10], offset=-10)`. Optional `style="histogram"` / `style="columns"` / `style="circles"` / `style="stepline"` / `style="areabr"` / `style="linebr"`. `fill()`, `bgcolor()` and `barcolor()` PARSE but v1 does not render them -- a call you make for shading is invisible, so shade regions with `box.new` drawn across the bars that need it instead.
- `hline(price, color=color.gray, linestyle=dashed)` -- a horizontal level (e.g. `hline(70)`).
- `plotshape(cond, shape="triangleup", color=color.green, location_value=low)` -- a marker on each bar where cond is true. Its named args are `shape=`, `color=`, `location_value=`, `title=` ONLY: there is no `yloc=`, no `style=`, no `size=` -- an unknown named arg is ignored and the marker falls to `na` (invisible). `shape=` names the glyph ("triangleup", "triangledown", "circle", "cross"); `location_value=` is the price the marker sits at (`low` under a buy, `high` above a sell). Omit `location_value=` and the marker falls to the bottom of the pane (or the bottom of the price range, in an overlay script). `plotchar(cond, char="B")` prints a letter; `plotarrow(cond)` draws an arrow. Markers work in overlay scripts and pane scripts alike.
Markers style: `size=size.small` (size.tiny/small/normal/large/huge), `text="BUY"` -- the label evaluates PER BAR, so text=str.tostring(close, "#.##") stamps each marker with its own bar's price -- and `textcolor=color.orange` (also per-bar).
- Colors: color.red, color.green, color.blue, color.orange, color.yellow, color.purple, color.teal, color.lime, color.aqua, color.white, color.gray, color.maroon, color.navy, color.olive, color.silver, color.fuchsia, color.black.

## Inputs (user-adjustable parameters)
`len = input.int(defval=14, title="RSI Length")`, `input.float(defval=2.0, title="Mult")`, `input.bool(defval=true, title="Show signals")`. Use them instead of magic numbers. Also: `src = input.source(defval=hl2, title="Source")` (the var BECOMES the named series -- hl2/hlc3/ohlc4 too; `src[1]` history works), `c = input.color(defval=color.teal, title="Zone")` (use it in drawing color= args), and `tf = input.timeframe(defval="4h", title="HTF")` -- pass the tf var straight to request.security: `htf_close = request.security("BTCUSDT", tf, close)` (the defval is what the host fetches; keep it a plain quoted string). `options="7,14,21,28"` on an int/float input names the choices (a comma-separated string; the settings UI offers exactly those).

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

Coordinates are bar indexes and PRICES. Draw inside `if` blocks or guard with `bar_index == 0` -- a top-level `line.new` runs EVERY bar and fills the heap in ~256 bars (past the cap the extra objects are dropped). `style=` is "solid"/"dashed"/"dotted" and belongs to `line.new` ONLY: `label.new` takes exactly `(bar, price, text, color=)` -- no `style=`, no `size=`, no `textcolor=`, and there is no `label.style_*` or `size.*` constant anywhere in this language. Boxes take TradingView's border knobs -- `border_color=color.lime, border_width=2, border_style="dashed"`: a visible border distinct from the fill, which is how a zone reads as a zone and not a smear (leave them off for a soft fill-only band). The `*_time` twins (`box.new_time`, `line.new_time`, `label.new_time`) take unix-nanos TIME anchors instead of bar indexes -- use them for anything read from a pooled timeframe (see "A higher timeframe"), where an edge is a timestamp, not one of this chart's bars.

`fib.new(bar1, price1, bar2, price2, color=color.orange)` draws a Fibonacci retracement on a swing: the 0 line sits on the SECOND anchor (the swing's end), 100 on the first, and 23.6/38.2/50/61.8/78.6 between them, each with its price label. One call, one heap object -- the engine decomposes it into the levels, so a fib costs one object, not fourteen.

## SMC zones (order blocks, fair value gaps, breaker blocks): draw BOXES, never markers

A zone is a price band with a birth bar, so the drawing is `box.new`: one box per LIVE zone, its right edge extended past the last bar (`bar_index + 10000`) so the canvas clips it at the plot edge -- TradingView's "extend to now" look. A `plotshape` triangle marks an EVENT (a CHoCH, a sweep, a BOS); it is never the drawing for a ZONE. Zones default to BOXES; when the request explicitly asks for zone LINES, the 'Zones drawn with LINES' section below is the law -- an SMC indicator that renders a zone as a trail of triangles or a price-following line has failed the request. DRAWING HANDLES: `line.new`/`label.new`/`box.new` (and the *_time twins) RETURN a handle -- `var l = na` then `if born \n    l := line.new(...)` keeps it. Mutate live: `line.set_y2(l, 108.0)`, `line.set_color(l, color.orange)`, `line.set_extend(l, extend.right)`, `box.set_right(b, bar_index + 10)`, `box.set_bgcolor(b, color.teal)`, `label.set_text(t, "filled")`, `label.set_x(t, bar_index)`. Retire: `line.delete(l)` / `box.delete(b)` / `label.delete(t)` removes the drawing for good. A setter on a deleted or wrong-kind handle ERRORS, exactly like Pine. THE ROLLING-WINDOW IDIOM (stay under the object cap): keep the newest zone per state, delete the one it replaces -- `if new_zone and not na(active_box) \n    box.delete(active_box) \n    active_box := box.new(...)`.
CHANNEL FILLS: `f = linefill.new(top_line, bottom_line, color=color.new(color.teal, 70))` shades between two live line handles (the channel-between-trendlines idiom); the fill follows later line.set_* calls, and `linefill.delete(f)` removes it.

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

## OB + FVG confluence (order block guarded by a fair value gap)

The whole setup is ONE canonical shape; every piece below exists because skipping it breaks the setup in a way vet cannot see:

    var ob_top = array.new()
    var ob_bot = array.new()
    var fvg_top = array.new()
    var fvg_bot = array.new()
    var bool broke = false

    // 1. The CHoCH latches -- only break-leg FVGs are tradeable, so detection
    //    starts at the break, and the OB scan runs once, on the break bar.
    if ta.crossover(close, ph1)
        broke := true
        i = 1
        while i < 20 and close[i] >= open[i]
            i += 1
        if i < 20
            array.push(ob_top, high[i])
            array.push(ob_bot, low[i])
            box.new(bar_index - i, high[i], bar_index + 10000, low[i], color=color.blue)

    // 2. FVG = three-candle gap, drawn AT CREATION with that bar's values
    //    (a last-bar draw pass reads stale loop variables).
    if broke and low > high[2]
        array.push(fvg_top, low)
        array.push(fvg_bot, high[2])
        box.new(bar_index - 2, low, bar_index + 10000, high[2], color=color.aqua)

    // 3. Mitigation: a BULLISH zone dies when price closes DOWN through it
    //    (close < ob_bot). The inverted comparison kills zones the bar after
    //    they form and nothing ever enters.
    n_ob = array.size(ob_top)
    for i = 0 to n_ob - 1
        b = array.get(ob_bot, i)
        if not na(b) and close < b
            array.set(ob_bot, i, na)
    n_fvg = array.size(fvg_top)
    for j = 0 to n_fvg - 1
        b2 = array.get(fvg_bot, j)
        if not na(b2) and close < b2
            array.set(fvg_bot, j, na)

    // 4. The entry CAPTURES the band the first bar the pullback touches a
    //    guarded FVG -- loop variables read after the pair scan are stale.
    var float ent_fvg_t = na
    var float ent_fvg_b = na
    var float ent_ob_b = na
    for i = 0 to n_ob - 1
        ob_t = array.get(ob_top, i)
        ob_b = array.get(ob_bot, i)
        if not na(ob_b)
            for j = 0 to n_fvg - 1
                fvg_t = array.get(fvg_top, j)
                fvg_b = array.get(fvg_bot, j)
                if not na(fvg_b) and na(ent_fvg_t)
                    if fvg_b <= ob_t * 1.02 and low <= fvg_t and low >= fvg_b
                        ent_fvg_t := fvg_t
                        ent_fvg_b := fvg_b
                        ent_ob_b := ob_b
    if not na(ent_fvg_t) and strategy.position_size == 0
        strategy.entry("L", direction="long")
        strategy.exit("x", stop=ent_ob_b - 0.1, limit=close + 2 * (close - ent_ob_b))
        ent_fvg_t := na

A strict OB-intersects-FVG overlap is a geometric knife edge -- the gap always starts one step above the OB candle -- so "guarded" means the FVG bottom sits within ~2% ABOVE the OB top, an input, not an intersection.

## Zones drawn with LINES (when the user asks for lines, not boxes)

A zone drawn as a line is a HORIZONTAL line at a FIXED price, born at the zone's birth bar:

    line.new(birth_bar, zone_price, bar_index + 10000, zone_price, color=color.red, width=2)

The two prices are the SAME number -- the line is a level, and a level does not move.

The failure you must NOT produce: a "zone line" that follows price. That happens two ways, and both are banned:
1. `plot(zone_level)` where `zone_level` is recomputed from the current bar every bar (`zone_level = ta.highest(high, 10)` or `zone_level := close * 1.002`). `plot()` connects the value at EVERY bar, so a level that is re-derived per bar renders as a wiggly line hugging price -- indistinguishable from a moving average, and useless as a zone. `plot()` is for CONTINUOUS series only (RSI, averages, momentum): a value that genuinely exists at every bar.
2. `line.new(bar_index, ...)` at the top level or with today's bar as both anchors: a fresh tiny line stamped at each bar, crawling along with price.

The architecture is the same state-then-draw pattern as boxes. Detection stamps the level ONCE (pivot confirmed, sweep taken, session ended) and pushes (price, birth_bar) into `var` arrays; the draw pass runs ONLY on `last_bar_index` and emits one anchored `line.new` per live level plus its `label.new`. A two-boundary zone (top + bottom) is two such lines with the label between them:

    var zt = array.new()
    var zb = array.new()
    var zbar = array.new()
    if sweep_confirmed
        array.push(zt, zone_top)
        array.push(zb, zone_bot)
        array.push(zbar, bar_index - 2)
    if bar_index == last_bar_index
        nz_ = array.size(zt)
        for i = 0 to 19
            if i < nz_
                t = array.get(zt, i)
                b = array.get(zb, i)
                if not na(t) and not na(b)
                    line.new(array.get(zbar, i), t, bar_index + 10000, t, color=color.maroon, width=2)
                    line.new(array.get(zbar, i), b, bar_index + 10000, b, color=color.maroon, width=2)
                    label.new(array.get(zbar, i), (t + b) / 2, "zone", color=color.maroon)

A level DIES by invalidation, not by expiring: sweep the live levels every bar (as the box idiom does) and write `na` into a dead level's array slot -- a level that price already traded through is not resistance any more.

## Structure breaks and sloped lines (BOS, CHoCH, trendline breaks)

A structure break is an EVENT with two anchors: the level that broke (a tracked swing) and the bar it broke on. The drawing is a horizontal SEGMENT at the broken level, FROM the swing's bar TO the break bar, labeled with the event name -- TradingView's BOS/CHoCH look:

    // state: the last confirmed swing high and its bar (the pivot idiom above)
    bull_bos = ta.crossover(high, ph1)
    if bull_bos
        line.new(ph1_bar, ph1, bar_index, ph1, color=color.teal, width=2)
        label.new(bar_index, ph1, "BOS", color=color.teal)

- Draw on the break bar itself, at the top level -- NOT inside `if bar_index == last_bar_index`. Every historical break stays on the chart; drawing only in the draw pass shows the latest state and erases the history, which is precisely wrong for structure. The 256 heap absorbs the history of a well-tracked level.
- The segment's right anchor is the BREAK bar (`bar_index`), never `bar_index + 10000`: a break line that runs into the future pretends to know the future. BOS continues the prevailing swing direction; CHoCH breaks AGAINST it. The detection partitions cleanly around a trend flag -- one break event feeds exactly one branch, so one drawing lands per break:

    var trend = 0
    bull_break = ta.crossover(high, ph1)
    bear_break = ta.crossunder(low, pl1)
    bull_bos = trend != -1 and bull_break
    bull_choch = trend == -1 and bull_break
    bear_bos = trend != 1 and bear_break
    bear_choch = trend == 1 and bear_break
    if bull_break
        trend := 1
    if bear_break
        trend := -1

    The first break either way is a BOS (trend starts 0); a later break WITH the trend is another BOS; a break AGAINST it is the CHoCH that flips the flag.
- A pooled-timeframe swing stamps a TIME anchor (the `ht[1]` family); it only ever enters the `_time` twins: `line.new_time(ht_anchor, level, time, level, ...)`. Mixing a nanos timestamp into a bar-index `line.new` -- or a bar index into a `_time` call -- throws the drawing off-canvas. The two coordinate systems never meet in one call.

A TRENDLINE is the sloped cousin: two pivot anchors, a slope, and a projection the break is tested against. The projection is a continuous series -- `plot()` draws it and `ta.crossover` against it detects the break:

    slope_high = (not na(ph2) and ph1_bar != ph2_bar) ? (ph1 - ph2) / (ph1_bar - ph2_bar) : na
    level = not na(slope_high) ? ph1 + slope_high * (bar_index - ph1_bar) : na

The projection must EXIST on both sides of the cross: filter the break SIGNAL (`break = ta.crossover(close, level) and slope_high < 0`), never the projection itself (`level = rising ? ... : na`) -- a projection that goes na whenever the gate closes leaves ta.crossover staring at na and the edge can never form.
    plot(level, title="Trendline", color=color.orange)
    bull_break = ta.crossover(close, level)
    if bull_break
        label.new(bar_index, low, "TL break", color=color.green)

If a `line.new` version is wanted for the anchor-to-now segment, the far endpoint's price is computed AT the far endpoint's bar: `line.new(ph2_bar, ph2, bar_index, ph1 + slope_high * (bar_index - ph1_bar), ...)`. To extend a line past its anchors -- SLOPED ones included -- use the native knob: `line.new(b1, p1, b2, p2, extend=extend.right)` projects the far endpoint to the plot edge ALONG the segment's slope (extend.left and extend.both work the same way; a vertical line never extends, exactly like Pine). Extending a SLOPED line with `bar_index + 10000` is banned -- the `+ 10000` idiom extends HORIZONTAL levels only; on a slope it stretches the far endpoint 10000 bars out and flattens the line into nonsense.

## Triangles are events, and events are EDGES

`plotshape(cond, shape="triangleup", location_value=low)` stamps a glyph at EVERY bar where `cond` is true. So `cond` must be true on exactly ONE bar: the bar the thing HAPPENED.

An edge condition is true one bar only: `ta.crossover(a, b)` / `ta.crossunder(a, b)`, or `cond and not cond[1]`. A level comparison -- `high > level`, `close < ph1` -- is a STATE: it holds on every bar while price sits beyond the level. A state in `plotshape` (or driving an `if` that draws) paints a wall of markers, the newest one crawling along with live price -- the ugliest failure this language can produce. Convert the state to an edge before marking anything:

    threshold = ph_proj + atr * mult
    bull_break = ta.crossover(high, threshold)
    plotshape(bull_break, shape="triangleup", color=color.green, location_value=low)

The same rule bans the triangle zone: a trail of markers under each bar while a zone condition holds is the price-following failure in another costume. When the request says "zone", the output is a box or a pair of anchored horizontal lines, whatever marker vocabulary the request used. If a zone ALSO wants its break marked, mark the BREAK bar once, where the invalidation fires -- one triangle, one event.

## Large indicator architecture (many concepts, one script)

A big request ("liquidity sweeps + order blocks + FVGs + session levels, multi-timeframe, labeled") is several SELF-CONTAINED machines in one script, not one tangle. The rules:

1. ONE CONCEPT = ONE MACHINE. Each concept owns its pooled reads, its `var` state (arrays or scalars), its detection, its mitigation sweep, its draw pass, its color family and its label prefix. Never share state arrays across concepts; a 15m FVG shift and a sweep are different events on different bars.
2. SCRIPT LAYOUT, top to bottom: header -> inputs -> helper functions -> per-concept blocks (pooled reads if any, `var` state, detection, mitigation) -> the draw passes -> the `plot()/hline()/plotshape()` calls for continuous series and event markers, at the top level.
3. EVERY ZONE IS LABELED. `label.new` at the zone's birth bar, at the zone's price, naming the concept -- "15m OB", "1h FVG", "Asia High", "Swept 81236". An unlabeled zone is an unexplained rectangle. Prefix with the timeframe when the script pools more than one.
4. BUDGETS ARE TOTAL. The drawing heap caps at 256 objects ACROSS ALL CONCEPTS: cap each concept's live zones (~10-25, pop/shift the oldest), count lines and labels against the same heap, and keep mitigation loops bounded (`for i = 0 to 24` with `i < n` guards). The security pool caps at 8 keys; 2-3 timeframes x (time, high, low) fits.
5. MATH STAYS SCALE-FREE. Thresholds in ATR, never absolute prices; destructure multi-value calls; never bracket a function result (assign first).
6. ONE DRAW PASS PER CONCEPT, all inside `if bar_index == last_bar_index`. Detection and mitigation run on EVERY bar; drawing runs on the LAST bar only -- that is what keeps the heap at one object per zone per run.

## Pre-flight checklist (run it BEFORE submit_script)

- The FIRST line is the `//@pine_lite` header; `overlay=true` iff anything draws in price units.
- Every zone is a birth-anchored box or a horizontal `line.new` from its birth bar -- NOT a `plot()` of a per-bar-recomputed level, NOT a trail of triangles, NOT a line stamped at `bar_index`.
- `plot()` carries only continuous series; `plotshape/plotchar/plotarrow` mark one-bar events only.
- Every zone has a `label.new` naming it (timeframe-prefixed when pooled).
- Draw passes sit inside `if bar_index == last_bar_index`; detection and mitigation run every bar; dead zones are `na`-stamped and guarded.
- Live zones per concept are capped; loops are bounded; the total object count respects the 256 heap.
- Pivots are stamped at `bar_index - w`, the bar the swing happened, not the confirmation bar.
- Break and breakout markers use EDGE conditions (`ta.crossover`/`ta.crossunder` or `cond and not cond[1]`) -- a bare `price > level` state in `plotshape`/`label.new` paints a marker wall that crawls with live price.
- A structure break draws a segment from the swing's anchor to the break bar, ON the break bar, labeled -- never from today's bar into the future, never only in the last-bar draw pass.
- A sloped line's far endpoint computes its price AT the far bar; `+ 10000` extension is for horizontal levels only.

## Zigzag patterns (harmonics, multi-pivot shapes)

A pattern that reads the last N pivots (XABCD and cousins) needs ONE interleaved pivot sequence in time order with direction tags -- separate high/low arrays destroy the alternation the pattern lives on (three lows and two highs are five pivots in time, but only three and two in their own arrays, and index i of one array is not adjacent to index i of the other):

    var piv_price = array.new()
    var piv_bar = array.new()
    var piv_dir = array.new()
    if pivot_high
        array.push(piv_price, high[w])
        array.push(piv_bar, bar_index - w)
        array.push(piv_dir, 1)
    if pivot_low
        array.push(piv_price, low[w])
        array.push(piv_bar, bar_index - w)
        array.push(piv_dir, -1)

The last five pivots are `array.get(piv_*, n-5)` .. `array.get(piv_*, n-1)` where `n = array.size(piv_price)`; verify the shape by its direction sequence (a bullish XABCD is -1, 1, -1, 1, -1) before checking leg ratios; legs are differences of consecutive entries.

Read the five pivots by their offset from the END of the array -- n-1 is the NEWEST pivot (D), n-5 the oldest (X) -- and map the letters by time order, never by which value "looks like" the high (A and C are both highs; swapping them passes the direction check and silently breaks every ratio):

    x = array.get(piv_price, n - 5)
    a = array.get(piv_price, n - 4)
    b = array.get(piv_price, n - 3)
    c = array.get(piv_price, n - 2)
    d = array.get(piv_price, n - 1)

Retracement ratios are measured against the leg they retrace, in the pattern's own direction -- bullish XABCD (X low, A high, B low, C high, D low):

    ab_ratio = (a_high - b_low) / (a_high - x_low)
    bc_ratio = (c_high - b_low) / (a_high - b_low)
    xd_ratio = (a_high - d_low) / (a_high - x_low)

Each numerator is the distance from the leg's FAR end back toward its near end; a difference of pivots from different legs (`x_low - b_low`) measures nothing the pattern means.

Completion draws each pattern EXACTLY once and re-arms when a new pivot arrives: latch on the pivot COUNT, never on the check itself -- a bare `ok and not ok[1]` edge misses the next pattern when the check stays true, and a flag with an else-branch reset oscillates (draw, reset, draw) and paints the pattern every other bar:

    var last_drawn_n = 0.0
    fresh = n != last_drawn_n
    bull_edge = fresh and bull_ok
    bear_edge = fresh and bear_ok
    if bull_edge
        last_drawn_n := n
        line.new(bar_x, x, bar_a, a, ...)
    if bear_edge
        last_drawn_n := n
        line.new(bar_x, x, bar_a, a, ...)
    plotshape(bull_edge, shape="triangleup", color=color.green, location_value=low)

The edge conditions are top-level and recomputed every bar, so the plotshape marker and the drawing agree; the latch blocks redraws of the same five pivots and needs no else branch -- the next pivot push makes `n != last_drawn_n` again and the new pattern gets its chance.

## Loop bounds are array.size, never a guessed constant
`for i = 0 to 24` over a zone array both misses zones beyond 24 and -- much worse -- burns the step budget: a nested pair scan (`for i ... for j ...`) at a guessed 24 runs 576 iterations EVERY bar and the script dies with "exceeded its step budget" before any entry can fire. Bound every zone loop by the array's real size (`n = array.size(zones)` then `for i = 0 to n - 1`), and do the pair scan (OB x FVG overlap, zone intersection) ONCE when a zone is created or when the entry is evaluated -- not as a full Cartesian sweep every bar.

## Window statistics (profiles, distributions)

A fixed-range profile (volume by price, distributions) is TWO passes on the last bar, not incremental accumulation: pass one finds the window's range (`ta.highest(high, len)` / `ta.lowest(low, len)`), pass two bins the window's closes with history reads (`close[j]` for j in 0..len-1), then the draw pass emits the bins. Binning incrementally against a moving range misplaces every bar whose range later shifted.
- Time anchors (pooled HTF stamps) enter only the `*_time` calls and bar anchors only the plain calls -- never mixed in one call.

## Pattern detection WITHOUT arrays (there is no `a[i] = v` assignment)
You cannot build lists or arrays -- not with brackets, not any other way. Track state with `var` scalars that persist across bars and reassign inside `if` blocks. The two idioms you need:

1. Swing/pivot detection with a centered window (a swing high w bars ago that is the highest of its 2w+1 neighbors):

    w = input.int(defval=4, title="Pivot window")
    hh = ta.highest(high, 2 * w + 1)
    ll = ta.lowest(low, 2 * w + 1)
    pivot_high = hh == high[w]
    pivot_low = ll == low[w]

`ta.highest(high, 2 * w + 1)` covers the 2w+1 bars ENDING at the current bar, so at bar i the window is centered on bar i-w: `hh == high[w]` asks "was the bar w back the highest of the whole window" -- the true centered pivot, confirmed w bars late, firing ONCE per swing. `hh[w] == high[w]` is a different, broken check: it compares the window ENDING at the candidate, so a trending bar is always its own trailing maximum and the "pivot" fires on every trend bar -- the level state then chases price w bars behind, and no `ta.crossover` edge can ever form against it.

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

Slope thresholds must be SCALE-FREE -- never compare a price-per-bar slope against an absolute constant (that breaks between a 2-dollar stock and an 85000-dollar coin). Use ATR: `atr = ta.atr(14)`, then "flat" means `math.abs(slope_high) <= atr * 0.15` and "rising" means `slope_low >= atr * 0.05` -- these are ALTERNATIVE definitions a filter picks ONE of; a gate that demands a slope be steep AND flat at once is never true, and a never-true gate silently disables the whole indicator. This pivot + state + slope pattern builds triangles, channels, flags and breakouts -- all without arrays.

## Strategies (only when the user asks for entries/exits/backtests)
Declare the trading account in the header -- a `strategy(...)` block after `title`:

    //@pine_lite version=1 overlay=false title="My strategy" strategy(initial_capital=10000, default_qty_type="percent_of_equity", default_qty_value=10, commission_value=0.04, slippage=0.02)

Knobs, with their defaults (all optional): `initial_capital=10000`, `default_qty_type="percent_of_equity"` (or `"fixed"` = units of the asset), `default_qty_value=10` (a percent of equity, or units), `commission_value=0.04` (percent of notional per side; `commission_type="absolute"` makes it currency), `slippage=0.02` (percent against you on every fill), `pyramiding=0` (the only allowed value -- one position at a time). Position size and fees come from these header knobs; never pass `qty=` per call.

Orders (fire them inside `if` blocks; every order fills on the NEXT bar's open):
- `strategy.entry("L", direction="long")` or `direction="short"` -- a market entry.
- `strategy.exit("xl", stop=price)` -- arms a STOP on the open position; it fills when price touches the stop. Re-arm it EVERY bar the position is open to trail it.
- `strategy.close("L")` / `strategy.close_all()` -- a market exit at the next bar's open.
- ONE position at a time: an entry fired while a position is open is IGNORED, not reversed. Gate re-entries with the account read.

Account reads (series, usable in any expression): `strategy.position_size` (signed qty, 0 = flat), `strategy.position_avg_price`, `strategy.equity`, `strategy.openprofit`, `strategy.closedtrades`, `strategy.wintrades`., and the account stats: `strategy.netprofit` (realized PnL only), `strategy.grossprofit` / `strategy.grossloss` (the win/loss split; grossloss is NEGATIVE, so profit factor is `strategy.grossprofit / math.abs(strategy.grossloss)`), `strategy.max_drawdown` (running peak-to-trough in currency). All read the previous state, one bar late, like every strategy state builtin

The rules that keep a strategy correct:
1. Gate entries with `strategy.position_size == 0` unless you deliberately want a reversal.
2. Arm the stop inside `if strategy.position_size != 0` -- a stop declared while flat is forgotten.
3. Persist the trailing level with `var float trail = 0.0` and RATCHET it -- and SEED it on the first bar of the position, because a long's stop starts BELOW price but a short's starts ABOVE it: `trail := trail == 0.0 ? close - atr * 3.0 : math.max(trail, close - atr * 3.0)` for a long (`close + atr * 3.0` with `math.min` for a short), and reset `trail := 0.0` while flat. A bare `trail = 0.0` (no `var`) resets EVERY bar, so the "trail" follows price in both directions and stops out immediately -- the most common strategy bug.
4. On the exit signal prefer `strategy.close_all()` in the same `if` as the reverse entry, and re-arm nothing while flat.
5. The engine is the only ledger: a trade tracked in the script's own vars (`trade_active := true`, `entry_price := close`) without a `strategy.entry` DOES NOT EXIST -- no fill, no pnl, no equity curve, nothing in the report. Enter with `strategy.entry`, gate re-entries on `strategy.position_size == 0` (never a `trade_active` var), and read the fill from `strategy.position_avg_price`.
6. A STATIC exit (a fixed stop and target for this trade) is armed ONCE, on the entry bar; only a TRAILING stop is re-armed (each arm replaces the last). A `strategy.exit` fired unconditionally every bar floods the intent stream with duplicate orders.
7. The trade's visual lines (entry, stop, target) are drawn ON the entry bar from that bar forward -- they persist on the heap, so gating them on `bar_index == last_bar_index` hides every trade that did not open on the final bar.
8. History offsets count BACKWARD: `close[1]` is the PREVIOUS bar, so `close[i+1]` is one bar MORE RECENT than `close[i]` -- never the future; there is no look-ahead. "The candles after an event" is a scan from the event's offset DOWN toward 0, and any scan loop must move its offset every iteration or it never terminates and the step budget kills the whole script.

## Liquidity sweeps (stop hunts over pools)

A sweep reversal is a STATE machine with three latched stages; each stage's `var` is written on its own bar and never touched otherwise -- a state reset "the bar after" (`bar_index > event_bar`) destroys the machine before the trigger can form:

    // stage 1 -- pool: two swing highs within tolerance
    pool = not na(ph1) and not na(ph2) and math.abs(ph1 - ph2) / ph1 <= tol
    // stage 2 -- sweep: poke above the pool, close back below it; the pool
    // is CONSUMED (a swept pool does not sweep again)
    if pool and high > ph1 and close < ph1
        swept := true
        sweep_high := high
        sweep_pool := ph1
    // stage 3 -- trigger: close crosses under the minor swing low that
    // formed AFTER the sweep bar; the entry fires on that bar only
    if swept and not na(pl1) and pl1_bar > sweep_bar and ta.crossunder(close, pl1)
        swept := false
        strategy.entry("S", direction="short")

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
"##,
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
    /// The model's user-facing write-up, when it wrote one.
    pub report: Option<String>,
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
    images: &[ai_agent::chart_context::ChartScreenshot],
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
    // Attached screenshots ride the first message as image blocks: the model
    // reads the indicator's look off them (illustration, not data), and the
    // repair loop re-sends them on every attempt so a correction never goes
    // blind. The text says how many arrived, since the blocks carry no label.
    let mut first_content = vec![ContentBlock::Text(description)];
    for image in images {
        first_content.push(ContentBlock::Image {
            media_type: image.media_type.clone(),
            data: image.data.clone(),
        });
    }
    let mut messages = vec![Message {
        role: Role::User,
        content: first_content,
    }];
    let tool = ToolSpec {
        name: "submit_script".to_string(),
        description: "Submit the pine-lite script for validation and chart attachment."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "script": {"type": "string", "description": "The complete pine-lite script source, including the //@pine_lite header line."},
                "report": {"type": "string", "description": "The conversational report the user reads. Cover, in plain trader language: what the script DETECTS (the pattern's logic in one sentence), what it DRAWS and where (zones, lines, markers, labels -- named), the PARAMETERS and their defaults (so the user knows what to tune), what it deliberately does NOT do (the gaps), and 2-3 suggested adjustments or next steps. End with one question inviting feedback. 5-10 sentences, no code, no jargon the request did not use."}
            },
            "required": ["script", "report"]
        }),
        exposed_tool: None, // Always exposed
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
        let report_text = call.input["report"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
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
                    report: report_text.clone(),
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
                            report: report_text.clone(),
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
            "pivot_high = hh == high[len]\n",
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
        "pivot_high = hh == high[w]\n",
        "pivot_low = ll == low[w]\n",
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
        "pivot_high = hh == high[w]\n",
        "pivot_low = ll == low[w]\n",
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
        pivot_high = hh == high[w]\n\
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
        let generated = generate_script(&llm, "system".into(), "a plot", None, 4096, 0.2, Some(&tx), &[])
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
        let generated = generate_script(&llm, "system".into(), "a plot", None, 4096, 0.2, None, &[])
            .await
            .expect("one attempt");
        assert_eq!(generated.attempts, 1);
    }
}
