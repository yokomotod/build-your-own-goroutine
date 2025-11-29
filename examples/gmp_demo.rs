use mygoroutine::gmp::{go, gosched, start_runtime};

const NUM_PROCESSORS: usize = 4; // P: logical processors
const NUM_WORKERS: usize = 16; // M: OS threads

fn main() {
    go(|| {
        println!("Task 1: start");
        gosched();
        println!("Task 1: end");
    });

    go(|| {
        println!("Task 2: start");
        gosched();
        println!("Task 2: end");
    });

    go(|| {
        println!("Task 3: start");
        gosched();
        println!("Task 3: end");
    });

    println!(
        "Running GMP scheduler with {} processors...",
        NUM_PROCESSORS
    );
    start_runtime(NUM_PROCESSORS, NUM_WORKERS);
    println!("All tasks completed!");
}
