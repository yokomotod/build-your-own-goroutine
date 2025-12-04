//! Platform-specific network polling implementations.
//!
//! Provides a unified interface for epoll (Linux) and kqueue (macOS/BSD).

#[cfg(target_os = "linux")]
mod epoll;
#[cfg(target_os = "linux")]
pub use epoll::*;

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
mod kqueue;
#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
pub use kqueue::*;
