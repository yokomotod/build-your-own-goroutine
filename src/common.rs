//! Common components for green thread runtimes.

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

/// A green thread task - pure data structure
pub struct Task {
    pub context: Context,
    _stack: Vec<u8>, // Keep stack alive
    pub finished: bool,
}

// Task needs to be Send because it's moved between threads via the global queue (M:N runtime)
unsafe impl Send for Task {}

impl Task {
    pub fn new(context: Context, stack: Vec<u8>) -> Self {
        Task {
            context,
            _stack: stack,
            finished: false,
        }
    }
}
