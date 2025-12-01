//! aarch64 implementation of context switching

use std::arch::asm;
use std::arch::naked_asm;

/// Saved CPU context for context switching
///
/// On aarch64 (AAPCS64), these are the callee-saved registers
/// that must be preserved across function calls:
/// - x19-x28: general purpose callee-saved registers
/// - d8-d15: floating-point/SIMD callee-saved registers (lower 64 bits of v8-v15)
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
    /// Floating-point/SIMD (callee-saved, lower 64 bits)
    d8: u64,
    d9: u64,
    d10: u64,
    d11: u64,
    d12: u64,
    d13: u64,
    d14: u64,
    d15: u64,
}

impl Context {
    /// Create a new context for a task.
    ///
    /// - `stack_top`: The top of the stack (highest address), 16-byte aligned
    /// - `entry`: The entry point function address
    /// - `closure_ptr`: Pointer to pass to the entry function via callee-saved register
    pub fn new(stack_top: usize, entry: usize, closure_ptr: u64) -> Self {
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
        // Save floating-point callee-saved registers
        "str d8,  [x0, #0x68]",
        "str d9,  [x0, #0x70]",
        "str d10, [x0, #0x78]",
        "str d11, [x0, #0x80]",
        "str d12, [x0, #0x88]",
        "str d13, [x0, #0x90]",
        "str d14, [x0, #0x98]",
        "str d15, [x0, #0xa0]",
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
        // Load floating-point callee-saved registers
        "ldr d8,  [x1, #0x68]",
        "ldr d9,  [x1, #0x70]",
        "ldr d10, [x1, #0x78]",
        "ldr d11, [x1, #0x80]",
        "ldr d12, [x1, #0x88]",
        "ldr d13, [x1, #0x90]",
        "ldr d14, [x1, #0x98]",
        "ldr d15, [x1, #0xa0]",
        // Return to the new context
        "ret",
    );
}
