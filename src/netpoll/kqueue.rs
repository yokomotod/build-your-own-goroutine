//! macOS/BSD kqueue-based network poller.

use std::collections::HashMap;
use std::os::fd::RawFd;
use std::sync::{Mutex, OnceLock};

/// Network poller using kqueue.
pub struct NetPoller {
    kqueue_fd: RawFd,
    /// Map from fd to waiting task_id
    waiting_fds: Mutex<HashMap<RawFd, u64>>,
}

static NET_POLLER: OnceLock<NetPoller> = OnceLock::new();

/// Get the global network poller instance.
pub fn net_poller() -> &'static NetPoller {
    NET_POLLER.get_or_init(|| {
        let kqueue_fd = unsafe { libc::kqueue() };
        if kqueue_fd < 0 {
            panic!("kqueue failed");
        }
        NetPoller {
            kqueue_fd,
            waiting_fds: Mutex::new(HashMap::new()),
        }
    })
}

impl NetPoller {
    /// Register an fd for read readiness.
    pub fn register_read(&self, fd: RawFd, task_id: u64) {
        let mut event = libc::kevent {
            ident: fd as libc::uintptr_t,
            filter: libc::EVFILT_READ,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };

        let ret = unsafe {
            libc::kevent(
                self.kqueue_fd,
                &event,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if ret < 0 {
            panic!("kevent register failed");
        }

        self.waiting_fds.lock().unwrap().insert(fd, task_id);
    }

    /// Unregister an fd.
    pub fn unregister(&self, fd: RawFd) {
        let mut event = libc::kevent {
            ident: fd as libc::uintptr_t,
            filter: libc::EVFILT_READ,
            flags: libc::EV_DELETE,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };

        unsafe {
            libc::kevent(
                self.kqueue_fd,
                &event,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            );
        }
        self.waiting_fds.lock().unwrap().remove(&fd);
    }

    /// Poll for ready fds with timeout (in milliseconds).
    /// Returns list of ready task_ids.
    pub fn poll(&self, timeout_ms: i32) -> Vec<u64> {
        let timeout = libc::timespec {
            tv_sec: (timeout_ms / 1000) as libc::time_t,
            tv_nsec: ((timeout_ms % 1000) * 1_000_000) as libc::c_long,
        };

        let mut events: [libc::kevent; 64] = unsafe { std::mem::zeroed() };

        let n = unsafe {
            libc::kevent(
                self.kqueue_fd,
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as i32,
                &timeout,
            )
        };

        if n < 0 {
            // EINTR is ok, just return empty
            return Vec::new();
        }

        let mut ready_tasks = Vec::new();
        let waiting = self.waiting_fds.lock().unwrap();

        for i in 0..(n as usize) {
            let fd = events[i].ident as RawFd;
            if let Some(&task_id) = waiting.get(&fd) {
                ready_tasks.push(task_id);
            }
        }

        ready_tasks
    }

    /// Check if there are any fds being waited on.
    pub fn has_waiters(&self) -> bool {
        !self.waiting_fds.lock().unwrap().is_empty()
    }
}
