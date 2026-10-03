// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0
//
// Epoll-compatible event notification for Windows using WaitForMultipleObjects.

use std::collections::HashMap;
use std::io;

use bitflags::bitflags;
use log::debug;
use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    WaitForMultipleObjects, WaitForSingleObject, INFINITE,
};

const WAIT_TIMEOUT: u32 = 0x00000102;
const WAIT_FAILED: u32 = 0xFFFFFFFF;
const MAXIMUM_WAIT_OBJECTS: usize = 64;

#[repr(i32)]
pub enum ControlOperation {
    Add,
    Modify,
    Delete,
}

bitflags! {
    pub struct EventSet: u32 {
        const IN = 0b00000001;
        const OUT = 0b00000010;
        const HANG_UP = 0b00000100;
        const READ_HANG_UP = 0b00001000;
        const EDGE_TRIGGERED = 0b00010000;
    }
}

#[derive(Clone, Copy, Default)]
pub struct EpollEvent {
    pub events: u32,
    u64: u64,
}

impl std::fmt::Debug for EpollEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{{ events: {}, data: {} }}", self.events(), self.data())
    }
}

impl EpollEvent {
    pub fn new(events: EventSet, data: u64) -> Self {
        debug!("EpollEvent new: {data}");
        EpollEvent {
            events: events.bits(),
            u64: data,
        }
    }

    pub fn events(&self) -> u32 {
        self.events
    }

    pub fn event_set(&self) -> EventSet {
        EventSet::from_bits(self.events()).unwrap()
    }

    pub fn data(&self) -> u64 {
        debug!("EpollEvent data: {}", self.u64);
        self.u64
    }

    /// Returns the data field as a pollable handle.
    pub fn fd(&self) -> isize {
        self.u64 as isize
    }
}

/// Entry tracking a registered handle and its associated event data.
#[derive(Clone, Debug)]
struct HandleEntry {
    events: EventSet,
    data: u64,
}

/// Epoll-like interface backed by WaitForMultipleObjects on Windows.
///
/// Limitations:
/// - Maximum of 64 handles (MAXIMUM_WAIT_OBJECTS). Typical libkrun usage is <30.
/// - Edge-triggered semantics are approximated (handles are always level-triggered).
#[derive(Clone, Debug)]
pub struct Epoll {
    /// Maps HANDLE (as isize) → registration info.
    registered: HashMap<isize, HandleEntry>,
    /// Ordered list of handles for WaitForMultipleObjects.
    handles: Vec<HANDLE>,
}

impl Epoll {
    pub fn new() -> io::Result<Self> {
        Ok(Epoll {
            registered: HashMap::new(),
            handles: Vec::new(),
        })
    }

    pub fn ctl(
        &mut self,
        operation: ControlOperation,
        fd: isize,
        event: &EpollEvent,
    ) -> io::Result<()> {
        let eset = EventSet::from_bits(event.events).unwrap_or(EventSet::empty());

        match operation {
            ControlOperation::Add => {
                if self.registered.contains_key(&fd) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "handle already registered",
                    ));
                }
                if self.handles.len() >= MAXIMUM_WAIT_OBJECTS {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "maximum wait objects exceeded",
                    ));
                }
                debug!("epoll add: handle={fd}");
                self.registered.insert(
                    fd,
                    HandleEntry {
                        events: eset,
                        data: event.u64,
                    },
                );
                self.handles.push(fd as HANDLE);
            }
            ControlOperation::Modify => {
                if let Some(entry) = self.registered.get_mut(&fd) {
                    debug!("epoll modify: handle={fd}");
                    entry.events = eset;
                    entry.data = event.u64;
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "handle not registered",
                    ));
                }
            }
            ControlOperation::Delete => {
                debug!("epoll delete: handle={fd}");
                self.registered.remove(&fd);
                self.handles.retain(|&h| (h as isize) != fd);
            }
        }
        Ok(())
    }

    pub fn wait(
        &self,
        max_events: usize,
        timeout: i32,
        events: &mut [EpollEvent],
    ) -> io::Result<usize> {
        if self.handles.is_empty() {
            if timeout > 0 {
                std::thread::sleep(std::time::Duration::from_millis(timeout as u64));
            }
            return Ok(0);
        }

        let timeout_ms = if timeout < 0 {
            INFINITE
        } else {
            timeout as u32
        };

        let n = self.handles.len() as u32;

        // FALSE = wait for any one object (not all).
        let result = unsafe { WaitForMultipleObjects(n, self.handles.as_ptr(), 0, timeout_ms) };

        if result == WAIT_TIMEOUT {
            return Ok(0);
        }
        if result == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }

        let first_index = (result - WAIT_OBJECT_0) as usize;
        if first_index >= self.handles.len() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "unexpected wait result",
            ));
        }

        let mut count = 0;

        // Record the first signaled event.
        let handle_key = self.handles[first_index] as isize;
        if let Some(entry) = self.registered.get(&handle_key) {
            if count < max_events && count < events.len() {
                events[count] = EpollEvent {
                    events: entry.events.bits(),
                    u64: entry.data,
                };
                count += 1;
            }
        }

        // Check remaining handles with timeout 0 to find other signaled objects.
        for i in (first_index + 1)..self.handles.len() {
            if count >= max_events || count >= events.len() {
                break;
            }
            let r = unsafe { WaitForSingleObject(self.handles[i], 0) };
            if r == WAIT_OBJECT_0 {
                let hkey = self.handles[i] as isize;
                if let Some(entry) = self.registered.get(&hkey) {
                    events[count] = EpollEvent {
                        events: entry.events.bits(),
                        u64: entry.data,
                    };
                    count += 1;
                }
            }
        }

        // Also check handles before first_index (they might have been signaled too).
        for i in 0..first_index {
            if count >= max_events || count >= events.len() {
                break;
            }
            let r = unsafe { WaitForSingleObject(self.handles[i], 0) };
            if r == WAIT_OBJECT_0 {
                let hkey = self.handles[i] as isize;
                if let Some(entry) = self.registered.get(&hkey) {
                    events[count] = EpollEvent {
                        events: entry.events.bits(),
                        u64: entry.data,
                    };
                    count += 1;
                }
            }
        }

        debug!("epoll wait returned {count} events");
        Ok(count)
    }
}

// SAFETY: Win32 HANDLEs are safe to send/share across threads. On windows-sys 0.59,
// HANDLE is *mut c_void which does not auto-implement Send/Sync.
unsafe impl Send for Epoll {}
unsafe impl Sync for Epoll {}

impl std::fmt::Display for Epoll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Epoll({} handles)", self.handles.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventfd::{EventFd, EFD_NONBLOCK};

    #[test]
    fn test_event_ops() {
        let mut event = EpollEvent::default();
        assert_eq!(event.events(), 0);
        assert_eq!(event.data(), 0);

        event = EpollEvent::new(EventSet::IN, 2);
        assert_eq!(event.events(), 1);
        assert_eq!(event.event_set(), EventSet::IN);
        assert_eq!(event.data(), 2);
    }

    #[test]
    fn test_events_debug() {
        let events = EpollEvent::new(EventSet::IN, 42);
        assert_eq!(format!("{:?}", events), "{ events: 1, data: 42 }")
    }

    #[test]
    fn test_epoll() {
        const DEFAULT_TIMEOUT: i32 = 250;
        const MAX_EVENTS: usize = 10;

        let mut epoll = Epoll::new().unwrap();

        let event_fd_1 = EventFd::new(EFD_NONBLOCK).unwrap();
        event_fd_1.write(1).unwrap();

        let handle1 = event_fd_1.as_pollable();
        let event_1 = EpollEvent::new(EventSet::IN, handle1 as u64);

        assert!(epoll.ctl(ControlOperation::Add, handle1, &event_1).is_ok());

        let event_fd_2 = EventFd::new(EFD_NONBLOCK).unwrap();
        event_fd_2.write(1).unwrap();
        let handle2 = event_fd_2.as_pollable();

        assert!(epoll
            .ctl(
                ControlOperation::Add,
                handle2,
                &EpollEvent::new(EventSet::IN, 10)
            )
            .is_ok());

        let mut ready_events = vec![EpollEvent::default(); 128];
        let ev_count = epoll
            .wait(MAX_EVENTS, DEFAULT_TIMEOUT, &mut ready_events[..])
            .unwrap();

        assert!(ev_count >= 1);

        assert!(epoll
            .ctl(
                ControlOperation::Delete,
                handle2,
                &EpollEvent::default()
            )
            .is_ok());
    }
}
