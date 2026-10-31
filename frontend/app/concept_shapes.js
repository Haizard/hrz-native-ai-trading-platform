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
//
// The shape a concept draws is the vocabulary chapter's contract
// (`docs/superpowers/specs/concept-visual-audit/00-vocabulary.md`), one entry
// per registry concept, and the audit gate checks this table against the
// ledger: a concept cannot quietly change its drawing.
// ---------------------------------------------------------------------------

(function (global, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else global.ConceptShapes = api;
})(typeof globalThis === "object" ? globalThis : this, function () {
  "use strict";

  /// The vocabulary: every concept's one shape. Candles are covered by the
  /// prefix rule below (34 patterns and growing); the rest are listed so the
  /// gate can compare id-for-id.
  const SHAPE_MEMBERS = {
    bar_marker: ["pa.higher_highs", "volume.spike", "volume.acceptance"],
    candle_mark: ["smc.liquidity_grab", "smc.displacement", "smc.mtf_alignment"],
    segment: ["smc.liquidity_sweep", "smc.turtle_soup"],
    level_run: [
      "pa.breakout", "pa.failed_breakout", "pa.retest", "pa.round_numbers",
      "pa.level_flip", "pa.volume_confirmed_breakout",
      "session.previous_day_levels", "session.previous_week_levels",
      "session.previous_month_levels", "session.monthly_open",
      "session.opening_range_breakout", "session.daily_open", "session.weekly_open",
      "smc.bos", "smc.choch", "smc.equal_levels", "smc.inducement",
      "smc.internal_range_liquidity", "smc.external_range_liquidity", "smc.mss",
      "smc.equilibrium", "smc.consequent_encroachment",
      "volume.poor_extreme", "volume.poc",
    ],
    lifecycle_box: [
      "pa.range_contraction", "pa.level_rejection", "pa.compression_breakout",
      "sd.zone_tested", "sd.zone_compression", "sd.proximal_distal",
      "session.opening_gap",
      "smc.fvg", "smc.ifvg", "smc.bpr", "smc.order_block", "smc.breaker_block",
      "smc.premium_discount", "smc.rejection_block", "smc.mitigation_block",
      "smc.silver_bullet", "smc.unicorn", "smc.liquidity_void_fill",
      "smc.mtf_zone_alignment",
    ],
    time_box: [
      "pa.regime", "session.london_killzone", "session.new_york_killzone",
      "session.asian_range", "smc.judas_swing", "smc.power_of_three",
      "smc.model2022", "smc.market_maker_model", "indicators.hmm",
    ],
    outline: [
      "chart.double_top_bottom", "chart.head_shoulders", "chart.triangle",
      "chart.flag", "chart.wedge", "chart.triple_top_bottom", "chart.rectangle",
      "chart.cup_and_handle", "chart.pennant", "chart.trend_channel",
      "chart.diamond", "chart.inverse_cup_handle", "chart.rounding_bottom_top",
      "chart.broadening", "chart.trendline", "smc.trendline_sweep",
    ],
    legged_path: [
      "harmonics.gartley", "harmonics.bat", "harmonics.butterfly",
      "harmonics.crab", "harmonics.shark", "harmonics.cypher",
      "harmonics.abcd", "harmonics.three_drives", "harmonics.wolfe_wave",
    ],
    ladder: [
      "fib.retracement", "fib.golden_pocket", "fib.extension",
      "fib.confluence", "fib.ote",
    ],
    price_band: [
      "volume.initial_balance", "volume.single_prints", "volume.value_area",
      "volume.hvn", "volume.lvn",
    ],
    readout: [
      "sd.zone_strength", "session.daily_bias", "session.htf_candle_bias",
      "session.weekly_bias",
    ],
    time_line: [
      "econ.nfp", "econ.cpi", "econ.fomc", "econ.claims", "econ.pce", "econ.gdp",
      "fib.time_zones",
    ],
    pair_link: ["smc.smt_divergence"],
    series_line: [
      "indicators.rsi_pca", "indicators.rsi", "indicators.rsi_divergence",
      "indicators.rsi_family", "indicators.stochastic", "indicators.cci",
      "indicators.williams_r", "indicators.roc", "indicators.tsi", "indicators.hv",
      "indicators.zscore", "indicators.kalman", "indicators.changepoint",
      "indicators.entropy", "indicators.ar_forecast", "indicators.garch",
      "indicators.pairs", "indicators.adx", "indicators.keltner",
      "indicators.donchian", "indicators.supertrend", "indicators.chandelier",
      "indicators.macd_family", "indicators.ema_family", "indicators.macd",
      "indicators.ema", "indicators.sma", "indicators.bollinger_percent",
      "indicators.atr_family", "indicators.atr", "indicators.bollinger",
      "indicators.obv", "indicators.ad_line", "indicators.cmf", "indicators.mfi",
      "indicators.anchored_vwap",
    ],
    series_band: ["indicators.bayesian_drift", "indicators.ichimoku"],
    series_dots: ["indicators.psar"],
    series_histogram: ["indicators.awesome", "order_flow.delta_shift"],
  };

  const SHAPE_INDEX = new Map();
  for (const shape of Object.keys(SHAPE_MEMBERS)) {
    for (const id of SHAPE_MEMBERS[shape]) SHAPE_INDEX.set(id, shape);
  }

  /// The shape a concept draws. A candle pattern is always a bar marker,
  /// whatever the registry's length; everything else is the table's entry.
  function shapeOf(id) {
    if (id.startsWith("candles.")) return "bar_marker";
    return SHAPE_INDEX.get(id) || null;
  }

  /// A shape that claims no price may travel the price frame even when its
  /// concept reads its measurement in a pane (`indicators.hmm` shows its
  /// states as time boxes while its posterior stays in the pane).
  function claimsNoPrice(id) {
    const shape = shapeOf(id);
    return shape === "time_box" || shape === "time_line";
  }

  const BULL = 0xcc26a69a; // teal, 0xAARRGGBB
  const BEAR = 0xccef5350; // red
  const NEUT = 0xcc90a4ae; // slate (premium/discount, sweeps)

  function conceptColor(e) {
    if (e.direction === "bullish") return BULL;
    if (e.direction === "bearish") return BEAR;
    return NEUT;
  }

  /// The liquidity pools wear their ICT names: equal highs are buy-side
  /// liquidity (the stops resting above), equal lows sell-side.
  function shortName(e) {
    if (e.concept_id === "smc.equal_levels") {
      return e.direction === "bullish" ? "BSL" : "SSL";
    }
    const parts = e.concept_id.split(".");
    return (parts[1] || e.concept_id).replace(/_/g, " ");
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

  /// Event times per shape. `bars` is `[origin, event]`; an event that names
  /// no origin (`0` or equal to its own time) runs right by policy.
  function originOf(e) {
    return e.bars && e.bars[0] ? e.bars[0] : e.event_time_ns;
  }

  /// The lifecycle state a ConceptEvent's status names. A filled gap and a
  /// mitigated block are both "the zone was consumed": the box stops there.
  function stateOf(e) {
    if (e.status === "forming") return "forming";
    if (e.status === "invalidated") return "invalidated";
    if (e.status === "expired") return "mitigated";
    return "confirmed";
  }

  const STATE_RANK = { forming: 0, confirmed: 1, mitigated: 2, invalidated: 3 };

  /// The session concepts whose span is the clock, never a price: their box
  /// is full height whatever the detector's zonal reading is.
  const CLOCK_ONLY = new Set([
    "session.london_killzone", "session.new_york_killzone",
    "session.asian_range", "indicators.hmm",
  ]);

  /// Map the events to semantic concept shapes, filtered by the enabled ids.
  function from(events, enabled) {
    if (!enabled.size || !events.length) return [];
    const out = [];
    // Lifecycle boxes merge their events: a gap's creation, fill and flip are
    // one box with one right edge, not three little rectangles.
    const groups = new Map();
    // A ladder is ONE measurement: the detector attaches its rungs to the
    // first event of the concept, and the rest of that concept's events are
    // rungs of the same study -- drawing them again would draw it five times.
    const ladders = new Map();
    const rungConcepts = new Set();
    for (const e of events) {
      if (!enabled.has(e.concept_id) || shapeOf(e.concept_id) !== "ladder") continue;
      const rungs = e.evidence && e.evidence.rungs;
      if (Array.isArray(rungs) && rungs.length >= 2) rungConcepts.add(e.concept_id);
    }
    for (const e of events) {
      if (!enabled.has(e.concept_id)) continue;
      const shape = shapeOf(e.concept_id);
      if (!shape) continue;
      const short = shortName(e);
      const color = conceptColor(e);
      const origin = originOf(e);
      if (shape === "bar_marker") {
        // The glyph is the mark; the name is the panel's. Below the candle
        // for bullish patterns, above for bearish, beside for neutral.
        const below = e.direction !== "bearish";
        const price = below
          ? (e.zone_low ?? e.price_level ?? 0)
          : (e.zone_high ?? e.price_level ?? 0);
        out.push({
          type: "bar_marker",
          at_ns: e.event_time_ns,
          price,
          below,
          glyph: e.direction === "bullish" ? "▲" : e.direction === "bearish" ? "▼" : "◆",
          color,
        });
      } else if (shape === "candle_mark") {
        // A signal marks its own candle with its name -- the candle pattern
        // family's glyph treatment would hide *which* SMC read fired.
        const bullish = e.direction === "bullish";
        out.push({
          type: "candle_mark",
          at_ns: e.event_time_ns,
          price: bullish ? (e.zone_low ?? e.price_level ?? 0) : (e.zone_high ?? e.price_level ?? 0),
          below: bullish,
          label: short,
          color,
        });
      } else if (shape === "segment") {
        if (e.price_level == null) continue;
        out.push({
          type: "segment",
          from_ns: origin,
          to_ns: e.event_time_ns,
          price: e.price_level,
          label: short,
          color,
        });
      } else if (shape === "time_line") {
        // A release or a time count is a vertical rule at its instant: the
        // figure rides in the label, and neither line nor label touches a price.
        out.push({
          type: "time_line",
          at_ns: e.event_time_ns,
          label: (e.evidence && e.evidence.label) || short,
          color: NEUT,
        });
      } else if (shape === "level_run") {
        if (e.price_level == null) continue;
        out.push({
          type: "level_run",
          from_ns: origin,
          // A level with a measured end stops there; one without runs to the
          // plot's right edge -- the visible length is the meaning.
          until_ns: e.bars && e.bars[1] > origin ? e.bars[1] : 0,
          price: e.price_level,
          label: short,
          color,
        });
      } else if (shape === "lifecycle_box") {
        if (e.zone_low == null || e.zone_high == null) continue;
        const key = `${e.concept_id}|${e.zone_low.toPrecision(8)}|${e.zone_high.toPrecision(8)}`;
        let g = groups.get(key);
        if (!g) {
          g = {
            type: "lifecycle_box", from_ns: origin, to_ns: e.event_time_ns,
            low: e.zone_low, high: e.zone_high, label: short, color,
            state: "forming", rank: -1,
          };
          groups.set(key, g);
        }
        g.from_ns = Math.min(g.from_ns, origin);
        g.to_ns = Math.max(g.to_ns, e.event_time_ns);
        const state = stateOf(e);
        // The highest-precedence state wins, and its event's side paints the
        // box -- an invalidated gap flips polarity, so the flip must show.
        if (STATE_RANK[state] >= g.rank) {
          g.rank = STATE_RANK[state];
          g.state = state;
          g.color = color;
        }
      } else if (shape === "time_box") {
        const clock = CLOCK_ONLY.has(e.concept_id);
        out.push({
          type: "time_box",
          from_ns: origin,
          to_ns: e.event_time_ns,
          // A clock state claims no price; a measured state draws its extent.
          low: clock ? null : (e.zone_low ?? null),
          high: clock ? null : (e.zone_high ?? null),
          label: short,
          color,
        });
      } else if (shape === "price_band") {
        if (e.zone_low == null || e.zone_high == null) {
          // A point of control is a level, not a band: its zone-less event
          // still has a price, and a zero-height band is not drawn.
          if (e.price_level != null) {
            out.push({
              type: "level_run", from_ns: origin, until_ns: 0,
              price: e.price_level, label: short, color,
            });
          }
          continue;
        }
        out.push({
          type: "price_band",
          at_ns: e.event_time_ns,
          low: e.zone_low,
          high: e.zone_high,
          label: short,
          color,
        });
      } else if (shape === "readout") {
        // The figure and the window ride the label; the host carries the
        // geometry. A strength score is the event's magnitude; a bias is the
        // period's own direction.
        let text = short;
        if (e.concept_id === "sd.zone_strength" && e.evidence && e.evidence.magnitude != null) {
          text = `strength ${Math.round(e.evidence.magnitude)}`;
        } else if (e.direction !== "neutral") {
          text = `${short} ${e.direction}`;
        }
        out.push({
          type: "readout",
          at_ns: e.event_time_ns,
          price: e.price_level ?? null,
          text,
          color,
        });
      } else if (shape === "outline" || shape === "legged_path") {
        // The detector's own vertices when they made the wire; until they
        // do, the entry-level span is the honest fallback.
        const points = e.evidence && e.evidence.points;
        if (Array.isArray(points) && points.length >= 2) {
          // The rig that makes a path a pattern: an outline carries its
          // neckline, a harmonic its terminal PRZ band, Wolfe its 1-4
          // projection. None of these touches the pivots themselves.
          const neck = shape === "outline" && e.price_level != null ? e.price_level : null;
          let target = null;
          let bandLow = null;
          let bandHigh = null;
          if (shape === "legged_path") {
            if (e.concept_id === "harmonics.wolfe_wave") {
              target = e.zone_high ?? null;
            } else if (e.zone_low != null && e.zone_high != null) {
              bandLow = e.zone_low;
              bandHigh = e.zone_high;
            }
          }
          out.push({
            type: "polyline",
            points: points.map((p) => ({ at_ns: p[0], price: p[1] })),
            closed: false,
            label: short,
            color,
            neck,
            target,
            band_low: bandLow,
            band_high: bandHigh,
          });
        } else if (e.price_level != null) {
          out.push({
            type: "segment",
            from_ns: origin,
            to_ns: e.event_time_ns,
            price: e.price_level,
            label: short,
            color,
          });
        }
      } else if (shape === "ladder") {
        const rungs = e.evidence && e.evidence.rungs;
        if (Array.isArray(rungs) && rungs.length >= 2) {
          // One ladder per study: the rung-carrying event starts it, and the
          // concept's other events only widen its window.
          let g = ladders.get(e.concept_id);
          if (!g) {
            const anchors = e.evidence && e.evidence.anchors;
            g = {
              type: "ladder",
              from_ns: origin,
              to_ns: e.event_time_ns,
              rungs: rungs.map((r) => ({ ratio: r[0], price: r[1] })),
              label: short,
              color,
              anchors: Array.isArray(anchors) && anchors.length === 4 ? anchors : null,
              pocket_low: null,
              pocket_high: null,
            };
            ladders.set(e.concept_id, g);
          }
          g.from_ns = Math.min(g.from_ns, origin);
          g.to_ns = Math.max(g.to_ns, e.event_time_ns);
          // The pocket concept's own band is the shaded slice between its
          // two rungs -- the thing that makes it a pocket and not two lines.
          if (e.concept_id === "fib.golden_pocket" && e.zone_low != null && e.zone_high != null) {
            g.pocket_low = e.zone_low;
            g.pocket_high = e.zone_high;
          }
        } else if (rungConcepts.has(e.concept_id)) {
          // A rung-less event of a concept whose ladder is already drawn is
          // the same measurement said again, not a second study.
          continue;
        } else if (e.zone_low != null && e.zone_high != null) {
          out.push({
            type: "zone",
            from_ns: origin,
            to_ns: e.event_time_ns,
            low: e.zone_low,
            high: e.zone_high,
            label: short,
            color,
          });
        } else if (e.price_level != null) {
          out.push({
            type: "segment",
            from_ns: origin,
            to_ns: e.event_time_ns,
            price: e.price_level,
            label: short,
            color,
          });
        }
      }
      // `pair_link` and the series shapes draw elsewhere: a link needs a
      // second pane, a series its own host. They have no price-frame shape.
    }
    // A lifecycle box paints oldest first so a newer box's border wins where
    // two overlap -- the same rule the zones always had.
    for (const g of [...groups.values()].sort((a, b) => a.from_ns - b.from_ns)) {
      out.push({ ...g, rank: undefined, state: g.state });
    }
    for (const g of ladders.values()) out.push(g);
    // The scan bounds the events to the newest 200; the shapes show ALL of
    // them. A 40-shape cap hid exactly the history the backscan exists to
    // reveal -- an attached indicator must mark every detection across the
    // visible window, or there is no way to spot one that does not work.
    return out.slice(-200);
  }

  /// Concept shapes in two passes: `"under"` paints the zones, runs and paths
  /// (they sit BEHIND the candles -- a fill over the price is a curtain, not a
  /// measurement), `"over"` paints their labels (a name under a candle is a
  /// name nobody reads). Zones paint oldest first so a newer zone's border
  /// wins where they overlap; the 0.06 wash keeps stacked fills a tint.
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
      } else if (phase === "under" && o.Polyline) {
        const pl = o.Polyline;
        if (!pl.points || pl.points.length < 2) continue;
        ctx.strokeStyle = rgba(pl.line_color, 0.85);
        ctx.lineWidth = pl.width || 1.25;
        ctx.beginPath();
        ctx.moveTo(pl.points[0][0], pl.points[0][1]);
        for (const p of pl.points.slice(1)) ctx.lineTo(p[0], p[1]);
        if (pl.closed) ctx.closePath();
        if (pl.fill_color != null) {
          ctx.fillStyle = rgba(pl.fill_color, 0.10);
          ctx.fill();
        }
        ctx.stroke();
      } else if (phase === "over" && o.Label) {
        const lb = o.Label;
        ctx.font = "600 9px ui-sans-serif, system-ui";
        ctx.fillStyle = rgba(lb.color, 1.0);
        ctx.fillText(lb.text, lb.x, lb.y);
      }
    }
  }

  return { rgba, from, paint, shapeOf, claimsNoPrice, SHAPE_MEMBERS };
});
