//! M:N Green Thread Runtime with Network Polling and Timers
//!
//! Extends mn_poll with:
//! - Timer heap for sleep()
//! - epoll/kqueue timeout integration with timers

use crate::common::{Context, Task, context_switch, get_closure_ptr, prepare_stack};
use crate::netpoll;
use std::cell::{RefCell, UnsafeCell};
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// Unique identifier for each task
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TaskId(u64);

impl TaskId {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }

    fn as_u64(self) -> u64 {
        self.0
    }

    fn from_u64(id: u64) -> Self {
        TaskId(id)
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
    // - No waiting tasks (including network waiters and timers)
    // - All workers are idle
    queue.runnable.is_empty()
        && queue.waiting.is_empty()
        && !network_poller().has_waiters()
        && !has_timers()
        && queue.idle_workers == queue.all_workers
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
                    // No runnable tasks - try polling before going idle
                    // Drop lock before polling (polling may block)
                    drop(q);

                    // Try to become the poller
                    if try_poll_network() {
                        // We did some polling, loop back to check for work
                        None
                    } else {
                        // Another worker is polling, or no waiters
                        // Check external state before taking queue lock (avoid lock order issues)
                        let has_net_waiters = network_poller().has_waiters();
                        let has_timer_waiters = has_timers();

                        // Go idle
                        let mut q = queue.lock().unwrap();
                        q.idle_workers += 1;

                        // Check termination (like Go's checkdead() in mput())
                        let should_term = q.runnable.is_empty()
                            && q.waiting.is_empty()
                            && !has_net_waiters
                            && !has_timer_waiters
                            && q.idle_workers == q.all_workers;

                        if should_term {
                            condvar.notify_all();
                            q.all_workers -= 1;
                            return;
                        }

                        // Wait for work
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

/// Sleep for the specified duration
///
/// Parks the current task and wakes it up after the duration has elapsed.
pub fn sleep(duration: Duration) {
    let wake_time = Instant::now() + duration;
    let task_id = current_task_id();

    // Add to timer heap
    timer_heap().lock().unwrap().push(TimerEntry {
        wake_time,
        task_id,
    });

    // Park until timer fires
    gopark();
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
// Network Poller
// ============================================================================

/// Flag to indicate if a worker is currently polling
static POLLING: AtomicBool = AtomicBool::new(false);

fn network_poller() -> &'static netpoll::NetPoller {
    netpoll::net_poller()
}

// ============================================================================
// Timer Heap
// ============================================================================

/// Entry in the timer heap
#[derive(Eq, PartialEq)]
struct TimerEntry {
    wake_time: Instant,
    task_id: TaskId,
}

impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse order for min-heap (earliest wake time first)
        other.wake_time.cmp(&self.wake_time)
    }
}

impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Global timer heap
static TIMER_HEAP: OnceLock<Mutex<BinaryHeap<TimerEntry>>> = OnceLock::new();

fn timer_heap() -> &'static Mutex<BinaryHeap<TimerEntry>> {
    TIMER_HEAP.get_or_init(|| Mutex::new(BinaryHeap::new()))
}

/// Check and fire expired timers, returns the next wake time if any
fn check_timers() -> Option<Instant> {
    let now = Instant::now();
    let mut heap = timer_heap().lock().unwrap();

    // Fire all expired timers
    while let Some(entry) = heap.peek() {
        if entry.wake_time <= now {
            let entry = heap.pop().unwrap();
            let task_id = entry.task_id;
            // Drop lock before calling goready to avoid deadlock
            drop(heap);
            goready(task_id);
            heap = timer_heap().lock().unwrap();
        } else {
            break;
        }
    }

    // Return next wake time
    heap.peek().map(|e| e.wake_time)
}

/// Check if there are any pending timers
fn has_timers() -> bool {
    !timer_heap().lock().unwrap().is_empty()
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

/// Try to become the poller and poll for network/timer events
/// Returns true if we did polling, false if another worker is already polling
fn try_poll_network() -> bool {
    // Try to become the poller (only one worker can poll at a time)
    if POLLING.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return false;
    }

    let poller = network_poller();
    let has_net_waiters = poller.has_waiters();
    let has_timer_waiters = has_timers();

    // Only poll if there are waiters (network or timer)
    if !has_net_waiters && !has_timer_waiters {
        POLLING.store(false, Ordering::Release);
        return false;
    }

    // Check timers first, get timeout for next timer
    let timeout_ms = match check_timers() {
        Some(next_wake) => {
            let now = Instant::now();
            if next_wake <= now {
                0
            } else {
                // Time until next timer (max 100ms)
                (next_wake - now).as_millis().min(100) as i32
            }
        }
        None => {
            // No more timers (check_timers already woke up expired ones)
            if !has_net_waiters {
                // Nothing left to wait for - return so caller can check termination
                POLLING.store(false, Ordering::Release);
                return true;
            }
            100 // Default timeout for network only
        }
    };

    // Poll for network events or wait for timer
    let ready_tasks = if has_net_waiters {
        poller.poll(timeout_ms)
    } else {
        // No network waiters, just sleep for timer
        if timeout_ms > 0 {
            std::thread::sleep(Duration::from_millis(timeout_ms as u64));
        }
        Vec::new()
    };

    // Check timers again after poll/sleep
    check_timers();

    // Release polling flag before waking tasks
    POLLING.store(false, Ordering::Release);

    // Wake up ready tasks (from network)
    for task_id in ready_tasks {
        goready(TaskId::from_u64(task_id));
    }

    true
}
