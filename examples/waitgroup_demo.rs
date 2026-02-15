//! WaitGroup example demonstrating gopark/goready synchronization
//!
//! Shows that a task can wait for a group of other tasks to complete
//! using WaitGroup, which internally uses gopark (to park the waiter)
//! and goready (to wake it when the counter reaches zero).

use mygoroutine::runtime::timer::{go, sleep, start_runtime};
use mygoroutine::sync::WaitGroup;
use std::time::{Duration, Instant};

const NUM_THREADS: usize = 2;
const NUM_TASKS: usize = 5;
const SLEEP_MS: u64 = 100;

fn main() {
    println!("=== WaitGroup Demo ===\n");
    println!(
        "{} tasks sleeping {}ms each on {} workers\n",
        NUM_TASKS, SLEEP_MS, NUM_THREADS
    );

    let start = Instant::now();
    let wg = WaitGroup::new();

    // Spawn worker tasks
    for i in 0..NUM_TASKS {
        wg.add(1);
        let wg = wg.clone();
        go(move || {
            println!(
                "[{:>6.3}s] Task {} starting",
                start.elapsed().as_secs_f64(),
                i
            );

            sleep(Duration::from_millis(SLEEP_MS));

            println!(
                "[{:>6.3}s] Task {} done",
                start.elapsed().as_secs_f64(),
                i
            );
            wg.done();
        });
    }

    // Spawn a waiter task
    let wg2 = wg.clone();
    go(move || {
        println!(
            "[{:>6.3}s] Waiter: waiting for all tasks...",
            start.elapsed().as_secs_f64()
        );

        wg2.wait();

        println!(
            "[{:>6.3}s] Waiter: all tasks completed!",
            start.elapsed().as_secs_f64()
        );
    });

    start_runtime(NUM_THREADS);

    println!("\nTotal elapsed: {:?}", start.elapsed());
}
