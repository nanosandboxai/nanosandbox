use std::io;

#[cfg(unix)]
use std::os::fd::RawFd;

/// Cross-platform pollable file descriptor type for the net backend.
/// On Unix this is a RawFd; on Windows it is an isize (HANDLE).
#[cfg(unix)]
pub type PollableFd = RawFd;
#[cfg(target_os = "windows")]
pub type PollableFd = isize;

#[allow(dead_code)]
#[derive(Debug)]
pub enum ConnectError {
    #[cfg(unix)]
    InvalidAddress(nix::Error),
    #[cfg(unix)]
    CreateSocket(nix::Error),
    #[cfg(unix)]
    Binding(nix::Error),
    #[cfg(unix)]
    SendingMagic(nix::Error),
    // Tap backend errors.
    #[cfg(unix)]
    OpenNetTun(nix::Error),
    TunSetIff(io::Error),
    TunSetVnetHdrSz(io::Error),
    TunSetOffload(io::Error),
    /// Generic I/O error (used on Windows and as a fallback).
    Io(io::Error),
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum ReadError {
    /// Nothing was written
    NothingRead,
    /// Another internal error occurred (Unix: nix::Error)
    #[cfg(unix)]
    Internal(nix::Error),
    /// Another internal error occurred (Windows: io::Error)
    #[cfg(target_os = "windows")]
    Internal(io::Error),
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum WriteError {
    /// Nothing was written, you can drop the frame or try to resend it later
    NothingWritten,
    /// Part of the buffer was written, the write has to be finished using try_finish_write
    PartialWrite,
    /// Passt doesnt seem to be running (received EPIPE)
    ProcessNotRunning,
    /// Another internal error occurred (Unix: nix::Error)
    #[cfg(unix)]
    Internal(nix::Error),
    /// Another internal error occurred (Windows: io::Error)
    #[cfg(target_os = "windows")]
    Internal(io::Error),
}

pub trait NetBackend {
    fn read_frame(&mut self, buf: &mut [u8]) -> Result<usize, ReadError>;
    fn write_frame(&mut self, hdr_len: usize, buf: &mut [u8]) -> Result<(), WriteError>;
    fn has_unfinished_write(&self) -> bool;
    fn try_finish_write(&mut self, hdr_len: usize, buf: &[u8]) -> Result<(), WriteError>;
    fn pollable_fd(&self) -> PollableFd;

    /// Delay in microseconds before retrying after NothingWritten.
    /// Returns 0 if no delay-based retry is needed (e.g. on Linux where
    /// EAGAIN + EPOLLET handles retries via writable events).
    #[allow(dead_code)]
    fn write_retry_delay_us(&self) -> u64 {
        0
    }
}
