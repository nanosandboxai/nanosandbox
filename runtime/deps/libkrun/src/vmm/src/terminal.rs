#[cfg(unix)]
mod unix {
    use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, LocalFlags, SetArg, Termios};
    use std::os::fd::BorrowedFd;

    #[must_use]
    pub struct TerminalMode(Termios);

    // Enable raw mode for the terminal and return the old state to be restored
    pub fn term_set_raw_mode(
        term: BorrowedFd,
        handle_signals_by_terminal: bool,
    ) -> Result<TerminalMode, nix::Error> {
        let mut termios = tcgetattr(term)?;
        let old_state = termios.clone();

        cfmakeraw(&mut termios);

        if handle_signals_by_terminal {
            termios.local_flags |= LocalFlags::ISIG;
        }

        tcsetattr(term, SetArg::TCSANOW, &termios)?;
        Ok(TerminalMode(old_state))
    }

    pub fn term_restore_mode(term: BorrowedFd, restore: &TerminalMode) -> Result<(), nix::Error> {
        tcsetattr(term, SetArg::TCSANOW, &restore.0)
    }
}

#[cfg(unix)]
pub use unix::*;

#[cfg(target_os = "windows")]
mod windows {
    use std::io;
    use windows_sys::Win32::Foundation::HANDLE;

    /// Placeholder for saved terminal mode on Windows.
    #[must_use]
    pub struct TerminalMode {
        console_mode: u32,
        handle: HANDLE,
    }

    /// Enable raw mode for the Windows console.
    pub fn term_set_raw_mode(
        handle: HANDLE,
        _handle_signals_by_terminal: bool,
    ) -> Result<TerminalMode, io::Error> {
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, SetConsoleMode, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
            ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
        };

        let mut old_mode: u32 = 0;
        let ret = unsafe { GetConsoleMode(handle, &mut old_mode) };
        if ret == 0 {
            return Err(io::Error::last_os_error());
        }

        // Disable line input, echo, and processed input (like Unix raw mode).
        let new_mode =
            (old_mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT;

        let ret = unsafe { SetConsoleMode(handle, new_mode) };
        if ret == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(TerminalMode {
            console_mode: old_mode,
            handle,
        })
    }

    /// Restore the original terminal mode.
    pub fn term_restore_mode(_handle: HANDLE, restore: &TerminalMode) -> Result<(), io::Error> {
        use windows_sys::Win32::System::Console::SetConsoleMode;

        let ret = unsafe { SetConsoleMode(restore.handle, restore.console_mode) };
        if ret == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "windows")]
pub use windows::*;
