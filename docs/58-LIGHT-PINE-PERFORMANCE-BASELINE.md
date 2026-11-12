# Light Pine Runtime Performance Baseline

## Purpose and scope

This reproducible local harness measures Pine-Lite execution and allocations
on fixed workloads. The baseline section records the pre-optimization
interpreter; the Phase 6 section below compares those workloads with the
bounded TA cache. It is not a comparison against another implementation.

Run it from the repository root with:

```sh
cargo bench -p pine-lite --bench runtime_baseline
```

The dependency-free harness uses five deterministic, synthetic 1,000-bar
workloads: simple TA, loops/state, drawing-heavy, security/MTF, and strategy.
It additionally measures simple TA on a 10,000-bar history and a 5,000-bar
last-bar replay. It reports:

- `vet`: parse/type-check/vet cost for source.
- `run_vetted`: execution of an already vetted script.
- `replay_after_last_bar_update`: full interpreter execution after changing the
  last candle in a 500-bar window, including the matching final MTF slot where
  applicable. This is not an incremental-runtime or browser-rendering
  measurement.
- Median nanoseconds per operation, mean allocation/reallocation request count
  per operation, and mean requested bytes per operation. Allocated bytes are
  cumulative requested sizes, not peak or retained memory.

Each timing row uses 3 warmup samples and 15 measured samples, with 8
operations per sample (16 for last-bar replay). Reported allocations exclude
warmups. Workload validation and output checks happen before timing.

## Recorded environment

- Target: `x86_64-apple-darwin`
- CPU: Intel Core i7-8750H @ 2.20 GHz (12 logical CPUs)
- Rust: `rustc 1.99.0 (b940084d7 2026-09-28)`, LLVM 23.1.1
- Crate: `pine-lite` 0.1.0; optimized Cargo `bench` profile
- Input: deterministic synthetic candles; 1,000 bars for vet/run and 500 for
  last-bar replay

## Baseline

The following medians are from one run immediately before the Phase 6 cache
change:

| Workload | Operation | Bars | Median ns/op | Allocations/op | Requested bytes/op |
| --- | --- | ---: | ---: | ---: | ---: |
| simple_ta | vet | 1,000 | 25,678 | 137 | 10,633 |
| simple_ta | run_vetted | 1,000 | 14,326,051 | 14,026 | 40,540,330 |
| simple_ta | replay_after_last_bar_update | 500 | 4,266,790 | 7,026 | 10,271,330 |
| loop_and_state | vet | 1,000 | 42,731 | 207 | 14,686 |
| loop_and_state | run_vetted | 1,000 | 18,585,462 | 57,020 | 80,252,182 |
| loop_and_state | replay_after_last_bar_update | 500 | 6,782,261 | 28,520 | 20,127,182 |
| drawing_heavy | vet | 1,000 | 35,481 | 166 | 12,369 |
| drawing_heavy | run_vetted | 1,000 | 251,697 | 123 | 35,149 |
| drawing_heavy | replay_after_last_bar_update | 500 | 124,540 | 70 | 17,899 |
| security_mtf | vet | 1,000 | 14,206 | 83 | 6,599 |
| security_mtf | run_vetted | 1,000 | 2,303,919 | 12,023 | 370,518 |
| security_mtf | replay_after_last_bar_update | 500 | 1,123,928 | 6,023 | 186,518 |
| strategy | vet | 1,000 | 51,802 | 207 | 16,574 |
| strategy | run_vetted | 1,000 | 37,783,041 | 23,045 | 80,849,791 |
| strategy | replay_after_last_bar_update | 500 | 10,761,855 | 11,530 | 20,425,000 |

## Phase 6 optimization experiment

On 2026-11-12, the same harness, inputs, and optimized bench profile were run
once before and twice after adding a bounded per-call-site cache for pure
`ta.sma`, `ta.ema`, `ta.rma`, `ta.wma`, `ta.rsi`, and `ta.atr` calls. The table
uses the pre-change medians and the median of the two post-change run medians.
Allocation counts and requested bytes were identical across both post-change
runs.

| Workload | Operation | Bars | Before ns/op | After ns/op | Before allocations / bytes | After allocations / bytes |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| simple_ta | run_vetted | 1,000 | 14,326,051 | 1,152,058 | 14,026 / 40,540,330 | 6,035 / 100,958 |
| simple_ta | replay_after_last_bar_update | 500 | 4,266,790 | 560,023 | 7,026 / 10,271,330 | 3,035 / 51,958 |
| loop_and_state | run_vetted | 1,000 | 18,585,462 | 17,837,061 | 57,020 / 80,252,182 | 57,020 / 80,252,182 |
| loop_and_state | replay_after_last_bar_update | 500 | 6,782,261 | 6,531,073 | 28,520 / 20,127,182 | 28,520 / 20,127,182 |
| drawing_heavy | run_vetted | 1,000 | 251,697 | 255,681 | 123 / 35,149 | 123 / 35,149 |
| drawing_heavy | replay_after_last_bar_update | 500 | 124,540 | 124,375 | 70 / 17,899 | 70 / 17,899 |
| security_mtf | run_vetted | 1,000 | 2,303,919 | 2,109,605 | 12,023 / 370,518 | 12,023 / 370,518 |
| security_mtf | replay_after_last_bar_update | 500 | 1,123,928 | 1,065,666 | 6,023 / 186,518 | 6,023 / 186,518 |
| strategy | run_vetted | 1,000 | 37,783,041 | 21,051,434 | 23,045 / 80,849,791 | 12,057 / 48,178,643 |
| strategy | replay_after_last_bar_update | 500 | 10,761,855 | 6,002,094 | 11,530 / 20,425,000 | 6,042 / 12,089,852 |

The long-history extension completed on the optimized implementation at
12,348,738 ns/op and 60,035 allocations / 982,958 requested bytes for 10,000
bars; the 5,000-bar last-bar replay measured 6,174,644 ns/op and 30,035
allocations / 492,958 requested bytes. These long-history rows extend the
workload coverage; they do not have a pre-optimization comparison.

The targeted improvement is material on TA-heavy workloads: simple TA execution
fell by about 92% in latency and 99.8% in requested allocation bytes; strategy
execution fell by about 44% in latency and 40% in requested bytes. Other
workloads do not use the eligible call pattern, and their allocation metrics
were unchanged; small latency differences are run noise, not claimed gains.
Vetting remains uncached and unchanged.

The cache is per VM and stores at most 1,000,000 `f64` result values
(8,000,000 bytes of value payload) across at most 500 call sites; the map
overhead is therefore bounded as well. When either budget is exhausted, or the
input history alone exceeds the remaining value budget, the call uses the
original interpreter path. Cache entries are discarded with the VM and reset
between strategy simulation passes. Variable-dependent calls, function
bodies/parameters, and loop scopes are intentionally excluded.
Correctness was checked against that uncached path and against shorter input
prefixes in `pure_ta_call_cache_matches_reference_and_preserves_prefixes`.

This is a targeted execution-plan optimization, not a general typed IR or
bytecode backend. The existing append-only `run_incremental` state does not
capture TA state and cannot safely be assumed to support last-bar amendment;
incremental TA state and parallel execution remain deferred rather than
implemented without equivalent parity and workload evidence.

## Interpretation and limitations

- Execution and loop/state workloads show substantial per-run allocation
  activity. This is evidence for profiling allocation hot spots, not proof that
  caching or a new IR would reduce total cost.
- Vetting costs are small relative to full execution in these workloads, but
  chart-engine vet/execute call frequency and end-to-end rendering are not
  measured here.
- Replay reruns the interpreter across a shortened history after a final-bar
  mutation. It does not measure a persistent incremental state engine.
- Results are host- and compiler-specific, use synthetic candles, and have no
  external comparison. The Phase 6 before/after comparison uses matching local
  runs, not unrelated benchmarks.
- The allocation counters are process-global and report allocation requests;
  they do not measure peak live memory, retained memory, or allocator-internal
  behavior.

For future optimization work, retain these workloads and compare equivalent
source, inputs, compiler profile, and environment. Include correctness/parity
tests and report trade-offs as well as timing.
