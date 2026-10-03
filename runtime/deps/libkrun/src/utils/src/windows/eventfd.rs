// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0
//
// EventFd implementation for Windows using Win32 Events.

use std::io;
use std::os::windows::io::{AsRawHandle, RawHandle};

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, FALSE, HANDLE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, ResetEvent, SetEvent, WaitForSingleObject, INFINITE,
};

pub const EFD_NONBLOCK: i32 = 1;
pub const EFD_SEMAPHORE: i32 = 2;

const WAIT_TIMEOUT: u32 = 0x00000102;

#[derive(Debug)]
pub struct EventFd {
    handle: HANDLE,
    nonblocking: bool,
}

// SAFETY: Win32 Event HANDLEs are safe to send/share across threads.
unsafe impl Send for EventFd {}
unsafe impl Sync for EventFd {}

impl EventFd {
    pub fn new(flag: i32) -> Result<EventFd, io::Error> {
        let nonblocking = (flag & EFD_NONBLOCK) != 0;

        // Create a manual-reset event (second param TRUE) so we control reset behavior.
        // Initial state is non-signaled (third param FALSE).
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }

        Ok(EventFd {
            handle,
            nonblocking,
        })
    }

    pub fn write(&self, _v: u64) -> Result<(), io::Error> {
        let ret = unsafe { SetEvent(self.handle) };
        if ret == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn read(&self) -> Result<u64, io::Error> {
        let timeout = if self.nonblocking { 0 } else { INFINITE };
        let ret = unsafe { WaitForSingleObject(self.handle, timeout) };
        match ret {
            WAIT_OBJECT_0 => {
                // Reset the event so subsequent reads block until next write.
                unsafe {
                    ResetEvent(self.handle);
                }
                Ok(1)
            }
            WAIT_TIMEOUT => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub fn try_clone(&self) -> Result<EventFd, io::Error> {
        let mut new_handle: HANDLE = std::ptr::null_mut();
        let ret = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.handle,
                GetCurrentProcess(),
                &mut new_handle,
                0,
                FALSE,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ret == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(EventFd {
            handle: new_handle,
            nonblocking: self.nonblocking,
        })
    }

    /// Returns the underlying handle for polling.
    /// On Windows, both read and write use the same handle.
    pub fn get_write_fd(&self) -> RawHandle {
        self.handle as RawHandle
    }

    /// Returns the raw HANDLE as isize for use as a pollable identifier.
    pub fn as_pollable(&self) -> isize {
        self.handle as isize
    }
}

impl AsRawHandle for EventFd {
    fn as_raw_handle(&self) -> RawHandle {
        self.handle as RawHandle
    }
}

impl Drop for EventFd {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        EventFd::new(EFD_NONBLOCK).unwrap();
        EventFd::new(0).unwrap();
    }

    #[test]
    fn test_read_write() {
        let evt = EventFd::new(EFD_NONBLOCK).unwrap();
        evt.write(55).unwrap();
        assert_eq!(evt.read().unwrap(), 1);
    }

    #[test]
    fn test_read_nothing() {
        let evt = EventFd::new(EFD_NONBLOCK).unwrap();
        let r = evt.read();
        match r {
            Err(ref inner) if inner.kind() == io::ErrorKind::WouldBlock => (),
            _ => panic!("Unexpected"),
        }
    }

    #[test]
    fn test_clone() {
        let evt = EventFd::new(EFD_NONBLOCK).unwrap();
        let evt_clone = evt.try_clone().unwrap();
        evt.write(923).unwrap();
        assert_eq!(evt_clone.read().unwrap(), 1);
    }
}
