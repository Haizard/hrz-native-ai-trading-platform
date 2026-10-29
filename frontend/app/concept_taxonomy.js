/*
 * The concept taxonomy: which family a concept id belongs to.
 *
 * The chart's Indicators face groups the library by these rows, and the visual
 * fidelity audit shards its ledger by them, so the two must not hold separate
 * tables -- a concept the panel files under Sessions and the audit files under
 * Everything Else is a concept nobody can find twice. Hence the same wrapper
 * `builder.js` uses: a classic script for the page, CommonJS for Node.
 *
 * Order is the rule. `familyOf` returns the first match, and the catch-all is
 * last because it matches everything -- an unpinned concept stays reachable
 * rather than invisible, and the audit reports it as a gap in this table rather
 * than inventing a family per run.
 */
(function (global, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else global.ConceptTaxonomy = api;
})(typeof globalThis === "object" ? globalThis : this, function () {
  "use strict";

  /// The taxonomy, by the id's namespace.
  ///
  /// The old menu table grouped the classical indicators by hand and stopped there,
  /// so anything the engine grew afterwards -- the statistical family, the economic
  /// releases, Ichimoku -- fell out of the tree entirely and could only be found by
  /// searching. Here every id lands somewhere: a match table for the named groups,
  /// and a catch-all that keeps an unpinned concept reachable rather than invisible.
  const GROUPS = [
    { id: "smc", name: "Smart Money Concepts", note: "Structure, liquidity and the blocks price is drawn back to.", match: (id) => id.startsWith("smc.") },
    { id: "pa", name: "Price Action", note: "Breaks, retests and the ranges a level is built from.", match: (id) => id.startsWith("pa.") },
    { id: "candles", name: "Candlestick Patterns", note: "One bar or three: what a single close says about who won it.", match: (id) => id.startsWith("candles.") },
    { id: "chart", name: "Chart Patterns", note: "Shapes drawn across many bars -- heads, wedges, flags and channels.", match: (id) => id.startsWith("chart.") },
    { id: "harmonics", name: "Harmonics", note: "Pruned Fibonacci geometries: Gartley, Bat, Crab, Butterfly and their kin.", match: (id) => id.startsWith("harmonics.") },
    { id: "fib", name: "Fibonacci", note: "Retracements, extensions and the pockets where several agree.", match: (id) => id.startsWith("fib.") },
    { id: "session", name: "Sessions", note: "Killzones, day and week opens, and the levels yesterday left behind.", match: (id) => id.startsWith("session.") },
    { id: "volume-profile", name: "Volume Profile", note: "Where interest actually traded: point of control, value area, single prints.", match: (id) => id.startsWith("volume.") },
    { id: "sd", name: "Supply & Demand", note: "The untested zones above and below price that a move started from.", match: (id) => id.startsWith("sd.") },
    { id: "order-flow", name: "Order Flow", note: "Aggression read from the tape: delta, and the bars where it turns.", match: (id) => id.startsWith("order_flow.") },
    { id: "trend", name: "Trend", note: "Direction and its strength -- averages, channels, stops-and-reversals.", match: (id) => /^indicators\.(ema|sma|ema_family|supertrend|psar|adx|macd|macd_family|keltner|donchian|ichimoku|chandelier)$/.test(id) },
    { id: "momentum", name: "Momentum", note: "The pace of the move and the divergences that warn it is running out.", match: (id) => /^indicators\.(rsi|rsi_divergence|rsi_family|rsi_pca|stochastic|cci|williams_r|roc|tsi|awesome)$/.test(id) },
    { id: "volatility", name: "Volatility", note: "How wide price is swinging, as bands or as an absolute range.", match: (id) => /^indicators\.(bollinger|bollinger_percent|atr|atr_family)$/.test(id) },
    { id: "volume", name: "Volume", note: "Participation behind the move, summed, weighted or anchored.", match: (id) => /^indicators\.(obv|ad_line|cmf|mfi|anchored_vwap)$/.test(id) },
    { id: "statistical", name: "Statistical", note: "Modelled regimes: filters, forecasts, change points and pair spreads.", match: (id) => /^indicators\.(hv|zscore|kalman|changepoint|entropy|bayesian_drift|ar_forecast|garch|hmm|pairs)$/.test(id) },
    { id: "econ", name: "Economic Releases", note: "Scheduled numbers, marked on the chart the moment they land.", match: (id) => id.startsWith("econ.") },
    { id: "more", name: "Everything Else", note: "Anything the taxonomy above has not been taught to name.", match: () => true },
  ];

  return {
    GROUPS,
    /// The family a concept id belongs to. First match wins; `more` is last and
    /// matches anything, so this never returns nothing.
    familyOf(id) {
      return (GROUPS.find((group) => group.match(id)) || GROUPS[GROUPS.length - 1]).id;
    },
  };
});
