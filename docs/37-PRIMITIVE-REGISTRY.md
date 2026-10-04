# 37 — The primitive registry: positioned plugins that decompose

## Purpose

#8-full. Custom visual primitives — drawings richer than a bare line, box,
or label — needed an architecture, not another match arm. The one chosen:

**a primitive is a named, parameterized drawing whose positioning lives in
the engine, and whose output is the wire geometry the shell already
paints.** The shell never learns what the primitive *is*; a Fibonacci
retracement arrives as seven `ScriptDraw::Line`s and seven
`ScriptDraw::Label`s, drawn by painters that have existed since the first
script drew a line. Box borders (docs/31) were the lite slice — one knob on
an existing shape. This is the full shape: one registration point, zero
shell coupling per primitive.

## The architecture

- **The VM stays dumb.** `fib.new(bar1, price1, bar2, price2, color=...)`
  lands one raw `ScriptObject::Fib` on the drawing heap — the same heap,
  the same 256-object cap, the same arity vetting as every other shape. The
  VM knows the anchors; it knows nothing about ratios.
- **The registry owns the decomposition.** `chart-engine::primitives` holds
  `PRIMITIVES`: rows of `{ name, position }`. The position function gets a
  `Placement` — the frame plus the slot width, i.e. exactly the two mappings
  the built-in shapes use — so a primitive's bar index and a built-in line's
  bar index mean the same pixel by construction.
- **The shell gains nothing.** `script.rs` maps objects with `flat_map`:
  built-ins keep their one-to-one shape as a one-element Vec; a registry
  primitive expands into many parts. Every part is a `ScriptDraw` variant
  the shell painted yesterday.

Adding a primitive is one registry row plus one position function — a
plugin in the honest sense. What this registry is **not**: dynamically
loaded (wasm host plugins are a different security story), and not a shell
painter extension point (the shell stays primitive-agnostic on purpose).

## The first primitive: the Fibonacci retracement

`fib.new(start_bar, start_price, end_bar, end_price, color=...)`. Seven
ratios — 0, 23.6, 38.2, 50, 61.8, 78.6, 100 — as horizontal segments across
the anchors' span, each with its price label at the right edge. The 0 line
sits **on the second anchor** (the swing's end), the 100 line on the first:
retracement reads backwards from the end. Boundaries are solid, levels
dashed — the TradingView reading. One call, one heap object, fourteen
painted parts.

## Tests

- `primitives::tests::a_fib_decomposes_into_seven_positioned_levels_and_labels`
  — hand-checkable fixture frame; level prices at the right ratios, span
  between the anchors' slots either order, solid/dashed split, label text
  with ratio and price.
- `primitives::tests::a_fib_with_a_hole_decomposes_to_nothing` — non-finite
  anchors draw nothing; an unknown registry name positions as nothing,
  never a panic.
- `drawing_objects::a_fib_is_one_object_with_four_anchors` and
  `a_fib_respects_the_heap_cap_and_arity` — the VM side: one object, the
  shared cap, arity vetting.

## What this is not

Not yet: time-anchored primitives (`fib.new_time` for HTF swings — the
decomposition exists, the VM variant is a small follow-up), user-facing
drawing-tool fibs (the drawing toolbar is `drawing.rs`'s registry, a
deliberately separate vocabulary), and dynamically loaded plugins (a
capability story, not a table row).
