//! Basic test for mn_poll module - testing gopark/goready without network I/O

use mygoroutine::runtime::poll::{current_task_id, go, gopark, goready, start_runtime, TaskId};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const NUM_THREADS: usize = 4;

fn main() {
    println!("=== mn_poll basic test: gopark/goready ===\n");

    let start = Instant::now();

    // Store task IDs that are parked, so another task can wake them
    let parked_tasks: Arc<Mutex<Vec<TaskId>>> = Arc::new(Mutex::new(Vec::new()));

    // Spawn tasks that will park themselves
    for i in 0..4 {
        let parked = parked_tasks.clone();
        go(move || {
            println!(
                "[{:>6.3}s] Task {} started, will park",
                start.elapsed().as_secs_f64(),
                i
            );

            // Get our ID before parking
            let my_id = current_task_id();
            parked.lock().unwrap().push(my_id);

            // Park ourselves
            gopark();

            println!(
                "[{:>6.3}s] Task {} woke up!",
                start.elapsed().as_secs_f64(),
                i
            );
        });
    }

    // Spawn a task that wakes up parked tasks after a delay
    let parked = parked_tasks.clone();
    go(move || {
        println!(
            "[{:>6.3}s] Waker task: sleeping 500ms then waking parked tasks",
            start.elapsed().as_secs_f64()
        );

        // Sleep to let other tasks park first
        thread::sleep(Duration::from_millis(500));

        // Wake up all parked tasks
        let tasks = parked.lock().unwrap().clone();
        println!(
            "[{:>6.3}s] Waker task: waking {} tasks",
            start.elapsed().as_secs_f64(),
            tasks.len()
        );

        for task_id in tasks {
            goready(task_id);
        }

        println!(
            "[{:>6.3}s] Waker task: done",
            start.elapsed().as_secs_f64()
        );
    });

    start_runtime(NUM_THREADS);

    println!("\nTotal elapsed: {:?}", start.elapsed());
    println!("Expected: ~500ms (wait for waker to sleep, then all wake up)");
}
