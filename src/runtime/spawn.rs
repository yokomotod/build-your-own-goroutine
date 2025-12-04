//! M:N Green Thread Runtime with Dynamic Worker Spawning
//!
//! Extends the basic M:N runtime with blocking I/O support.
//! When a task enters a blocking operation (like file I/O),
//! the runtime spawns a new worker to keep other tasks running.

use crate::common::{Context, Task, context_switch, get_closure_ptr, prepare_stack};
use std::cell::{RefCell, UnsafeCell};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread;

/// Global task queue
static GLOBAL_QUEUE: OnceLock<Mutex<GlobalQueue>> = OnceLock::new();

/// Get or initialize the global queue
fn global_queue() -> &'static Mutex<GlobalQueue> {
    GLOBAL_QUEUE.get_or_init(|| {
        Mutex::new(GlobalQueue {
            tasks: VecDeque::new(),
            idle_workers: 0,
            all_workers: 0,
            next_worker_id: 0,
        })
    })
}

/// Condition variable for worker wakeup
static WORKER_CONDVAR: OnceLock<Condvar> = OnceLock::new();

fn worker_condvar() -> &'static Condvar {
    WORKER_CONDVAR.get_or_init(Condvar::new)
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
            .finished = true;

        worker.switch_to_scheduler();
    });
}

/// Entry point for new tasks
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
    /// Number of idle workers (waiting on condvar)
    idle_workers: usize,
    /// Total number of workers
    all_workers: usize,
    /// Next worker ID to assign
    next_worker_id: usize,
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
        let task_ctx: *mut Context = {
            let mut task = self.current_task.borrow_mut();
            &mut task
                .as_mut()
                .expect("switch_to_scheduler called without current task")
                .context as *mut Context
        };

        let worker_ctx: *const Context = self.context.get();
        context_switch(task_ctx, worker_ctx);
    }
}

/// Check if the runtime should terminate
fn should_terminate(queue: &GlobalQueue) -> bool {
    queue.tasks.is_empty() && queue.idle_workers == queue.all_workers - 1
}

fn worker_loop(worker_id: usize) {
    let queue = global_queue();
    let condvar = worker_condvar();

    // Register this worker
    {
        let mut q = queue.lock().unwrap();
        q.all_workers += 1;
    }

    CURRENT_WORKER.with(|worker| {
        loop {
            // Get task from global queue
            let task = {
                let mut q = queue.lock().unwrap();

                if let Some(task) = q.tasks.pop_front() {
                    Some(task)
                } else {
                    // No runnable tasks - check for termination
                    if should_terminate(&q) {
                        condvar.notify_all();
                        q.all_workers -= 1;
                        return;
                    }

                    // Go idle and wait for work
                    q.idle_workers += 1;
                    let mut q = condvar.wait(q).unwrap();
                    q.idle_workers -= 1;

                    // After wakeup, check termination again
                    if should_terminate(&q) {
                        condvar.notify_all();
                        q.all_workers -= 1;
                        return;
                    }

                    q.tasks.pop_front()
                }
            };

            let Some(task) = task else {
                continue;
            };

            // Set current task
            *worker.current_task.borrow_mut() = Some(task);

            // Get pointers before context_switch
            let worker_ctx: *mut Context = worker.context.get();
            let task_ctx: *const Context = {
                let task = worker.current_task.borrow();
                &task.as_ref().unwrap().context as *const Context
            };

            context_switch(worker_ctx, task_ctx);

            // Task yielded or finished
            if let Some(task) = worker.current_task.borrow_mut().take()
                && !task.finished
            {
                queue.lock().unwrap().tasks.push_back(task);
            }
        }
    });

    println!("[Worker {}] Shutting down", worker_id);
}

/// Spawn a new worker thread
fn spawn_worker() {
    let queue = global_queue();
    let worker_id = {
        let mut q = queue.lock().unwrap();
        let id = q.next_worker_id;
        q.next_worker_id += 1;
        id
    };

    thread::spawn(move || {
        worker_loop(worker_id);
    });
}

/// Spawn a new green thread
pub fn go<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    let (stack, stack_top) = prepare_stack();
    let f_ptr = Box::into_raw(Box::new(f)) as u64;
    let context = Context::new(stack_top, task_entry::<F> as usize, f_ptr);
    let task = Task::new(context, stack);

    let queue = global_queue();
    let condvar = worker_condvar();

    let mut q = queue.lock().unwrap();
    q.tasks.push_back(task);

    if q.idle_workers > 0 {
        condvar.notify_one();
    }
}

/// Yield execution to another green thread
pub fn gosched() {
    CURRENT_WORKER.with(|worker| {
        worker.switch_to_scheduler();
    });
}

/// Start the runtime and run until all tasks complete
pub fn start_runtime(num_threads: usize) {
    {
        let mut q = global_queue().lock().unwrap();
        q.next_worker_id = num_threads;
    }

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

// ============================================================================
// Blocking I/O Support
// ============================================================================

/// Internal: called before entering a blocking operation.
/// Spawns a new worker if needed (like Go 1.0's entersyscall).
fn enter_blocking() {
    let queue = global_queue();

    let should_spawn = {
        let q = queue.lock().unwrap();
        !q.tasks.is_empty() && q.idle_workers == 0
    };

    if should_spawn {
        spawn_worker();
    }
}

/// Internal: called after returning from a blocking operation.
fn exit_blocking() {
    // No-op for now
}

/// I/O module providing blocking-aware wrappers.
pub mod io {
    use super::{enter_blocking, exit_blocking};
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::path::Path;

    pub fn read<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
        enter_blocking();
        let result = reader.read(buf);
        exit_blocking();
        result
    }

    pub fn read_to_end<R: Read>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<usize> {
        enter_blocking();
        let result = reader.read_to_end(buf);
        exit_blocking();
        result
    }

    pub fn read_exact<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<()> {
        enter_blocking();
        let result = reader.read_exact(buf);
        exit_blocking();
        result
    }

    pub fn read_file<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
        enter_blocking();
        let result = std::fs::read(path);
        exit_blocking();
        result
    }

    pub fn read_to_string<P: AsRef<Path>>(path: P) -> io::Result<String> {
        enter_blocking();
        let result = std::fs::read_to_string(path);
        exit_blocking();
        result
    }

    pub fn write<W: Write>(writer: &mut W, buf: &[u8]) -> io::Result<usize> {
        enter_blocking();
        let result = writer.write(buf);
        exit_blocking();
        result
    }

    pub fn write_all<W: Write>(writer: &mut W, buf: &[u8]) -> io::Result<()> {
        enter_blocking();
        let result = writer.write_all(buf);
        exit_blocking();
        result
    }

    pub fn write_file<P: AsRef<Path>>(path: P, contents: &[u8]) -> io::Result<()> {
        enter_blocking();
        let result = std::fs::write(path, contents);
        exit_blocking();
        result
    }

    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<File> {
        enter_blocking();
        let result = File::open(path);
        exit_blocking();
        result
    }

    pub fn create<P: AsRef<Path>>(path: P) -> io::Result<File> {
        enter_blocking();
        let result = File::create(path);
        exit_blocking();
        result
    }
}
