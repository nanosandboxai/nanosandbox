use std::io;

/// Return a Linux EBADF error for the FUSE protocol.
pub fn ebadf() -> io::Error {
    // Linux EBADF = 9
    io::Error::from_raw_os_error(9)
}

/// Return a Linux EINVAL error for the FUSE protocol.
pub fn einval() -> io::Error {
    // Linux EINVAL = 22
    io::Error::from_raw_os_error(22)
}
