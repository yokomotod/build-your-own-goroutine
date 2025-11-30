//! N:1 Green Thread Runtime
//!
//! # Example
//!
//! ```no_run
//! use mygoroutine::n1::{go, start_runtime, gosched};
//!
//! go(|| {
//!     println!("Task 1");
//!     gosched();
//!     println!("Task 1 done");
//! });
//!
//! go(|| {
//!     println!("Task 2");
//! });
//!
//! start_runtime();
//! ```

use crate::context::{Context, STACK_SIZE, context_switch, get_closure_ptr};
use std::cell::UnsafeCell;
use std::collections::VecDeque;

/// A green thread task
struct Task {
    context: Context,
    #[allow(dead_code)]
    stack: Vec<u8>, // Keep stack alive
    finished: bool,
}

impl Task {
    /// Create a new task with the given entry function
    fn new<F>(f: F) -> Self
    where
        F: FnOnce() + 'static,
    {
        let mut stack = vec![0u8; STACK_SIZE];

        // Stack grows downward, so we start at the top
        let stack_top = stack.as_mut_ptr() as usize + STACK_SIZE;

        // Align stack to 16 bytes (required by ABI)
        let stack_top = stack_top & !0xF;

        // Box the closure and leak it to get a raw pointer
        let f_ptr = Box::into_raw(Box::new(f));

        let context = Context::new_for_task(stack_top, task_entry::<F> as usize, f_ptr as u64);

        Task {
            context,
            stack,
            finished: false,
        }
    }
}

/// Entry point for new tasks
///
/// The closure pointer is passed via a callee-saved register.
extern "C" fn task_entry<F>()
where
    F: FnOnce() + 'static,
{
    unsafe {
        let f_ptr = get_closure_ptr();

        let f = Box::from_raw(f_ptr as *mut F);
        f();
    }

    task_finished();
}

/// Called when a task completes
fn task_finished() {
    unsafe {
        let w = worker();
        if let Some(ref mut task) = (*w).current_task {
            task.finished = true;
        }
        (*w).switch_to_scheduler();
    }
}

thread_local! {
    static WORKER: UnsafeCell<Worker> = UnsafeCell::new(Worker::new());
}

/// Get a raw pointer to the worker (unsafe, but avoids RefCell borrow issues during context switch)
fn worker() -> *mut Worker {
    WORKER.with(|rt| rt.get())
}

/// N:1 Green Thread Worker (internal)
struct Worker {
    /// Queue of runnable tasks
    tasks: VecDeque<Task>,
    /// Context to return to when a task yields
    context: Context,
    /// Currently running task
    current_task: Option<Task>,
    /// Flag to track if run() is currently executing
    running: bool,
}

impl Worker {
    fn new() -> Self {
        Worker {
            tasks: VecDeque::new(),
            context: Context::default(),
            current_task: None,
            running: false,
        }
    }

    unsafe fn switch_to_scheduler(&mut self) {
        if let Some(ref mut task) = self.current_task {
            context_switch(&mut task.context, &self.context);
        }
    }
}

/// Spawn a new green thread
///
/// Can be called either before `start_runtime()` to register initial tasks,
/// or from within a running task to spawn child tasks.
pub fn go<F>(f: F)
where
    F: FnOnce() + 'static,
{
    unsafe {
        let rt = worker();
        let task = Task::new(f);
        (*rt).tasks.push_back(task);
    }
}

/// Start the runtime and run until all tasks complete
///
/// Panics if called while already running.
pub fn start_runtime() {
    unsafe {
        let rt = worker();

        // Check if already running
        if (*rt).running {
            panic!("run() called while already running");
        }
        (*rt).running = true;

        loop {
            // Get the next task
            let Some(task) = (*rt).tasks.pop_front() else {
                break;
            };

            // Move task to current_task
            (*rt).current_task = Some(task);

            // Switch to the task
            let task_ctx = &(*rt).current_task.as_ref().unwrap().context as *const Context;
            context_switch(&mut (*rt).context, task_ctx);

            // We're back! Task either yielded or finished
            if let Some(task) = (*rt).current_task.take()
                && !task.finished
            {
                // Task yielded, put it back in the queue
                (*rt).tasks.push_back(task);
            }
            // If finished, just drop it
        }

        // Cleanup
        (*rt).running = false;
    }
}

/// Yield execution to another green thread
pub fn gosched() {
    unsafe {
        let rt = worker();
        if (*rt).running {
            (*rt).switch_to_scheduler();
        }
    }
}
