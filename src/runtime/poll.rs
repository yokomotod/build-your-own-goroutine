//! M:N Green Thread Runtime with Network Polling
//!
//! Extends the basic M:N runtime with:
//! - gopark/goready for cooperative waiting
//! - Network I/O polling with epoll
//! - Workers that sleep when idle (instead of terminating)

use crate::common::{Context, Task, context_switch, get_closure_ptr, prepare_stack};
use std::cell::{RefCell, UnsafeCell};
use std::collections::{HashMap, VecDeque};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread;

/// Unique identifier for each task
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TaskId(u64);

impl TaskId {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Task state
#[derive(Clone, Copy, Debug, PartialEq)]
enum TaskState {
    /// In the runnable queue, waiting to be scheduled
    Runnable,
    /// Currently running on a worker
    Running,
    /// Waiting for I/O or other event (not in any queue)
    Waiting,
}

/// Extended task with ID and state
struct TaskEntry {
    id: TaskId,
    task: Task,
    state: TaskState,
}

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
            .task
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
    runnable: VecDeque<TaskEntry>,
    /// Tasks waiting for I/O or other events
    waiting: HashMap<TaskId, TaskEntry>,
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
    current_task: RefCell<Option<TaskEntry>>,
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
                .task
                .context as *mut Context
        };

        let worker_ctx: *const Context = self.context.get();
        context_switch(task_ctx, worker_ctx);
    }
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
            let task_entry = {
                let mut q = queue.lock().unwrap();

                if let Some(mut entry) = q.runnable.pop_front() {
                    entry.state = TaskState::Running;
                    Some(entry)
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
                        if let Some(mut entry) = q.runnable.pop_front() {
                            entry.state = TaskState::Running;
                            Some(entry)
                        } else {
                            None
                        }
                    }
                }
            };

            let Some(task_entry) = task_entry else {
                // Spurious wakeup, loop again
                continue;
            };

            // Set current task
            *worker.current_task.borrow_mut() = Some(task_entry);

            // Get pointers before context_switch
            let worker_ctx: *mut Context = worker.context.get();
            let task_ctx: *const Context = {
                let task = worker.current_task.borrow();
                &task.as_ref().unwrap().task.context as *const Context
            };

            context_switch(worker_ctx, task_ctx);

            // Task yielded or finished
            if let Some(mut entry) = worker.current_task.borrow_mut().take() {
                if entry.task.finished {
                    // Task finished, drop it
                } else if entry.state == TaskState::Waiting {
                    // gopark was called - move task to waiting map
                    let mut q = queue.lock().unwrap();
                    q.waiting.insert(entry.id, entry);
                } else {
                    // Normal yield (gosched), put back to runnable queue
                    entry.state = TaskState::Runnable;
                    queue.lock().unwrap().runnable.push_back(entry);
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
            let entry = current.as_mut().expect("gopark called without current task");
            entry.state = TaskState::Waiting;
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

    if let Some(mut entry) = q.waiting.remove(&task_id) {
        entry.state = TaskState::Runnable;
        q.runnable.push_back(entry);

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

/// Spawn a new green thread
pub fn go<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    let (stack, stack_top) = prepare_stack();
    let f_ptr = Box::into_raw(Box::new(f)) as u64;
    let context = Context::new(stack_top, task_entry::<F> as usize, f_ptr);
    let task = Task::new(context, stack);

    let entry = TaskEntry {
        id: TaskId::new(),
        task,
        state: TaskState::Runnable,
    };

    let queue = global_queue();
    let condvar = worker_condvar();

    let mut q = queue.lock().unwrap();
    q.runnable.push_back(entry);

    // Wake up an idle worker if any
    if q.idle_workers > 0 {
        condvar.notify_one();
    }
}

// ============================================================================
// Blocking I/O Support (File I/O etc.)
// ============================================================================

/// Internal: called before entering a blocking operation.
/// Like Go 1.0's entersyscall.
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

/// Internal: called after returning from a blocking operation.
fn exit_blocking() {
    // No-op for now
}

/// I/O module providing blocking-aware wrappers.
///
/// Use these functions instead of std::io/std::fs when running inside
/// the mn_poll runtime. They notify the runtime before blocking,
/// allowing other tasks to continue running.
pub mod io {
    use super::{enter_blocking, exit_blocking};
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::path::Path;

    /// Read from a reader (blocking-aware wrapper for Read::read)
    pub fn read<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
        enter_blocking();
        let result = reader.read(buf);
        exit_blocking();
        result
    }

    /// Read all bytes from a reader (blocking-aware wrapper for Read::read_to_end)
    pub fn read_to_end<R: Read>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<usize> {
        enter_blocking();
        let result = reader.read_to_end(buf);
        exit_blocking();
        result
    }

    /// Read exact number of bytes (blocking-aware wrapper for Read::read_exact)
    pub fn read_exact<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<()> {
        enter_blocking();
        let result = reader.read_exact(buf);
        exit_blocking();
        result
    }

    /// Read entire file contents as bytes
    pub fn read_file<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
        enter_blocking();
        let result = std::fs::read(path);
        exit_blocking();
        result
    }

    /// Read entire file contents as string
    pub fn read_to_string<P: AsRef<Path>>(path: P) -> io::Result<String> {
        enter_blocking();
        let result = std::fs::read_to_string(path);
        exit_blocking();
        result
    }

    /// Write to a writer (blocking-aware wrapper for Write::write)
    pub fn write<W: Write>(writer: &mut W, buf: &[u8]) -> io::Result<usize> {
        enter_blocking();
        let result = writer.write(buf);
        exit_blocking();
        result
    }

    /// Write all bytes to a writer (blocking-aware wrapper for Write::write_all)
    pub fn write_all<W: Write>(writer: &mut W, buf: &[u8]) -> io::Result<()> {
        enter_blocking();
        let result = writer.write_all(buf);
        exit_blocking();
        result
    }

    /// Write bytes to a file
    pub fn write_file<P: AsRef<Path>>(path: P, contents: &[u8]) -> io::Result<()> {
        enter_blocking();
        let result = std::fs::write(path, contents);
        exit_blocking();
        result
    }

    /// Open a file
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<File> {
        enter_blocking();
        let result = File::open(path);
        exit_blocking();
        result
    }

    /// Create a file
    pub fn create<P: AsRef<Path>>(path: P) -> io::Result<File> {
        enter_blocking();
        let result = File::create(path);
        exit_blocking();
        result
    }
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

/// Yield execution to another green thread
pub fn gosched() {
    CURRENT_WORKER.with(|worker| {
        worker.switch_to_scheduler();
    });
}

/// Start the runtime and run until all tasks complete
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
// Network Poller (epoll-based)
// ============================================================================

/// Network poller using epoll
struct NetworkPoller {
    epoll_fd: RawFd,
    /// Map from fd to waiting TaskId
    waiting_fds: Mutex<HashMap<RawFd, TaskId>>,
}

/// Global network poller
static NETWORK_POLLER: OnceLock<NetworkPoller> = OnceLock::new();

/// Flag to indicate if a worker is currently polling
static POLLING: AtomicBool = AtomicBool::new(false);

fn network_poller() -> &'static NetworkPoller {
    NETWORK_POLLER.get_or_init(|| {
        let epoll_fd = unsafe { libc::epoll_create1(0) };
        if epoll_fd < 0 {
            panic!("epoll_create1 failed");
        }
        NetworkPoller {
            epoll_fd,
            waiting_fds: Mutex::new(HashMap::new()),
        }
    })
}

impl NetworkPoller {
    /// Register an fd for read readiness, associated with a task
    fn register_read(&self, fd: RawFd, task_id: TaskId) {
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: fd as u64,
        };

        let ret = unsafe {
            libc::epoll_ctl(self.epoll_fd, libc::EPOLL_CTL_ADD, fd, &mut event)
        };
        if ret < 0 {
            panic!("epoll_ctl ADD failed");
        }

        self.waiting_fds.lock().unwrap().insert(fd, task_id);
    }

    /// Unregister an fd
    fn unregister(&self, fd: RawFd) {
        unsafe {
            libc::epoll_ctl(self.epoll_fd, libc::EPOLL_CTL_DEL, fd, std::ptr::null_mut());
        }
        self.waiting_fds.lock().unwrap().remove(&fd);
    }

    /// Poll for ready fds with timeout (in milliseconds)
    /// Returns list of ready TaskIds
    fn poll(&self, timeout_ms: i32) -> Vec<TaskId> {
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 64];

        let n = unsafe {
            libc::epoll_wait(
                self.epoll_fd,
                events.as_mut_ptr(),
                events.len() as i32,
                timeout_ms,
            )
        };

        if n < 0 {
            // EINTR is ok, just return empty
            return Vec::new();
        }

        let mut ready_tasks = Vec::new();
        let waiting = self.waiting_fds.lock().unwrap();

        for i in 0..(n as usize) {
            let fd = events[i].u64 as RawFd;
            if let Some(&task_id) = waiting.get(&fd) {
                ready_tasks.push(task_id);
            }
        }

        ready_tasks
    }

    /// Check if there are any fds being waited on
    fn has_waiters(&self) -> bool {
        !self.waiting_fds.lock().unwrap().is_empty()
    }
}

/// Wait for an fd to become readable
///
/// Parks the current task until the fd is ready for reading.
pub fn net_poll_read<T: AsRawFd>(fd: &T) {
    let raw_fd = fd.as_raw_fd();
    let task_id = current_task_id();
    let poller = network_poller();

    // Register for read readiness
    poller.register_read(raw_fd, task_id);

    // Park until ready
    gopark();

    // Unregister after wakeup
    poller.unregister(raw_fd);
}

/// Try to become the poller and poll for network events
/// Returns true if we did polling, false if another worker is already polling
fn try_poll_network() -> bool {
    // Try to become the poller (only one worker can poll at a time)
    if POLLING.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
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
        goready(task_id);
    }

    true
}
