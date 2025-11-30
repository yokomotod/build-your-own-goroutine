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

thread_local! {
    static CURRENT_WORKER: UnsafeCell<Worker> = UnsafeCell::new(Worker::new());
}

fn current_worker() -> *mut Worker {
    CURRENT_WORKER.with(|w| w.get())
}

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

/// Called when a task completes
fn task_finished() {
    unsafe {
        let worker = current_worker();
        if let Some(ref mut task) = (*worker).current_task {
            task.finished = true;
        }
        (*worker).switch_to_scheduler();
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

/// N:1 Green Thread Worker (internal)
struct Worker {
    /// Queue of runnable tasks
    tasks: VecDeque<Task>,
    /// Context to return to when a task yields
    context: Context,
    /// Currently running task
    current_task: Option<Task>,
}

impl Worker {
    fn new() -> Self {
        Worker {
            tasks: VecDeque::new(),
            context: Context::default(),
            current_task: None,
        }
    }

    unsafe fn switch_to_scheduler(&mut self) {
        if let Some(ref mut task) = self.current_task {
            context_switch(&mut task.context, &self.context);
        }
    }
}

fn worker_loop() {
    unsafe {
        let worker = current_worker();

        loop {
            // Get task from queue
            let Some(task) = (*worker).tasks.pop_front() else {
                break;
            };

            // Run the task
            (*worker).current_task = Some(task);

            let task_ctx = &(*worker).current_task.as_ref().unwrap().context as *const Context;
            context_switch(&mut (*worker).context, task_ctx);

            // Task yielded or finished
            if let Some(task) = (*worker).current_task.take()
                && !task.finished
            {
                // Task yielded, put back to queue
                (*worker).tasks.push_back(task);
            }
            // If finished, just drop it
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
        let worker = current_worker();
        let task = Task::new(f);
        (*worker).tasks.push_back(task);
    }
}

/// Yield execution to another green thread
pub fn gosched() {
    unsafe {
        (*current_worker()).switch_to_scheduler();
    }
}

/// Start the runtime and run until all tasks complete
///
/// # Warning
/// Do not call this from within a running task.
pub fn start_runtime() {
    worker_loop();
}
