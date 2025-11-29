//! Low-level context switching primitives using inline assembly.
//!
//! This module provides the core building blocks for green thread implementation:
//! - `Context`: CPU register state for a green thread
//! - `context_switch`: Switch execution between two contexts

use std::arch::naked_asm;

/// Stack size for each green thread (64KB)
pub const STACK_SIZE: usize = 64 * 1024;

/// Saved CPU context for context switching
///
/// On x86_64 System V ABI, these are the callee-saved registers
/// that must be preserved across function calls.
#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// Stack pointer
    pub rsp: u64,
    /// Frame pointer
    pub rbp: u64,
    /// General purpose (callee-saved)
    pub rbx: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

/// Switch from one context to another
///
/// Saves the current CPU state into `old` and restores state from `new`.
/// This function returns when another context switches back to `old`.
///
/// # Safety
/// Both pointers must be valid. The `new` context must have been properly
/// initialized (either by a previous `context_switch` or by manual setup).
#[unsafe(naked)]
pub extern "C" fn context_switch(_old: *mut Context, _new: *const Context) {
    naked_asm!(
        // Save callee-saved registers to old context (rdi)
        "mov [rdi + 0x00], rsp",
        "mov [rdi + 0x08], rbp",
        "mov [rdi + 0x10], rbx",
        "mov [rdi + 0x18], r12",
        "mov [rdi + 0x20], r13",
        "mov [rdi + 0x28], r14",
        "mov [rdi + 0x30], r15",
        // Load callee-saved registers from new context (rsi)
        "mov rsp, [rsi + 0x00]",
        "mov rbp, [rsi + 0x08]",
        "mov rbx, [rsi + 0x10]",
        "mov r12, [rsi + 0x18]",
        "mov r13, [rsi + 0x20]",
        "mov r14, [rsi + 0x28]",
        "mov r15, [rsi + 0x30]",
        // Return to the new context
        // For a fresh task: pops task_entry address and jumps there
        // For a yielded task: returns to where it called context_switch
        "ret",
    );
}
