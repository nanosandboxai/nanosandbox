//! Minimal AF_VSOCK listener/stream wrapper (Linux guest only).

#![cfg(target_os = "linux")]

use std::io::{self, Read, Write};
use std::os::fd::RawFd;

/// A connected vsock stream.
pub struct VsockStream {
    fd: RawFd,
}

/// A vsock listener bound to a port (guest serves; host connects in).
pub struct VsockListener {
    fd: RawFd,
}

/// CID of the host as seen from the guest.
const VMADDR_CID_HOST: u32 = 2;

fn make_sockaddr(port: u32, cid: u32) -> libc::sockaddr_vm {
    let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    addr.svm_port = port;
    addr.svm_cid = cid;
    addr
}

impl VsockListener {
    /// Bind to `AF_VSOCK` on any CID at `port` and listen for connections.
    pub fn bind(port: u32) -> Result<Self, String> {
        let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(format!("socket(AF_VSOCK): {}", io::Error::last_os_error()));
        }
        let addr = make_sockaddr(port, libc::VMADDR_CID_ANY);
        let ret = unsafe {
            libc::bind(
                fd,
                &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(format!("bind(vsock:{}): {}", port, e));
        }
        if unsafe { libc::listen(fd, 16) } < 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(format!("listen(vsock:{}): {}", port, e));
        }
        Ok(Self { fd })
    }

    /// Block until a host connection arrives.
    pub fn accept(&self) -> Result<VsockStream, String> {
        let fd = unsafe { libc::accept(self.fd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if fd < 0 {
            return Err(format!("accept(vsock): {}", io::Error::last_os_error()));
        }
        Ok(VsockStream { fd })
    }
}

impl Drop for VsockListener {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

impl VsockStream {
    /// Connect to the host at `port` over vsock.
    ///
    /// libkrun's `krun_add_vsock_port` bridges the host's unix socket to this
    /// port and expects the *guest* to dial out, so the agent connects here.
    pub fn connect(port: u32) -> Result<Self, String> {
        let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(format!("socket(AF_VSOCK): {}", io::Error::last_os_error()));
        }
        // Non-blocking connect so a hang (device missing / packet dropped) is
        // reported as an errno instead of blocking forever.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };

        let addr = make_sockaddr(port, VMADDR_CID_HOST);
        let ret = unsafe {
            libc::connect(
                fd,
                &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            let err = io::Error::last_os_error();
            // EINPROGRESS / EAGAIN: connect in flight — poll for completion.
            if err.raw_os_error() == Some(libc::EINPROGRESS) {
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let pr = unsafe { libc::poll(&mut pfd, 1, 3000) };
                if pr <= 0 {
                    unsafe { libc::close(fd) };
                    return Err(format!(
                        "connect(vsock host:{}) timed out (device missing or host not listening)",
                        port
                    ));
                }
                let mut soerr: libc::c_int = 0;
                let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                unsafe {
                    libc::getsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        &mut soerr as *mut _ as *mut libc::c_void,
                        &mut len,
                    );
                }
                if soerr != 0 {
                    unsafe { libc::close(fd) };
                    return Err(format!(
                        "connect(vsock host:{}) failed: {}",
                        port,
                        io::Error::from_raw_os_error(soerr)
                    ));
                }
            } else {
                unsafe { libc::close(fd) };
                return Err(format!("connect(vsock host:{}) failed: {}", port, err));
            }
        }
        // Back to blocking for normal reads/writes.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
        Ok(Self { fd })
    }
}

impl VsockStream {
    /// Duplicate the underlying fd for a second reader/writer.
    pub fn try_clone(&self) -> Result<Self, String> {
        let fd = unsafe { libc::dup(self.fd) };
        if fd < 0 {
            return Err(format!("dup(vsock): {}", io::Error::last_os_error()));
        }
        Ok(Self { fd })
    }

    /// Shut down the write side (used after responding).
    pub fn shutdown_write(&self) {
        unsafe { libc::shutdown(self.fd, libc::SHUT_WR) };
    }
}

impl Read for VsockStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}

impl Write for VsockStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = unsafe { libc::write(self.fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for VsockStream {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
