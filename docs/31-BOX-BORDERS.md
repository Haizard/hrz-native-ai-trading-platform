# 31 — Box borders: the TradingView zone look

## Purpose

Custom visual vocabulary, first increment (roadmap #8): TradingView's boxes
have a **border distinct from the fill** — that contrast is what makes a zone
read as a zone and not a translucent smear. Our script boxes were fill-only,
with the shell deriving a same-color edge. Both box forms now take the
border knobs:

```
box.new(left, top, right, bottom, color=color.green,
        border_color=color.lime, border_width=2, border_style="dashed")
box.new_time(t1, top, t2, bottom, color=color.teal, border_color=color.teal)
```

- `border_color=` — a `#rrggbb` literal or a `color.*` name, the same
  spellings as `color=`. **Off when unasked**: no key on the wire, and the
  shell falls back to the fill-derived edge it has always drawn, so
  pre-border scripts and pre-border scenes are unchanged.
- `border_width=` — CSS pixels, 1 when unset.
- `border_style=` — "solid" | "dashed" | "dotted", the line styles.

## Contract notes

- The wire fields are additive and optional: `border_color` is omitted when
  unset (`skip_serializing_if`), `border_width`/`border_style` deserialize
  with defaults. An old scene parses into the new struct unchanged — pinned
  by `a_box_border_rides_the_wire_only_when_set`.
- The border strokes **inside** the clipped band (the fill is plot-clipped
  first, the border strokes the clipped rect), so an "extend to now" zone's
  border stops at the price axis like the fill does.
- The lifecycle tiers (docs/25) for concept zones keep their own
  shell-computed edges; the border knobs are the script vocabulary, not a
  restyle of the tier system.

## Tests

- `pine-lite/tests/drawing_objects.rs::a_box_border_is_distinct_from_the_fill_and_optional`
  — both box forms parse the knobs; unset means `None` + defaults.
- chart-engine `script::tests::a_box_border_rides_the_wire_only_when_set` —
  the wire carries the border only when set, and the old wire shape still
  deserializes.

## What this is not

The full #8 (a plugin registry for custom primitives — user-defined glyph
renderers) stays open; it needs a trait design in the engine and a
capability story for the shell. Borders are the vocabulary item with the
highest zone-rendering value, shipped first.
