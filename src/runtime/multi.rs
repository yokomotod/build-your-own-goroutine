//! M:N Green Thread Runtime
//!
//! # Example
//!
//! ```no_run
//! use mygoroutine::mn::{go, start_runtime, gosched};
//!
//! const NUM_THREADS: usize = 4;
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
//! start_runtime(NUM_THREADS);
//! ```

use crate::common::{Context, Task, TaskState, context_switch, get_closure_ptr, prepare_stack};
use std::cell::{RefCell, UnsafeCell};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::thread;

/// Global task queue
static GLOBAL_QUEUE: OnceLock<Mutex<GlobalQueue>> = OnceLock::new();

/// Get or initialize the global queue
fn global_queue() -> &'static Mutex<GlobalQueue> {
    GLOBAL_QUEUE.get_or_init(|| {
        Mutex::new(GlobalQueue {
            tasks: VecDeque::new(),
        })
    })
}

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
    F: FnOnce() + Send + 'static,
{
    let f = unsafe {
        let f_ptr = get_closure_ptr();
        Box::from_raw(f_ptr as *mut F)
    };
    f();

    task_finished();
}

/// Global task queue shared by all workers
struct GlobalQueue {
    /// Queue of runnable tasks
    tasks: VecDeque<Task>,
}

/// Per-thread worker state
struct Worker {
    /// Context to return to when a task yields
    context: UnsafeCell<Context>,
    /// Currently running task
    current_task: RefCell<Option<Task>>,
}

impl Worker {
    fn new() -> Self {
        Worker {
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

fn worker_loop(worker_id: usize) {
    let queue = global_queue();

    CURRENT_WORKER.with(|worker| {
        loop {
            // Get task from global queue
            let task = {
                let mut q = queue.lock().unwrap();

                if let Some(task) = q.tasks.pop_front() {
                    task
                } else {
                    break;
                }
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
            if let Some(task) = worker.current_task.borrow_mut().take() {
                if task.state != TaskState::Dead {
                    // Task yielded, put back to global queue
                    queue.lock().unwrap().tasks.push_back(task);
                }
            }
            // If finished, just drop it
        }
    });

    println!("[Worker {}] Shutting down", worker_id);
}

/// Spawn a new green thread
///
/// Can be called either before `start_runtime()` to register initial tasks,
/// or from within a running task to spawn child tasks.
pub fn go<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    let (stack, stack_top) = prepare_stack();
    let f_ptr = Box::into_raw(Box::new(f)) as u64;
    let context = Context::new(stack_top, task_entry::<F> as usize, f_ptr);
    let task = Task::new(context, stack);

    global_queue().lock().unwrap().tasks.push_back(task);
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
pub fn start_runtime(num_threads: usize) {
    let mut handles = Vec::new();

    for worker_id in 0..num_threads {
        let handle = thread::spawn(move || {
            worker_loop(worker_id);
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }
}
