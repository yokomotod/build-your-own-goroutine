//! Linux epoll-based network poller.

use std::collections::HashMap;
use std::os::fd::RawFd;
use std::sync::{Mutex, OnceLock};

/// Network poller using epoll.
pub struct NetPoller {
    epoll_fd: RawFd,
    /// Map from fd to waiting task_id
    waiting_fds: Mutex<HashMap<RawFd, u64>>,
}

static NET_POLLER: OnceLock<NetPoller> = OnceLock::new();

/// Get the global network poller instance.
pub fn net_poller() -> &'static NetPoller {
    NET_POLLER.get_or_init(|| {
        let epoll_fd = unsafe { libc::epoll_create1(0) };
        if epoll_fd < 0 {
            panic!("epoll_create1 failed");
        }
        NetPoller {
            epoll_fd,
            waiting_fds: Mutex::new(HashMap::new()),
        }
    })
}

impl NetPoller {
    /// Register an fd for read readiness.
    pub fn register_read(&self, fd: RawFd, task_id: u64) {
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: fd as u64,
        };

        let ret = unsafe { libc::epoll_ctl(self.epoll_fd, libc::EPOLL_CTL_ADD, fd, &mut event) };
        if ret < 0 {
            panic!("epoll_ctl ADD failed");
        }

        self.waiting_fds.lock().unwrap().insert(fd, task_id);
    }

    /// Unregister an fd.
    pub fn unregister(&self, fd: RawFd) {
        unsafe {
            libc::epoll_ctl(self.epoll_fd, libc::EPOLL_CTL_DEL, fd, std::ptr::null_mut());
        }
        self.waiting_fds.lock().unwrap().remove(&fd);
    }

    /// Poll for ready fds with timeout (in milliseconds).
    /// Returns list of ready task_ids.
    pub fn poll(&self, timeout_ms: i32) -> Vec<u64> {
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

    /// Check if there are any fds being waited on.
    pub fn has_waiters(&self) -> bool {
        !self.waiting_fds.lock().unwrap().is_empty()
    }
}
