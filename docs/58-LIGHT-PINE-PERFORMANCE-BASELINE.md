# Light Pine Runtime Performance Baseline

## Purpose and scope

This is the first reproducible local baseline for deciding whether later work
on compile-once execution, caching, or an intermediate representation is
worthwhile. It measures the current Pine-Lite interpreter; it is not a
comparison against another implementation or a before/after optimization
result.

Run it from the repository root with:

```sh
cargo bench -p pine-lite --bench runtime_baseline
```

The dependency-free harness uses five deterministic, synthetic 1,000-bar
workloads: simple TA, loops/state, drawing-heavy, security/MTF, and strategy.
It reports:

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

The following medians are from one run. A second run completed successfully and
produced similar timings and identical allocation counts/bytes:

| Workload | Operation | Bars | Median ns/op | Allocations/op | Requested bytes/op |
| --- | --- | ---: | ---: | ---: | ---: |
| simple_ta | vet | 1,000 | 27,587 | 131 | 10,521 |
| simple_ta | run_vetted | 1,000 | 11,887,797 | 10,026 | 48,060,330 |
| simple_ta | replay_after_last_bar_update | 500 | 3,532,446 | 5,026 | 12,031,330 |
| loop_and_state | vet | 1,000 | 42,964 | 204 | 14,681 |
| loop_and_state | run_vetted | 1,000 | 20,955,059 | 57,020 | 80,252,182 |
| loop_and_state | replay_after_last_bar_update | 500 | 7,568,578 | 28,520 | 20,127,182 |
| drawing_heavy | vet | 1,000 | 30,271 | 145 | 11,862 |
| drawing_heavy | run_vetted | 1,000 | 278,325 | 123 | 35,149 |
| drawing_heavy | replay_after_last_bar_update | 500 | 143,212 | 70 | 17,899 |
| security_mtf | vet | 1,000 | 16,676 | 82 | 6,598 |
| security_mtf | run_vetted | 1,000 | 2,124,762 | 11,025 | 291,518 |
| security_mtf | replay_after_last_bar_update | 500 | 1,080,292 | 5,525 | 147,018 |
| strategy | vet | 1,000 | 43,666 | 195 | 16,353 |
| strategy | run_vetted | 1,000 | 29,389,903 | 16,045 | 96,145,791 |
| strategy | replay_after_last_bar_update | 500 | 8,057,911 | 8,030 | 24,073,000 |

The second run's `run_vetted` medians ranged from about 0.2% to 2.6% below the
recorded run for these workloads; allocation metrics matched across all
operations. This is a sanity check on local repeatability, not a statistical
confidence interval.

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
  external or pre-change comparison. Do not compare them directly with
  unrelated benchmarks.
- The allocation counters are process-global and report allocation requests;
  they do not measure peak live memory, retained memory, or allocator-internal
  behavior.

Before deciding on caching or an IR, retain these workloads and compare
equivalent source, inputs, compiler profile, and environment against any
proposed implementation. Include correctness/parity tests and report the
trade-offs as well as timing.
