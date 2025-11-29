//! M:N Green Thread Runtime
//!
//! Multi-threaded green thread runtime using hand-written asm context switch.

use crate::context::{Context, STACK_SIZE, context_switch};
use std::arch::asm;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::thread;

/// A green thread task
struct Task {
    context: Context,
    #[allow(dead_code)]
    stack: Vec<u8>, // Keep stack alive
    finished: bool,
}

// Task needs to be Send because it's moved between threads via the shared queue
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

        // Align stack to 16 bytes (required by System V ABI)
        let stack_top = stack_top & !0xF;

        // Box the closure and leak it to get a raw pointer
        let f_ptr = Box::into_raw(Box::new(f));

        // Set up initial stack (same as n1)
        let initial_rsp = stack_top - 16;

        unsafe {
            ptr::write(initial_rsp as *mut u64, task_entry::<F> as usize as u64);
        }

        let context = Context {
            rsp: initial_rsp as u64,
            r15: f_ptr as u64,
            ..Default::default()
        };

        Task {
            context,
            stack,
            finished: false,
        }
    }
}

/// Entry point for new tasks
extern "C" fn task_entry<F>()
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        let f_ptr: u64;
        asm!(
            "mov {}, r15",
            out(reg) f_ptr,
            options(nomem, nostack, preserves_flags)
        );

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
            (*worker).current_task_finished = true;
            (*worker).switch_to_scheduler();
        }
    }
}

thread_local! {
    static CURRENT_WORKER: RefCell<Option<*mut Worker>> = const { RefCell::new(None) };
}

/// Per-thread worker state
struct Worker {
    /// Local queue of tasks
    local_tasks: VecDeque<Task>,
    /// Scheduler context for this worker
    scheduler_context: Context,
    /// Currently running task
    current_task: Option<Task>,
    /// Flag set by task_finished()
    current_task_finished: bool,
}

impl Worker {
    fn new() -> Self {
        Worker {
            local_tasks: VecDeque::new(),
            scheduler_context: Context::default(),
            current_task: None,
            current_task_finished: false,
        }
    }

    unsafe fn switch_to_scheduler(&mut self) {
        if let Some(ref mut task) = self.current_task {
            context_switch(&mut task.context, &self.scheduler_context);
        }
    }
}

/// Shared state for the M:N runtime
struct SharedQueue {
    /// Queue of pending tasks
    pending: VecDeque<Task>,
    /// Flag to signal shutdown
    shutdown: bool,
}

pub struct Runtime {
    num_threads: usize,
    shared: Arc<Mutex<SharedQueue>>,
}

impl Runtime {
    pub fn new(num_threads: usize) -> Self {
        Runtime {
            num_threads,
            shared: Arc::new(Mutex::new(SharedQueue {
                pending: VecDeque::new(),
                shutdown: false,
            })),
        }
    }

    pub fn go<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let task = Task::new(f);
        let mut queue = self.shared.lock().unwrap();
        queue.pending.push_back(task);
    }

    pub fn run(&self) {
        let mut handles = Vec::new();

        for worker_id in 0..self.num_threads {
            let shared = Arc::clone(&self.shared);
            let handle = thread::spawn(move || {
                worker_loop(worker_id, shared);
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }
}

fn worker_loop(worker_id: usize, shared: Arc<Mutex<SharedQueue>>) {
    let mut worker = Worker::new();

    // Register this worker in thread-local storage
    CURRENT_WORKER.with(|w| {
        *w.borrow_mut() = Some(&mut worker as *mut Worker);
    });

    loop {
        // First, try to run local tasks
        if let Some(task) = worker.local_tasks.pop_front() {
            if task.finished {
                continue;
            }

            worker.current_task = Some(task);
            worker.current_task_finished = false;

            let task_ctx = &worker.current_task.as_ref().unwrap().context as *const Context;
            context_switch(&mut worker.scheduler_context, task_ctx);

            if let Some(mut task) = worker.current_task.take() {
                if worker.current_task_finished {
                    task.finished = true;
                } else {
                    worker.local_tasks.push_back(task);
                }
            }
            continue;
        }

        // No local tasks, try to get from shared queue
        let mut queue = shared.lock().unwrap();

        if let Some(task) = queue.pending.pop_front() {
            drop(queue);
            worker.local_tasks.push_back(task);
            continue;
        }

        if queue.shutdown {
            break;
        }

        queue.shutdown = true;
        break;
    }

    // Cleanup
    CURRENT_WORKER.with(|w| {
        *w.borrow_mut() = None;
    });

    println!("[Worker {}] Shutting down", worker_id);
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
