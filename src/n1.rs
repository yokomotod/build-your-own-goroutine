//! N:1 Green Thread Runtime
//!
//! Single-threaded green thread runtime using hand-written asm context switch.
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
        // Get the closure pointer from callee-saved register
        let f_ptr = get_closure_ptr();

        // Take ownership of the closure and run it
        let f = Box::from_raw(f_ptr as *mut F);
        f();
    }

    // Task finished - mark as done and yield back
    task_finished();
}

/// Called when a task completes
fn task_finished() {
    unsafe {
        let rt = runtime();
        (*rt).current_task_finished = true;
        (*rt).switch_to_scheduler();
    }
}

thread_local! {
    static RUNTIME: UnsafeCell<Runtime> = UnsafeCell::new(Runtime::new());
}

/// Get a raw pointer to the runtime (unsafe, but avoids RefCell borrow issues during context switch)
fn runtime() -> *mut Runtime {
    RUNTIME.with(|rt| rt.get())
}

/// N:1 Green Thread Runtime (internal)
struct Runtime {
    /// Queue of runnable tasks
    tasks: VecDeque<Task>,
    /// Context to return to when a task yields
    scheduler_context: Context,
    /// The currently running task (moved out of queue during execution)
    current_task: Option<Task>,
    /// Flag set by task_finished()
    current_task_finished: bool,
    /// Flag to track if run() is currently executing
    running: bool,
}

impl Runtime {
    fn new() -> Self {
        Runtime {
            tasks: VecDeque::new(),
            scheduler_context: Context::default(),
            current_task: None,
            current_task_finished: false,
            running: false,
        }
    }

    /// Switch from current task back to scheduler
    unsafe fn switch_to_scheduler(&mut self) {
        if let Some(ref mut task) = self.current_task {
            context_switch(&mut task.context, &self.scheduler_context);
        }
    }
}

/// Spawn a new green thread
///
/// Can be called either before `run()` to register initial tasks,
/// or from within a running task to spawn child tasks.
pub fn go<F>(f: F)
where
    F: FnOnce() + 'static,
{
    unsafe {
        let rt = runtime();
        let task = Task::new(f);
        (*rt).tasks.push_back(task);
    }
}

/// Start the runtime and run until all tasks complete
///
/// Panics if called while already running.
pub fn start_runtime() {
    unsafe {
        let rt = runtime();

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

            if task.finished {
                continue;
            }

            // Move task to current_task
            (*rt).current_task = Some(task);
            (*rt).current_task_finished = false;

            // Switch to the task
            let task_ctx = &(*rt).current_task.as_ref().unwrap().context as *const Context;
            context_switch(&mut (*rt).scheduler_context, task_ctx);

            // We're back! Task either yielded or finished
            if let Some(mut task) = (*rt).current_task.take() {
                if (*rt).current_task_finished {
                    task.finished = true;
                    // Task is dropped here
                } else {
                    // Task yielded, put it back in the queue
                    (*rt).tasks.push_back(task);
                }
            }
        }

        // Cleanup
        (*rt).running = false;
    }
}

/// Yield execution to another green thread
pub fn gosched() {
    unsafe {
        let rt = runtime();
        if (*rt).running {
            (*rt).switch_to_scheduler();
        }
    }
}
