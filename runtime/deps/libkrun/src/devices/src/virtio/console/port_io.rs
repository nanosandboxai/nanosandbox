#[cfg(unix)]
use libc::{
    fcntl, F_GETFL, F_SETFL, O_NONBLOCK, STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO, TIOCGWINSZ,
};
use log::Level;
#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::ioctl_read_bad;
#[cfg(unix)]
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
#[cfg(unix)]
use nix::unistd::{dup, isatty};
#[cfg(unix)]
use std::fs::File;
use std::io::{self, ErrorKind};
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use utils::eventfd::EventFd;
use utils::eventfd::EFD_NONBLOCK;
use vm_memory::bitmap::Bitmap;
use vm_memory::{VolatileMemoryError, VolatileSlice, WriteVolatile};

pub trait PortInput {
    fn read_volatile(&mut self, buf: &mut VolatileSlice) -> Result<usize, io::Error>;

    fn wait_until_readable(&self, stopfd: Option<&EventFd>);
}

pub trait PortOutput {
    fn write_volatile(&mut self, buf: &VolatileSlice) -> Result<usize, io::Error>;

    fn wait_until_writable(&self);
}

/// Terminal properties associated with this port
pub trait PortTerminalProperties: Send + Sync {
    fn get_win_size(&self) -> (u16, u16);
}

#[cfg(unix)]
pub fn stdin() -> Result<Box<dyn PortInput + Send>, nix::Error> {
    let fd = dup_raw_fd_into_owned(STDIN_FILENO)?;
    make_non_blocking(&fd)?;
    Ok(Box::new(PortInputFd(fd)))
}

#[cfg(unix)]
pub fn input_to_raw_fd_dup(fd: RawFd) -> Result<Box<dyn PortInput + Send>, nix::Error> {
    let fd = dup_raw_fd_into_owned(fd)?;
    make_non_blocking(&fd)?;
    Ok(Box::new(PortInputFd(fd)))
}

#[cfg(unix)]
pub fn stdout() -> Result<Box<dyn PortOutput + Send>, nix::Error> {
    output_to_raw_fd_dup(STDOUT_FILENO)
}

#[cfg(unix)]
pub fn stderr() -> Result<Box<dyn PortOutput + Send>, nix::Error> {
    output_to_raw_fd_dup(STDERR_FILENO)
}

#[cfg(unix)]
pub fn term_fd(
    term_fd: RawFd,
) -> Result<Box<dyn PortTerminalProperties + Send + Sync>, nix::Error> {
    let fd = dup_raw_fd_into_owned(term_fd)?;
    assert!(
        isatty(&fd).is_ok_and(|v| v),
        "Expected fd {fd:?}, to be a tty, to query the window size!"
    );
    Ok(Box::new(PortTerminalPropertiesFd(fd)))
}

pub fn term_fixed_size(width: u16, height: u16) -> Box<dyn PortTerminalProperties + Send + Sync> {
    Box::new(PortTerminalPropertiesFixed((width, height)))
}

pub fn input_empty() -> Result<Box<dyn PortInput + Send>, io::Error> {
    Ok(Box::new(PortInputEmpty {}))
}

#[cfg(unix)]
pub fn output_file(file: File) -> Result<Box<dyn PortOutput + Send>, nix::Error> {
    output_to_raw_fd_dup(file.as_raw_fd())
}

#[cfg(unix)]
pub fn output_to_raw_fd_dup(fd: RawFd) -> Result<Box<dyn PortOutput + Send>, nix::Error> {
    let fd = dup_raw_fd_into_owned(fd)?;
    make_non_blocking(&fd)?;
    Ok(Box::new(PortOutputFd(fd)))
}

pub fn output_to_log_as_err() -> Box<dyn PortOutput + Send> {
    Box::new(PortOutputLog::new())
}

#[cfg(unix)]
struct PortInputFd(OwnedFd);

#[cfg(unix)]
impl AsRawFd for PortInputFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[cfg(unix)]
impl PortInput for PortInputFd {
    fn read_volatile(&mut self, buf: &mut VolatileSlice) -> io::Result<usize> {
        // This source code is copied from vm-memory, except it fixes an issue, where
        // the original code would does not handle handle EWOULDBLOCK

        let fd = self.as_raw_fd();
        let guard = buf.ptr_guard_mut();

        let dst = guard.as_ptr().cast::<libc::c_void>();

        // SAFETY: We got a valid file descriptor from `AsRawFd`. The memory pointed to by `dst` is
        // valid for writes of length `buf.len() by the invariants upheld by the constructor
        // of `VolatileSlice`.
        let bytes_read = unsafe { libc::read(fd, dst, buf.len()) };

        if bytes_read < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() != ErrorKind::WouldBlock {
                // We don't know if a partial read might have happened, so mark everything as dirty
                buf.bitmap().mark_dirty(0, buf.len());
            }

            Err(err)
        } else {
            let bytes_read = bytes_read.try_into().unwrap();
            buf.bitmap().mark_dirty(0, bytes_read);
            Ok(bytes_read)
        }
    }

    fn wait_until_readable(&self, stopfd: Option<&EventFd>) {
        let mut poll_fds = Vec::new();
        poll_fds.push(PollFd::new(self.0.as_fd(), PollFlags::POLLIN));
        if let Some(stopfd) = stopfd {
            // SAFETY: we trust stopfd won't go away to avoid a dup call here.
            let borrowed_fd = unsafe { BorrowedFd::borrow_raw(stopfd.as_raw_fd()) };
            poll_fds.push(PollFd::new(borrowed_fd, PollFlags::POLLIN));
        }
        poll(&mut poll_fds, PollTimeout::NONE).expect("Failed to poll");
    }
}

#[cfg(unix)]
struct PortOutputFd(OwnedFd);

#[cfg(unix)]
impl AsRawFd for PortOutputFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[cfg(unix)]
impl PortOutput for PortOutputFd {
    fn write_volatile(&mut self, buf: &VolatileSlice) -> Result<usize, io::Error> {
        self.0.write_volatile(buf).map_err(|e| match e {
            VolatileMemoryError::IOError(e) => e,
            e => {
                log::error!("Unsuported error from write_volatile: {e:?}");
                io::Error::other(e)
            }
        })
    }

    fn wait_until_writable(&self) {
        let mut poll_fds = [PollFd::new(self.0.as_fd(), PollFlags::POLLOUT)];
        poll(&mut poll_fds, PollTimeout::NONE).expect("Failed to poll");
    }
}

#[cfg(unix)]
fn dup_raw_fd_into_owned(raw_fd: RawFd) -> Result<OwnedFd, nix::Error> {
    // SAFETY: if raw_fd is invalid the `dup` call below will fail
    let borrowed_fd = unsafe { BorrowedFd::borrow_raw(raw_fd) };
    let fd = dup(borrowed_fd)?;
    Ok(fd)
}

#[cfg(unix)]
fn make_non_blocking(as_rw_fd: &impl AsRawFd) -> Result<(), nix::Error> {
    let fd = as_rw_fd.as_raw_fd();
    unsafe {
        let flags = fcntl(fd, F_GETFL, 0);
        if flags < 0 {
            return Err(Errno::last());
        }

        if fcntl(fd, F_SETFL, flags | O_NONBLOCK) < 0 {
            return Err(Errno::last());
        }
    }
    Ok(())
}

// Utility to relay log from the VM (the kernel boot log and messages from init)
// to the rust log
#[derive(Default)]
pub struct PortOutputLog {
    buf: Vec<u8>,
}

impl PortOutputLog {
    const FORCE_FLUSH_TRESHOLD: usize = 512;
    const LOG_TARGET: &'static str = "init_or_kernel";

    fn new() -> Self {
        Self::default()
    }

    fn force_flush(&mut self) {
        log::log!(target: PortOutputLog::LOG_TARGET, Level::Error, "[missing newline]{}", String::from_utf8_lossy(&self.buf));
        self.buf.clear();
    }
}

impl PortOutput for PortOutputLog {
    fn write_volatile(&mut self, buf: &VolatileSlice) -> Result<usize, io::Error> {
        self.buf.write_volatile(buf).map_err(io::Error::other)?;

        let mut start = 0;
        for (i, ch) in self.buf.iter().cloned().enumerate() {
            if ch == b'\n' {
                log::log!(target: PortOutputLog::LOG_TARGET, Level::Error, "{}", String::from_utf8_lossy(&self.buf[start..i]));
                start = i + 1;
            }
        }
        self.buf.drain(0..start);
        // Make sure to not grow the internal buffer forever!
        if self.buf.len() > PortOutputLog::FORCE_FLUSH_TRESHOLD {
            self.force_flush()
        }
        Ok(buf.len())
    }

    fn wait_until_writable(&self) {}
}

pub struct PortInputSigInt {
    sigint_evt: EventFd,
}

impl PortInputSigInt {
    pub fn new() -> Self {
        PortInputSigInt {
            sigint_evt: EventFd::new(EFD_NONBLOCK)
                .expect("Failed to create EventFd for SIGINT signaling"),
        }
    }

    pub fn sigint_evt(&self) -> &EventFd {
        &self.sigint_evt
    }
}

impl Default for PortInputSigInt {
    fn default() -> Self {
        Self::new()
    }
}

impl PortInput for PortInputSigInt {
    fn read_volatile(&mut self, buf: &mut VolatileSlice) -> Result<usize, io::Error> {
        self.sigint_evt.read()?;
        log::trace!("SIGINT received");
        buf.copy_from(&[3u8]); //ASCII 'ETX' -> generates SIGINIT in a terminal
        Ok(1)
    }

    #[cfg(unix)]
    fn wait_until_readable(&self, stopfd: Option<&EventFd>) {
        let mut poll_fds = Vec::with_capacity(2);
        // SAFETY: we trust sigint_evt won't go away to avoid a dup call here.
        let sigint_bfd = unsafe { BorrowedFd::borrow_raw(self.sigint_evt.as_raw_fd()) };
        poll_fds.push(PollFd::new(sigint_bfd, PollFlags::POLLIN));
        if let Some(stopfd) = stopfd {
            // SAFETY: we trust stopfd won't go away to avoid a dup call here.
            let stop_bfd = unsafe { BorrowedFd::borrow_raw(stopfd.as_raw_fd()) };
            poll_fds.push(PollFd::new(stop_bfd, PollFlags::POLLIN));
        }

        poll(&mut poll_fds, PollTimeout::NONE).expect("Failed to poll");
    }

    #[cfg(target_os = "windows")]
    fn wait_until_readable(&self, stopfd: Option<&EventFd>) {
        if let Some(stopfd) = stopfd {
            let _ = stopfd.read(); // blocking read
        } else {
            let _ = self.sigint_evt.read(); // blocking read
        }
    }
}

pub struct PortInputEmpty {}

impl PortInputEmpty {
    pub fn new() -> Self {
        PortInputEmpty {}
    }
}

impl Default for PortInputEmpty {
    fn default() -> Self {
        Self::new()
    }
}

impl PortInput for PortInputEmpty {
    fn read_volatile(&mut self, _buf: &mut VolatileSlice) -> Result<usize, io::Error> {
        Ok(0)
    }

    #[cfg(unix)]
    fn wait_until_readable(&self, stopfd: Option<&EventFd>) {
        if let Some(stopfd) = stopfd {
            // SAFETY: we trust stopfd won't go away to avoid a dup call here.
            let borrowed_fd = unsafe { BorrowedFd::borrow_raw(stopfd.as_raw_fd()) };
            let mut poll_fds = [PollFd::new(borrowed_fd, PollFlags::POLLIN)];
            poll(&mut poll_fds, PollTimeout::NONE).expect("Failed to poll");
        } else {
            std::thread::sleep(std::time::Duration::MAX);
        }
    }

    #[cfg(target_os = "windows")]
    fn wait_until_readable(&self, stopfd: Option<&EventFd>) {
        if let Some(stopfd) = stopfd {
            let _ = stopfd.read(); // blocking read
        } else {
            std::thread::sleep(std::time::Duration::MAX);
        }
    }
}

struct PortTerminalPropertiesFixed((u16, u16));

impl PortTerminalProperties for PortTerminalPropertiesFixed {
    fn get_win_size(&self) -> (u16, u16) {
        self.0
    }
}

#[cfg(unix)]
struct PortTerminalPropertiesFd(OwnedFd);

#[cfg(unix)]
impl PortTerminalProperties for PortTerminalPropertiesFd {
    fn get_win_size(&self) -> (u16, u16) {
        let mut ws: WS = WS::default();

        if let Err(err) = unsafe { tiocgwinsz(self.0.as_raw_fd(), &mut ws) } {
            error!("Couldn't get terminal dimensions: {err}");
            return (0, 0);
        }
        (ws.cols, ws.rows)
    }
}

#[cfg(unix)]
#[repr(C)]
#[derive(Default)]
struct WS {
    rows: u16,
    cols: u16,
    xpixel: u16,
    ypixel: u16,
}
#[cfg(unix)]
ioctl_read_bad!(tiocgwinsz, TIOCGWINSZ, WS);

// =============================================================================
// Windows-specific console I/O implementations
// =============================================================================

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::*;
    use std::os::windows::io::{AsRawHandle, RawHandle};
    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
    };
    use windows_sys::Win32::System::Console::{
        GetConsoleScreenBufferInfo, GetStdHandle, CONSOLE_SCREEN_BUFFER_INFO,
        STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Threading::{WaitForMultipleObjects, WaitForSingleObject, INFINITE};

    /// Reads from a Windows HANDLE (stdin or pipe).
    pub(super) struct PortInputHandle {
        handle: HANDLE,
    }

    // SAFETY: Windows HANDLEs are safe to send across threads.
    unsafe impl Send for PortInputHandle {}

    impl PortInputHandle {
        pub fn new(handle: HANDLE) -> Self {
            PortInputHandle { handle }
        }
    }

    impl PortInput for PortInputHandle {
        fn read_volatile(&mut self, buf: &mut VolatileSlice) -> io::Result<usize> {
            let guard = buf.ptr_guard_mut();
            let dst = guard.as_ptr();
            let len = buf.len().min(u32::MAX as usize) as u32;
            let mut bytes_read: u32 = 0;

            // SAFETY: `dst` points to valid memory of length `buf.len()` per VolatileSlice
            // invariants. `self.handle` is a valid Windows HANDLE.
            let ret = unsafe {
                ReadFile(
                    self.handle,
                    dst.cast(),
                    len,
                    &mut bytes_read,
                    std::ptr::null_mut(),
                )
            };

            if ret == 0 {
                let err = io::Error::last_os_error();
                if err.kind() != ErrorKind::WouldBlock {
                    buf.bitmap().mark_dirty(0, buf.len());
                }
                Err(err)
            } else {
                let n = bytes_read as usize;
                buf.bitmap().mark_dirty(0, n);
                Ok(n)
            }
        }

        fn wait_until_readable(&self, stopfd: Option<&EventFd>) {
            if let Some(stopfd) = stopfd {
                let stop_handle = stopfd.as_raw_handle() as HANDLE;
                let handles = [self.handle, stop_handle];
                // SAFETY: Both handles are valid. We wait for either the input
                // handle to become signaled or the stop event to fire.
                unsafe {
                    WaitForMultipleObjects(
                        handles.len() as u32,
                        handles.as_ptr(),
                        0, // bWaitAll = FALSE
                        INFINITE,
                    );
                }
            } else {
                // SAFETY: self.handle is a valid HANDLE.
                unsafe {
                    WaitForSingleObject(self.handle, INFINITE);
                }
            }
        }
    }

    /// Writes to a Windows HANDLE (stdout, stderr, or file).
    pub(super) struct PortOutputHandle {
        handle: HANDLE,
    }

    // SAFETY: Windows HANDLEs are safe to send across threads.
    unsafe impl Send for PortOutputHandle {}

    impl PortOutputHandle {
        pub fn new(handle: HANDLE) -> Self {
            PortOutputHandle { handle }
        }
    }

    impl PortOutput for PortOutputHandle {
        fn write_volatile(&mut self, buf: &VolatileSlice) -> Result<usize, io::Error> {
            let guard = buf.ptr_guard();
            let src = guard.as_ptr();
            let len = buf.len().min(u32::MAX as usize) as u32;
            let mut bytes_written: u32 = 0;

            // SAFETY: `src` points to valid readable memory of length `buf.len()` per
            // VolatileSlice invariants. `self.handle` is a valid Windows HANDLE.
            let ret = unsafe {
                WriteFile(
                    self.handle,
                    src.cast(),
                    len,
                    &mut bytes_written,
                    std::ptr::null_mut(),
                )
            };

            if ret == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(bytes_written as usize)
            }
        }

        fn wait_until_writable(&self) {
            // For console handles and pipes, writes are typically synchronous
            // and always writable, so this is a no-op. For true async I/O on
            // Windows, overlapped I/O would be needed.
        }
    }

    /// Queries Windows console dimensions via GetConsoleScreenBufferInfo.
    pub(super) struct PortTerminalPropertiesConsole {
        handle: HANDLE,
    }

    // SAFETY: Windows HANDLEs are safe to send/share across threads.
    unsafe impl Send for PortTerminalPropertiesConsole {}
    unsafe impl Sync for PortTerminalPropertiesConsole {}

    impl PortTerminalPropertiesConsole {
        pub fn new(handle: HANDLE) -> Self {
            PortTerminalPropertiesConsole { handle }
        }
    }

    impl PortTerminalProperties for PortTerminalPropertiesConsole {
        fn get_win_size(&self) -> (u16, u16) {
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
            // SAFETY: self.handle is a valid console HANDLE.
            let ret = unsafe { GetConsoleScreenBufferInfo(self.handle, &mut info) };
            if ret == 0 {
                // Not a console (e.g., pipe or file) — return a sensible default.
                (80, 24)
            } else {
                let width = (info.srWindow.Right - info.srWindow.Left + 1) as u16;
                let height = (info.srWindow.Bottom - info.srWindow.Top + 1) as u16;
                (width, height)
            }
        }
    }

    /// An output handle that owns the underlying Windows HANDLE and closes it on drop.
    pub(super) struct PortOutputOwnedHandle {
        handle: HANDLE,
    }

    // SAFETY: Windows HANDLEs are safe to send across threads.
    unsafe impl Send for PortOutputOwnedHandle {}

    impl PortOutputOwnedHandle {
        pub fn new(handle: HANDLE) -> Self {
            PortOutputOwnedHandle { handle }
        }
    }

    impl PortOutput for PortOutputOwnedHandle {
        fn write_volatile(&mut self, buf: &VolatileSlice) -> Result<usize, io::Error> {
            let guard = buf.ptr_guard();
            let src = guard.as_ptr();
            let len = buf.len().min(u32::MAX as usize) as u32;
            let mut bytes_written: u32 = 0;

            let ret = unsafe {
                WriteFile(
                    self.handle,
                    src.cast(),
                    len,
                    &mut bytes_written,
                    std::ptr::null_mut(),
                )
            };

            if ret == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(bytes_written as usize)
            }
        }

        fn wait_until_writable(&self) {
            // Synchronous writes are always writable.
        }
    }

    impl Drop for PortOutputOwnedHandle {
        fn drop(&mut self) {
            // SAFETY: We own this handle and are closing it exactly once.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    // ---- Factory functions ----

    /// Wraps the Windows standard input handle for console port input.
    pub fn stdin_handle() -> Result<Box<dyn PortInput + Send>, io::Error> {
        // SAFETY: GetStdHandle is safe to call with a valid std handle constant.
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Failed to get STD_INPUT_HANDLE",
            ));
        }
        Ok(Box::new(PortInputHandle::new(handle)))
    }

    /// Wraps the Windows standard output handle for console port output.
    pub fn stdout_handle() -> Result<Box<dyn PortOutput + Send>, io::Error> {
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Failed to get STD_OUTPUT_HANDLE",
            ));
        }
        Ok(Box::new(PortOutputHandle::new(handle)))
    }

    /// Wraps the Windows standard error handle for console port output.
    pub fn stderr_handle() -> Result<Box<dyn PortOutput + Send>, io::Error> {
        let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Failed to get STD_ERROR_HANDLE",
            ));
        }
        Ok(Box::new(PortOutputHandle::new(handle)))
    }

    /// Opens a file by path for writing and wraps it as console port output.
    pub fn output_file_handle(path: &str) -> Result<Box<dyn PortOutput + Send>, io::Error> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: wide is a valid null-terminated UTF-16 string.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Box::new(PortOutputOwnedHandle::new(handle)))
    }

    /// Creates a PortTerminalProperties that queries the console for dimensions.
    pub fn term_console() -> Result<Box<dyn PortTerminalProperties + Send + Sync>, io::Error> {
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Failed to get STD_OUTPUT_HANDLE for terminal properties",
            ));
        }
        Ok(Box::new(PortTerminalPropertiesConsole::new(handle)))
    }

    /// Wraps an existing Windows HANDLE for console port input.
    pub fn input_from_handle(handle: HANDLE) -> Result<Box<dyn PortInput + Send>, io::Error> {
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid HANDLE for input",
            ));
        }
        Ok(Box::new(PortInputHandle::new(handle)))
    }

    /// Wraps an existing Windows HANDLE for console port output.
    pub fn output_from_handle(handle: HANDLE) -> Result<Box<dyn PortOutput + Send>, io::Error> {
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid HANDLE for output",
            ));
        }
        Ok(Box::new(PortOutputHandle::new(handle)))
    }
}

// Re-export Windows factory functions at the module level.
#[cfg(target_os = "windows")]
pub use windows_impl::{
    input_from_handle, output_file_handle, output_from_handle, stderr_handle, stdin_handle,
    stdout_handle, term_console,
};

/// Windows-compatible output_file: opens a std::fs::File as a PortOutput using its raw handle.
#[cfg(target_os = "windows")]
pub fn output_file(file: std::fs::File) -> Result<Box<dyn PortOutput + Send>, io::Error> {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    let handle = file.into_raw_handle() as HANDLE;
    Ok(Box::new(windows_impl::PortOutputOwnedHandle::new(handle)))
}
