//! Channel example demonstrating unbuffered channel communication
//!
//! Two goroutines exchange values through an unbuffered channel.
//! Sending blocks until a receiver is ready, and vice versa.

use mygoroutine::runtime::channel::{go, start_runtime, Chan};
use std::time::Instant;

const NUM_THREADS: usize = 2;

fn main() {
    println!("=== Channel: Unbuffered Channel Demo ===\n");

    let start = Instant::now();
    let ch = Chan::<i32>::new();

    // Sender goroutine
    let ch_send = ch.clone();
    go(move || {
        for i in 0..5 {
            println!(
                "[{:>6.3}s] sender: sending {}",
                start.elapsed().as_secs_f64(),
                i
            );
            ch_send.send(i);
            println!(
                "[{:>6.3}s] sender: sent {}",
                start.elapsed().as_secs_f64(),
                i
            );
        }
    });

    // Receiver goroutine
    let ch_recv = ch.clone();
    go(move || {
        for _ in 0..5 {
            let val = ch_recv.recv().unwrap();
            println!(
                "[{:>6.3}s] receiver: received {}",
                start.elapsed().as_secs_f64(),
                val
            );
        }
    });

    start_runtime(NUM_THREADS);

    println!("\nTotal elapsed: {:?}", start.elapsed());
}
