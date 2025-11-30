use mygoroutine::n1::{go, start_runtime};
use std::hint::black_box;
use std::time::Instant;

/// CPU-intensive work: compute sum of squares
fn cpu_work(n: u64) -> u64 {
    let mut sum = 0u64;
    for i in 0..n {
        sum = sum.wrapping_add(black_box(i).wrapping_mul(black_box(i)));
    }
    sum
}

fn main() {
    let work_size = 7_500_000_000u64;
    let num_tasks = 8;

    println!("=== N:1 Runtime (single thread) ===");
    println!("Running {} tasks with work_size = {}", num_tasks, work_size);
    println!("Watch CPU usage with: htop or top");
    println!();

    let start = Instant::now();

    for i in 0..num_tasks {
        go(move || {
            let result = cpu_work(work_size);
            println!("Task {}: result = {}", i, result);
        });
    }

    start_runtime();
    println!();
    println!("Elapsed: {:?}", start.elapsed());
}
