// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0
//
// Windows signal handler replacement using Console Ctrl handlers.
//
// On Linux, signal handlers (SIGINT, SIGBUS, etc.) are used to trigger VMM
// shutdown. On Windows, we use SetConsoleCtrlHandler to intercept console
// control events (Ctrl+C, Ctrl+Break, console close) and signal the VMM
// exit EventFd to initiate a clean shutdown.

use std::io;
use std::sync::atomic::{AtomicPtr, Ordering};

use windows_sys::Win32::Foundation::{BOOL, FALSE, HANDLE, TRUE};
use windows_sys::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
};
use windows_sys::Win32::System::Threading::SetEvent;

use utils::eventfd::EventFd;

/// Stores the raw HANDLE of the exit EventFd so the ctrl handler callback
/// can signal it. A null pointer means no handler has been registered yet.
static EXIT_EVENT_HANDLE: AtomicPtr<std::ffi::c_void> =
    AtomicPtr::new(std::ptr::null_mut());

/// Console control handler callback registered with `SetConsoleCtrlHandler`.
///
/// When the user presses Ctrl+C, Ctrl+Break, or closes the console window,
/// this function signals the VMM exit event to trigger a clean shutdown.
///
/// # Safety
///
/// This is called by the Windows runtime on a dedicated thread. It only
/// performs an atomic load and a single Win32 `SetEvent` call, both of
/// which are safe to invoke from any thread context.
unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> BOOL {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
            let handle: HANDLE = EXIT_EVENT_HANDLE.load(Ordering::SeqCst);
            if !handle.is_null() {
                // Signal the exit event so the VMM event loop wakes up and
                // initiates shutdown. Ignoring the return value: if SetEvent
                // fails, there is nothing meaningful we can do inside a ctrl
                // handler.
                SetEvent(handle);
            }
            TRUE
        }
        _ => FALSE,
    }
}

/// Registers a Windows console ctrl handler that will signal `exit_evt`
/// when Ctrl+C, Ctrl+Break, or a console close event is received.
///
/// This is the Windows equivalent of the Linux `register_signal_handlers`
/// and `register_sigint_handler` functions combined.
///
/// # Arguments
///
/// * `exit_evt` - The EventFd that the VMM event loop monitors. Writing to
///   it triggers the VMM shutdown path.
///
/// # Errors
///
/// Returns an error if `SetConsoleCtrlHandler` fails.
pub fn register_ctrl_handler(exit_evt: &EventFd) -> Result<(), io::Error> {
    // Store the raw HANDLE so the callback can access it.
    // as_pollable() returns isize; cast to HANDLE (*mut c_void).
    EXIT_EVENT_HANDLE.store(exit_evt.as_pollable() as HANDLE, Ordering::SeqCst);

    // Register the handler. The second parameter (TRUE) means "add" rather
    // than "remove".
    let ret = unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), TRUE) };
    if ret == 0 {
        // Reset the stored handle on failure so we don't leave stale state.
        EXIT_EVENT_HANDLE.store(std::ptr::null_mut(), Ordering::SeqCst);
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use utils::eventfd::EFD_NONBLOCK;

    #[test]
    fn test_register_ctrl_handler() {
        let evt = EventFd::new(EFD_NONBLOCK).unwrap();
        register_ctrl_handler(&evt).unwrap();

        // Verify the handle was stored.
        let stored = EXIT_EVENT_HANDLE.load(Ordering::SeqCst);
        assert_ne!(stored, 0);
        assert_eq!(stored, evt.as_pollable());
    }

    #[test]
    fn test_ctrl_handler_signals_event() {
        let evt = EventFd::new(EFD_NONBLOCK).unwrap();
        register_ctrl_handler(&evt).unwrap();

        // Simulate what the ctrl handler does: signal the event.
        unsafe {
            ctrl_handler(CTRL_C_EVENT);
        }

        // The event should now be signaled; a non-blocking read should succeed.
        let val = evt.read().unwrap();
        assert_eq!(val, 1);
    }

    #[test]
    fn test_ctrl_handler_unknown_event_returns_false() {
        let evt = EventFd::new(EFD_NONBLOCK).unwrap();
        register_ctrl_handler(&evt).unwrap();

        // An unknown event type should return FALSE (not handled).
        let result = unsafe { ctrl_handler(999) };
        assert_eq!(result, FALSE);
    }
}
