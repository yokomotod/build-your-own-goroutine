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

use crate::context::{Context, STACK_SIZE, context_switch, get_closure_ptr};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

/// A green thread task
struct Task {
    context: Context,
    #[allow(dead_code)]
    stack: Vec<u8>, // Keep stack alive
    finished: bool,
}

// Task needs to be Send because it's moved between threads via the global queue
unsafe impl Send for Task {}

impl Task {
    /// Create a new task with the given entry function
    fn new<F>(f: F) -> Self
    where
        F: FnOnce() + Send + 'static,
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
    F: FnOnce() + Send + 'static,
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
    let worker_ptr = CURRENT_WORKER.with(|w| *w.borrow());

    if let Some(worker) = worker_ptr {
        unsafe {
            if let Some(ref mut task) = (*worker).current_task {
                task.finished = true;
            }
            (*worker).switch_to_scheduler();
        }
    }
}

thread_local! {
    static CURRENT_WORKER: RefCell<Option<*mut Worker>> = const { RefCell::new(None) };
}

/// Per-thread worker state
struct Worker {
    /// Context to return to when a task yields
    context: Context,
    /// Currently running task
    current_task: Option<Task>,
    /// Reference to global queue for spawning new tasks
    global_queue: Arc<Mutex<GlobalQueue>>,
}

impl Worker {
    fn new(global_queue: Arc<Mutex<GlobalQueue>>) -> Self {
        Worker {
            context: Context::default(),
            current_task: None,
            global_queue,
        }
    }

    /// Spawn a new task from within this worker
    fn spawn<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let task = Task::new(f);
        self.global_queue.lock().unwrap().tasks.push_back(task);
    }

    unsafe fn switch_to_scheduler(&mut self) {
        if let Some(ref mut task) = self.current_task {
            context_switch(&mut task.context, &self.context);
        }
    }
}

/// Global task queue shared by all workers
struct GlobalQueue {
    /// Queue of runnable tasks
    tasks: VecDeque<Task>,
    /// Flag to signal shutdown
    shutdown: bool,
}

/// Global task queue
static GLOBAL_QUEUE: OnceLock<Arc<Mutex<GlobalQueue>>> = OnceLock::new();

/// Get or initialize the global queue
fn global_queue() -> Arc<Mutex<GlobalQueue>> {
    GLOBAL_QUEUE
        .get_or_init(|| {
            Arc::new(Mutex::new(GlobalQueue {
                tasks: VecDeque::new(),
                shutdown: false,
            }))
        })
        .clone()
}

fn worker_loop(worker_id: usize, global_queue: Arc<Mutex<GlobalQueue>>) {
    let mut worker = Worker::new(Arc::clone(&global_queue));

    // Register this worker in thread-local storage
    CURRENT_WORKER.with(|w| {
        *w.borrow_mut() = Some(&mut worker as *mut Worker);
    });

    loop {
        // Get task from global queue
        let task = {
            let mut queue = global_queue.lock().unwrap();

            if let Some(task) = queue.tasks.pop_front() {
                task
            } else {
                if queue.shutdown {
                    break;
                }
                queue.shutdown = true;
                break;
            }
        };

        // Run the task
        worker.current_task = Some(task);

        let task_ctx = &worker.current_task.as_ref().unwrap().context as *const Context;
        context_switch(&mut worker.context, task_ctx);

        // Task yielded or finished
        if let Some(task) = worker.current_task.take()
            && !task.finished
        {
            // Task yielded, put back to global queue
            global_queue.lock().unwrap().tasks.push_back(task);
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
    // If we're inside a worker, use the worker's spawn
    let worker_ptr = CURRENT_WORKER.with(|w| *w.borrow());
    if let Some(worker) = worker_ptr {
        unsafe {
            (*worker).spawn(f);
        }
        return;
    }

    // Otherwise, add to global queue
    let task = Task::new(f);
    global_queue().lock().unwrap().tasks.push_back(task);
}

/// Start the runtime and run until all tasks complete
///
/// Panics if called while already running.
pub fn start_runtime(num_threads: usize) {
    let global_queue = global_queue();

    let mut handles = Vec::new();

    for worker_id in 0..num_threads {
        let global_queue = Arc::clone(&global_queue);
        let handle = thread::spawn(move || {
            worker_loop(worker_id, global_queue);
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

/// Yield execution to another green thread
pub fn gosched() {
    let worker_ptr = CURRENT_WORKER.with(|w| *w.borrow());

    if let Some(worker) = worker_ptr {
        unsafe {
            (*worker).switch_to_scheduler();
        }
    }
}
