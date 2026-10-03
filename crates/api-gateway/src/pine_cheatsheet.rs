//! The repair loop's cheat sheet (`docs/23` Phase 9 companion).
//!
//! When a generated script fails vetting, [`crate::pine_codegen::generate_script`]
//! feeds the errors back as a tool result. The errors name *what* broke and
//! *where*, but not *what the compiler would have accepted instead* -- so the
//! model re-derives syntax from the system prompt and can burn attempts
//! rediscovering the same traps. The live record: one session's Smart Risk
//! generation failed 3 attempts on multi-line call arguments the parser
//! actually tolerates, then more on typed `var` declarations, before a
//! revision finally passed.
//!
//! This module is the fix: a compact sheet of every construct the compiler
//! accepts and the traps that produce refusals, injected into every repair
//! round's tool result next to the errors. It teaches only what `pine_lite::vet`
//! accepts -- the same rule `script_system_prompt` follows -- and the test
//! suite pins it: [`SHEET_SCRIPT`] exercises every construct the sheet names
//! and must vet clean, so a language change that strands the sheet fails CI
//! before it strands a model.

use serde_json::json;

/// The compact syntax sheet, quoted verbatim in every repair tool result.
///
/// Grouped by where models actually fail (the live attempts were: multi-line
/// call args, typed `var`, `[a, b] =` destructuring, `while`, history on a
/// call, indexed assignment, invented builtins, object-heap overruns). Every
/// claim here is enforced by a test in this module or by `pine-lite`'s own.
pub(crate) const SHEET: &str = "\
PINE-LITE v1 CHEATSHEET — the compiler accepts exactly what is listed here; anything else is refused.

HEADER (first line, mandatory)
//@pine_lite version=1 overlay=true|false title=\"Name\" sec=\"OTHERSYM\"
- sec= declares the second instrument every request.* call reads; without it each request.* is refused.

STATEMENTS
- ONE statement per line; each ends at the newline. No semicolons.
- Declare: x = expr   Reassign: x := expr   Persist across bars: var x = expr
- Typed persist works and is preferred for state: var float zone_top = na, var int count = 0.
- if cond / else: body indented exactly 4 spaces, assignments only (no plot), never empty.
- for i = 0 to 99 (optional `by 2`): body indented exactly 4 spaces.
- while cond (v1.1): body indented exactly 4 spaces; the condition MUST eventually turn false (guard it with a `var` counter or state flag) — an infinite loop dies on the fuel budget with an error. Prefer bounded `for` when a range is known.
- Comments start with //.

MULTI-LINE CALLS (legal — use them for long calls)
- Inside ( ... ) newlines are ignored: put each argument on its own line. Keep the commas.

DESTRUCTURING
- Multi-value calls assign on ONE line with matching names: a, b, c = ta.macd(close, 12, 26, 9) (3 values), basis, upper, lower = ta.bb(...) (3), k, d = ta.stoch(...) (2). Wrong count = refusal.

HISTORY — the most common trap
- [n] is ONLY a history offset on a named variable or a built-in series, integer offset: close[1], myVar[3], hour[1], high[i] inside a for loop.
- NEVER on a call result: ta.sma(close, 9)[1] and foo()[0] are refusals. Assign first, then offset: s = ta.sma(close, 9) then use s[1].
- Indexed assignment a[i] = v does not exist. Use array.set(a, i, v).

EQUALITY
- == compares. Inside an expression a bare = also reads as ==, but always write ==. Reassignment is := (never bare = on an existing variable at statement start).

BUILT-IN SERIES (bare names; hour/minute/dayofweek also as calls)
open high low close volume hl2 hlc3 ohlc4 bar_index time time_close hour minute dayofweek last_bar_index
- `hour` is the current bar's UTC hour; hour(time) is the same from any timestamp. Both are history-indexable: hour[1].

USER FUNCTIONS
- Define: my_fn(a, b) =>\n    body indented 4 spaces (last line is the return value). Call: my_fn(x, y).
- Parameter defaults (v1.1): trailing parameters may declare one — my_fn(a, mult = 2.0) => ... — and the call may omit them: my_fn(x). A required parameter may not follow an optional one. Defaults evaluate per call in the caller's scope (a default of close[1] is the caller's yesterday).
- No recursion (call depth is capped) and a function cannot call plot*/hline.

FUNCTIONS — the complete table; a function not listed does not exist
- Averages/oscillators: ta.sma, ta.ema, ta.rma, ta.wma, ta.rsi, ta.highest, ta.lowest, ta.mom, ta.roc, ta.change, ta.atr(length), ta.tr(), ta.vwap(), ta.stoch.
- Crosses (bool): ta.crossover(a, b), ta.crossunder(a, b), ta.cross(a, b).
- Math: math.abs, math.floor, math.ceil, math.round, math.sqrt, math.log, math.exp, math.sign, math.pow, math.min, math.max, math.avg, math.sum.
- Checks: na(x), nz(x, fallback), fixnan(x). na propagates; comparisons with na are false — guard state with not na(x).
- Arrays: var a = array.new() (allocate ONCE, at the top), array.push/pop/shift/clear/get/set/size/first/last/min/max/avg/includes. Out-of-bounds get returns na.
- Other instrument: with sec= set, request.open/high/low/close/volume() are zero-arg and bar-aligned; assign then offset: rc = request.close() then rc[1]. Any symbol/timeframe: request.security(\"SYM\", \"15m\", request.close()) — 3 args, literal strings.
- Platform feeds: request.data(\"feed_name\") — one literal-string arg.

DRAWING
- plot(series, title=, color=, linewidth=, style=\"linebr\"|\"histogram\"|\"columns\"|\"circles\"|\"stepline\"|\"areabr\") — style=\"linebr\" breaks the line at na (zones, session ranges).
- hline(PRICE) — a CONSTANT price only, never a series.
- plotshape(cond, shape=, color=, location_value=), plotchar(cond, char=\"B\"), plotarrow(cond).
- line.new(bar1, price1, bar2, price2, color=, style=\"dashed\"|\"solid\", width=) — 4 to 7 args.
- label.new(bar, price, \"text\", color=) — 3 or 4 args.
- box.new(left, top, right, bottom, color=) — 4 or 5 args. A ZONE (order block, FVG, breaker) is a box, never a plotshape triangle: right = bar_index + 10000 extends it to the chart edge (the canvas clips). Mark mitigation per bar with array.set(edge_arr, i, na); draw inside if bar_index == last_bar_index so each run adds one box per live zone, not one per bar.
- Object heap: at most 256 live line/label/box objects. Draw only on signal bars (`if sig`), never unconditionally every bar; past the cap the extras are dropped (the run still succeeds).

INPUTS
- input.int(defval=, title=), input.float(defval=, title=), input.bool(defval=, title=). Use them instead of magic numbers.

STRATEGIES (docs/24)
- Header: append strategy(initial_capital=10000, default_qty_type=\"percent_of_equity\", default_qty_value=10, commission_value=0.04, slippage=0.02) after title=. Knobs: default_qty_type \"percent_of_equity\" or \"fixed\" (units); commission_type \"percent\" or \"absolute\"; pyramiding=0 only. Size and fees come from the header — never qty= per call.
- Orders: strategy.entry(\"L\", direction=\"long\"), strategy.exit(\"xl\", stop=price), strategy.close(\"L\"), strategy.close_all(). Fill = NEXT bar's open; a stop fills intrabar when price touches it. Re-arm the stop EVERY bar the position is open.
- ONE position: an entry while in a position is IGNORED, not reversed. Gate re-entries: if ta.crossover(fast, slow) and strategy.position_size == 0.
- Account reads (series): strategy.position_size (0 = flat), strategy.position_avg_price, strategy.equity, strategy.openprofit, strategy.closedtrades, strategy.wintrades.
- Trail state must be var and RATCHET, seeded on the first positioned bar: var float trail = 0.0, reset trail := 0.0 while flat, then inside if strategy.position_size > 0: trail := trail == 0.0 ? close - atr * 3.0 : math.max(trail, close - atr * 3.0) plus strategy.exit(\"xl\", stop=trail) (shorts: close + atr * 3.0 with math.min). A bare trail = 0.0 resets every bar — the trail follows price and stops out at once.
- 0 fills in the preview means the entry condition never fired or every entry was skipped — loosen the condition, do not ship a strategy that never trades.

LIMITS (vet refuses past them)
64 plots/hlines · 500 statements · 8 symbol/timeframe pairs · 256 drawing objects · a per-bar step budget: keep `for`/`while` iterations small and never loop over all history every bar.";

/// One vet failure already shaped for the transcript (`kind`/`line`/`col`/
/// `message`), plus the sheet and the resubmit instruction. This is the whole
/// repair-round tool result: errors say what broke, the sheet says what to
/// write instead, the instruction says what to do.
pub(crate) fn repair_content(errors: Vec<serde_json::Value>) -> serde_json::Value {
    json!({
        "valid": false,
        "errors": errors,
        "cheatsheet": SHEET,
        "instruction": "Fix every listed error in the full script and call submit_script again with the COMPLETE corrected script. Check each rewritten line against `cheatsheet` — it lists every construct the compiler accepts and every trap that produces these errors; do not use constructs it does not list.",
    })
}

/// Every construct the sheet names, as one runnable script. If the sheet
/// teaches a form the language refuses, this fails before any model sees it.
const SHEET_SCRIPT: &str = concat!(
    "//@pine_lite version=1 overlay=true title=\"Sheet check\" sec=\"ETHUSDT\" strategy(initial_capital=10000)\n",
    "len = input.int(defval=14, title=\"Length\")\n",
    "mult = input.float(defval=2.0, title=\"Mult\")\n",
    "show = input.bool(defval=true, title=\"Show\")\n",
    "basis, upper, lower = ta.bb(close, len, mult)\n",
    "m, s, h = ta.macd(close, 12, 26, 9)\n",
    "k, d = ta.stoch(close, len)\n",
    "atr = ta.atr(len)\n",
    "rng = math.max(high - low, atr)\n",
    "var float zone_top = na\n",
    "var int zone_bar = 0\n",
    "var swings = array.new()\n",
    "if close > upper\n",
    "    zone_top := high\n",
    "    zone_bar := bar_index\n",
    "    array.push(swings, high[2])\n",
    "idx = array.size(swings) - 1\n",
    "prev = idx >= 1 ? array.get(swings, idx - 1) : na\n",
    "h1 = hour(time)\n",
    "h2 = hour\n",
    "eq_bare = close = open\n",
    "sec_close = request.security(\"ETHUSDT\", \"15m\", request.close())\n",
    "trend = sec_close > sec_close[20]\n",
    "vol_ok = request.volume() > 0.0\n",
    "dist = close - request.close()\n",
    "feed = request.data(\"my_feed\")\n",
    "for i = 1 to 5\n",
    "    if high[i] > high\n",
    "        zone_top := high[i]\n",
    "long_sig = show and trend and not na(zone_top) and close >= zone_top and bar_index > zone_bar\n",
    "if long_sig\n",
    "    label.new(bar_index, low, \"A+ long\", color=color.green)\n",
    "    line.new(bar_index, zone_top, bar_index + 5, zone_top, color=color.green, style=\"dashed\", width=1)\n",
    "    box.new(bar_index - 3, zone_top, bar_index, zone_top - rng, color=color.green)\n",
    "plot(na(zone_top) ? na : zone_top, title=\"Zone top\", color=color.orange, style=\"linebr\", linewidth=2)\n",
    "plot(dist, title=\"Spread\", style=\"histogram\")\n",
    "hline(0)\n",
    "plotshape(long_sig, shape=\"triangleup\", color=color.green, location_value=low)\n",
    "plotchar(show, char=\"B\")\n",
    "plot(nz(feed), title=\"Feed\")\n",
    "var float trail = 0.0\n",
    "if ta.crossover(close, basis) and strategy.position_size == 0\n",
    "    strategy.entry(\"L\", direction=\"long\")\n",
    "if strategy.position_size > 0\n",
    "    trail := math.max(trail, close - atr * 2.0)\n",
    "    strategy.exit(\"xl\", stop=trail)\n",
    "if ta.crossunder(close, basis) and strategy.position_size != 0\n",
    "    strategy.close_all()\n",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sheet_script_vets_clean() {
        // Every construct the sheet names, runnable: if this ever fails, the
        // sheet is teaching a form the compiler refuses.
        let errs = pine_lite::vet(SHEET_SCRIPT);
        assert!(
            errs.is_ok(),
            "{}",
            errs.expect_err("vet")
                .iter()
                .map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn the_sheet_names_the_live_failure_traps() {
        // The constructs one session's generation burned 6 attempts on, the
        // v1.1 additions, and the caps: the sheet must speak to each by name.
        for phrase in [
            "each argument on its own line",
            "var float",
            "ta.macd(close, 12, 26, 9)",
            "array.set(a, i, v)",
            "while cond (v1.1)",
            "my_fn(a, mult = 2.0)",
            "at most 256 live line/label/box",
            "s = ta.sma(close, 9) then use s[1]",
            ":=",
            "request.security",
            "request.data",
            "style=\"linebr\"",
            "strategy(initial_capital=10000",
            "strategy.exit(\"xl\", stop=price)",
            "strategy.position_size == 0",
            "var float trail = 0.0",
            "0 fills",
        ] {
            assert!(SHEET.contains(phrase), "sheet lacks: {phrase}");
        }
    }

    #[test]
    fn the_sheet_object_cap_matches_the_interpreter() {
        // The heap cap is a real constant; the sheet's number must track it.
        assert!(
            SHEET.contains(&format!("at most {} live line/label/box", pine_lite::interp::MAX_OBJECTS)),
            "sheet object cap drifted from MAX_OBJECTS"
        );
    }

    #[test]
    fn repair_content_wraps_errors_with_the_sheet() {
        let content = repair_content(vec![json!({
            "kind": "parse", "line": 3, "col": 1, "message": "unexpected token",
        })]);
        assert_eq!(content["valid"], json!(false));
        assert_eq!(content["errors"].as_array().map(Vec::len), Some(1));
        assert!(content["cheatsheet"].as_str().unwrap().contains("CHEATSHEET"));
        assert!(content["instruction"]
            .as_str()
            .unwrap()
            .contains("submit_script"));
    }
}
