use mygoroutine::n1::{Runtime, gosched};

fn main() {
    let runtime = Runtime::new();

    runtime.go(|| {
        println!("Task 1: start");
        gosched();
        println!("Task 1: end");
    });

    runtime.go(|| {
        println!("Task 2: start");
        gosched();
        println!("Task 2: end");
    });

    runtime.go(|| {
        println!("Task 3: start");
        gosched();
        println!("Task 3: end");
    });

    println!("Running scheduler...");
    runtime.run();
    println!("All tasks completed!");
}
