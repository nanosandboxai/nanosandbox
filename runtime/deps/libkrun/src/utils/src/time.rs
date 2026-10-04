// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt;

/// Constant to convert seconds to nanoseconds.
pub const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Wrapper over clock source identifiers.
pub enum ClockType {
    /// Monotonic clock.
    Monotonic,
    /// Real (wall-clock) time.
    Real,
    /// Process CPU time.
    ProcessCpu,
    /// Thread CPU time.
    ThreadCpu,
}

#[cfg(unix)]
impl From<ClockType> for libc::clockid_t {
    fn from(ctype: ClockType) -> libc::clockid_t {
        match ctype {
            ClockType::Monotonic => libc::CLOCK_MONOTONIC,
            ClockType::Real => libc::CLOCK_REALTIME,
            ClockType::ProcessCpu => libc::CLOCK_PROCESS_CPUTIME_ID,
            ClockType::ThreadCpu => libc::CLOCK_THREAD_CPUTIME_ID,
        }
    }
}

/// Structure representing the date in local time with nanosecond precision.
pub struct LocalTime {
    /// Seconds in current minute.
    sec: i32,
    /// Minutes in current hour.
    min: i32,
    /// Hours in current day, 24H format.
    hour: i32,
    /// Days in current month.
    mday: i32,
    /// Months in current year.
    mon: i32,
    /// Years passed since 1900 BC.
    year: i32,
    /// Nanoseconds in current second.
    nsec: i64,
}

impl LocalTime {
    /// Returns the [LocalTime](struct.LocalTime.html) structure for the calling moment.
    #[cfg(unix)]
    pub fn now() -> LocalTime {
        let mut timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let mut tm: libc::tm = libc::tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 0,
            tm_mon: 0,
            tm_year: 0,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
            tm_gmtoff: 0,
            #[cfg(target_os = "linux")]
            tm_zone: std::ptr::null(),
            #[cfg(target_os = "macos")]
            tm_zone: std::ptr::null_mut(),
        };

        // Safe because the parameters are valid.
        unsafe {
            libc::clock_gettime(libc::CLOCK_REALTIME, &mut timespec);
            libc::localtime_r(&timespec.tv_sec, &mut tm);
        }

        LocalTime {
            sec: tm.tm_sec,
            min: tm.tm_min,
            hour: tm.tm_hour,
            mday: tm.tm_mday,
            mon: tm.tm_mon,
            year: tm.tm_year,
            nsec: timespec.tv_nsec,
        }
    }

    /// Returns the local time on Windows using GetLocalTime.
    #[cfg(target_os = "windows")]
    pub fn now() -> LocalTime {
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;

        let mut st = windows_sys::Win32::Foundation::SYSTEMTIME {
            wYear: 0,
            wMonth: 0,
            wDayOfWeek: 0,
            wDay: 0,
            wHour: 0,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        };
        unsafe {
            GetLocalTime(&mut st);
        }

        LocalTime {
            sec: st.wSecond as i32,
            min: st.wMinute as i32,
            hour: st.wHour as i32,
            mday: st.wDay as i32,
            mon: (st.wMonth as i32) - 1, // Match Unix convention (0-11)
            year: (st.wYear as i32) - 1900, // Match Unix convention (years since 1900)
            nsec: (st.wMilliseconds as i64) * 1_000_000,
        }
    }
}

impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}",
            self.year + 1900,
            self.mon + 1,
            self.mday,
            self.hour,
            self.min,
            self.sec,
            self.nsec
        )
    }
}

/// Holds a micro-second resolution timestamp with both the real time and cpu time.
#[derive(Clone)]
pub struct TimestampUs {
    /// Real time in microseconds.
    pub time_us: u64,
    /// Cpu time in microseconds.
    pub cputime_us: u64,
}

impl Default for TimestampUs {
    fn default() -> TimestampUs {
        TimestampUs {
            time_us: get_time(ClockType::Monotonic) / 1000,
            cputime_us: get_time(ClockType::ProcessCpu) / 1000,
        }
    }
}

/// Returns a timestamp in nanoseconds from a monotonic clock.
///
/// Uses `_rdstc` on `x86_64` and [`get_time`](fn.get_time.html) on other architectures.
pub fn timestamp_cycles() -> u64 {
    #[cfg(target_arch = "x86_64")]
    // Safe because there's nothing that can go wrong with this call.
    unsafe {
        std::arch::x86_64::_rdtsc()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        get_time(ClockType::Monotonic)
    }
}

/// Returns a timestamp in nanoseconds based on the provided clock type.
#[cfg(unix)]
pub fn get_time(clock_type: ClockType) -> u64 {
    let mut time_struct = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Safe because the parameters are valid.
    unsafe { libc::clock_gettime(clock_type.into(), &mut time_struct) };
    seconds_to_nanoseconds(time_struct.tv_sec).unwrap() as u64 + (time_struct.tv_nsec as u64)
}

/// Returns a timestamp in nanoseconds based on the provided clock type (Windows).
#[cfg(target_os = "windows")]
pub fn get_time(clock_type: ClockType) -> u64 {
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };

    match clock_type {
        ClockType::Monotonic | ClockType::ProcessCpu | ClockType::ThreadCpu => {
            let mut counter: i64 = 0;
            let mut frequency: i64 = 0;
            unsafe {
                QueryPerformanceCounter(&mut counter);
                QueryPerformanceFrequency(&mut frequency);
            }
            if frequency == 0 {
                return 0;
            }
            // Convert to nanoseconds: counter * 1_000_000_000 / frequency
            // Use u128 to avoid overflow.
            ((counter as u128 * NANOS_PER_SECOND as u128) / frequency as u128) as u64
        }
        ClockType::Real => {
            use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;

            let mut ft = windows_sys::Win32::Foundation::FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            unsafe {
                GetSystemTimeAsFileTime(&mut ft);
            }
            // FILETIME is 100-nanosecond intervals since 1601-01-01.
            // Subtract the epoch difference (1601-01-01 to 1970-01-01) to get
            // nanoseconds since the Unix epoch, matching the Unix clock_gettime
            // CLOCK_REALTIME behavior.
            // Epoch difference: 11,644,473,600 seconds = 116,444,736,000,000,000
            // 100-ns intervals.
            const EPOCH_DIFFERENCE_100NS: u64 = 116_444_736_000_000_000;
            let intervals =
                (ft.dwHighDateTime as u64) << 32 | (ft.dwLowDateTime as u64);
            let unix_intervals = intervals.saturating_sub(EPOCH_DIFFERENCE_100NS);
            unix_intervals * 100
        }
    }
}

/// Converts a timestamp in seconds to an equivalent one in nanoseconds.
/// Returns `None` if the conversion overflows.
pub fn seconds_to_nanoseconds(value: i64) -> Option<i64> {
    value.checked_mul(NANOS_PER_SECOND as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_time() {
        for _ in 0..1000 {
            assert!(get_time(ClockType::Monotonic) <= get_time(ClockType::Monotonic));
        }

        for _ in 0..1000 {
            assert!(get_time(ClockType::ProcessCpu) <= get_time(ClockType::ProcessCpu));
        }

        for _ in 0..1000 {
            assert!(get_time(ClockType::ThreadCpu) <= get_time(ClockType::ThreadCpu));
        }

        assert_ne!(get_time(ClockType::Real), 0);
    }

    #[test]
    fn test_local_time_display() {
        let local_time = LocalTime {
            sec: 30,
            min: 15,
            hour: 10,
            mday: 4,
            mon: 6,
            year: 119,
            nsec: 123_456_789,
        };
        assert_eq!(
            String::from("2019-07-04T10:15:30.123456789"),
            local_time.to_string()
        );

        let local_time = LocalTime {
            sec: 5,
            min: 5,
            hour: 5,
            mday: 23,
            mon: 7,
            year: 44,
            nsec: 123,
        };
        assert_eq!(
            String::from("1944-08-23T05:05:05.000000123"),
            local_time.to_string()
        );

        let local_time = LocalTime::now();
        assert!(local_time.mon >= 0 && local_time.mon <= 11);
    }

    #[test]
    fn test_seconds_to_nanoseconds() {
        assert_eq!(
            seconds_to_nanoseconds(100).unwrap() as u64,
            100 * NANOS_PER_SECOND
        );

        assert!(seconds_to_nanoseconds(9_223_372_037).is_none());
    }
}
