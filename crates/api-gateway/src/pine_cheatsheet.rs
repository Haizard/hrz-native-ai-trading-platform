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
pub(crate) fn language_reference() -> &'static str {
    crate::pine_codegen::language_reference()
}

/// The repair-loop sheet: the same language reference the generation
/// prompt teaches, framed for "fix these errors against the accepted
/// constructs". One source of truth -- the two loops cannot teach
/// different languages.
pub(crate) fn sheet() -> String {
    let mut out = String::from(
        "PINE-LITE CHEATSHEET -- the compiler accepts exactly what is listed here; anything else is refused.\n\n",
    );
    out.push_str(language_reference());
    out
}

/// One vet failure already shaped for the transcript (`kind`/`line`/`col`/
/// `message`), plus the sheet and the resubmit instruction. This is the whole
/// repair-round tool result: errors say what broke, the sheet says what to
/// write instead, the instruction says what to do.
pub(crate) fn repair_content(errors: Vec<serde_json::Value>) -> serde_json::Value {
    json!({
        "valid": false,
        "errors": errors,
        "cheatsheet": sheet(),
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
    // ---- the tier surface: every family the reference now names ----
    "st, dirn = ta.supertrend(3.0, 10)\n",
    "dip, dim, adx = ta.dmi(14, 14)\n",
    "kc_b, kc_u, kc_l = ta.kc(20, 1.5)\n",
    "trend_sw = switch\n",
    "    basis > upper => 2.0\n",
    "    basis > close => 1.0\n",
    "    => -1.0\n",
    "string state = trend_sw > 0.0 ? \"UP\" : \"DOWN\"\n",
    "state_prev = state[1]\n",
    "string c_typed = color.new(color.teal, 70)\n",
    "heat = color.from_gradient(ta.rsi(close, 14), 30, 70, color.teal, color.red)\n",
    "corr = ta.correlation(close, volume, 20)\n",
    "pr = ta.percentrank(close, 50)\n",
    "vw = ta.vwma(close, 20)\n",
    "te = ta.tema(close, 9)\n",
    "wave = math.sin((bar_index % 40) / 40 * 2 * math.pi)\n",
    "obv = ta.obv()\n",
    "in_kz = not na(time(\"0800-1200:23456\"))\n",
    "yr = year + month + dayofmonth + timestamp(2024, 1, 15, 0, 0) * 0\n",
    "tick = syminfo.mintick + syminfo.pointvalue\n",
    "new4h = timeframe.change(\"2h\")\n",
    "htf_knob = request.security(\"BTCUSDT\", \"4H\", close, gaps=barmerge.gaps_off, lookahead=barmerge.lookahead_off)\n",
    "ltf_slice = request.security_lower_tf(\"BTCUSDT\", \"15m\", close)\n",
    "ltf_n = array.size(ltf_slice)\n",
    "var m_state = map.new()\n",
    "var g_state = matrix.new(2, 2, 0.0)\n",
    "if barstate.islast\n",
    "    map.put(m_state, \"last\", close)\n",
    "    matrix.set(g_state, 0, 0, close)\n",
    "var tags = array.new_string()\n",
    "if state != state_prev and bar_index > 0\n",
    "    array.push(tags, state)\n",
    "if barstate.islast\n",
    "    label.new(bar_index, high, text=\"log: \" + array.join(tags, \" | \"))\n",
    "var t_dash = na\n",
    "if bar_index == 0\n",
    "    t_dash := table.new(position.top_right, 2, 1)\n",
    "if barstate.islast\n",
    "    table.cell(t_dash, 0, 0, \"STATE\")\n",
    "    table.cell(t_dash, 1, 0, state, text_color=color.teal)\n",
    "ha_c = (open + high + low + close) / 4\n",
    "plotcandle(ha_c, ha_c, ha_c, ha_c, title=\"HA\")\n",
    "plotbar(open, high, low, close)\n",
    "plot(close, title=\"Displaced\", offset=-10, show_last=50)\n",
    "fill(basis, upper, color=basis >= upper ? color.teal : color.red)\n",
    "bgcolor(in_kz ? color.orange : na)\n",
    "barcolor(close > open ? color.purple : na)\n",
    "plotshape(long_sig, shape=\"diamond\", color=color.teal, location_value=low, size=size.small, text=\"B\", textcolor=color.teal)\n",
    "if long_sig and bar_index > 5\n",
    "    alert(\"A+ long at \" + str.tostring(close, \"#.##\"), alert.freq_once_per_bar_close)\n",
    "var pts = array.new()\n",
    "if not na(ta.pivothigh(high, 3, 3))\n",
    "    array.push(pts, chart.point.new(bar_index - 3, high[3]))\n",
    "if barstate.islast and array.size(pts) >= 2\n",
    "    polyline.new(pts, line_color=color.orange)\n",
    "if long_sig\n",
    "    ln = line.new(bar_index - 2, zone_top, bar_index, zone_top, extend=extend.right)\n",
    "    lf_box = box.new(bar_index - 2, zone_top, bar_index, zone_top - rng)\n",
    "    linefill.new(ln, ln, color=color.new(color.teal, 85))\n",
    "    line.set_color(ln, color.orange)\n",
    "    box.set_right(lf_box, bar_index + 10)\n",
    "strategy.risk.max_position_size(2)\n",
    "if strategy.position_size > 0\n",
    "    strategy.exit(\"xt\", trail_points=300, trail_offset=150, qty_percent=50)\n",
    "pf = strategy.grossprofit / math.abs(strategy.grossloss)\n",
    "dd_stat = strategy.max_drawdown + strategy.netprofit * 0\n",
    "src_in = input.source(defval=hl2)\n",
    "tf_in = input.timeframe(defval=\"4h\")\n",
    "sym_in = input.symbol(defval=\"ETHUSDT\")\n",
    "col_in = input.color(defval=color.teal)\n",
    "t_in = input.time(defval=1700000000000000000)\n",
    "p_in = input.price(defval=100.0)\n",
    "sec_var = request.security(sym_in, tf_in, close)\n",
    "type Level\n",
    "    float price\n",
    "    int touches\n",
    "method touch(Level self, float px) =>\n",
    "    self.touches := self.touches + 1\n",
    "var Level lv = Level.new(0.0, 0)\n",
    "if long_sig\n",
    "    lv.price := close\n",
    "    lv.touch(close)\n",
    "plot(lv.price, title=\"Level\")\n",
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
            "var float",
            "array.set(a, i, v)",
            "cap 256 per script",
            ":=",
            "request.security",
            "request.data",
            "style=\"linebr\"",
            "TRAILING STOPS",
            "trail_points",
            "qty_percent",
            "table.new",
            "string state",
            "switch picks",
        ] {
            assert!(sheet().contains(phrase), "sheet lacks: {phrase}");
        }
    }

    #[test]
    fn the_sheet_object_cap_matches_the_interpreter() {
        // The heap cap is a real constant; the sheet's number must track it.
        assert!(
            sheet().contains(&format!("cap {} per script", pine_lite::interp::MAX_OBJECTS)),
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
