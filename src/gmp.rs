//! GMP Green Thread Runtime
//!
//! Multi-threaded green thread runtime with Processor (P) and M-P handoff.
//! This is similar to Go's GMP scheduler model.
//!
//! # Example
//!
//! ```no_run
//! use mygoroutine::gmp::{go, start_runtime};
//!
//! const NUM_PROCESSORS: usize = 4;
//! const NUM_WORKERS: usize = 16;
//!
//! go(|| {
//!     println!("Task 1");
//! });
//!
//! go(|| {
//!     println!("Task 2");
//! });
//!
//! start_runtime(NUM_PROCESSORS, NUM_WORKERS);
//! ```

use crate::context::{Context, STACK_SIZE, context_switch};
use std::arch::asm;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ptr;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;

/// A green thread task (G in GMP)
struct Task {
    context: Context,
    #[allow(dead_code)]
    stack: Vec<u8>,
    finished: bool,
}

unsafe impl Send for Task {}

impl Task {
    fn new<F>(f: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        let mut stack = vec![0u8; STACK_SIZE];
        let stack_top = stack.as_mut_ptr() as usize + STACK_SIZE;
        let stack_top = stack_top & !0xF;
        let f_ptr = Box::into_raw(Box::new(f));
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

/// Processor (P in GMP) - holds a local task queue
struct Processor {
    local_queue: VecDeque<Task>,
}

unsafe impl Send for Processor {}

impl Processor {
    fn new() -> Self {
        Processor {
            local_queue: VecDeque::new(),
        }
    }
}

/// Per-thread worker state (M in GMP)
struct Worker {
    /// The processor this worker currently owns (None when blocked)
    processor: Option<Processor>,
    /// Scheduler context for this worker
    scheduler_context: Context,
    /// Currently running task
    current_task: Option<Task>,
    /// Flag set by task_finished()
    current_task_finished: bool,
    /// Reference to shared state
    shared: Arc<Shared>,
    /// Worker ID for debugging
    #[allow(dead_code)]
    id: usize,
}

impl Worker {
    fn new(id: usize, shared: Arc<Shared>) -> Self {
        Worker {
            processor: None,
            scheduler_context: Context::default(),
            current_task: None,
            current_task_finished: false,
            shared,
            id,
        }
    }

    unsafe fn switch_to_scheduler(&mut self) {
        if let Some(ref mut task) = self.current_task {
            context_switch(&mut task.context, &self.scheduler_context);
        }
    }

    /// Release the current processor back to the pool
    fn release_p(&mut self) {
        if let Some(p) = self.processor.take() {
            let mut shared = self.shared.state.lock().unwrap();
            shared.idle_processors.push_back(p);
            self.shared.p_available.notify_one();
        }
    }

    /// Acquire a processor from the pool (blocks until one is available)
    fn acquire_p(&mut self) {
        let mut shared = self.shared.state.lock().unwrap();
        loop {
            if let Some(p) = shared.idle_processors.pop_front() {
                self.processor = Some(p);
                return;
            }
            if shared.shutdown {
                return;
            }
            shared = self.shared.p_available.wait(shared).unwrap();
        }
    }

    /// Spawn a new task from within this worker
    fn spawn<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let task = Task::new(f);
        self.shared
            .state
            .lock()
            .unwrap()
            .global_queue
            .push_back(task);
    }
}

/// Shared state for the GMP runtime
struct SharedState {
    /// Pool of idle processors
    idle_processors: VecDeque<Processor>,
    /// Global task queue (for new tasks)
    global_queue: VecDeque<Task>,
    /// Flag to signal shutdown
    shutdown: bool,
    /// Number of active workers (not blocked)
    active_workers: usize,
}

struct Shared {
    state: Mutex<SharedState>,
    /// Signaled when a processor becomes available
    p_available: Condvar,
    /// Signaled when there might be work to do
    work_available: Condvar,
}

/// Global shared state
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

/// Get or initialize the global shared state
fn get_or_init_shared(num_processors: usize) -> Arc<Shared> {
    SHARED
        .get_or_init(|| {
            let mut idle_processors = VecDeque::new();
            for _ in 0..num_processors {
                idle_processors.push_back(Processor::new());
            }

            Arc::new(Shared {
                state: Mutex::new(SharedState {
                    idle_processors,
                    global_queue: VecDeque::new(),
                    shutdown: false,
                    active_workers: 0,
                }),
                p_available: Condvar::new(),
                work_available: Condvar::new(),
            })
        })
        .clone()
}

/// Get the shared state (panics if not initialized)
fn shared() -> Arc<Shared> {
    SHARED
        .get_or_init(|| {
            // Default: 4 processors
            let mut idle_processors = VecDeque::new();
            for _ in 0..4 {
                idle_processors.push_back(Processor::new());
            }

            Arc::new(Shared {
                state: Mutex::new(SharedState {
                    idle_processors,
                    global_queue: VecDeque::new(),
                    shutdown: false,
                    active_workers: 0,
                }),
                p_available: Condvar::new(),
                work_available: Condvar::new(),
            })
        })
        .clone()
}

fn worker_loop(worker_id: usize, shared: Arc<Shared>) {
    let mut worker = Worker::new(worker_id, Arc::clone(&shared));

    CURRENT_WORKER.with(|w| {
        *w.borrow_mut() = Some(&mut worker as *mut Worker);
    });

    loop {
        // Try to acquire a processor if we don't have one
        if worker.processor.is_none() {
            let mut state = shared.state.lock().unwrap();

            // Try to get a processor
            if let Some(p) = state.idle_processors.pop_front() {
                worker.processor = Some(p);
                state.active_workers += 1;
            } else if state.shutdown {
                break;
            } else if state.global_queue.is_empty() && state.active_workers == 0 {
                // No work left
                state.shutdown = true;
                shared.p_available.notify_all();
                shared.work_available.notify_all();
                break;
            } else {
                // Wait for a processor to become available
                let _state = shared.p_available.wait(state).unwrap();
                continue;
            }
        }

        // We have a processor, try to run tasks
        let p = worker.processor.as_mut().unwrap();

        // First check local queue
        if let Some(task) = p.local_queue.pop_front() {
            if !task.finished {
                run_task(&mut worker, task);
            }
            continue;
        }

        // Local queue empty, try global queue
        {
            let mut state = shared.state.lock().unwrap();
            if let Some(task) = state.global_queue.pop_front() {
                drop(state);
                run_task(&mut worker, task);
                continue;
            }

            // No tasks anywhere
            if state.shutdown {
                // Return processor and exit
                if let Some(p) = worker.processor.take() {
                    state.idle_processors.push_back(p);
                    state.active_workers -= 1;
                }
                shared.p_available.notify_all();
                break;
            }

            // Check if we should shutdown
            if state.global_queue.is_empty() {
                // Return processor to pool and decrement active count
                if let Some(p) = worker.processor.take() {
                    state.idle_processors.push_back(p);
                    state.active_workers -= 1;
                    shared.p_available.notify_one();
                }

                // Check if all work is done
                if state.active_workers == 0 && state.global_queue.is_empty() {
                    state.shutdown = true;
                    shared.p_available.notify_all();
                    shared.work_available.notify_all();
                    break;
                }
            }
        }
    }

    CURRENT_WORKER.with(|w| {
        *w.borrow_mut() = None;
    });

    println!("[Worker {}] Shutting down", worker_id);
}

fn run_task(worker: &mut Worker, task: Task) {
    worker.current_task = Some(task);
    worker.current_task_finished = false;

    let task_ctx = &worker.current_task.as_ref().unwrap().context as *const Context;
    context_switch(&mut worker.scheduler_context, task_ctx);

    if let Some(mut task) = worker.current_task.take() {
        if worker.current_task_finished {
            task.finished = true;
        } else {
            // Task yielded, put back in local queue
            if let Some(ref mut p) = worker.processor {
                p.local_queue.push_back(task);
            }
        }
    }
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

    // Otherwise, add to global shared queue
    let task = Task::new(f);
    let shared = shared();
    shared.state.lock().unwrap().global_queue.push_back(task);
    shared.work_available.notify_one();
}

/// Start the runtime and run until all tasks complete
///
/// Panics if called while already running.
pub fn start_runtime(num_processors: usize, num_workers: usize) {
    let shared = get_or_init_shared(num_processors);

    let mut handles = Vec::new();

    // Start worker threads (M in GMP model)
    for worker_id in 0..num_workers {
        let shared = Arc::clone(&shared);
        let handle = thread::spawn(move || {
            worker_loop(worker_id, shared);
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

/// Release the current processor (call before blocking operation)
pub fn release_p() {
    let worker_ptr = CURRENT_WORKER.with(|w| *w.borrow());

    if let Some(worker) = worker_ptr {
        unsafe {
            (*worker).release_p();
        }
    }
}

/// Acquire a processor (call after blocking operation)
pub fn acquire_p() {
    let worker_ptr = CURRENT_WORKER.with(|w| *w.borrow());

    if let Some(worker) = worker_ptr {
        unsafe {
            (*worker).acquire_p();
        }
    }
}

/// I/O module with blocking-aware wrappers
pub mod io {
    use super::{acquire_p, release_p};
    use std::thread;
    use std::time::Duration;

    /// Sleep that releases the processor during blocking
    pub fn sleep(duration: Duration) {
        release_p();
        thread::sleep(duration);
        acquire_p();
    }
}
