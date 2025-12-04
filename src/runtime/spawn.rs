//! M:N Green Thread Runtime with Dynamic Worker Spawning
//!
//! Extends the basic M:N runtime with blocking I/O support.
//! When a task enters a blocking operation (like file I/O),
//! the runtime spawns a new worker to keep other tasks running.

use crate::common::{
    Context, Task, TaskState, Worker, context_switch, get_closure_ptr, prepare_stack,
};
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
            next_worker_id: 0,
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
    /// Next worker ID to assign
    next_worker_id: usize,
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
            if let Some(task) = worker.current_task.borrow_mut().take()
                && task.state != TaskState::Dead
            {
                // Task yielded, put back to global queue
                queue.lock().unwrap().tasks.push_back(task);
            }
            // If finished, just drop it
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
        !q.tasks.is_empty()
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
