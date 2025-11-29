use mygoroutine::gmp::{go, io, start_runtime};
use std::time::Instant;

const NUM_PROCESSORS: usize = 4; // P: logical processors
const NUM_WORKERS: usize = 16; // M: OS threads
const NUM_TASKS: usize = 32;

fn main() {
    println!(
        "=== GMP Runtime (P={}, M={}) with blocking I/O ===",
        NUM_PROCESSORS, NUM_WORKERS
    );
    println!("Running {} tasks that each block for 100ms", NUM_TASKS);
    println!("Using io::sleep() which releases P during blocking");
    println!();

    let start = Instant::now();

    for i in 0..NUM_TASKS {
        go(move || {
            println!(
                "[{:>6.3}s] Task {} started",
                start.elapsed().as_secs_f64(),
                i
            );

            // Use GMP-aware sleep that releases P during blocking
            io::sleep(std::time::Duration::from_millis(100));

            println!("[{:>6.3}s] Task {} done", start.elapsed().as_secs_f64(), i);
        });
    }

    start_runtime(NUM_PROCESSORS, NUM_WORKERS);

    println!();
    println!("Total elapsed: {:?}", start.elapsed());
    println!();
    println!("M:N (4 threads, no handoff): ~800ms (32 tasks / 4 = 8 rounds)");
    println!("GMP (P=4, M=16):             ~200ms (32 tasks / 16 = 2 rounds)");
}
