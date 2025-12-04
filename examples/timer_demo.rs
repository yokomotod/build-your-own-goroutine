//! Timer example demonstrating gosleep()
//!
//! Shows that multiple tasks can sleep concurrently without
//! blocking workers, unlike thread::sleep.

use mygoroutine::runtime::timer::{go, sleep, start_runtime};
use std::time::{Duration, Instant};

const NUM_THREADS: usize = 2;
const NUM_TASKS: usize = 10;
const SLEEP_MS: u64 = 100;

fn main() {
    println!("=== mn_poll_timer: gosleep Demo ===\n");
    println!(
        "{} tasks sleeping {}ms each on {} workers\n",
        NUM_TASKS, SLEEP_MS, NUM_THREADS
    );

    let start = Instant::now();

    for i in 0..NUM_TASKS {
        go(move || {
            println!(
                "[{:>6.3}s] Task {} starting sleep",
                start.elapsed().as_secs_f64(),
                i
            );

            sleep(Duration::from_millis(SLEEP_MS));

            println!(
                "[{:>6.3}s] Task {} woke up",
                start.elapsed().as_secs_f64(),
                i
            );
        });
    }

    start_runtime(NUM_THREADS);

    println!("\nTotal elapsed: {:?}", start.elapsed());
    println!();
    println!(
        "With thread::sleep: ~{}ms ({} tasks / {} workers * {}ms)",
        NUM_TASKS / NUM_THREADS * SLEEP_MS as usize,
        NUM_TASKS,
        NUM_THREADS,
        SLEEP_MS
    );
    println!("With gosleep: ~{}ms (all tasks sleep concurrently)", SLEEP_MS);
}
