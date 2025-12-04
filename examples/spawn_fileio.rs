//! File I/O example demonstrating dynamic worker spawning
//!
//! Uses the io module's blocking-aware wrappers to automatically
//! spawn new workers when file I/O blocks.

use mygoroutine::runtime::spawn::{go, io, start_runtime};
use std::time::Instant;

const NUM_THREADS: usize = 1;
const NUM_TASKS: usize = 10;
const READ_SIZE: usize = 100 * 1024 * 1024; // 100MB

fn read_urandom() {
    let mut file = io::open("/dev/urandom").unwrap();
    let mut buf = vec![0u8; READ_SIZE];
    io::read_exact(&mut file, &mut buf).unwrap();
}

fn main() {
    println!("=== spawn: File I/O with dynamic worker spawning ===\n");

    // Baseline: 1 task
    println!("--- Baseline: 1 task ---");
    go(read_urandom);
    let start = Instant::now();
    start_runtime(1);
    let baseline = start.elapsed();
    println!("1 task: {:?}\n", baseline);

    // Main test: multiple tasks
    println!(
        "--- Main: {} tasks with {} initial workers ---",
        NUM_TASKS, NUM_THREADS
    );
    let start = Instant::now();
    for _ in 0..NUM_TASKS {
        go(read_urandom);
    }
    start_runtime(NUM_THREADS);
    let elapsed = start.elapsed();

    println!("\n{} tasks: {:?}", NUM_TASKS, elapsed);
    println!(
        "Speedup: {:.2}x vs sequential",
        (baseline.as_secs_f64() * NUM_TASKS as f64) / elapsed.as_secs_f64()
    );
}
