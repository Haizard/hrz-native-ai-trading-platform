# 62 — Markov Intelligence

- **Status:** first four adoption steps landed — seven independent,
  deterministic engines in `crates/markov-intelligence`, a read-only
  `analyze_markov` agent tool, a tool-doctrine skill with trigger phrases and a
  registry meta-test, and an immutable evidence journal with one-shot
  market-based settlement.
- **Scope:** regime, transition, volatility, duration, event-timing and
  strategy-selection evidence for the agent layer. It does not trade, does not
  call an LLM, and has no I/O.

## The seven engines

| Technology | Module | Evidence it returns |
|---|---|---|
| Time-varying transition probabilities (TVTP) | `transition::tvtp` | Transition weights shifted by volume, momentum, delta, ATR and spread. |
| Markov-switching autoregression (MS-AR) | `regime::ms_ar` | Regime-specific return dynamics. |
| Markov-switching GARCH (MS-GARCH) | `volatility::ms_garch` | Volatility conditional on the inferred regime. |
| Hidden semi-Markov model (HSMM) | `regime::Hsmm` | Expected regime duration and elapsed-state hazard. |
| Continuous-time Markov chain (CTMC) | `ctmc::exp_generator` | Event timing over irregular intervals (`P(t)=e^{Qt}`). |
| Higher-order / context-dependent Markov | `transition::higher_order` | Transitions conditioned on recent state sequences. |
| Markov decision process (MDP) | `mdp::value_iteration` | A proposed wait/act policy, never an authorization. |

`volatility::garch` and `volatility::egarch` are also provided as standalone
statistical baselines, so regime-switching volatility can be compared against a
plain GARCH and an asymmetric EGARCH estimate.

## Contract

`MarkovRequest` carries the instrument, timeframe, observable features,
returns, regime history, ATR reference, horizon and discount factor.
`MarkovResult` returns:

- current regime and regime probabilities;
- TVTP transition probabilities;
- CTMC transition probabilities;
- expected duration and transition risk;
- volatility regime;
- the MDP policy proposal;
- a model version for replay and audit.

`analyze()` combines all seven engines into one typed evidence record. Each
engine is also public on its own, so a caller can use Markov regime evidence
without pulling in the MDP or CTMC.

## Boundaries

- No LLM, no network, no broker, no clock: this is a pure computation crate.
- An MDP policy is a **proposal**. The risk gate and `LiveAuthorization` remain
  the only execution authority.
- All probabilities are evidence scores, not calibrated win probabilities.
- Kronos and other heavy models are deliberately not included; this crate runs
  cheaply beside the Rust core.

## Next adoption steps

1. ~~Expose `analyze()` as a narrow read-only agent tool with a typed JSON schema.~~ Done: `analyze_markov`.
2. ~~Add a skill/capability descriptor with trigger phrases and a meta-test.~~ Done: `markov-regime.yaml`.
3. ~~Persist `MarkovResult` into the decision journal with later outcome
   settlement.~~ Done: `markov_evidence` table, host writer trait, read-only
   `analyze_markov` recording, and `POST /markov/evidence/{id}/settle`.

## Verification

`cargo test -p markov-intelligence` — 16 tests pass, covering row-stochastic
normalization, TVTP feature shifts, MS-AR fitting, HSMM durations, GARCH/EGARCH
asymmetry, CTMC stationary behaviour, MDP policy selection and the combined
typed evidence record.
