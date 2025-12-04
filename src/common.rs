//! Common components for green thread runtimes.

use std::sync::atomic::{AtomicU64, Ordering};

// Re-export architecture-specific items
pub use crate::arch::{Context, context_switch, get_closure_ptr};

/// Stack size for each green thread (64KB)
pub const STACK_SIZE: usize = 64 * 1024;

/// Prepare a stack for a new task.
/// Returns (stack, aligned_stack_top).
pub fn prepare_stack() -> (Vec<u8>, usize) {
    let mut stack = vec![0u8; STACK_SIZE];
    let stack_top = stack.as_mut_ptr() as usize + STACK_SIZE;
    // Align to 16 bytes (required by ABI)
    let stack_top = stack_top & !0xF;
    (stack, stack_top)
}

/// Unique identifier for each task
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TaskId(u64);

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(0);

impl TaskId {
    pub fn new() -> Self {
        TaskId(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }

    pub fn from_u64(id: u64) -> Self {
        TaskId(id)
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

/// Task state (similar to Go's goroutine status)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TaskState {
    /// In the runnable queue, waiting to be scheduled
    Runnable,
    /// Currently running on a worker
    Running,
    /// Waiting for I/O or other event (not in any queue)
    Waiting,
    /// Task has finished execution (like Go's _Gdead)
    Dead,
}

/// A green thread task
pub struct Task {
    pub id: TaskId,
    pub context: Context,
    _stack: Vec<u8>, // Keep stack alive
    pub state: TaskState,
}

// Task needs to be Send because it's moved between threads via the global queue (M:N runtime)
unsafe impl Send for Task {}

impl Task {
    pub fn new(context: Context, stack: Vec<u8>) -> Self {
        Task {
            id: TaskId::new(),
            context,
            _stack: stack,
            state: TaskState::Runnable,
        }
    }
}
