use mygoroutine::n1::{Runtime, gosched};
use std::time::Instant;

fn main() {
    let start = Instant::now();
    println!(
        "[{:>8.3}s] Spawning many tasks with green threads...",
        start.elapsed().as_secs_f64()
    );

    let runtime = Runtime::new();
    let task_count = 100_000;

    for i in 0..task_count {
        runtime.go(move || {
            // Simple computation
            let _ = i * 2;
            gosched();
        });

        if (i + 1) % 10000 == 0 {
            println!(
                "[{:>8.3}s] Spawned {} tasks...",
                start.elapsed().as_secs_f64(),
                i + 1
            );
        }
    }

    println!(
        "[{:>8.3}s] Running scheduler...",
        start.elapsed().as_secs_f64()
    );
    runtime.run();
    println!(
        "[{:>8.3}s] Done! All {} tasks completed.",
        start.elapsed().as_secs_f64(),
        task_count
    );
}
