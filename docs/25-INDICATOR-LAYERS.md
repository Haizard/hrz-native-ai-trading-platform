# 25 — Indicator layers and zone lifecycle tiers

## Purpose

Amend the chart's attachment model (`docs/23` phase 9) from *one indicator per
chart* to a **layer stack**, and extend zone rendering from two paint
treatments to three **lifecycle tiers**. Both changes are shell-side: the
engine still draws exactly what it is sent, and all positioning stays in the
Rust scene.

## The layer stack

A chart pane holds:

- **One concept layer** — the attached indicator document (concepts/preview),
  live re-detection included, exactly as before. It keeps its teal chip.
- **Any number of script layers** — vetted pine-lite sources (`docs/23`) that
  run on the pane's candles every frame. Attaching used to *replace* the
  chart's attachments; it now *adds a layer*.

Layer rules:

1. **Compose, don't replace.** Saving a script from the studio, auto-attaching
   a workspace's active revision, and the revision picker's attach action all
   add (or update) a layer. Nothing is silently cleared.
2. **Re-attach is update.** Attaching the *same source* again replaces that
   layer in place, so save-and-run stays one gesture instead of stacking a
   duplicate per save.
3. **The eye hides, the × removes.** Each layer chip carries an eye toggle and
   a remove button. Hiding sets `visible: false` and the layer is **withheld
   from the scene request** — the same rule the AI drawing layer follows
   (`docs/21` phase 3): the shell decides what is sent, the engine draws what
   it is sent. The layer list keeps the source, so showing the layer again is
   a request change, not a re-attach.
4. **Chips are derived, never stored.** `syncIndicatorChip` rebuilds the layer
   chips from the pane's layer list on every attachment change. The DOM holds
   no layer state of its own, so the chips and the request cannot disagree.

The concept chip's × removes only the concept layer; script layers answer to
their own chips. One control removing a different layer's output was the old
all-or-nothing model.

## Zone lifecycle tiers

Zones carry a `ZoneState` (`created`, `active`, `tapped`, `mitigated`,
`invalidated`). The shell previously rendered two treatments — full or ghost.
It now renders three:

| State | Treatment | Meaning |
|---|---|---|
| `created` / `active` | Full: 0.28→0.10 gradient, 0.85 solid border, label pill | A fresh, untouched level |
| `tapped` | Half: 0.16→0.05 gradient, 0.50 solid border, label pill | Price entered but has not consumed the zone — the SMC "first touch" look |
| `mitigated` / `invalidated` | Ghost: 0.06→0.02 gradient, 0.35 dashed border, no pill | History, not a live level |

The tier is paint only: the engine computes the state, the shell reads the
string. The wire strings are pinned by a chart-engine test
(`zone_state_wire_strings_are_the_shells_lifecycle_tiers`) because a rename
compiles everywhere and silently turns the tiers off.

## Layer settings: the declared inputs, as a form

A pine-lite script declares its knobs with `input.int` / `input.float` /
`input.bool` / ... (`docs/23`). The vet pipeline already records those
declarations "for the settings UI"; this is that UI.

- **Declarations travel with the revision.** Both revision-creation paths
  (generated and hand-submitted) store the declarations in the revision's
  validation record, and the vet route returns them with every check. The
  defaults are JSON-typed the way the form sends values back: a number for
  `int`/`float`, a bool for `bool`, a string for `string`, and a `#rrggbb`
  string for a `color` literal (the packed value's alpha byte comes off).
- **The ⚙ on a layer chip opens the form.** One popover per pane, rebuilt
  from the layer's state on every open; fields apply on `change`, Reset
  empties the layer's values so the declared defaults stand again.
- **Values are typed end to end.** The scene request's `inputs` map is
  JSON-typed, and the engine sorts each scalar into the VM's typed input maps.
  A value whose kind disagrees with the declaration is not coerced — it lands
  in a map the declaration never reads, and the default applies. Re-attaching
  the same source (the editor's save-and-run) preserves the tuned values.
- **String and color inputs render as "edited in code".** They typecheck, but
  the VM has no string evaluator yet, so the form declines to offer a control
  that would do nothing.

## Non-goals

- No per-layer opacity sliders or blend modes (the eye is binary on purpose).
- No cross-layer pattern logic yet ("where layer A aligns with layer B") —
  that is a scene-level feature, not a shell one.
- No engine changes: the scene already accepted a list of scripts; the layers
  feature is the shell finally treating them as a list.
