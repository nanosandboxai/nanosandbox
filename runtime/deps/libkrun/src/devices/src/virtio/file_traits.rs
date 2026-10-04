// Copyright 2018 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fs::File;
use std::io::{Error, ErrorKind, Result};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

#[cfg(feature = "blk")]
use imago::io_buffers::{IoVector, IoVectorMut};
use vm_memory::VolatileSlice;

#[cfg(unix)]
use libc::{c_int, c_void, read, readv, size_t, write, writev};

#[cfg(unix)]
use super::bindings::{off64_t, pread64, preadv64, pwrite64, pwritev64};
#[cfg(feature = "blk")]
use super::block::device::DiskProperties;

// ---------- Windows file I/O helpers ----------
//
// These provide pread/pwrite/fsync/ftruncate equivalents on Windows using
// the Win32 API via `windows_sys`.  Each function operates on a raw HANDLE
// obtained from a `std::fs::File` (via `AsRawHandle`).

#[cfg(target_os = "windows")]
mod win_io {
    use std::fs::File;
    use std::io::{Error, Result};
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FlushFileBuffers, ReadFile, SetEndOfFile, SetFilePointerEx, WriteFile, FILE_BEGIN,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    /// Read from `file` at byte `offset` into `buf` (pread equivalent).
    ///
    /// Uses `ReadFile` with an `OVERLAPPED` structure so the file-pointer
    /// position is not modified and concurrent reads at different offsets
    /// are safe.
    pub fn win_pread(file: &File, buf: &mut [u8], offset: u64) -> Result<usize> {
        let handle = file.as_raw_handle() as HANDLE;
        let len = buf.len().min(u32::MAX as usize) as u32;
        let mut bytes_read: u32 = 0;

        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.Anonymous.Anonymous.Offset = offset as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;

        // SAFETY: `buf` is valid for `len` bytes; `handle` is a valid file
        // HANDLE obtained from `File::as_raw_handle`.
        let ret = unsafe {
            ReadFile(
                handle,
                buf.as_mut_ptr().cast(),
                len,
                &mut bytes_read,
                &mut overlapped,
            )
        };

        if ret == 0 {
            Err(Error::last_os_error())
        } else {
            Ok(bytes_read as usize)
        }
    }

    /// Write `buf` to `file` at byte `offset` (pwrite equivalent).
    ///
    /// Uses `WriteFile` with an `OVERLAPPED` structure so the file-pointer
    /// position is not modified.
    pub fn win_pwrite(file: &File, buf: &[u8], offset: u64) -> Result<usize> {
        let handle = file.as_raw_handle() as HANDLE;
        let len = buf.len().min(u32::MAX as usize) as u32;
        let mut bytes_written: u32 = 0;

        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.Anonymous.Anonymous.Offset = offset as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;

        // SAFETY: `buf` is valid for `len` bytes; `handle` is a valid file HANDLE.
        let ret = unsafe {
            WriteFile(
                handle,
                buf.as_ptr().cast(),
                len,
                &mut bytes_written,
                &mut overlapped,
            )
        };

        if ret == 0 {
            Err(Error::last_os_error())
        } else {
            Ok(bytes_written as usize)
        }
    }

    /// Flush file buffers to disk (fsync equivalent).
    ///
    /// Calls `FlushFileBuffers` which is the Win32 equivalent of `fsync`.
    pub fn win_fsync(file: &File) -> Result<()> {
        let handle = file.as_raw_handle() as HANDLE;

        // SAFETY: `handle` is a valid file HANDLE.
        let ret = unsafe { FlushFileBuffers(handle) };

        if ret == 0 {
            Err(Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Truncate or extend `file` to `len` bytes (ftruncate equivalent).
    ///
    /// Uses `SetFilePointerEx` to seek to the desired position, then
    /// `SetEndOfFile` to set the new end of file.
    pub fn win_ftruncate(file: &File, len: u64) -> Result<()> {
        let handle = file.as_raw_handle() as HANDLE;

        // SAFETY: `handle` is a valid file HANDLE.
        let ret = unsafe { SetFilePointerEx(handle, len as i64, std::ptr::null_mut(), FILE_BEGIN) };

        if ret == 0 {
            return Err(Error::last_os_error());
        }

        // SAFETY: file pointer has been set to the desired position above.
        let ret = unsafe { SetEndOfFile(handle) };

        if ret == 0 {
            Err(Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "windows")]
pub use win_io::{win_fsync, win_ftruncate, win_pread, win_pwrite};

/// A trait for setting the size of a file.
/// This is equivalent to File's `set_len` method, but
/// wrapped in a trait so that it can be implemented for
/// other types.
pub trait FileSetLen {
    // Set the size of this file.
    // This is the moral equivalent of `ftruncate()`.
    fn set_len(&self, _len: u64) -> Result<()>;
}

impl FileSetLen for File {
    fn set_len(&self, len: u64) -> Result<()> {
        File::set_len(self, len)
    }
}

/// A trait similar to `Read` and `Write`, but uses volatile memory as buffers.
pub trait FileReadWriteVolatile {
    /// Read bytes from this file into the given slice, returning the number of bytes read on
    /// success.
    fn read_volatile(&mut self, slice: VolatileSlice) -> Result<usize>;

    /// Like `read_volatile`, except it reads to a slice of buffers. Data is copied to fill each
    /// buffer in order, with the final buffer written to possibly being only partially filled. This
    /// method must behave as a single call to `read_volatile` with the buffers concatenated would.
    /// The default implementation calls `read_volatile` with either the first nonempty buffer
    /// provided, or returns `Ok(0)` if none exists.
    fn read_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> Result<usize> {
        bufs.iter()
            .find(|b| !b.is_empty())
            .map(|&b| self.read_volatile(b))
            .unwrap_or(Ok(0))
    }

    /// Reads bytes from this into the given slice until all bytes in the slice are written, or an
    /// error is returned.
    fn read_exact_volatile(&mut self, mut slice: VolatileSlice) -> Result<()> {
        while !slice.is_empty() {
            let bytes_read = self.read_volatile(slice)?;
            if bytes_read == 0 {
                return Err(Error::from(ErrorKind::UnexpectedEof));
            }
            // Will panic if read_volatile read more bytes than we gave it, which would be worthy of
            // a panic.
            slice = slice.offset(bytes_read).unwrap();
        }
        Ok(())
    }

    /// Write bytes from the slice to the given file, returning the number of bytes written on
    /// success.
    fn write_volatile(&mut self, slice: VolatileSlice) -> Result<usize>;

    /// Like `write_volatile`, except that it writes from a slice of buffers. Data is copied from
    /// each buffer in order, with the final buffer read from possibly being only partially
    /// consumed. This method must behave as a call to `write_volatile` with the buffers
    /// concatenated would. The default implementation calls `write_volatile` with either the first
    /// nonempty buffer provided, or returns `Ok(0)` if none exists.
    fn write_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> Result<usize> {
        bufs.iter()
            .find(|b| !b.is_empty())
            .map(|&b| self.write_volatile(b))
            .unwrap_or(Ok(0))
    }

    /// Write bytes from the slice to the given file until all the bytes from the slice have been
    /// written, or an error is returned.
    fn write_all_volatile(&mut self, mut slice: VolatileSlice) -> Result<()> {
        while !slice.is_empty() {
            let bytes_written = self.write_volatile(slice)?;
            if bytes_written == 0 {
                return Err(Error::from(ErrorKind::WriteZero));
            }
            // Will panic if read_volatile read more bytes than we gave it, which would be worthy of
            // a panic.
            slice = slice.offset(bytes_written).unwrap();
        }
        Ok(())
    }
}

impl<T: FileReadWriteVolatile + ?Sized> FileReadWriteVolatile for &mut T {
    fn read_volatile(&mut self, slice: VolatileSlice) -> Result<usize> {
        (**self).read_volatile(slice)
    }

    fn read_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> Result<usize> {
        (**self).read_vectored_volatile(bufs)
    }

    fn read_exact_volatile(&mut self, slice: VolatileSlice) -> Result<()> {
        (**self).read_exact_volatile(slice)
    }

    fn write_volatile(&mut self, slice: VolatileSlice) -> Result<usize> {
        (**self).write_volatile(slice)
    }

    fn write_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> Result<usize> {
        (**self).write_vectored_volatile(bufs)
    }

    fn write_all_volatile(&mut self, slice: VolatileSlice) -> Result<()> {
        (**self).write_all_volatile(slice)
    }
}

/// A trait similar to the unix `ReadExt` and `WriteExt` traits, but for volatile memory.
pub trait FileReadWriteAtVolatile {
    /// Reads bytes from this file at `offset` into the given slice, returning the number of bytes
    /// read on success.
    fn read_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize>;

    /// Like `read_at_volatile`, except it reads to a slice of buffers. Data is copied to fill each
    /// buffer in order, with the final buffer written to possibly being only partially filled. This
    /// method must behave as a single call to `read_at_volatile` with the buffers concatenated
    /// would. The default implementation calls `read_at_volatile` with either the first nonempty
    /// buffer provided, or returns `Ok(0)` if none exists.
    fn read_vectored_at_volatile(&self, bufs: &[VolatileSlice], offset: u64) -> Result<usize> {
        if let Some(&slice) = bufs.first() {
            self.read_at_volatile(slice, offset)
        } else {
            Ok(0)
        }
    }

    /// Reads bytes from this file at `offset` into the given slice until all bytes in the slice are
    /// read, or an error is returned.
    fn read_exact_at_volatile(&self, mut slice: VolatileSlice, mut offset: u64) -> Result<()> {
        while !slice.is_empty() {
            match self.read_at_volatile(slice, offset) {
                Ok(0) => return Err(Error::from(ErrorKind::UnexpectedEof)),
                Ok(n) => {
                    slice = slice.offset(n).unwrap();
                    offset = offset.checked_add(n as u64).unwrap();
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Writes bytes from this file at `offset` into the given slice, returning the number of bytes
    /// written on success.
    fn write_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize>;

    /// Like `write_at_at_volatile`, except that it writes from a slice of buffers. Data is copied
    /// from each buffer in order, with the final buffer read from possibly being only partially
    /// consumed. This method must behave as a call to `write_at_volatile` with the buffers
    /// concatenated would. The default implementation calls `write_at_volatile` with either the
    /// first nonempty buffer provided, or returns `Ok(0)` if none exists.
    fn write_vectored_at_volatile(&self, bufs: &[VolatileSlice], offset: u64) -> Result<usize> {
        if let Some(&slice) = bufs.first() {
            self.write_at_volatile(slice, offset)
        } else {
            Ok(0)
        }
    }

    /// Writes bytes from this file at `offset` into the given slice until all bytes in the slice
    /// are written, or an error is returned.
    fn write_all_at_volatile(&self, mut slice: VolatileSlice, mut offset: u64) -> Result<()> {
        while !slice.is_empty() {
            match self.write_at_volatile(slice, offset) {
                Ok(0) => return Err(Error::from(ErrorKind::WriteZero)),
                Ok(n) => {
                    slice = slice.offset(n).unwrap();
                    offset = offset.checked_add(n as u64).unwrap();
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl<T: FileReadWriteAtVolatile + ?Sized> FileReadWriteAtVolatile for &T {
    fn read_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize> {
        (**self).read_at_volatile(slice, offset)
    }

    fn read_vectored_at_volatile(&self, bufs: &[VolatileSlice], offset: u64) -> Result<usize> {
        (**self).read_vectored_at_volatile(bufs, offset)
    }

    fn read_exact_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<()> {
        (**self).read_exact_at_volatile(slice, offset)
    }

    fn write_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize> {
        (**self).write_at_volatile(slice, offset)
    }

    fn write_vectored_at_volatile(&self, bufs: &[VolatileSlice], offset: u64) -> Result<usize> {
        (**self).write_vectored_at_volatile(bufs, offset)
    }

    fn write_all_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<()> {
        (**self).write_all_at_volatile(slice, offset)
    }
}

// Unix implementation using raw fd-based I/O (libc::read/write/pread/pwrite/readv/writev).
#[cfg(unix)]
macro_rules! volatile_impl {
    ($ty:ty) => {
        impl FileReadWriteVolatile for $ty {
            fn read_volatile(&mut self, slice: VolatileSlice) -> Result<usize> {
                let ret = unsafe {
                    read(
                        self.as_raw_fd(),
                        slice.ptr_guard_mut().as_ptr() as *mut c_void,
                        slice.len(),
                    )
                };
                if ret >= 0 {
                    Ok(ret as usize)
                } else {
                    Err(Error::last_os_error())
                }
            }

            fn read_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> Result<usize> {
                let iovecs: Vec<libc::iovec> = bufs
                    .iter()
                    .map(|s| libc::iovec {
                        iov_base: s.ptr_guard_mut().as_ptr() as *mut c_void,
                        iov_len: s.len() as size_t,
                    })
                    .collect();

                if iovecs.is_empty() {
                    return Ok(0);
                }

                let ret = unsafe { readv(self.as_raw_fd(), &iovecs[0], iovecs.len() as c_int) };
                if ret >= 0 {
                    Ok(ret as usize)
                } else {
                    Err(Error::last_os_error())
                }
            }

            fn write_volatile(&mut self, slice: VolatileSlice) -> Result<usize> {
                let ret = unsafe {
                    write(
                        self.as_raw_fd(),
                        slice.ptr_guard().as_ptr() as *const c_void,
                        slice.len(),
                    )
                };
                if ret >= 0 {
                    Ok(ret as usize)
                } else {
                    Err(Error::last_os_error())
                }
            }

            fn write_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> Result<usize> {
                let iovecs: Vec<libc::iovec> = bufs
                    .iter()
                    .map(|s| libc::iovec {
                        iov_base: s.ptr_guard_mut().as_ptr() as *mut c_void,
                        iov_len: s.len() as size_t,
                    })
                    .collect();

                if iovecs.is_empty() {
                    return Ok(0);
                }

                let ret = unsafe { writev(self.as_raw_fd(), &iovecs[0], iovecs.len() as c_int) };
                if ret >= 0 {
                    Ok(ret as usize)
                } else {
                    Err(Error::last_os_error())
                }
            }
        }

        impl FileReadWriteAtVolatile for $ty {
            fn read_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize> {
                let ret = unsafe {
                    pread64(
                        self.as_raw_fd(),
                        slice.ptr_guard_mut().as_ptr() as *mut c_void,
                        slice.len(),
                        offset as off64_t,
                    )
                };

                if ret >= 0 {
                    Ok(ret as usize)
                } else {
                    Err(Error::last_os_error())
                }
            }

            fn read_vectored_at_volatile(
                &self,
                bufs: &[VolatileSlice],
                offset: u64,
            ) -> Result<usize> {
                let iovecs: Vec<libc::iovec> = bufs
                    .iter()
                    .map(|s| libc::iovec {
                        iov_base: s.ptr_guard_mut().as_ptr() as *mut c_void,
                        iov_len: s.len() as size_t,
                    })
                    .collect();

                if iovecs.is_empty() {
                    return Ok(0);
                }

                let ret = unsafe {
                    preadv64(
                        self.as_raw_fd(),
                        &iovecs[0],
                        iovecs.len() as c_int,
                        offset as off64_t,
                    )
                };
                if ret >= 0 {
                    Ok(ret as usize)
                } else {
                    Err(Error::last_os_error())
                }
            }

            fn write_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize> {
                let ret = unsafe {
                    pwrite64(
                        self.as_raw_fd(),
                        slice.ptr_guard().as_ptr() as *const c_void,
                        slice.len(),
                        offset as off64_t,
                    )
                };

                if ret >= 0 {
                    Ok(ret as usize)
                } else {
                    Err(Error::last_os_error())
                }
            }

            fn write_vectored_at_volatile(
                &self,
                bufs: &[VolatileSlice],
                offset: u64,
            ) -> Result<usize> {
                let iovecs: Vec<libc::iovec> = bufs
                    .iter()
                    .map(|s| libc::iovec {
                        iov_base: s.ptr_guard_mut().as_ptr() as *mut c_void,
                        iov_len: s.len() as size_t,
                    })
                    .collect();

                if iovecs.is_empty() {
                    return Ok(0);
                }

                let ret = unsafe {
                    pwritev64(
                        self.as_raw_fd(),
                        &iovecs[0],
                        iovecs.len() as c_int,
                        offset as off64_t,
                    )
                };
                if ret >= 0 {
                    Ok(ret as usize)
                } else {
                    Err(Error::last_os_error())
                }
            }
        }
    };
}

#[cfg(unix)]
volatile_impl!(File);

// Windows implementation using std::io::Read/Write and Win32 ReadFile/WriteFile
// with OVERLAPPED for offset-based I/O (pread/pwrite equivalents).
#[cfg(target_os = "windows")]
impl FileReadWriteVolatile for File {
    fn read_volatile(&mut self, slice: VolatileSlice) -> Result<usize> {
        use std::io::Read;
        // Safety: the slice gives us a valid pointer and length.
        let buf = unsafe {
            std::slice::from_raw_parts_mut(slice.ptr_guard_mut().as_ptr(), slice.len())
        };
        self.read(buf)
    }

    fn read_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> Result<usize> {
        use std::io::Read;
        // Read into each buffer sequentially. Windows does not have readv(),
        // so we emulate vectored reads by iterating over each slice.
        let mut total = 0usize;
        for buf in bufs {
            if buf.is_empty() {
                continue;
            }
            let slice = unsafe {
                std::slice::from_raw_parts_mut(buf.ptr_guard_mut().as_ptr(), buf.len())
            };
            match self.read(slice) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    if total > 0 {
                        break;
                    }
                    return Err(e);
                }
            }
        }
        Ok(total)
    }

    fn write_volatile(&mut self, slice: VolatileSlice) -> Result<usize> {
        use std::io::Write;
        let buf = unsafe { std::slice::from_raw_parts(slice.ptr_guard().as_ptr(), slice.len()) };
        self.write(buf)
    }

    fn write_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> Result<usize> {
        use std::io::Write;
        // Write from each buffer sequentially (emulate writev).
        let mut total = 0usize;
        for buf in bufs {
            if buf.is_empty() {
                continue;
            }
            let slice =
                unsafe { std::slice::from_raw_parts(buf.ptr_guard().as_ptr(), buf.len()) };
            match self.write(slice) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    if total > 0 {
                        break;
                    }
                    return Err(e);
                }
            }
        }
        Ok(total)
    }
}

#[cfg(target_os = "windows")]
impl FileReadWriteAtVolatile for File {
    fn read_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize> {
        let buf = unsafe {
            std::slice::from_raw_parts_mut(slice.ptr_guard_mut().as_ptr(), slice.len())
        };
        win_pread(self, buf, offset)
    }

    fn read_vectored_at_volatile(&self, bufs: &[VolatileSlice], offset: u64) -> Result<usize> {
        // Emulate preadv: read into each buffer at incrementing offsets.
        let mut total = 0usize;
        let mut current_offset = offset;
        for buf in bufs {
            if buf.is_empty() {
                continue;
            }
            let slice = unsafe {
                std::slice::from_raw_parts_mut(buf.ptr_guard_mut().as_ptr(), buf.len())
            };
            match win_pread(self, slice, current_offset) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    current_offset += n as u64;
                    // Short read means we hit EOF or similar.
                    if n < slice.len() {
                        break;
                    }
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    if total > 0 {
                        break;
                    }
                    return Err(e);
                }
            }
        }
        Ok(total)
    }

    fn write_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize> {
        let buf = unsafe { std::slice::from_raw_parts(slice.ptr_guard().as_ptr(), slice.len()) };
        win_pwrite(self, buf, offset)
    }

    fn write_vectored_at_volatile(&self, bufs: &[VolatileSlice], offset: u64) -> Result<usize> {
        // Emulate pwritev: write from each buffer at incrementing offsets.
        let mut total = 0usize;
        let mut current_offset = offset;
        for buf in bufs {
            if buf.is_empty() {
                continue;
            }
            let slice =
                unsafe { std::slice::from_raw_parts(buf.ptr_guard().as_ptr(), buf.len()) };
            match win_pwrite(self, slice, current_offset) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    current_offset += n as u64;
                    if n < slice.len() {
                        break;
                    }
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    if total > 0 {
                        break;
                    }
                    return Err(e);
                }
            }
        }
        Ok(total)
    }
}

#[cfg(feature = "blk")]
impl FileReadWriteAtVolatile for DiskProperties {
    fn read_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize> {
        self.read_vectored_at_volatile(&[slice], offset)
    }

    fn read_vectored_at_volatile(&self, bufs: &[VolatileSlice], offset: u64) -> Result<usize> {
        if bufs.is_empty() {
            return Ok(0);
        }

        let (iovec, _guard) = IoVectorMut::from_volatile_slice(bufs);
        let full_length = iovec
            .len()
            .try_into()
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
        self.file.lock().unwrap().readv(iovec, offset)?;
        Ok(full_length)
    }

    fn write_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize> {
        self.write_vectored_at_volatile(&[slice], offset)
    }

    fn write_vectored_at_volatile(&self, bufs: &[VolatileSlice], offset: u64) -> Result<usize> {
        if bufs.is_empty() {
            return Ok(0);
        }

        let (iovec, _guard) = IoVector::from_volatile_slice(bufs);
        let full_length = iovec
            .len()
            .try_into()
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
        self.file.lock().unwrap().writev(iovec, offset)?;
        Ok(full_length)
    }
}
