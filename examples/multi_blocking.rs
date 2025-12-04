//! File I/O example WITHOUT dynamic worker spawning (for comparison)
//!
//! Uses std::fs/std::io directly, so workers get blocked and can't run other tasks.

use mygoroutine::runtime::multi::{go, start_runtime};
use std::io::Read;
use std::time::Instant;

const NUM_THREADS: usize = 1;
const NUM_TASKS: usize = 10;
const READ_SIZE: usize = 100 * 1024 * 1024; // 100MB

fn read_urandom() {
    let mut file = std::fs::File::open("/dev/urandom").unwrap();
    let mut buf = vec![0u8; READ_SIZE];
    file.read_exact(&mut buf).unwrap();
}

fn main() {
    println!("=== M:N Runtime (NO dynamic worker spawning) ===\n");

    // Baseline: 1 task
    println!("--- Baseline: 1 task ---");
    go(read_urandom);
    let start = Instant::now();
    start_runtime(1);
    let baseline = start.elapsed();
    println!("1 task: {:?}\n", baseline);

    // Main test: 8 tasks
    println!("--- Main: {} tasks with {} initial workers ---", NUM_TASKS, NUM_THREADS);
    let start = Instant::now();
    for _ in 0..NUM_TASKS {
        go(read_urandom);
    }
    start_runtime(NUM_THREADS);
    let elapsed = start.elapsed();

    println!("\n{} tasks: {:?}", NUM_TASKS, elapsed);
    println!("Speedup: {:.2}x vs sequential", (baseline.as_secs_f64() * NUM_TASKS as f64) / elapsed.as_secs_f64());
}
