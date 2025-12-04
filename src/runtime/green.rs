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

use crate::common::{Context, Task, TaskState, context_switch, get_closure_ptr, prepare_stack};
use std::cell::{RefCell, UnsafeCell};
use std::collections::VecDeque;

thread_local! {
    static CURRENT_WORKER: Worker = Worker::new();
}

/// Called when a task completes
fn task_finished() {
    CURRENT_WORKER.with(|worker| {
        worker
            .current_task
            .borrow_mut()
            .as_mut()
            .expect("task_finished called without current task")
            .state = TaskState::Dead;

        worker.switch_to_scheduler();
    });
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
    tasks: RefCell<VecDeque<Task>>,
    /// Context to return to when a task yields
    context: UnsafeCell<Context>,
    /// Currently running task
    current_task: RefCell<Option<Task>>,
}

impl Worker {
    fn new() -> Self {
        Worker {
            tasks: RefCell::new(VecDeque::new()),
            context: UnsafeCell::new(Context::default()),
            current_task: RefCell::new(None),
        }
    }

    fn switch_to_scheduler(&self) {
        // Get pointers before context_switch (to avoid holding RefCell borrow across switch)
        let task_ctx: *mut Context = {
            let mut task = self.current_task.borrow_mut();
            &mut task
                .as_mut()
                .expect("switch_to_scheduler called without current task")
                .context as *mut Context
        }; // RefMut is dropped here

        let worker_ctx: *const Context = self.context.get();

        // Note: We use raw pointers because context_switch requires simultaneous
        // access to two Contexts, which Rust's borrow checker cannot express.
        context_switch(task_ctx, worker_ctx);
    }
}

fn worker_loop() {
    CURRENT_WORKER.with(|worker| {
        loop {
            // Get task from queue (borrow ends immediately)
            let Some(task) = worker.tasks.borrow_mut().pop_front() else {
                break;
            };

            // Set current task (borrow ends immediately)
            *worker.current_task.borrow_mut() = Some(task);

            // Get pointers before context_switch
            let worker_ctx: *mut Context = worker.context.get();
            let task_ctx: *const Context = {
                let task = worker.current_task.borrow();
                &task.as_ref().unwrap().context as *const Context
            }; // Ref is dropped here

            // Note: We use raw pointers because context_switch requires simultaneous
            // access to two Contexts, which Rust's borrow checker cannot express.
            context_switch(worker_ctx, task_ctx);

            // Task yielded or finished (borrow ends immediately)
            if let Some(task) = worker.current_task.borrow_mut().take()
                && task.state != TaskState::Dead
            {
                // Task yielded, put back to queue
                worker.tasks.borrow_mut().push_back(task);
            }
            // If finished, just drop it
        }
    });
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

    CURRENT_WORKER.with(|worker| {
        worker.tasks.borrow_mut().push_back(task);
    });
}

/// Yield execution to another green thread
pub fn gosched() {
    CURRENT_WORKER.with(|worker| {
        worker.switch_to_scheduler();
    });
}

/// Start the runtime and run until all tasks complete
///
/// # Warning
/// Do not call this from within a running task.
pub fn start_runtime() {
    worker_loop();
}
