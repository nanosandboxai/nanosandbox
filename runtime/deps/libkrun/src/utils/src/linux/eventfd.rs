pub use libc::EFD_NONBLOCK;

/// Extension trait adding `as_pollable()` to vmm-sys-util's EventFd on Linux.
pub trait EventFdExt {
    /// Returns the EventFd's file descriptor as a platform-generic pollable handle.
    fn as_pollable(&self) -> std::os::unix::io::RawFd;
}

impl EventFdExt for vmm_sys_util::eventfd::EventFd {
    fn as_pollable(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd;
        self.as_raw_fd()
    }
}
