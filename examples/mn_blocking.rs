use mygoroutine::mn::{go, start_runtime};
use std::thread;
use std::time::{Duration, Instant};

const NUM_THREADS: usize = 4;
const NUM_TASKS: usize = 32;

fn main() {
    println!(
        "=== M:N Runtime ({} threads) with blocking I/O ===",
        NUM_THREADS
    );
    println!("Running {} tasks that each block for 100ms", NUM_TASKS);
    println!();

    let start = Instant::now();

    for i in 0..NUM_TASKS {
        go(move || {
            println!(
                "[{:>6.3}s] Task {} started",
                start.elapsed().as_secs_f64(),
                i
            );

            // Simulate blocking I/O (e.g., file read, network request)
            thread::sleep(Duration::from_millis(100));

            println!("[{:>6.3}s] Task {} done", start.elapsed().as_secs_f64(), i);
        });
    }

    start_runtime(NUM_THREADS);

    println!();
    println!("Total elapsed: {:?}", start.elapsed());
    println!();
    println!("Expected if no blocking: ~100ms (all tasks run in parallel)");
    println!("Expected with blocking:  ~800ms (4 threads, 32 tasks, 8 rounds)");
    println!("Actual result shows threads are blocked during sleep!");
}
