# Phase 5 verification — AI Agent Engine & Skills System

Date: 2026-09-14. Live BTCUSDT 5m data from the managed Postgres instance, live
AWS Bedrock (`qwen.qwen3-coder-next`, `us-east-1`). Nothing here is mocked except
the unit tests.

## Exit criterion

> given "find me a long setup on BTC using my liquidity-sweep skill," the agent
> returns a thesis object with real numbers pulled from Analytics Core (not
> hallucinated), citing which skill and which timeframe conditions fired.

Met. `POST /agent/ask` with that exact question returns HTTP 200:

| field | value |
|---|---|
| skill | `liquidity-sweep-absorption-v2` |
| direction / confidence | `long` / 65% |
| entry / stop / target | 77221.10 / 77125.03 / 77438.01 |
| risk:reward | 2.26 (recomputed in Rust, never taken from the model) |
| checks | 3 pass, 0 fail, 2 unknown |
| turns | 6 |

Conditions as reported:

```
[unknown] 4h trend bullish     -- 4h candles unavailable; cannot verify HTF structure
[pass]    Sell-side liquidity swept -- swept: true for sell_side at 77215.37
[pass]    Long reclaim occurred     -- long_reclaim.reclaimed: true at swept_level 77215.37
[unknown] Absorption present        -- tick data unavailable; check not possible
[pass]    Below sweep low stop      -- stop 77125.03 is below swept level 77215.37
```

Two things make this "real numbers" rather than plausible ones. Every level is
checked against the prices the tools actually returned (`PriceRange`), and the two
things that genuinely could not be evaluated come back `unknown` rather than
`fail` — there are no 4h candles in the window and no tick data for footprint,
so an honest blank is recorded as an honest blank.

Raw response: `phase5-exit-criterion-final.json`.

## NL → Strategy DSL

`POST /agent/generate-strategy` and `agent-cli strategy` both validate against
`strategy-dsl` before returning. Result: `phase5-generated-strategy.yaml`
(`Sweep Swing Low Reversal`, BTCUSDT, 5m entry, 1% risk, 2R target).

The retry loop is what makes this work — drafts come back wrong and are corrected
against the validator's own error text, not re-rolled:

```
attempts: 2
repaired: ['timeframes.entry must be `5m` -- it was `15m`']
```

## Suite state

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets` clean.
`cargo test --workspace`: 507 passed, 0 failed.

## Bugs found by running against the live provider

All five were invisible to the unit tests and only appeared on real data with the
real model. Each is pinned by a test.

1. **The check schema offered a boolean.** A condition the model could not
   evaluate came back `passed: false` while its own detail said "tick data
   unavailable". There was nowhere to say "I could not check this", so an honest
   blank was recorded as a confident failure. Now a three-way
   `pass`/`fail`/`unknown`, and `unknown` is not counted as failed.

2. **A stand-aside thesis was rejected.** The model correctly concluded there was
   no setup and returned zeroed levels; `finalize` refused them as unusable
   prices. Non-directional theses no longer need levels.

3. **Liquidity sides were mislabelled.** `LiquidityKind::is_above`'s doc comment
   was self-contradictory and wrong, and a live run reported a swept *high* as
   "sell-side liquidity swept" — the opposite of the truth. Highs hold buy-side
   liquidity. The doc is fixed and `detect_liquidity` now states the side
   outright (`side`, `above_market`) so the model has nothing to infer.

4. **The reclaim comparison was the model's to get wrong.** It reported
   "price 77221.1 > swept level 77290.0", which is false, on the one condition
   the sweep skill rests on. `detect_liquidity` now returns a computed
   `reclaim` verdict (`long_reclaim` / `short_reclaim`, each with `reclaimed`
   and `swept_level`) and the tool description tells the model to cite it rather
   than compare.

5. **The turn reserved for the answer could be spent on data.** The agent
   announced only `submit_thesis` on the final turn and forced tool choice onto
   it, and the model called `detect_market_structure` anyway — because the
   registry still dispatched it. Roughly one question in three ended in
   "N turns without producing a thesis". Two turns are now reserved, and in
   them a non-submit call is *refused* rather than run.

Also: the generated strategy silently came back with `entry: 15m` against a
request for 5m. Valid, and useless — `strategy-dsl` has no opinion on whether a
document is the one that was asked for, so `generate_strategy` checks
`timeframes.entry` itself and sends it back.

## Observation, not a bug

One run chose a stop 5.73 below a 77221.10 entry and produced a 44R "setup".
Structurally valid and grounding-clean, but a stop that close is noise, not
risk. `finalize` now rejects risk under 2bp of entry (`MIN_RISK_FRACTION`) —
deliberately loose, so it catches the absurd case without legislating how wide a
real stop should be.

## Not verified

- 4h/1h candles are not in the database for this window, so the higher-timeframe
  half of the skill has never fired `pass` against live data. It reports
  `unknown`, correctly, but that path is untested end to end.
- Footprint, absorption and imbalance all report `unavailable`: no tick data is
  being collected for BTCUSDT. Same conclusion.
- `backtest_strategy` / `backtest_similar_setups` are registered and return a
  clear "not attached" error; no runner is wired to the agent yet (Phase 6).
- Skill writes via `POST /skills` return 501 — the `skills` table needs a
  `user_id` and auth lands in Phase 7.
