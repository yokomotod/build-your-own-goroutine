//! Low-level context switching primitives using inline assembly.
//!
//! This module provides the core building blocks for green thread implementation:
//! - `Context`: CPU register state for a green thread
//! - `context_switch`: Switch execution between two contexts

use std::arch::naked_asm;

/// Stack size for each green thread (64KB)
pub const STACK_SIZE: usize = 64 * 1024;

// ============================================================================
// x86_64 implementation
// ============================================================================

#[cfg(target_arch = "x86_64")]
mod arch {
    use super::*;
    use std::arch::asm;

    /// Saved CPU context for context switching
    ///
    /// On x86_64 System V ABI, these are the callee-saved registers
    /// that must be preserved across function calls.
    #[repr(C)]
    #[derive(Debug, Clone, Default)]
    pub struct Context {
        /// Stack pointer
        rsp: u64,
        /// Frame pointer
        rbp: u64,
        /// General purpose (callee-saved)
        rbx: u64,
        r12: u64,
        r13: u64,
        r14: u64,
        r15: u64,
    }

    impl Context {
        /// Create a new context for a task.
        ///
        /// - `stack_top`: The top of the stack (highest address), 16-byte aligned
        /// - `entry`: The entry point function address
        /// - `closure_ptr`: Pointer to pass to the entry function via callee-saved register
        pub fn new_for_task(stack_top: usize, entry: usize, closure_ptr: u64) -> Self {
            // System V ABI requires RSP to be 16-byte aligned BEFORE `call` instruction.
            // After `call`, RSP becomes 16n+8 (due to pushed return address).
            // Since we use `ret` instead of `call`, we need to simulate this:
            //
            // Stack layout (growing downward):
            //   stack_top - 8:  (padding for alignment)
            //   stack_top - 16: return address (entry)
            //
            // After `ret`: RSP = stack_top - 8, which is 16n+8 as required.
            let initial_rsp = stack_top - 16;

            unsafe {
                std::ptr::write(initial_rsp as *mut u64, entry as u64);
            }

            Context {
                rsp: initial_rsp as u64,
                r15: closure_ptr,
                ..Default::default()
            }
        }
    }

    /// Get the closure pointer passed via callee-saved register.
    ///
    /// Must be called at the start of task_entry before any function calls.
    pub fn get_closure_ptr() -> u64 {
        let ptr: u64;
        unsafe {
            asm!(
                "mov {}, r15",
                out(reg) ptr,
                options(nomem, nostack, preserves_flags)
            );
        }
        ptr
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
}

// ============================================================================
// aarch64 implementation
// ============================================================================

#[cfg(target_arch = "aarch64")]
mod arch {
    use super::*;
    use std::arch::asm;

    /// Saved CPU context for context switching
    ///
    /// On aarch64 (AAPCS64), these are the callee-saved registers
    /// that must be preserved across function calls.
    #[repr(C)]
    #[derive(Debug, Clone, Default)]
    pub struct Context {
        /// Stack pointer
        sp: u64,
        /// Link register (return address)
        lr: u64,
        /// Frame pointer
        fp: u64,
        /// General purpose (callee-saved)
        x19: u64,
        x20: u64,
        x21: u64,
        x22: u64,
        x23: u64,
        x24: u64,
        x25: u64,
        x26: u64,
        x27: u64,
        x28: u64,
    }

    impl Context {
        /// Create a new context for a task.
        ///
        /// - `stack_top`: The top of the stack (highest address), 16-byte aligned
        /// - `entry`: The entry point function address
        /// - `closure_ptr`: Pointer to pass to the entry function via callee-saved register
        pub fn new_for_task(stack_top: usize, entry: usize, closure_ptr: u64) -> Self {
            // On aarch64, `ret` jumps to the address in lr (link register).
            // No need to push return address on stack like x86_64.
            Context {
                sp: stack_top as u64,
                lr: entry as u64,
                x19: closure_ptr,
                ..Default::default()
            }
        }
    }

    /// Get the closure pointer passed via callee-saved register.
    ///
    /// Must be called at the start of task_entry before any function calls.
    pub fn get_closure_ptr() -> u64 {
        let ptr: u64;
        unsafe {
            asm!(
                "mov {}, x19",
                out(reg) ptr,
                options(nomem, nostack, preserves_flags)
            );
        }
        ptr
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
        // Arguments: x0 = old, x1 = new
        naked_asm!(
            // Save callee-saved registers to old context (x0)
            "mov x9, sp",
            "str x9,  [x0, #0x00]", // sp
            "str lr,  [x0, #0x08]", // lr (x30)
            "str fp,  [x0, #0x10]", // fp (x29)
            "str x19, [x0, #0x18]",
            "str x20, [x0, #0x20]",
            "str x21, [x0, #0x28]",
            "str x22, [x0, #0x30]",
            "str x23, [x0, #0x38]",
            "str x24, [x0, #0x40]",
            "str x25, [x0, #0x48]",
            "str x26, [x0, #0x50]",
            "str x27, [x0, #0x58]",
            "str x28, [x0, #0x60]",
            // Load callee-saved registers from new context (x1)
            "ldr x9,  [x1, #0x00]", // sp
            "mov sp, x9",
            "ldr lr,  [x1, #0x08]", // lr (x30)
            "ldr fp,  [x1, #0x10]", // fp (x29)
            "ldr x19, [x1, #0x18]",
            "ldr x20, [x1, #0x20]",
            "ldr x21, [x1, #0x28]",
            "ldr x22, [x1, #0x30]",
            "ldr x23, [x1, #0x38]",
            "ldr x24, [x1, #0x40]",
            "ldr x25, [x1, #0x48]",
            "ldr x26, [x1, #0x50]",
            "ldr x27, [x1, #0x58]",
            "ldr x28, [x1, #0x60]",
            // Return to the new context
            "ret",
        );
    }
}

// Re-export from arch module
pub use arch::{Context, context_switch, get_closure_ptr};
