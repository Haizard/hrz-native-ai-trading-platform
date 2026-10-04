# 28 — Time-anchored drawing objects: MTF zones on the chart

## Purpose

Cross-timeframe synthesis, lite: a script that reads a pooled higher
timeframe (`request.security("BTCUSDT", "1h", ...)`, docs/23 Phase 11) can
detect a pattern there — an order block, a fair value gap — but before this
document it could not *draw* it. Drawing objects anchored by **bar index**
(`box.new(left, top, right, bottom)`), and a 1h bar's span is not a bar
index on the 5m chart the trader is looking at. The zone's edges are
**timestamps**.

The language now has the time-anchored twins:

```
line.new_time(t1, price1, t2, price2, color=, style=, width=)
label.new_time(t, price, text=, color=)
box.new_time(t1, top, t2, bottom, color=)
```

and `request.time()` exposes the aligned `sec=` instrument's open time (the
pooled form's `time` — `request.security("SYM", "tf", time)` — already read
the pool's own candle times).

Anchors are unix-nanos timestamps riding in f64, the same convention as the
builtin `time` series; at epoch-nanos magnitude the f64 rounding is ~256ns,
invisible inside a bar slot.

## The taught idiom: the HTF fair value gap

The higher timeframe's bar boundary is **a change in its pooled time**:
`ht != ht[1]` marks the chart bar where one 1h bar closed and the next
opened. HTF patterns need *closed* HTF bars, so levels shift into `var`s at
each boundary and the just-closed bar is tested **before** the shift:

```
ht = request.security("BTCUSDT", "1h", time)
hh = request.security("BTCUSDT", "1h", high)
hl = request.security("BTCUSDT", "1h", low)
newbar = ht != ht[1]
var ph1 = na
var ph2 = na
if newbar and not na(ph2)
    if hl[1] > ph2
        box.new_time(ht[1], hl[1], ht + 10000000000000000.0, ph2, color=color.teal)
if newbar
    ph2 = ph1
    ph1 = hh[1]
```

A bullish 1h FVG: the just-closed 1h bar's low gapped above the high two
closed 1h bars back. The zone spans the gapping bar's open time
(`ht[1]`) to a far-future right edge; the canvas clips it at the plot edge —
the same "extend to now" look as `bar_index + 10000` on the bar-index path.

## Engine semantics

- Time anchors map through `Frame::x_at_nanos` — the same absolute-time
  mapping regions and user drawings use — so an HTF zone lands exactly where
  its bars sit on this chart's axis, regardless of the chart's timeframe.
- An anchor outside the visible window maps off-plot and the canvas clips,
  exactly as a bar-index object does (no culling for script objects: scripts
  run on the visible slice, docs/27).
- The wire shape is unchanged: the engine resolves times to canvas x before
  the scene crosses the ABI, so the shell's `drawScriptOverlays` needed no
  changes — the shell decides what is sent, the engine positions it, the
  shell paints.
- Same heap, same cap (256 objects, `MAX_OBJECTS`), same truncation flag.

## Multi-timeframe synthesis

The same idiom, one boundary machine per pooled timeframe, is multi-TF
synthesis (roadmap #7): each timeframe gets its own pooled reads, its own
`var` pair, its own color, and a tf-tagged label on every zone. Zones never
merge across timeframes — the overlapping bands ARE the confluence the trader
asked for, and a merged box would hide which timeframe confirmed what. The
higher timeframe draws first (under), the lower on top.

- One pooled timeframe = 3 keys (time, high, low); the 8-key pool budget
  covers the chart's TF plus two steps up comfortably (5m chart → 15m + 1h;
  1h chart → 4h + 1d). Steps go UP from the chart, never sideways or down.
- Never share boundary `var`s across timeframes: a 15m shift and a 1h shift
  happen on different chart bars.
- Pinned end-to-end:
  `two_pooled_timeframes_synthesize_onto_one_chart`
  (`pine-lite/tests/time_anchored_objects.rs`) — one zone per timeframe, each
  born at its own timeframe's bar open, each labeled with its TF.

## What this is not

- **Not a merged-zone ranker.** Synthesis is per-timeframe detection drawn
  together; a confluence SCORE (a zone's strength as a function of how many
  timeframes share its band) would need band-clustering in the engine and is
  not built.
- **Not lookahead.** The pooled read at a chart bar sees the HTF bar covering
  that moment — the aligned series the host carries forward — never the HTF
  bar's final values before it closes. The idiom's boundary vars read closed
  bars only; a script that draws from the *forming* HTF bar draws a zone that
  can still change, which is the truth of that data, not a leak.
- **Not a new wire contract.** `ScriptDraw` already carried canvas
  coordinates; time anchoring is resolved engine-side.

## Tests

- `pine-lite/tests/time_anchored_objects.rs` — vet/arity, `request.time()`
  alignment, and the HTF FVG idiom end-to-end over a hand-built aligned pool
  (one box, born at the gapping bar's open time, at `[B0.high, B2.low]`).
- chart-engine `time_anchored_objects_map_by_absolute_time_not_bar_index` —
  the mapping pins x to the bar-slot of the anchor's timestamp, and a
  far-future edge maps off-plot.
- api-gateway `the_taught_mtf_idiom_vets_runs_and_draws_the_gap` +
  `the_prompt_teaches_the_mtf_section_and_time_anchored_drawing` — the
  generation prompt teaches exactly what the VM runs.
