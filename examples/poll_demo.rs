//! Network polling example using TCP connections
//!
//! This example demonstrates that network I/O doesn't block workers.
//! Unlike thread::sleep which blocks the worker, net_poll_read uses epoll
//! to park the task and allow other tasks to run.

use mygoroutine::runtime::poll::{go, net_poll_read, start_runtime};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

const NUM_THREADS: usize = 4;
const NUM_TASKS: usize = 32;

fn main() {
    println!("=== mn_poll Network I/O Demo ===");
    println!(
        "Running {} tasks with network I/O on {} workers\n",
        NUM_TASKS, NUM_THREADS
    );

    // Start a simple echo server in a separate thread
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    println!("Echo server listening on {}\n", addr);

    let server_handle = thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            thread::spawn(move || {
                // Wait 100ms before echoing (simulate slow server)
                thread::sleep(Duration::from_millis(100));
                let mut buf = [0u8; 64];
                let n = stream.read(&mut buf).unwrap();
                stream.write_all(&buf[..n]).unwrap();
            });
        }
    });

    let start = Instant::now();

    // Spawn tasks that do network I/O
    for i in 0..NUM_TASKS {
        let addr = addr;
        go(move || {
            println!(
                "[{:>6.3}s] Task {} connecting...",
                start.elapsed().as_secs_f64(),
                i
            );

            let mut stream = TcpStream::connect(addr).unwrap();
            stream.set_nonblocking(true).unwrap();

            // Send request
            let msg = format!("Hello from task {}", i);
            // For non-blocking write, we might get WouldBlock, but for small writes it usually succeeds
            stream.write_all(msg.as_bytes()).ok();

            // Wait for response using epoll
            net_poll_read(&stream);

            // Read response
            let mut buf = [0u8; 64];
            match stream.read(&mut buf) {
                Ok(n) => {
                    let response = String::from_utf8_lossy(&buf[..n]);
                    println!(
                        "[{:>6.3}s] Task {} received: {}",
                        start.elapsed().as_secs_f64(),
                        i,
                        response
                    );
                }
                Err(e) => {
                    println!(
                        "[{:>6.3}s] Task {} read error: {}",
                        start.elapsed().as_secs_f64(),
                        i,
                        e
                    );
                }
            }
        });
    }

    start_runtime(NUM_THREADS);

    println!("\nTotal elapsed: {:?}", start.elapsed());
    println!();
    println!("Expected with blocking (thread::sleep): ~800ms (4 threads, 32 tasks, 8 rounds)");
    println!("Expected with epoll: ~100ms (all tasks wait concurrently)");
    println!("The difference shows that network I/O doesn't block workers!");

    drop(server_handle); // Server thread continues until process exits
}
