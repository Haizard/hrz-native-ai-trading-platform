// ---------------------------------------------------------------------------
// The concept path: detections in, shapes out, shapes painted.
//
// This file existed inside `app.js` until the visual fidelity audit needed to
// photograph it. A tool that re-implemented the routing and the painting would
// have audited the tool, not the chart -- so the two halves move here and become
// loadable by Node under the same `ConceptShapes` name the page uses. The paint
// order, the colours, the alphas and the one-line-per-shape dispatch are the
// trader's experience of a concept, and they now have exactly one owner.
//
// The state stays with the pane (`enabledConcepts`, `conceptEvents` live in the
// chart's closure, because two charts share nothing else); this file is a pure
// function of them. Same events in, same shapes out, in the app or in a report.
// ---------------------------------------------------------------------------

(function (global, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else global.ConceptShapes = api;
})(typeof globalThis === "object" ? globalThis : this, function () {
  "use strict";

  /// The visual identity per concept family: zones are filled rectangles
  /// (origin to end), structure is dashed segments (swing to break), candle
  /// patterns are names on their own candle. Sides carry the color; the
  /// shape carries the concept -- never one line for everything.
  const CONCEPT_ZONE_IDS = new Set(["smc.fvg", "smc.ifvg", "smc.bpr", "smc.order_block", "smc.breaker_block", "sd.zone_tested", "smc.premium_discount", "smc.mitigation_block", "smc.rejection_block", "smc.equal_levels", "session.london_killzone", "session.new_york_killzone", "session.asian_range", "volume.value_area", "volume.hvn", "volume.lvn", "fib.golden_pocket", "fib.confluence", "session.previous_day_levels", "session.previous_week_levels", "session.previous_month_levels", "fib.ote", "smc.internal_range_liquidity", "smc.external_range_liquidity", "smc.judas_swing", "smc.power_of_three"]);
  const BULL = 0xcc26a69a; // teal, 0xAARRGGBB
  const BEAR = 0xccef5350; // red
  const NEUT = 0xcc90a4ae; // slate (premium/discount, sweeps)

  function conceptColor(e) {
    if (e.direction === "bullish") return BULL;
    if (e.direction === "bearish") return BEAR;
    return NEUT;
  }

  /// A packed-RGBA u32 (from a script's `color=`) to a CSS colour string.
  /// Pure formatting: the packing is the engine's, the alpha byte is its top
  /// one, and a missing alpha is opaque.
  /// The VM packs colors as 0xAARRGGBB. A packed alpha below 1.0 is the
  /// script's OWN transparency (color.new) and wins over the renderer's
  /// derived alpha; full-alpha colors keep the derived look.
  function rgba(color, alpha) {
    const packedA = ((color >>> 24) & 0xFF) / 255;
    const r = (color >>> 16) & 0xFF;
    const g = (color >>> 8) & 0xFF;
    const b = color & 0xFF;
    const a = packedA < 0.999 ? packedA : (alpha == null ? 1.0 : alpha);
    return `rgba(${r}, ${g}, ${b}, ${a.toFixed(3)})`;
  }

  /// Map the events to semantic concept shapes, filtered by the enabled ids.
  function from(events, enabled) {
    if (!enabled.size || !events.length) return [];
    const out = [];
    for (const e of events) {
      if (!enabled.has(e.concept_id)) continue;
      // The liquidity pools wear their ICT names: equal highs are buy-side
      // liquidity (the stops resting above), equal lows sell-side.
      const short = e.concept_id === "smc.equal_levels"
        ? (e.direction === "bullish" ? "BSL" : "SSL")
        : e.concept_id.split(".")[1] || e.concept_id;
      const color = conceptColor(e);
      const fromNs = e.bars && e.bars[0] ? e.bars[0] : e.event_time_ns;
      const toNs = e.event_time_ns;
      if (CONCEPT_ZONE_IDS.has(e.concept_id) && e.zone_low != null && e.zone_high != null) {
        // A zone: origin to its end -- the mitigation bar when the lifecycle
        // closed it, the newest bar while it is still open.
        const end = e.status === "forming" || e.status === "confirmed" ? toNs : toNs;
        out.push({
          type: "zone",
          from_ns: fromNs,
          to_ns: end,
          low: e.zone_low,
          high: e.zone_high,
          label: short,
          color,
        });
      } else if (e.concept_id.startsWith("candles.") || e.concept_id.startsWith("indicators.")) {
        // A pattern or an indicator signal: its name on its own candle,
        // below for bullish, above for bearish.
        const bullish = e.direction === "bullish";
        out.push({
          type: "candle_mark",
          at_ns: e.event_time_ns,
          price: bullish ? (e.zone_low ?? e.price_level ?? 0) : (e.zone_high ?? e.price_level ?? 0),
          below: bullish,
          label: short.replace(/_/g, " "),
          color,
        });
      } else if (e.price_level != null) {
        // A structure level: the segment from the swing's bar to the break's.
        out.push({
          type: "segment",
          from_ns: fromNs,
          to_ns: toNs,
          price: e.price_level,
          label: short,
          color,
        });
      }
    }
    // The scan bounds the events to the newest 200; the shapes show ALL of
    // them. A 40-shape cap hid exactly the history the backscan exists to
    // reveal -- an attached indicator must mark every detection across the
    // visible window, or there is no way to spot one that does not work.
    return out.slice(-200);
  }

  /// Concept shapes in two passes: `"under"` paints the zones and segments
  /// (they sit BEHIND the candles -- a fill over the price is a curtain,
  /// not a measurement), `"over"` paints their labels (a name under a
  /// candle is a name nobody reads). Zones paint oldest first so a newer
  /// zone's border wins where they overlap; the 0.08 wash keeps stacked
  /// fills a tint rather than mud.
  function paint(ctx, scene, phase) {
    const shapes = scene.concept_shapes || [];
    if (!shapes.length) return;
    for (const o of shapes) {
      if (phase === "under" && o.Box) {
        const b = o.Box;
        const plot = scene.plot;
        const x = Math.max(Math.min(b.x1, b.x2), plot.x);
        const right = Math.min(Math.max(b.x1, b.x2), plot.x + plot.w);
        const w = right - x;
        const y = Math.min(b.y1, b.y2);
        const h = Math.abs(b.y2 - b.y1);
        if (w <= 0 || h <= 0) continue;
        ctx.fillStyle = rgba(b.color, b.fill_alpha ?? 0.08);
        ctx.fillRect(x, y, w, h);
        ctx.strokeStyle = rgba(b.border_color != null ? b.border_color : b.color, b.border_color != null ? 1.0 : 0.8);
        ctx.lineWidth = b.border_width || 1;
        ctx.setLineDash(b.border_style === "dashed" ? [6, 4] : b.border_style === "dotted" ? [2, 3] : []);
        ctx.strokeRect(Math.round(x) + 0.5, Math.round(y) + 0.5, Math.max(1, Math.round(w) - 1), Math.max(1, Math.round(h) - 1));
        ctx.setLineDash([]);
      } else if (phase === "under" && o.Line) {
        const l = o.Line;
        ctx.strokeStyle = rgba(l.color, 0.85);
        ctx.lineWidth = l.width || 1.25;
        ctx.setLineDash(l.style === "dashed" ? [6, 4] : l.style === "dotted" ? [2, 3] : []);
        ctx.beginPath();
        ctx.moveTo(l.x1, l.y1);
        ctx.lineTo(l.x2, l.y2);
        ctx.stroke();
        ctx.setLineDash([]);
      } else if (phase === "over" && o.Label) {
        const lb = o.Label;
        ctx.font = "600 9px ui-sans-serif, system-ui";
        ctx.fillStyle = rgba(lb.color, 1.0);
        ctx.fillText(lb.text, lb.x, lb.y);
      }
    }
  }

  return { rgba, from, paint };
});
