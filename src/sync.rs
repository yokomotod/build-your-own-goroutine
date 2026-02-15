//! Synchronization primitives built on gopark/goready.
//!
//! WaitGroup: A counting semaphore for waiting on a group of tasks to finish.
//! Mirrors Go's sync.WaitGroup, backed by gopark (to park waiters) and
//! goready (to wake them when the counter reaches zero).

use crate::common::TaskId;
use crate::runtime::timer::{current_task_id, gopark, goready};
use std::sync::{Arc, Mutex};

/// A counting semaphore for waiting on a group of tasks to finish.
///
/// # How it works
///
/// - `add(delta)` adjusts the internal counter.
/// - `done()` decrements the counter by 1 (equivalent to `add(-1)`).
/// - `wait()` parks the calling task (via gopark) until the counter reaches zero.
/// - When the counter hits zero, all parked waiters are woken (via goready).
///
/// This is the same pattern as Go's sync.WaitGroup:
///   Wait → gopark, Done → counter==0 → goready × waiters
///
/// # Example
///
/// ```ignore
/// let wg = WaitGroup::new();
/// for i in 0..5 {
///     wg.add(1);
///     let wg = wg.clone();
///     go(move || {
///         // ... do work ...
///         wg.done();
///     });
/// }
/// // In another task:
/// wg.wait(); // blocks until all 5 tasks call done()
/// ```
#[derive(Clone)]
pub struct WaitGroup {
    inner: Arc<Mutex<WaitGroupInner>>,
}

struct WaitGroupInner {
    counter: i32,
    waiters: Vec<TaskId>,
}

impl Default for WaitGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitGroup {
    /// Create a new WaitGroup with counter 0.
    pub fn new() -> Self {
        WaitGroup {
            inner: Arc::new(Mutex::new(WaitGroupInner {
                counter: 0,
                waiters: Vec::new(),
            })),
        }
    }

    /// Add delta to the counter. If the counter reaches zero,
    /// all waiters are woken up.
    ///
    /// Panics if the counter goes negative.
    pub fn add(&self, delta: i32) {
        let wake_list = {
            let mut inner = self.inner.lock().unwrap();
            inner.counter += delta;

            if inner.counter < 0 {
                panic!("sync: negative WaitGroup counter");
            }

            if inner.counter == 0 && !inner.waiters.is_empty() {
                // Counter hit zero - collect all waiters to wake up
                std::mem::take(&mut inner.waiters)
            } else {
                Vec::new()
            }
            // Lock is released here, before calling goready
        };

        // Wake all waiters outside the lock
        for task_id in wake_list {
            goready(task_id);
        }
    }

    /// Decrement the counter by 1. Equivalent to `add(-1)`.
    pub fn done(&self) {
        self.add(-1);
    }

    /// Block the current task until the counter reaches zero.
    ///
    /// If the counter is already zero, returns immediately.
    pub fn wait(&self) {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.counter == 0 {
                return;
            }
            // Register as waiter before releasing the lock.
            // If goready fires before gopark completes, the pre_ready mechanism
            // in the runtime will catch it and put the task straight back to runnable.
            inner.waiters.push(current_task_id());
        }
        // Park until woken by done() → goready
        gopark();
    }
}
