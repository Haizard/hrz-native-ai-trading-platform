# Pass 4 — Sessions (14 rows)

Judged 2026-10-29 against `main`. The machine-readable verdicts are the 14 rows of
[`ledger/session.json`](../../../../reports/concept-visual-audit/ledger/session.json);
`node tools/concept_visual_audit.mjs check --family session` reports **14 rows, 0 incomplete**.

**Headline: this family draws the clock as a price band.** Three of the fourteen concepts are
about *when* — the London and New York killzones and the Asian range — and all three are drawn
as a rectangle between two *prices*, 12 to 28 bars wide, shaded over a fraction of the plot
height. Both reference platforms draw the opposite thing: a full-height region containing the
session's bars, with no price extent at all
([TradingView: "the indicator draws a colored box containing all the bars that opened within the
session"](https://www.tradingview.com/support/solutions/43000729030-trading-sessions/)), because
"the killzone is the time container; the session range is the price object it produces"
([Killzones](https://www.luxalgo.com/library/concept/killzones/)). Our box is the price object
standing in for the container, and — as the next paragraph shows — it is not even the right
price object.

**Seven other rows are levels that never run, and none of them is drawn where it is used.** The
six level-and-open concepts produce 12 events on this window between them — the two monthly ones
produce none at all — and not one draws a horizontal line that extends past the bar that produced
it: the daily and weekly opens are zero-length segments floored to about one bar-width
([`position_concept_shape`](../../../../frontend/chart-engine/src/scene.rs#L2893)), the prior
day and prior week are boxes drawn *over the completed period* that stop dead on the roll bar,
and the opening range is a box from the session's first bar to its break bar. The reference is
unanimous on the missing half: "Project the high, low, and close … forward as horizontal rays"
([Prior Period Levels](https://www.luxalgo.com/library/concept/prior-period-levels/)), "Plot
Open_P as a horizontal level from bar s forward until the period ends"
([Period Opens](https://www.luxalgo.com/library/concept/period-opens/)), "When the session
closes, freeze both levels and extend them to the right as horizontal lines"
([Session Liquidity](https://www.luxalgo.com/library/concept/session-liquidity/)).

**The clock is pinned to UTC, which both references name as *the* failure mode.** Our windows
are "the UTC equivalents for the winter offset … documented rather than DST-fudged"
([events.rs:2124-2128](../../../../crates/analytics-core/src/events.rs#L2124)); the killzone
page says the opposite is required: "Set the chart's timezone to New York … a chart left on UTC
or local time puts the windows on the wrong candles, and daylight-saving weeks make the error
worse". TradingView's help page spells out the same arithmetic. Measured on the day of this
pass: US Eastern is on EDT, so the London window runs 03:00-06:00 New York instead of 02:00-05:00
and New York runs 08:00-11:00 instead of 07:00-10:00, and the Asian range is 7 hours against the
cited 4. The windows are correct for about half the year and wrong for the other half — which is
worse than consistently wrong, because a trader cannot tell which half they are in.

**And one row's boundary is on the wrong day of the week.** The week bucket is
`open_time.div_euclid(7 days)`, which is anchored to the epoch, and 1970-01-01 was a Thursday,
so every weekly concept rolls at Thursday 00:00 UTC. Measured over four weeks: 5 week rolls,
**5 on a Thursday**, and the `weekly_open` event on this window is stamped
2026-10-29T00:00 — a Thursday. Period Opens fixes the crypto convention literally:
"crypto weeks start Monday 00:00 UTC". Three rows share that bucket (`weekly_open`,
`previous_week_levels`, `weekly_bias`).

## How it was measured

Same window and route as passes 1-3:
`GET /concept-events/scan?symbol=BTCUSDT&timeframe=15m&limit=500` — 501 bars
(2026-10-24T01:45 to 2026-10-29T06:45 UTC), 4,229 events, the same three caps
([concept_routes.rs:215-266](../../../../crates/api-gateway/src/concept_routes.rs#L215)):
replay from bar 22, at most 50 events per concept keeping the newest, `EventConfig::default()`.
`node tools/concept_visual_audit.mjs render session` photographs the fourteen; every number
below comes from that same fetch:

| concept | events | shapes | draws | span (bars) min/med/max | zero-span | distinct levels | longest run |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `session.london_killzone` | 4 | 4 | 8 | 12 / 12 / 12 | 0 | 4 | 1 |
| `session.new_york_killzone` | 5 | 5 | 10 | 12 / 12 / 12 | 0 | 5 | 1 |
| `session.asian_range` | 4 | 4 | 8 | 28 / 28 / 28 | 0 | 4 | 1 |
| `session.previous_day_levels` | 5 | 5 | 10 | 60 / 96 / 96 | 0 | 5 | 1 |
| `session.previous_week_levels` | 1 | 1 | 2 | 444 / 444 / 444 | 0 | 1 | 1 |
| `session.previous_month_levels` | 0 | 0 | 0 | — | — | — | — |
| `session.daily_open` | 5 | 5 | 10 | 0 / 0 / 0 | **5** | 5 | 1 |
| `session.weekly_open` | 1 | 1 | 2 | 0 / 0 / 0 | **1** | 1 | 1 |
| `session.monthly_open` | 0 | 0 | 0 | — | — | — | — |
| `session.daily_bias` | 0 | 0 | 0 | — | — | — | — |
| `session.weekly_bias` | 0 | 0 | 0 | — | — | — | — |
| `session.htf_candle_bias` | 50 | 50 | 100 | 4 / 6 / 7 | 0 | 13 | **16** |
| `session.opening_gap` | 0 | 0 | 0 | — | — | — | — |
| `session.opening_range_breakout` | 30 | 30 | 60 | 4 / 53 / 92 | 0 | 11 | 1 |

"9 drawn, 0 fired-but-routed-nothing, 5 silent on this window." All 105 events answer
`status: "confirmed"`; 99 of them carry both zone edges. The render pages are working artifacts
and stay gitignored; [the fourteen on the real chart](evidence/pass4-chart.png) and
[the same concepts in the panel](evidence/pass4-chart-panel.png) are committed because they are
the finding — a curtain of translucent grey boxes over most of the plot, two snake_case labels
(`htf_candle_bias`, `opening_range_breakout`) and a flat 14/14 in the Sessions group.

**Three measurements needed a window the route does not serve.** The silent concepts are silent
for reasons a 501-bar window cannot show, so the verdicts were also tested against a four-week
fetch (2,976 bars, 2026-09-28 to 2026-10-29) replayed with the same detectors:

- 31 UTC day rolls: **0** had the new day's open anywhere outside the prior day's whole range,
  so `daily_bias` cannot fire on its price clause alone, lean or no lean.
- 31 rolls: the largest open-to-prior-close move was **0.00033%** against `opening_gap`'s
  0.05% floor — a threshold that never has anything to threshold.
- 5 week rolls, all Thursdays (above), and 1 month roll (2026-10-01), which a 15m replay of
  1000 bars — the route's ceiling, `clamp(50, 1000)`
  ([concept_routes.rs:229](../../../../crates/api-gateway/src/concept_routes.rs#L229)) — can
  only reach during the first ten days of a month. That is why the two monthly rows are `0`,
  and it is a transport fact, not a detector one.

## What the reference set says

Six LuxAlgo Library concept pages, one LuxAlgo docs page and one TradingView help page carry
this family. All were read on 2026-10-29; every sentence below is verbatim. They agree on one
structural point the render table contradicts: **a clock concept is a time container, and the
prices it produces are separate objects drawn separately.**

1. **Killzones are mechanical, and anchored to New York.** "Killzones are defined by the clock
   rather than by price action, so marking them is mechanical"; "Fixed intraday time windows,
   anchored to the New York clock, that box off when each trading session's liquidity runs and
   directional moves are expected to occur"; the calculation's own steps are "Set the chart to
   New York time (America/New_York); killzone boundaries are defined on this clock so they stay
   aligned through US daylight-saving changes", "Mark the London open killzone: 02:00 to 05:00
   NY time" and "Mark the New York open killzone: 07:00 to 10:00 NY time (index-futures variants
   use 08:30 to 11:00)"
   ([Killzones](https://www.luxalgo.com/library/concept/killzones/)).
2. **The container is full-height and contains bars; the prices are a second drawing.** "For each
   trading session, the indicator draws a colored box containing all the bars that opened within
   the session in the specified time zone"; "'Draw session open and close lines': Draws two
   additional dashed lines within each session box to mark the open and close prices of the
   session"; and on the timezone, "using a 'GMT-5' time zone instead highlights the sessions
   correctly only in the non-DST period, and deviates by one hour during DST. Therefore … use
   the IANA time zone notation"
   ([TradingView: Trading Sessions](https://www.tradingview.com/support/solutions/43000729030-trading-sessions/)).
   Note the second quote: even the *prices* are lines inside the box, not the box's own height.
3. **The session range is the price object, measured on a four-hour window.** "The killzone is
   the time container; the session range is the price object it produces"; and for the Asian
   range specifically, "Box the Asian range from 8:00 pm to midnight New York time; write-ups
   differ on bodies versus wicks here, so pick one convention and keep it" — four hours, not
   seven — followed by "Project the deviations: replicate each box's height above its high and
   below its low" ([ICT Session Ranges](https://www.luxalgo.com/library/concept/ict-session-ranges/)).
   The page opens with the warning: "These are rule-drawn boxes rather than judgment calls".
4. **A frozen session's levels are frozen and extended.** "When the session closes, freeze both
   levels and extend them to the right as horizontal lines"; "A level is swept when a later bar
   trades through it … mark the sweep and stop extending the line"
   ([Session Liquidity](https://www.luxalgo.com/library/concept/session-liquidity/)).
5. **Prior period levels are three rays per period, projected forward.** "Prior period levels are
   the high, low, and close of a completed period … projected forward onto the current chart as
   horizontal references"; "Project the high, low, and close (plus settlement on futures) forward
   as horizontal rays, keeping daily, weekly, and monthly sets visually distinct so
   higher-timeframe levels stand out"; and on the daily boundary for crypto, "Midnight UTC is the
   most common daily close for crypto"
   ([Prior Period Levels](https://www.luxalgo.com/library/concept/prior-period-levels/)).
6. **An open is a level with a life, and the life is the period.** "Plot Open_P as a horizontal
   level from bar s forward until the period ends"; "Period boundaries follow the venue's calendar
   and timezone, so weekly opens differ between markets (crypto weeks start Monday 00:00 UTC, CME
   futures Sunday 18:00 ET)"; "Read the side first: above or below each open is the period's
   green/red bookkeeping, the base bias layer"
   ([Period Opens](https://www.luxalgo.com/library/concept/period-opens/)).
7. **The ICT midnight open is New York midnight, and it is a ray.** "Find the 00:00 candle and
   extend its opening price forward as a horizontal ray. That line is the midnight open … and it
   stays relevant for the entire session"; and explicitly, "Midnight New York time (00:00 ET),
   not midnight UTC" ([ICT Time Anchors](https://www.luxalgo.com/library/concept/ict-time-anchors/)).
   This is why the daily-open row cites Period Opens rather than the anchors: the generic
   calendar open for crypto is midnight UTC, the ICT anchor is not, and the two must not be
   conflated when the state architecture starts varying concepts by venue.
8. **An opening range is measured once, frozen, and extended.** "Mark the highest high and lowest
   low printed inside that window. Those two levels are the opening range; extend them across the
   rest of the session"; "When the window ends, freeze ORH, ORL, and ORM and extend them across
   the rest of the session"; "A first close beyond the range on your trading timeframe is stricter
   than a simple wick through it"; and for this venue, "24-hour markets have no single open, so
   traders anchor the range to a chosen session open … or the daily open at 00:00 UTC in crypto"
   ([Opening Range & ORB](https://www.luxalgo.com/library/concept/opening-range-and-orb/)).
9. **A gap is a box however small — and crypto has none.** "Mark the closing print and the
   reopening print, and box the span between them; that box is the NDOG, however small"; "Only
   where trading actually pauses. Crypto trades continuously, so there is no daily closing print
   and no true NDOG"
   ([New Day Opening Gap](https://www.luxalgo.com/library/concept/new-day-opening-gap/)).
10. **A higher-timeframe bias is one state per completed bar, shown as a reading.** "The filter
    reduces the higher timeframe to a simple directional state (long-permitted, short-permitted,
    sometimes neutral)"; "a higher-timeframe value is only final when its bar closes. A filter
    that reads the live, still-forming bar repaints"; "The fix is to reference the last completed
    higher-timeframe bar"
    ([Higher-timeframe Trend Filter](https://www.luxalgo.com/library/concept/higher-timeframe-trend-filter/)),
    and the readout has a house form: "The cell color is based on the signal returned by the
    signal algorithm … with a green color indicating a bullish signal and a red color indicating
    a bearish signal"
    ([docs: Dashboard](https://docs.luxalgo.com/platform/algos/signals-overlays/dashboard)).

## What we draw today

**1. Three concepts ask *when* and the vocabulary answers *where*.** `detect_sessions` emits a
`zone_low`/`zone_high` for each killzone and for the Asian range
([events.rs:2317-2332](../../../../crates/analytics-core/src/events.rs#L2317)), and the router's
zone branch paints it as a rectangle between those two prices
([concept_shapes.js:67](../../../../frontend/app/concept_shapes.js#L67)). The prices are real —
the window's own high and low — but the reference makes them a *second* object: the box is the
time, the levels are the drawing. Measured: all three windows are exactly the right length in
bars (12, 12, 28) and all three shade a band, so the x-extent is right and the y-extent is
invented. This is the pass that settles `time_box` ([vocabulary](00-vocabulary.md)): it is
**wiring**, because `scene_overlay_tints` already emits precisely this draw for a Pine
`bgcolor` — `ScriptDraw::Box { y1: plot.y, y2: plot.y + plot.h, border_width: 0.0,
fill_alpha: Some(0.10) }`, full plot height, no price extent, clipped by the shell
([script.rs:785-809](../../../../frontend/chart-engine/src/script.rs#L785)). Nothing in the
engine has to learn to shade a time range; the concept path has to stop calling it a zone.

**2. The windows are UTC-fixed, so they are right half the year.** The comment above
`detect_sessions` states the choice: the windows are "the UTC equivalents for the winter offset
(London 07:00-10:00 UTC ~= 02:00-05:00 ET, New York 12:00-15:00 UTC ~= 07:00-10:00 ET),
documented rather than DST-fudged"
([events.rs:2124-2128](../../../../crates/analytics-core/src/events.rs#L2124)). Every reference
in the set says that is the error to avoid, in almost the same words, and this pass measured the
consequence on the day it was judged: EDT is UTC-4, so the London box covers 03:00-06:00 New
York (an hour late), the New York box covers 08:00-11:00 (an hour late), and the Asian range
covers 20:00-03:00 (an hour early, three hours long). Two of the three are also the wrong
*length*: the cited killzones are three hours and four respectively, and the Asian range is
four, where ours is seven.

**3. The Asian range's window is the whole UTC night, not the cited four hours.** `WINDOWS` sets
`(AsianRange, 0, 7)` ([events.rs:2131-2135](../../../../crates/analytics-core/src/events.rs#L2131)),
i.e. 00:00-07:00 UTC, which is the gap between the New York close and the London open rather than
ICT's 20:00-00:00 New York. The page's comparison section says the difference is expected —
generic session tools and ICT's Asian range "often disagree on the same chart" — but ours is
labelled with the ICT name (`Asian Range`) and draws the generic object, so a trader who learned
the range from ICT material gets a box 75% too wide with the wrong high and low.

**4. The week starts on Thursday.** `let newest_week = last.open_time.div_euclid(WEEK_NS)` with
`WEEK_NS = 7 * DAY_NS` ([events.rs:2166](../../../../crates/analytics-core/src/events.rs#L2166))
buckets time against the Unix epoch, and 1970-01-01 was a Thursday. The comment above it says
"Day-roll for the daily open, Monday-roll for the weekly (UTC week)" — the comment is the intent
and the arithmetic is not. Measured over four weeks: 5 rolls, 5 Thursdays, and the `weekly_open`
event on this window is stamped 2026-10-29T00:00. Three rows share the bucket, and the failure is
silent: the level exists, it is just on the wrong bar and therefore the wrong price.

**5. `daily_bias` and `weekly_bias` define bias upside-down, and neither has fired in a month.**
`period_bias` asks whether the new period's *open* sits outside the entire range of the finished
one ([regime.rs:177-182](../../../../crates/analytics-core/src/regime.rs#L177)), and then
requires the structure lean to agree. Over 31 day rolls the first clause alone never held: the
open never cleared the prior day's whole range, so the extra clause is unreachable on this
window. The reference's bias is the other quantity entirely — "above or below each open is the
period's green/red bookkeeping" — price *relative to the open*, which is a reading available on
every bar and flips when price crosses. Ours is a gap-up/gap-down detector under a bias's name,
and on a continuous market there are no gaps, so it says nothing. `weekly_bias` is the same
predicate on the Thursday bucket: 0 of 5.

**6. `htf_candle_bias` is a state re-emitted on every bar — and it is the one thing this family
gets right.** The rule uses the newest *completed* upper bar
([events.rs:1849-1859](../../../../crates/analytics-core/src/events.rs#L1849)), which is exactly
the anti-repaint convention the filter page insists on ("reference the last completed
higher-timeframe bar"), and its window is right. What it does with that correctness is the
problem: it is evaluated on every 15m bar, so one completed H1 candle is re-announced on 4 to 7
consecutive bars (one run reaches 16), yielding 50 events for **13 distinct candles** — and the
scan's 50-event cap then truncates the history to the newest handful. Each event also carries the
candle's own high and low, which the router ignores (no `session.*` HTF id is in
`CONCEPT_ZONE_IDS`), so the chart shows a 1.3px stub while the card shows a box
([concept_shapes.js:27](../../../../frontend/app/concept_shapes.js#L27),
[app.js:14778-14791](../../../../frontend/app/app.js#L14778)).

**7. The opening range is measured right and never frozen.** The first four bars of the UTC day —
the 60-minute range the reference calls the first-hour range — is a correct window
([events.rs:1821-1824](../../../../crates/analytics-core/src/events.rs#L1821)). But the event is
emitted from the same predicate on every bar: `prev.close <= ib_hi && last.close > ib_hi` is true
again the moment price closes back inside and out again, so the concept fired **11 times on
2026-10-26** and 30 times across six sessions, at 11 distinct prices, each event boxing from the
session's first bar to its own break bar (median span 53 bars, max 92). The reference freezes the
range once, extends it across the session, and marks *the first* close beyond. Ours re-draws the
same break with a longer box each time — which is why the photograph is a stack of nested
rectangles.

**8. The opening gap's threshold is three orders of magnitude above this market's behaviour.**
`(gap / prev_close).abs() >= 0.0005` ([events.rs:1801](../../../../crates/analytics-core/src/events.rs#L1801))
means 0.05%; the largest move over 31 rolls was 0.00033%. The floor exists for a real reason — on
a continuous market the "gap" is just the next bar's open — but the reference says the object
simply is not there: "Crypto trades continuously, so there is no daily closing print and no true
NDOG", and where it does exist it is boxed "however small". A threshold that never trips does not
make the concept honest; it makes it invisible. If it did trip it would draw a band across one
bar, with neither the box nor the midpoint the page asks for.

**9. The two monthly rows are unreachable, and the month they would draw is truncated when it is
reachable.** The scan route clamps the replay to 1000 bars
([concept_routes.rs:229](../../../../crates/api-gateway/src/concept_routes.rs#L229)), which at
15m is about ten days, so on the 29th of a month the 1st is out of reach and both rows are `0`.
Inside that window there is a second problem: `previous_month_levels` finds the month's first bar
with `candles.iter().position(...)` over the *fetched* slice
([events.rs:2267-2272](../../../../crates/analytics-core/src/events.rs#L2267)), so a fetch that
starts mid-month draws that partial month as if it were whole. The concept needs a fetch contract
(current period + one full prior period), not a bigger cap.

**10. Every label is the detector's name, in snake_case, again.** `concept_shapes.js` derives the
label from `concept_id.split(".")[1]` and only the `candle_mark` branch humanises it
([concept_shapes.js:47](../../../../frontend/app/concept_shapes.js#L47),
[96](../../../../frontend/app/concept_shapes.js#L96)), so the chart prints `london_killzone`,
`previous_day_levels`, `htf_candle_bias`, `opening_range_breakout`. Visible in
[pass4-chart.png](evidence/pass4-chart.png). This is the third pass to record it; it now covers
61 rows.

**11. The registry says these concepts draw nowhere, and it is not wrong.** All fourteen answer
`pane: false, overlay: false` ([concepts.rs](../../../../crates/market-data/src/concepts.rs)), the
same silence pass 1 found on the nine interval concepts. For an overlay this is doubly wrong: the
router decides placement from a hardcoded `CONCEPT_ZONE_IDS` set, so the registry cannot say
"price frame" even if it wanted to, and 80 of this family's 99 zone-bearing events (the HTF bias
and the ORB) fall through to the segment branch because their ids are absent from that set.

## The card is a second renderer, and it disagrees on every row that has anything to show

The card's counts are the API's counts exactly — 4, 5, 4, 5, 1, 0, 5, 1, 0, 0, 0, 50, 0, 30 — so
the two renderers agree perfectly about *how much*. About *what*, they split four ways, because
the card branches on payload fields ([`previewEvents`, app.js:14768](../../../../frontend/app/app.js#L14768))
while the chart branches on the registry id:

| | concepts | events | the chart | the card |
| --- | --- | --- | --- | --- |
| zone fields, id in `CONCEPT_ZONE_IDS` | killzones, asian_range, previous_day/week_levels | 19 | a price box ending at `to_ns` | the same box, **running to the right edge** |
| zone fields, id absent from the set | htf_candle_bias, opening_range_breakout | 80 | a 1.3px stub | a box running to the right edge |
| `price_level`, zero span | daily_open, weekly_open | 6 | a 1.3px stub | **nothing at all** |
| silent | previous_month_levels, monthly_open, daily_bias, weekly_bias, opening_gap | 0 | — | bare candlesticks under a `0` |

The first two rows disagree for the same reason: every one of the 105 events answers
`status: "confirmed"`, so the card's `forming || confirmed` test is always true and always
extends the box to `axis.bars - 1`
([app.js:14780-14781](../../../../frontend/app/app.js#L14780)). On the chart a confirmed killzone
stops at the window's close and a completed prior day stops at the roll; in the card both run to
the edge of the crop, which turns a finished window into an open-ended one. The third row is the
card's own zero-length problem: it draws `moveTo(x(from)) … lineTo(x(at))`, and with `from == at`
the dashed path paints nothing, so the two concepts whose whole job is a level are invisible in
the preview that sells them. `preview_agrees`: 9 `differs`, 5 `not_rendered`.

## No parameter is honoured

All fourteen declare `parameters: []` in the registry — the fourth consecutive family, and the
one where the gap costs the most, because this family *is* a table of settings in every reference
implementation: the session hours, the session timezone, the opening-range window ("5, 15, 30, or
60 minutes"), the retained-gap count, the higher-timeframe ratio. Ours hard-codes
`(AsianRange, 0, 7)`, `(LondonKillZone, 7, 10)`, `(NewYorkKillZone, 12, 15)`
([events.rs:2131](../../../../crates/analytics-core/src/events.rs#L2131)), the four-bar ORB
window ([events.rs:1823](../../../../crates/analytics-core/src/events.rs#L1823)), the 4×
higher-timeframe multiplier ([events.rs:1838-1845](../../../../crates/analytics-core/src/events.rs#L1838))
and the 0.05% gap floor ([events.rs:1801](../../../../crates/analytics-core/src/events.rs#L1801)).
A trader on index futures who wants the 08:30-11:00 New York variant the killzone page names has
no way to ask, and a trader in London cannot shift the window to their own clock at all.

## The 14 rows, grouped by the shape they need

| Target shape | Rows | `target_shape.status` | What it takes |
| --- | --- | --- | --- |
| `time_box` (3) | london_killzone, new_york_killzone, asian_range | **3 wiring** | no new painter: `scene_overlay_tints` already draws the full-height box ([script.rs:785](../../../../frontend/chart-engine/src/script.rs#L785)); the concept path needs the variant, and the router needs to stop treating these ids as zones |
| `level_run` (7) | previous_day/week/month_levels, daily/weekly/monthly_open, opening_range_breakout | 7 new_rendering | pass 1's shape, unchanged — and in every one of the seven the missing half is a *forward* run from the bar where the level is fixed |
| `readout` (3) | daily_bias, weekly_bias, htf_candle_bias | 3 new_rendering | pass 2's shape, unchanged: a word whose host carries the geometry. The host decides the scale — `price` for the two open-anchored biases, `time_only` for the HTF bias, whose host is the completed candle |
| `lifecycle_box` (1) | opening_gap | 1 new_rendering | the box the reference draws "however small", with its midpoint; on this venue the row is a statement that the object does not exist rather than a request for geometry |

`draw.verdict`: **14 `differs_major`**, 0 `matches`, 0 `differs_minor`, 0
`impossible_in_vocabulary` — the first pass with no minor and no impossible row, and the fourth
pass running with no `matches` at all: in 61 rows across four families, not one concept yet draws
the way the cited reference draws it. `fires.verdict`: 2 `same_bars` (`daily_open`,
`monthly_open` — the UTC calendar boundary is the right anchor for both), 12 `different_rule`, no
`no_convention` rows. Every row in this family had a cited page, including the two that cannot
fire and the one that cannot exist on this venue.

## Fixes this pass asks for

1. **Add the `time_box` variant and route the three windows to it.** The draw exists
   ([script.rs:785](../../../../frontend/chart-engine/src/script.rs#L785)); the router does not.
   The window's high and low then becomes an optional second drawing, which is what the killzone
   page's "extend those levels forward as liquidity references" actually asks for.
2. **Make the session clock a timezone, not a table of UTC hours.** Anchor the windows to
   `America/New_York` (the reference's exact prescription) and expose the window hours as
   parameters — including the 08:30-11:00 index-futures variant, which the page names and we
   cannot express.
3. **Fix the week boundary to Monday 00:00 UTC.** `div_euclid(7 * DAY_NS)` is epoch-anchored and
   1970-01-01 was a Thursday; shift the epoch by the four days that separate Monday from Thursday,
   in one place, and all three weekly rows move together.
4. **Give the six level concepts a run.** `daily_open`/`weekly_open`/`monthly_open` need
   `from = s, to = period_end`; `previous_*_levels` need their high, low and close projected
   forward from the roll instead of a box over the finished period; `opening_range_breakout` needs
   the range frozen at the window's end and extended. `level_run` is already settled — this pass
   only needs the events to carry the end time the renderer can read.
5. **Redefine the two biases as readings against the open.** Price above the period open is the
   green side, below is red; the structure lean can be a confluence, not a gate. As written,
   `daily_bias` cannot fire on a market without gaps, which is the market it runs on.
6. **Emit `htf_candle_bias` once per completed higher-timeframe bar**, not once per chart bar,
   and give the direction a readout so the side is visible. The correct anti-repaint anchor is
   already in the code; only the cadence is wrong.
7. **Freeze the opening range and mark the first close beyond.** Re-firing on every re-break
   turns one session's range into 11 overlapping boxes; the reference's "first close beyond"
   is both quieter and the actual signal.
8. **Retire or re-venue `opening_gap`.** On a continuously traded market the object does not
   exist; either gate the concept to venues that halt, or relabel it as a threshold move and stop
   calling it a gap. The 0.05% floor currently guarantees silence.
9. **Give the two monthly concepts a fetch contract.** The 1000-bar clamp is not a detector bug,
   but a concept whose period is longer than the route's reach is a concept that silently cannot
   speak; the route should fetch at least one full prior period for period-based concepts.
10. **Humanise the labels** (`london_killzone` → `London Killzone`), the fix passes 1, 3 and now
    4 have asked for, and add the four session ids to whatever replaces `CONCEPT_ZONE_IDS` so the
    router stops deciding placement from a hardcoded list the registry cannot see.
