//! Channel example: producer-consumer with buffered channel and close
//!
//! A producer sends values into a buffered channel, then closes it.
//! A consumer receives values in a loop until the channel is closed.
//! This is equivalent to Go's `for v := range ch` pattern.

use mygoroutine::runtime::channel::{Chan, go, start_runtime};
use std::time::Instant;

const NUM_THREADS: usize = 2;

fn main() {
    println!("=== Channel Demo ===\n");

    let start = Instant::now();

    // --- Part 1: Unbuffered channel ---
    println!("--- Unbuffered channel ---\n");

    let ch = Chan::<i32>::new();

    let ch_send = ch.clone();
    go(move || {
        for i in 0..3 {
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
        ch_send.close();
        println!(
            "[{:>6.3}s] sender: closed channel",
            start.elapsed().as_secs_f64()
        );
    });

    let ch_recv = ch.clone();
    go(move || {
        // for v := range ch pattern
        while let Some(val) = ch_recv.recv() {
            println!(
                "[{:>6.3}s] receiver: received {}",
                start.elapsed().as_secs_f64(),
                val
            );
        }
        println!(
            "[{:>6.3}s] receiver: channel closed, exiting",
            start.elapsed().as_secs_f64()
        );
    });

    start_runtime(NUM_THREADS);

    println!("\n--- Buffered channel (cap=3) ---\n");

    let start2 = Instant::now();
    let ch = Chan::<i32>::with_capacity(3);

    let ch_send = ch.clone();
    go(move || {
        for i in 0..5 {
            println!(
                "[{:>6.3}s] sender: sending {}",
                start2.elapsed().as_secs_f64(),
                i
            );
            ch_send.send(i);
            println!(
                "[{:>6.3}s] sender: sent {} (buf fills without blocking up to cap)",
                start2.elapsed().as_secs_f64(),
                i
            );
        }
        ch_send.close();
        println!(
            "[{:>6.3}s] sender: closed channel",
            start2.elapsed().as_secs_f64()
        );
    });

    let ch_recv = ch.clone();
    go(move || {
        while let Some(val) = ch_recv.recv() {
            println!(
                "[{:>6.3}s] receiver: received {}",
                start2.elapsed().as_secs_f64(),
                val
            );
        }
        println!(
            "[{:>6.3}s] receiver: channel closed, exiting",
            start2.elapsed().as_secs_f64()
        );
    });

    start_runtime(NUM_THREADS);

    println!("\nDone!");
}
