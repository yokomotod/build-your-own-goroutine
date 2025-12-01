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

use crate::common::{Context, Task, context_switch, get_closure_ptr, prepare_stack};
use std::cell::RefCell;
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
            shutdown: false,
        })
    })
}

thread_local! {
    static CURRENT_WORKER: RefCell<Option<*mut Worker>> = const { RefCell::new(None) };
}

fn current_worker() -> *mut Worker {
    CURRENT_WORKER.with(|w| (*w.borrow()).unwrap())
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
    /// Flag to signal shutdown
    shutdown: bool,
}

/// Per-thread worker state
struct Worker {
    /// Context to return to when a task yields
    context: Context,
    /// Currently running task
    current_task: Option<Task>,
}

impl Worker {
    fn new() -> Self {
        Worker {
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

fn worker_loop(worker_id: usize) {
    let mut worker = Worker::new();
    let queue = global_queue();

    // Register this worker in thread-local storage
    CURRENT_WORKER.with(|w| {
        *w.borrow_mut() = Some(&mut worker as *mut Worker);
    });

    loop {
        // Get task from global queue
        let task = {
            let mut q = queue.lock().unwrap();

            if let Some(task) = q.tasks.pop_front() {
                task
            } else {
                if q.shutdown {
                    break;
                }
                q.shutdown = true;
                break;
            }
        };

        // Run the task
        worker.current_task = Some(task);

        let task_ctx = &worker.current_task.as_ref().unwrap().context;
        context_switch(&mut worker.context, task_ctx);

        // Task yielded or finished
        if let Some(task) = worker.current_task.take()
            && !task.finished
        {
            // Task yielded, put back to global queue
            queue.lock().unwrap().tasks.push_back(task);
        }
        // If finished, just drop it
    }

    // Cleanup
    CURRENT_WORKER.with(|w| {
        *w.borrow_mut() = None;
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
    unsafe {
        (*current_worker()).switch_to_scheduler();
    }
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
