use mygoroutine::runtime::m1::{go, gosched, start_runtime};

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

    println!("Starting runtime...");
    start_runtime();
    println!("All tasks completed!");
}
