# 32 — Legend values: TradingView's "RSI 61.3" row

## Purpose

TradingView shows every plot's current value next to its name — in the pane
legend and on overlay line labels — so the chart reads as numbers, not just
shapes. Script panes showed only the pane title; overlay plots only their
title at the line's right end. Now both carry the value.

## Contract

- `SceneScriptPlot.last_value: Option<f64>` rides the wire from the engine,
  filled at both mapping sites (pane and overlay paths). The value is the
  series' **last finite** value: a series whose tail went `na` reports its
  last real value, never a hole; an all-`na` plot omits the key entirely
  (`skip_serializing_if`), and old scenes without the key deserialize to
  `None` (`serde(default)`).
- The shell formats and places it, never re-derives it: pane legends append
  ` title value` per plot in the plot's own color; overlay plot labels
  become `"RSI 61.3"` instead of `"RSI"`.

## The na nuance (why the value comes from the plot's stored series)

pine-lite reads of a bare variable identifier carry forward: a slot holding
`na` resolves to the last finite slot, so `var` handles (array references)
stay readable across bars. A *plotted expression*, though, stores its raw
per-bar result — holes included. The legend therefore reads the plot's own
`values` vector (the same series the polyline skips holes from), never a
re-read of a variable. The pinned test uses a ternary expression plot for
its trailing-`na` fixture for exactly this reason.

## Tests

- chart-engine `script::tests::a_plots_legend_value_is_its_last_finite_value`
  — trailing-`na` series reports its last finite value (polyline stops where
  the values do), an all-`na` plot omits the key on the wire, and the
  pre-legend wire shape still deserializes.
