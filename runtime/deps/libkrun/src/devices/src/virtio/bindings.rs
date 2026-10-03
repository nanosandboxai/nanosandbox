#![allow(clippy::missing_safety_doc)]
#[cfg(unix)]
use libc;

// Linux error codes (used by FUSE protocol, always the same values regardless of host).
pub const LINUX_EACCES: i32 = 13;
pub const LINUX_ENODATA: i32 = 61;
pub const LINUX_ENOSYS: i32 = 38;
pub const LINUX_ENOTEMPTY: i32 = 39;

// Linux open flags (used by FUSE protocol, always the same values regardless of host).
pub const LINUX_O_APPEND: i32 = 1024;
pub const LINUX_O_CLOEXEC: i32 = 0x80000;
pub const LINUX_O_DIRECT: i32 = 0x4000;
pub const LINUX_O_DIRECTORY: i32 = 0x10000;
pub const LINUX_O_LARGEFILE: i32 = 0;
pub const LINUX_O_NOFOLLOW: i32 = 0x20000;
pub const LINUX_O_CREAT: i32 = 64;
pub const LINUX_O_EXCL: i32 = 128;
pub const LINUX_O_NOCTTY: i32 = 256;
pub const LINUX_O_NONBLOCK: i32 = 2048;
pub const LINUX_O_SYNC: i32 = 1052672;
pub const LINUX_O_TRUNC: i32 = 512;
pub const LINUX_O_RSYNC: i32 = 1052672;
pub const LINUX_O_DSYNC: i32 = 4096;
pub const LINUX_O_ASYNC: i32 = 0x2000;

pub const LINUX_RENAME_NOREPLACE: i32 = 1 << 0;
pub const LINUX_RENAME_EXCHANGE: i32 = 1 << 1;
pub const LINUX_RENAME_WHITEOUT: i32 = 1 << 2;

pub const LINUX_XATTR_CREATE: i32 = 1;
pub const LINUX_XATTR_REPLACE: i32 = 2;

// ---------- Platform-specific type aliases ----------

#[cfg(target_os = "macos")]
pub type stat64 = libc::stat;
#[cfg(target_os = "linux")]
pub use libc::stat64;

#[cfg(target_os = "macos")]
pub type off64_t = libc::off_t;
#[cfg(target_os = "linux")]
pub use libc::off64_t;

#[cfg(target_os = "macos")]
pub type statvfs64 = libc::statvfs;
#[cfg(target_os = "linux")]
pub use libc::statvfs64;

#[cfg(target_os = "macos")]
pub type ino64_t = libc::ino_t;
#[cfg(target_os = "linux")]
pub use libc::ino64_t;

// ---------- Windows type definitions ----------
// On Windows, we define these types from scratch since there's no libc.
// These must be layout-compatible with what the FUSE protocol expects
// (i.e., Linux x86_64 stat64 layout).

#[cfg(target_os = "windows")]
pub type off64_t = i64;

#[cfg(target_os = "windows")]
pub type ino64_t = u64;

/// Synthetic stat64 for Windows. Fields match Linux x86_64 stat64 layout
/// as expected by the FUSE protocol. The Windows passthrough fills these
/// from `GetFileInformationByHandle` / `GetFileInformationByHandleEx`.
#[cfg(target_os = "windows")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct stat64 {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_nlink: u64,
    pub st_mode: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub __pad0: u32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: i64,
    pub st_mtime: i64,
    pub st_mtime_nsec: i64,
    pub st_ctime: i64,
    pub st_ctime_nsec: i64,
}

/// Synthetic statvfs64 for Windows. Filled from `GetDiskFreeSpaceExW`.
#[cfg(target_os = "windows")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct statvfs64 {
    pub f_bsize: u64,
    pub f_frsize: u64,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_favail: u64,
    pub f_fsid: u64,
    pub f_flag: u64,
    pub f_namemax: u64,
}

// ---------- Unix I/O functions ----------

#[cfg(target_os = "linux")]
pub unsafe fn pread64(
    fd: libc::c_int,
    buf: *mut libc::c_void,
    count: libc::size_t,
    offset: off64_t,
) -> libc::ssize_t {
    libc::pread64(fd, buf, count, offset)
}
#[cfg(target_os = "macos")]
pub unsafe fn pread64(
    fd: libc::c_int,
    buf: *mut libc::c_void,
    count: libc::size_t,
    offset: off64_t,
) -> libc::ssize_t {
    libc::pread(fd, buf, count, offset)
}

#[cfg(target_os = "linux")]
pub unsafe fn preadv64(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    offset: off64_t,
) -> libc::ssize_t {
    libc::preadv64(fd, iov, iovcnt, offset)
}
#[cfg(target_os = "macos")]
pub unsafe fn preadv64(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    offset: off64_t,
) -> libc::ssize_t {
    libc::preadv(fd, iov, iovcnt, offset)
}

#[cfg(target_os = "linux")]
pub unsafe fn pwrite64(
    fd: libc::c_int,
    buf: *const libc::c_void,
    count: libc::size_t,
    offset: off64_t,
) -> libc::ssize_t {
    libc::pwrite64(fd, buf, count, offset)
}
#[cfg(target_os = "macos")]
pub unsafe fn pwrite64(
    fd: libc::c_int,
    buf: *const libc::c_void,
    count: libc::size_t,
    offset: off64_t,
) -> libc::ssize_t {
    libc::pwrite(fd, buf, count, offset)
}

#[cfg(target_os = "linux")]
pub unsafe fn pwritev64(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    offset: off64_t,
) -> libc::ssize_t {
    libc::pwritev64(fd, iov, iovcnt, offset)
}
#[cfg(target_os = "macos")]
pub unsafe fn pwritev64(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    offset: off64_t,
) -> libc::ssize_t {
    libc::pwritev(fd, iov, iovcnt, offset)
}

#[cfg(target_os = "linux")]
pub unsafe fn fstatat64(
    dirfd: libc::c_int,
    pathname: *const libc::c_char,
    buf: *mut stat64,
    flags: libc::c_int,
) -> libc::c_int {
    libc::fstatat64(dirfd, pathname, buf, flags)
}
#[cfg(target_os = "macos")]
pub unsafe fn fstatat64(
    dirfd: libc::c_int,
    pathname: *const libc::c_char,
    buf: *mut stat64,
    flags: libc::c_int,
) -> libc::c_int {
    libc::fstatat(dirfd, pathname, buf, flags)
}

#[cfg(target_os = "linux")]
pub unsafe fn fallocate64(
    fd: libc::c_int,
    mode: libc::c_int,
    offset: off64_t,
    len: off64_t,
) -> libc::c_int {
    libc::fallocate64(fd, mode, offset, len)
}
#[cfg(target_os = "macos")]
pub unsafe fn fallocate64(
    _fd: libc::c_int,
    _mode: libc::c_int,
    _offset: off64_t,
    _len: off64_t,
) -> libc::c_int {
    -LINUX_ENOSYS
}

#[cfg(target_os = "linux")]
pub unsafe fn ftruncate64(fd: libc::c_int, length: off64_t) -> libc::c_int {
    libc::ftruncate64(fd, length)
}
#[cfg(target_os = "macos")]
pub unsafe fn ftruncate64(fd: libc::c_int, length: off64_t) -> libc::c_int {
    libc::ftruncate(fd, length)
}

#[cfg(target_os = "linux")]
pub unsafe fn lseek64(fd: libc::c_int, offset: off64_t, whence: libc::c_int) -> off64_t {
    libc::lseek64(fd, offset, whence)
}
#[cfg(target_os = "macos")]
pub unsafe fn lseek64(fd: libc::c_int, offset: off64_t, whence: libc::c_int) -> off64_t {
    libc::lseek(fd, offset, whence)
}

#[cfg(target_os = "macos")]
pub unsafe fn statvfs64(path: *const libc::c_char, buf: *mut statvfs64) -> libc::c_int {
    libc::statvfs(path, buf)
}

#[cfg(target_os = "linux")]
pub unsafe fn fstatvfs64(fd: libc::c_int, buf: *mut statvfs64) -> libc::c_int {
    libc::fstatvfs64(fd, buf)
}
#[cfg(target_os = "macos")]
pub unsafe fn fstatvfs64(fd: libc::c_int, buf: *mut statvfs64) -> libc::c_int {
    libc::fstatvfs(fd, buf)
}

#[cfg(target_os = "linux")]
pub unsafe fn mknodat(
    dirfd: libc::c_int,
    pathname: *const libc::c_char,
    mode: libc::mode_t,
    dev: libc::dev_t,
) -> libc::c_int {
    libc::mknodat(dirfd, pathname, mode, dev)
}
#[cfg(target_os = "macos")]
pub unsafe fn mknodat(
    _dirfd: libc::c_int,
    _pathname: *const libc::c_char,
    _mode: libc::mode_t,
    _dev: u64,
) -> libc::c_int {
    -LINUX_ENOSYS
}
