use mygoroutine::runtime::multi::{go, gosched, start_runtime};

const NUM_THREADS: usize = 4;

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

    println!("Starting runtime with {} threads...", NUM_THREADS);
    start_runtime(NUM_THREADS);
    println!("All tasks completed!");
}
