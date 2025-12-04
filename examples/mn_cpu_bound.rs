use mygoroutine::runtime::mn::{go, start_runtime};
use std::hint::black_box;
use std::time::Instant;

const NUM_THREADS: usize = 4;
const NUM_TASKS: usize = 8;
const WORK_SIZE: u64 = 7_500_000_000;

/// CPU-intensive work: compute sum of squares
fn cpu_work(n: u64) -> u64 {
    let mut sum = 0u64;
    for i in 0..n {
        sum = sum.wrapping_add(black_box(i).wrapping_mul(black_box(i)));
    }
    sum
}

fn main() {
    println!("=== M:N Runtime ({} threads) ===", NUM_THREADS);
    println!("Running {} tasks with work_size = {}", NUM_TASKS, WORK_SIZE);
    println!("Watch CPU usage with: htop or top");
    println!();

    let start = Instant::now();

    for i in 0..NUM_TASKS {
        go(move || {
            let result = cpu_work(WORK_SIZE);
            println!("Task {}: result = {}", i, result);
        });
    }

    start_runtime(NUM_THREADS);
    println!();
    println!("Elapsed: {:?}", start.elapsed());
}
