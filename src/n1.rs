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

use crate::common::{Context, Task, context_switch, get_closure_ptr, prepare_stack};
use std::cell::UnsafeCell;
use std::collections::VecDeque;

thread_local! {
    static CURRENT_WORKER: UnsafeCell<Worker> = UnsafeCell::new(Worker::new());
}

fn current_worker() -> *mut Worker {
    CURRENT_WORKER.with(|w| w.get())
}

/// Called when a task completes
fn task_finished() {
    unsafe {
        let worker = current_worker();
        (*worker)
            .current_task
            .as_mut()
            .expect("task_finished called without current task")
            .finished = true;
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
    let f = unsafe {
        let f_ptr = get_closure_ptr();
        Box::from_raw(f_ptr as *mut F)
    };
    f();

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
        let task = self
            .current_task
            .as_mut()
            .expect("switch_to_scheduler called without current task");
        context_switch(&mut task.context, &self.context);
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

            let task_ctx = &(*worker).current_task.as_ref().unwrap().context;
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
    let (stack, stack_top) = prepare_stack();
    let f_ptr = Box::into_raw(Box::new(f)) as u64;
    let context = Context::new(stack_top, task_entry::<F> as usize, f_ptr);
    let task = Task::new(context, stack);

    unsafe {
        let worker = current_worker();
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
