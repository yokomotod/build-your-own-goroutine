//! M:N Green Thread Runtime with Network Polling
//!
//! Extends the basic M:N runtime with:
//! - gopark/goready for cooperative waiting
//! - Network I/O polling with epoll (Linux) or kqueue (macOS/BSD)
//! - Workers that sleep when idle (instead of terminating)

use crate::common::{
    Context, Task, TaskId, TaskState, Worker, context_switch, get_closure_ptr, prepare_stack,
};
use crate::netpoll;
use std::collections::{HashMap, VecDeque};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread;

/// Global task queue
static GLOBAL_QUEUE: OnceLock<Mutex<GlobalQueue>> = OnceLock::new();

/// Get or initialize the global queue
fn global_queue() -> &'static Mutex<GlobalQueue> {
    GLOBAL_QUEUE.get_or_init(|| {
        Mutex::new(GlobalQueue {
            runnable: VecDeque::new(),
            waiting: HashMap::new(),
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
    runnable: VecDeque<Task>,
    /// Tasks waiting for I/O or other events
    waiting: HashMap<TaskId, Task>,
    /// Number of idle workers (waiting on condvar)
    idle_workers: usize,
    /// Total number of workers
    all_workers: usize,
    /// Next worker ID to assign
    next_worker_id: usize,
}

/// Check if the runtime should terminate
fn should_terminate(queue: &GlobalQueue) -> bool {
    // Terminate when:
    // - No runnable tasks
    // - No waiting tasks (including network waiters)
    // - All workers are idle (except us, who is checking)
    queue.runnable.is_empty()
        && queue.waiting.is_empty()
        && !network_poller().has_waiters()
        && queue.idle_workers == queue.all_workers - 1
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

                if let Some(mut task) = q.runnable.pop_front() {
                    task.state = TaskState::Running;
                    Some(task)
                } else {
                    // No runnable tasks - check for termination
                    if should_terminate(&q) {
                        // Wake up all other workers so they can also terminate
                        condvar.notify_all();
                        q.all_workers -= 1;
                        return; // Exit the closure, ending the loop
                    }

                    // Drop lock before polling (polling may block)
                    drop(q);

                    // Try to become the poller
                    if try_poll_network() {
                        // We did some polling, there might be work now
                        // Loop back to check runnable queue
                        None
                    } else {
                        // Another worker is polling, or no network waiters
                        // Go idle and wait for work
                        let mut q = queue.lock().unwrap();
                        q.idle_workers += 1;
                        let mut q = condvar.wait(q).unwrap();
                        q.idle_workers -= 1;

                        // After wakeup, check termination again
                        if should_terminate(&q) {
                            condvar.notify_all();
                            q.all_workers -= 1;
                            return;
                        }

                        // Try to get a task
                        if let Some(mut task) = q.runnable.pop_front() {
                            task.state = TaskState::Running;
                            Some(task)
                        } else {
                            None
                        }
                    }
                }
            };

            let Some(task) = task else {
                // Spurious wakeup, loop again
                continue;
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
            if let Some(mut task) = worker.current_task.borrow_mut().take() {
                match task.state {
                    TaskState::Dead => {
                        // Task finished, drop it
                    }
                    TaskState::Waiting => {
                        // gopark was called - move task to waiting map
                        let mut q = queue.lock().unwrap();
                        q.waiting.insert(task.id, task);
                    }
                    _ => {
                        // Normal yield (gosched), put back to runnable queue
                        task.state = TaskState::Runnable;
                        queue.lock().unwrap().runnable.push_back(task);
                    }
                }
            }
        }
    });

    println!("[Worker {}] Shutting down", worker_id);
}

/// Park the current task (move to waiting state)
///
/// The task will be woken up by calling `goready(task_id)`.
pub fn gopark() {
    CURRENT_WORKER.with(|worker| {
        {
            let mut current = worker.current_task.borrow_mut();
            let task = current
                .as_mut()
                .expect("gopark called without current task");
            task.state = TaskState::Waiting;
        }

        // Switch to scheduler - it will move this task to waiting map
        worker.switch_to_scheduler();
    })
}

/// Wake up a parked task (move back to runnable state)
pub fn goready(task_id: TaskId) {
    let queue = global_queue();
    let condvar = worker_condvar();

    let mut q = queue.lock().unwrap();

    if let Some(mut task) = q.waiting.remove(&task_id) {
        task.state = TaskState::Runnable;
        q.runnable.push_back(task);

        // Wake up an idle worker if any
        if q.idle_workers > 0 {
            condvar.notify_one();
        }
    }
}

/// Get the current task's ID
pub fn current_task_id() -> TaskId {
    CURRENT_WORKER.with(|worker| {
        worker
            .current_task
            .borrow()
            .as_ref()
            .expect("current_task_id called outside of task")
            .id
    })
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

    let queue = global_queue();
    let condvar = worker_condvar();

    let mut q = queue.lock().unwrap();
    q.runnable.push_back(task);

    // Wake up an idle worker if any
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
///
/// # Warning
/// Do not call this from within a running task.
pub fn start_runtime(num_threads: usize) {
    // Set initial worker IDs
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

    // Wait for initial workers
    // Note: dynamically spawned workers are detached (not joined)
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
        // Spawn a new worker if:
        // - There are runnable tasks waiting
        // - No idle workers to pick them up
        !q.runnable.is_empty() && q.idle_workers == 0
    };

    if should_spawn {
        spawn_worker();
    }
}

/// I/O module providing blocking-aware wrappers.
pub mod io {
    use super::enter_blocking;
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::path::Path;

    pub fn read<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
        enter_blocking();
        reader.read(buf)
    }

    pub fn read_to_end<R: Read>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<usize> {
        enter_blocking();
        reader.read_to_end(buf)
    }

    pub fn read_exact<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<()> {
        enter_blocking();
        reader.read_exact(buf)
    }

    pub fn read_file<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
        enter_blocking();
        std::fs::read(path)
    }

    pub fn read_to_string<P: AsRef<Path>>(path: P) -> io::Result<String> {
        enter_blocking();
        std::fs::read_to_string(path)
    }

    pub fn write<W: Write>(writer: &mut W, buf: &[u8]) -> io::Result<usize> {
        enter_blocking();
        writer.write(buf)
    }

    pub fn write_all<W: Write>(writer: &mut W, buf: &[u8]) -> io::Result<()> {
        enter_blocking();
        writer.write_all(buf)
    }

    pub fn write_file<P: AsRef<Path>>(path: P, contents: &[u8]) -> io::Result<()> {
        enter_blocking();
        std::fs::write(path, contents)
    }

    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<File> {
        enter_blocking();
        File::open(path)
    }

    pub fn create<P: AsRef<Path>>(path: P) -> io::Result<File> {
        enter_blocking();
        File::create(path)
    }
}

// ============================================================================
// Network Poller
// ============================================================================

/// Flag to indicate if a worker is currently polling
static POLLING: AtomicBool = AtomicBool::new(false);

fn network_poller() -> &'static netpoll::NetPoller {
    netpoll::net_poller()
}

/// Wait for an fd to become readable
///
/// Parks the current task until the fd is ready for reading.
pub fn net_poll_read<T: AsRawFd>(fd: &T) {
    let raw_fd = fd.as_raw_fd();
    let task_id = current_task_id();
    let poller = network_poller();

    // Register for read readiness
    poller.register_read(raw_fd, task_id.as_u64());

    // Park until ready
    gopark();

    // Unregister after wakeup
    poller.unregister(raw_fd);
}

/// Try to become the poller and poll for network events
/// Returns true if we did polling, false if another worker is already polling
fn try_poll_network() -> bool {
    // Try to become the poller (only one worker can poll at a time)
    if POLLING
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return false;
    }

    let poller = network_poller();

    // Only poll if there are waiters
    if !poller.has_waiters() {
        POLLING.store(false, Ordering::Release);
        return false;
    }

    // Poll with a timeout (100ms) to avoid blocking forever
    let ready_tasks = poller.poll(100);

    // Release polling flag before waking tasks
    POLLING.store(false, Ordering::Release);

    // Wake up ready tasks
    for task_id in ready_tasks {
        goready(TaskId::from_u64(task_id));
    }

    true
}
