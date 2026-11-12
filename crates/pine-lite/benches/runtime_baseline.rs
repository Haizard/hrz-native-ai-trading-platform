use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use analytics_core::types::{Candle, Timeframe};
use pine_lite::interp::Inputs;

const BAR_COUNT: usize = 1_000;
const UPDATE_BAR_COUNT: usize = 500;
const WARMUP_SAMPLES: usize = 3;
const SAMPLES: usize = 15;
const OPERATIONS_PER_SAMPLE: usize = 8;
const UPDATE_OPERATIONS_PER_SAMPLE: usize = 16;

static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let resized = unsafe { System.realloc(ptr, layout, new_size) };
        if !resized.is_null() {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        resized
    }
}

#[derive(Clone, Copy)]
struct AllocationSnapshot {
    count: usize,
    bytes: usize,
}

fn allocation_snapshot() -> AllocationSnapshot {
    AllocationSnapshot {
        count: ALLOCATION_COUNT.load(Ordering::Relaxed),
        bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
    }
}

struct Workload {
    name: &'static str,
    source: &'static str,
    uses_hourly_pool: bool,
}

fn workloads() -> [Workload; 5] {
    [
        Workload {
            name: "simple_ta",
            source: concat!(
                "//@pine_lite version=1 overlay=false title=\"Baseline TA\"\n",
                "fast = ta.sma(close, 20)\n",
                "slow = ta.ema(close, 50)\n",
                "plot(fast, title=\"SMA\")\n",
                "plot(slow, title=\"EMA\")\n",
            ),
            uses_hourly_pool: false,
        },
        Workload {
            name: "loop_and_state",
            source: concat!(
                "//@pine_lite version=1 overlay=false title=\"Baseline Loop State\"\n",
                "window = 0.0\n",
                "for i = 0 to 9\n",
                "    window := window + close[i]\n",
                "var float total = 0.0\n",
                "if close >= open\n",
                "    total := total + window\n",
                "else\n",
                "    total := total - window\n",
                "plot(total)\n",
            ),
            uses_hourly_pool: false,
        },
        Workload {
            name: "drawing_heavy",
            source: concat!(
                "//@pine_lite version=1 overlay=true title=\"Baseline Drawings\" max_boxes_count=256\n",
                "if bar_index >= 2 and bar_index % 20 == 0\n",
                "    box.new(bar_index - 2, high, bar_index, low, title=\"Zone\")\n",
                "plot(close)\n",
            ),
            uses_hourly_pool: false,
        },
        Workload {
            name: "security_mtf",
            source: concat!(
                "//@pine_lite version=1 overlay=false title=\"Baseline MTF\"\n",
                "hourly = request.security(\"BTCUSDT\", \"1h\", close)\n",
                "plot(hourly)\n",
            ),
            uses_hourly_pool: true,
        },
        Workload {
            name: "strategy",
            source: concat!(
                "//@pine_lite version=1 overlay=false title=\"Baseline Strategy\" strategy(initial_capital=10000, default_qty_type=\"fixed\", default_qty_value=1)\n",
                "fast = ta.sma(close, 10)\n",
                "slow = ta.sma(close, 30)\n",
                "if ta.crossover(fast, slow)\n",
                "    strategy.entry(\"L\", direction=\"long\")\n",
                "if ta.crossunder(fast, slow)\n",
                "    strategy.close_all()\n",
                "plot(fast)\n",
            ),
            uses_hourly_pool: false,
        },
    ]
}

fn candles(count: usize) -> Vec<Candle> {
    (0..count)
        .map(|index| {
            let phase = index as f64 * 0.035;
            let close = 100.0 + phase.sin() * 4.0 + index as f64 * 0.002;
            let open = close - phase.cos() * 0.5;
            let volume = 10.0 + (index % 17) as f64;
            Candle {
                symbol: "BTCUSDT".to_string(),
                timeframe: Timeframe::M1,
                open_time: index as i64 * Timeframe::M1.nanos(),
                open,
                high: open.max(close) + 0.25,
                low: open.min(close) - 0.25,
                close,
                volume,
                buy_volume: volume * 0.55,
                sell_volume: volume * 0.45,
            }
        })
        .collect()
}

fn hourly_aligned(candles: &[Candle]) -> Vec<Candle> {
    let mut source_by_bucket: std::collections::HashMap<i64, Candle> =
        std::collections::HashMap::new();
    let mut aligned = Vec::with_capacity(candles.len());
    for chart in candles {
        let bucket = Timeframe::H1.bucket_of(chart.open_time);
        let source = source_by_bucket.entry(bucket).or_insert_with(|| Candle {
            symbol: chart.symbol.clone(),
            timeframe: Timeframe::H1,
            open_time: bucket,
            open: chart.open,
            high: chart.high,
            low: chart.low,
            close: chart.close,
            volume: 0.0,
            buy_volume: 0.0,
            sell_volume: 0.0,
        });
        source.high = source.high.max(chart.high);
        source.low = source.low.min(chart.low);
        source.close = chart.close;
        source.volume += chart.volume;
        source.buy_volume += chart.buy_volume;
        source.sell_volume += chart.sell_volume;
        let mut slot = source.clone();
        slot.open_time = chart.open_time;
        aligned.push(slot);
    }
    aligned
}

fn inputs_for(workload: &Workload, candles: &[Candle]) -> Inputs {
    let series_pool = if workload.uses_hourly_pool {
        std::collections::HashMap::from([("BTCUSDT@1H".to_string(), hourly_aligned(candles))])
    } else {
        std::collections::HashMap::new()
    };
    Inputs {
        series_pool,
        ..Inputs::default()
    }
}

fn report(
    workload: &str,
    operation: &str,
    bars: usize,
    operations_per_sample: usize,
    mut operation_fn: impl FnMut(usize),
) {
    for sample in 0..WARMUP_SAMPLES {
        for operation_index in 0..operations_per_sample {
            operation_fn(sample * operations_per_sample + operation_index);
        }
    }

    let mut sample_ns = Vec::with_capacity(SAMPLES);
    let mut total_allocations = 0usize;
    let mut total_allocated_bytes = 0usize;
    for sample in 0..SAMPLES {
        let before = allocation_snapshot();
        let start = Instant::now();
        for operation_index in 0..operations_per_sample {
            operation_fn(sample * operations_per_sample + operation_index);
        }
        let elapsed = start.elapsed();
        let after = allocation_snapshot();
        sample_ns.push(elapsed.as_nanos() / operations_per_sample as u128);
        total_allocations += after.count.saturating_sub(before.count);
        total_allocated_bytes += after.bytes.saturating_sub(before.bytes);
    }
    sample_ns.sort_unstable();
    let median_ns = sample_ns[sample_ns.len() / 2];
    let measured_operations = SAMPLES * operations_per_sample;
    println!(
        "{workload},{operation},{bars},{median_ns},{:.1},{:.1}",
        total_allocations as f64 / measured_operations as f64,
        total_allocated_bytes as f64 / measured_operations as f64,
    );
}

fn main() {
    let chart_candles = candles(BAR_COUNT);
    println!(
        "# pine-lite={} target={}-{} profile=bench bars={} update_bars={}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        std::env::consts::OS,
        BAR_COUNT,
        UPDATE_BAR_COUNT,
    );
    println!(
        "# rustc={} samples={} warmup_samples={} operations_per_sample={} update_operations_per_sample={}",
        rustc_version(),
        SAMPLES,
        WARMUP_SAMPLES,
        OPERATIONS_PER_SAMPLE,
        UPDATE_OPERATIONS_PER_SAMPLE,
    );
    // Allocated bytes are cumulative alloc/realloc request sizes, not peak live memory.
    println!("workload,operation,bars,median_ns,allocations_per_op,allocated_bytes_per_op");

    for workload in workloads() {
        let (header, script) =
            pine_lite::vet(workload.source).expect("benchmark workload must pass vet");
        black_box(header);
        let inputs = inputs_for(&workload, &chart_candles);
        let result = pine_lite::run(&script, &chart_candles, &inputs)
            .expect("benchmark workload must execute");
        assert!(
            !result.plots.is_empty(),
            "{} must produce a plot",
            workload.name
        );

        report(
            workload.name,
            "vet",
            BAR_COUNT,
            OPERATIONS_PER_SAMPLE,
            |_| {
                let vetted = pine_lite::vet(black_box(workload.source))
                    .expect("benchmark source must pass vet");
                black_box(vetted);
            },
        );
        report(
            workload.name,
            "run_vetted",
            BAR_COUNT,
            OPERATIONS_PER_SAMPLE,
            |_| {
                let output = pine_lite::run(
                    black_box(&script),
                    black_box(&chart_candles),
                    black_box(&inputs),
                )
                .expect("benchmark workload must execute");
                black_box(output);
            },
        );

        let update_candles = &mut chart_candles[..UPDATE_BAR_COUNT].to_vec();
        let mut update_inputs = inputs_for(&workload, update_candles);
        report(
            workload.name,
            "replay_after_last_bar_update",
            UPDATE_BAR_COUNT,
            UPDATE_OPERATIONS_PER_SAMPLE,
            |update| {
                let last = update_candles.last_mut().expect("non-empty update window");
                last.close += (update % 7) as f64 * 0.001;
                if workload.uses_hourly_pool {
                    let hourly = update_inputs
                        .series_pool
                        .get_mut("BTCUSDT@1H")
                        .expect("hourly pool was prepared");
                    let final_slot = hourly.last_mut().expect("non-empty hourly pool");
                    final_slot.close = last.close;
                    final_slot.high = final_slot.high.max(last.high);
                    final_slot.low = final_slot.low.min(last.low);
                }
                let output = pine_lite::run(&script, update_candles, &update_inputs)
                    .expect("updated workload must execute");
                black_box(output);
            },
        );
    }
}

fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unavailable".to_string())
}
