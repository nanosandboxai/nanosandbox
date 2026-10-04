// fuse_mount.rs - Init process that mounts a FUSE filesystem via vsock.
//
// Used as /init.krun in the FUSE-mode initrd. It:
//   1. Connects to the host's FUSE server via AF_VSOCK
//   2. Opens /dev/fuse and mounts a FUSE filesystem at /mnt
//   3. Bridges FUSE kernel requests between /dev/fuse and the vsock socket
//   4. Chroots into /mnt and execs the user command
//
// The host runs a FUSE protocol server (Server<PassthroughFs>) that handles
// all filesystem operations. This binary simply relays FUSE messages between
// the Linux kernel's FUSE driver (/dev/fuse) and the host server (vsock).
//
// Build: CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
//        cargo build -p hcs --bin fuse_mount --target x86_64-unknown-linux-musl --release

use std::ffi::CString;
use std::io;
use std::os::raw::c_char;

const AF_VSOCK: i32 = 40;
const SOCK_STREAM: i32 = 1;
const VMADDR_CID_HOST: u32 = 2;
const SYS_MOUNT: i64 = 165;
const SYS_REBOOT: i64 = 169;
const LINUX_REBOOT_MAGIC1: i64 = 0xfee1dead;
const LINUX_REBOOT_MAGIC2: i64 = 672274793;
const LINUX_REBOOT_CMD_RESTART: i64 = 0x01234567;
const O_RDONLY: i32 = 0;
const O_WRONLY: i32 = 1;
const O_RDWR: i32 = 2;
const O_CREAT: i32 = 0o100;
const O_TRUNC: i32 = 0o1000;

/// FUSE InHeader size in bytes.
const FUSE_IN_HEADER_SIZE: usize = 40;
/// FUSE OutHeader size in bytes.
const FUSE_OUT_HEADER_SIZE: usize = 16;
/// Maximum FUSE message size (same as kernel default: 1 MB + overhead).
const MAX_FUSE_MSG: usize = (1 << 20) + 0x1000;

// FUSE opcodes that have no reply (the kernel does not expect a response).
const FUSE_FORGET: u32 = 2;
const FUSE_BATCH_FORGET: u32 = 42;
const FUSE_INTERRUPT: u32 = 36;
const FUSE_DESTROY: u32 = 38;

#[repr(C)]
struct SockaddrVm {
    svm_family: u16,
    svm_reserved1: u16,
    svm_port: u32,
    svm_cid: u32,
    svm_flags: u8,
    svm_zero: [u8; 3],
}

extern "C" {
    fn socket(domain: i32, socktype: i32, protocol: i32) -> i32;
    fn connect(sockfd: i32, addr: *const u8, addrlen: u32) -> i32;
    fn close(fd: i32) -> i32;
    fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
    fn write(fd: i32, buf: *const u8, count: usize) -> isize;
    fn syscall(num: i64, ...) -> i64;
    fn execv(path: *const c_char, argv: *const *const c_char) -> i32;
    fn chroot(path: *const c_char) -> i32;
    fn chdir(path: *const c_char) -> i32;
    fn open(path: *const c_char, flags: i32, ...) -> i32;
    fn dup2(oldfd: i32, newfd: i32) -> i32;
    fn sleep(seconds: u32) -> u32;
    fn mkdir(path: *const c_char, mode: u32) -> i32;
    fn chmod(path: *const c_char, mode: u32) -> i32;
    fn chown(path: *const c_char, uid: u32, gid: u32) -> i32;
    fn fork() -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn _exit(status: i32) -> !;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
    fn stat(path: *const c_char, buf: *mut StatBuf) -> i32;
    fn getuid() -> u32;
    fn getgid() -> u32;
    fn setresuid(ruid: u32, euid: u32, suid: u32) -> i32;
    fn setresgid(rgid: u32, egid: u32, sgid: u32) -> i32;
    fn setgroups(size: usize, list: *const u32) -> i32;
    fn opendir(path: *const c_char) -> *mut std::ffi::c_void;
    fn closedir(dir: *mut std::ffi::c_void) -> i32;
    fn readlink(path: *const c_char, buf: *mut u8, bufsize: usize) -> isize;
    fn pipe(fds: *mut i32) -> i32;
    fn access(path: *const c_char, mode: i32) -> i32;
    fn lstat(path: *const c_char, buf: *mut StatBuf) -> i32;
}

const R_OK: i32 = 4;
const W_OK: i32 = 2;
const X_OK: i32 = 1;

#[repr(C)]
struct StatBuf {
    st_dev: u64,
    st_ino: u64,
    st_nlink: u64,
    st_mode: u32,
    st_uid: u32,
    st_gid: u32,
    __pad0: u32,
    st_rdev: u64,
    st_size: i64,
    st_blksize: i64,
    st_blocks: i64,
    st_atime: i64,
    st_atime_nsec: i64,
    st_mtime: i64,
    st_mtime_nsec: i64,
    st_ctime: i64,
    st_ctime_nsec: i64,
    __unused: [i64; 3],
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

const POLLIN: i16 = 0x0001;

// ─── Helper functions ────────────────────────────────────────────────────────

fn console_write(msg: &str) {
    let console = CString::new("/dev/console").unwrap();
    let fd = unsafe { open(console.as_ptr(), O_WRONLY) };
    if fd >= 0 {
        unsafe { write(fd, msg.as_ptr(), msg.len()) };
        unsafe { write(fd, b"\n".as_ptr(), 1) };
        unsafe { close(fd) };
    }
}

fn do_mount(source: &str, target: &str, fstype: &str, flags: u64, data: &str) -> bool {
    let src = CString::new(source).unwrap();
    let tgt = CString::new(target).unwrap();
    let fst = CString::new(fstype).unwrap();
    let dat = CString::new(data).unwrap();

    let ret = unsafe {
        syscall(
            SYS_MOUNT,
            src.as_ptr() as i64,
            tgt.as_ptr() as i64,
            fst.as_ptr() as i64,
            flags as i64,
            dat.as_ptr() as i64,
        )
    };

    if ret != 0 {
        let err = io::Error::last_os_error();
        console_write(&format!(
            "fuse_mount: mount {} on {} failed: {} (errno={})",
            source,
            target,
            err,
            err.raw_os_error().unwrap_or(-1)
        ));
        false
    } else {
        true
    }
}

fn vsock_connect(port: u32) -> i32 {
    let fd = unsafe { socket(AF_VSOCK, SOCK_STREAM, 0) };
    if fd < 0 {
        console_write(&format!(
            "fuse_mount: socket(AF_VSOCK) failed: {}",
            io::Error::last_os_error()
        ));
        return -1;
    }

    let addr = SockaddrVm {
        svm_family: AF_VSOCK as u16,
        svm_reserved1: 0,
        svm_port: port,
        svm_cid: VMADDR_CID_HOST,
        svm_flags: 0,
        svm_zero: [0; 3],
    };

    let ret = unsafe {
        connect(
            fd,
            &addr as *const SockaddrVm as *const u8,
            std::mem::size_of::<SockaddrVm>() as u32,
        )
    };

    if ret < 0 {
        console_write(&format!(
            "fuse_mount: connect port {} failed: {}",
            port,
            io::Error::last_os_error()
        ));
        unsafe { close(fd) };
        return -1;
    }

    fd
}

fn reboot() -> ! {
    unsafe {
        syscall(
            SYS_REBOOT,
            LINUX_REBOOT_MAGIC1,
            LINUX_REBOOT_MAGIC2,
            LINUX_REBOOT_CMD_RESTART,
            0i64,
        );
    }
    loop {}
}

fn read_file(path: &str) -> Option<Vec<u8>> {
    let p = CString::new(path).ok()?;
    let fd = unsafe { open(p.as_ptr(), O_RDONLY) };
    if fd < 0 {
        return None;
    }
    let mut result = Vec::new();
    let mut buf = [0u8; 32768];
    loop {
        let n = unsafe { read(fd, buf.as_mut_ptr(), buf.len()) };
        if n <= 0 {
            break;
        }
        result.extend_from_slice(&buf[..n as usize]);
    }
    unsafe { close(fd) };
    if result.is_empty() {
        return None;
    }
    Some(result)
}

fn write_file(path: &str, data: &[u8]) -> bool {
    let p = CString::new(path).unwrap();
    let fd = unsafe { open(p.as_ptr(), O_WRONLY | O_CREAT | O_TRUNC, 0o644u32) };
    if fd < 0 {
        return false;
    }
    let mut offset = 0;
    while offset < data.len() {
        let n = unsafe { write(fd, data[offset..].as_ptr(), data.len() - offset) };
        if n <= 0 {
            break;
        }
        offset += n as usize;
    }
    unsafe { close(fd) };
    offset == data.len()
}

fn mkdir_p(path: &str) {
    let p = CString::new(path).unwrap();
    unsafe { mkdir(p.as_ptr(), 0o755) };
}

fn copy_file(src: &str, dst: &str) -> bool {
    if let Some(data) = read_file(src) {
        write_file(dst, &data)
    } else {
        false
    }
}

fn path_exists(path: &str) -> bool {
    let p = CString::new(path).unwrap();
    unsafe {
        let mut stat_buf = [0u8; 144];
        syscall(4i64, p.as_ptr() as i64, stat_buf.as_mut_ptr() as i64) == 0
    }
}

fn do_symlink(target: &str, linkpath: &str) {
    let tgt = CString::new(target).unwrap();
    let lnk = CString::new(linkpath).unwrap();
    unsafe { syscall(88i64, tgt.as_ptr() as i64, lnk.as_ptr() as i64) };
}

/// Read exactly `count` bytes from fd.
fn read_exact(fd: i32, buf: &mut [u8], count: usize) -> io::Result<()> {
    let mut offset = 0;
    while offset < count {
        let n = unsafe { read(fd, buf[offset..].as_mut_ptr(), count - offset) };
        if n <= 0 {
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read"));
            }
            return Err(io::Error::last_os_error());
        }
        offset += n as usize;
    }
    Ok(())
}

/// Write all bytes to fd.
fn write_all(fd: i32, data: &[u8]) -> io::Result<()> {
    let mut offset = 0;
    while offset < data.len() {
        let n = unsafe { write(fd, data[offset..].as_ptr(), data.len() - offset) };
        if n <= 0 {
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "short write"));
            }
            return Err(io::Error::last_os_error());
        }
        offset += n as usize;
    }
    Ok(())
}

// ─── FUSE bridge functions ───────────────────────────────────────────────────

/// Open /dev/fuse and mount a FUSE filesystem at the given mountpoint.
///
/// Returns the /dev/fuse file descriptor on success.
fn open_fuse_dev_and_mount(mountpoint: &str) -> i32 {
    let fuse_dev = CString::new("/dev/fuse").unwrap();
    let fuse_fd = unsafe { open(fuse_dev.as_ptr(), O_RDWR) };
    if fuse_fd < 0 {
        console_write(&format!(
            "fuse_mount: failed to open /dev/fuse: {}",
            io::Error::last_os_error()
        ));
        return -1;
    }

    // Mount the FUSE filesystem.
    //
    // The WSL2 kernel's fuse_allow_current_process() (fs/fuse/dir.c) has two
    // permission paths:
    //
    //   if (fc->allow_other)
    //       allow = current_in_userns(fc->user_ns);
    //   else
    //       allow = fuse_permissible_uidgid(fc);  // all 6 cred fields == user_id/group_id
    //
    //   if (!allow && allow_sys_admin_access && capable(CAP_SYS_ADMIN))
    //       allow = true;
    //
    // FUSE mount permission model (WSL2 6.x kernel):
    //
    // The kernel's fuse_permission() calls fuse_allow_current_process(fc):
    //   if (fc->allow_other)
    //       return current_in_userns(fc->user_ns);
    //   else
    //       return all-six-cred-fields-match-user_id/group_id;
    //   if (!allow && allow_sys_admin_access && capable(CAP_SYS_ADMIN))
    //       allow = true;
    //
    // EMPIRICAL FINDING (2026-05-17, rev3 DIAG):
    //   Even with all 6 dev cred fields == user_id=1000 (verified via
    //   /proc/self/status), the WSL2 kernel returns -EACCES from
    //   fuse_permission() when allow_other is NOT set. The "no allow_other"
    //   path does not work on this kernel.
    //
    // Therefore we MUST use allow_other. Since the FUSE mount is created in
    // init_user_ns and all guest processes also live in init_user_ns,
    // current_in_userns(init_user_ns) returns true → allow_other path passes.
    //
    // We add default_permissions so the kernel applies standard Unix
    // permission checks (mode/uid/gid from getattr) on top of allow_other.
    // Without default_permissions, the FUSE server has to enforce its own
    // permissions, which our passthrough server does not.
    //
    // Mount options:
    //   fd=N                       - kernel uses this fd for FUSE protocol
    //   rootmode=40777             - S_IFDIR | 0o777 root inode mode
    //   user_id=1000/group_id=1000 - developer user owns the FUSE mount
    //   max_read=1048576           - 1MB max read matching host buffer
    //   default_permissions        - kernel does standard Unix permission
    //                                checks based on st_mode/st_uid/st_gid
    //   allow_other                - lets non-owner users access the mount
    //                                (gated by current_in_userns check)
    // Mount options:
    //   allow_other         - lets non-owner processes access (kernel does
    //                         current_in_userns check; all guest procs are
    //                         in init_user_ns so this passes).
    //   default_permissions - kernel runs generic_permission() using the
    //                         st_mode/st_uid/st_gid returned by the host
    //                         FUSE server's getattr. With server returning
    //                         uid=0/gid=0 mode=0o755, developer (uid 1000)
    //                         passes the "other" rwx check.
    //   user_id=1000        - mount owner credentials (cosmetic — kernel
    //                         uses allow_other path, not the user_id match).
    let mount_data = format!(
        "fd={},rootmode=40777,user_id=1000,group_id=1000,max_read=1048576,default_permissions,allow_other",
        fuse_fd
    );

    // BUILD MARKER: 2026-05-17 rev7 - same options as rev6, but with a
    // comprehensive DIAG (root and uid 1000 tested separately, results
    // streamed back through a pipe).
    console_write(&format!(
        "fuse_mount: BUILD=2026-05-17-rev7 mount_data='{}'",
        mount_data
    ));
    if !do_mount("virtiofs", mountpoint, "fuse.virtiofs", 0, &mount_data) {
        // Fallback: try with plain "fuse" type
        console_write("fuse_mount: fuse.virtiofs failed, trying plain fuse type...");
        if !do_mount("fuse", mountpoint, "fuse", 0, &mount_data) {
            console_write("fuse_mount: FUSE mount failed");
            unsafe { close(fuse_fd) };
            return -1;
        }
    }

    console_write(&format!(
        "fuse_mount: FUSE mounted at {} (fd={})",
        mountpoint, fuse_fd
    ));
    fuse_fd
}

/// Bridge FUSE messages between /dev/fuse and a vsock socket.
///
/// This function runs in a child process (forked from init) and relays
/// FUSE protocol messages between the kernel and the host FUSE server.
///
/// The kernel writes FUSE requests to /dev/fuse; we read them and send
/// them to the host over vsock. The host's response is read from vsock
/// and written back to /dev/fuse.
fn fuse_bridge_loop(bridge_name: &str, fuse_fd: i32, vsock_fd: i32) {
    let mut req_buf = vec![0u8; MAX_FUSE_MSG];
    let mut resp_buf = vec![0u8; MAX_FUSE_MSG];
    console_write(&format!(
        "fuse_mount: bridge[{}]: started (fuse_fd={}, vsock_fd={})",
        bridge_name, fuse_fd, vsock_fd
    ));

    loop {
        // Read a FUSE request from /dev/fuse.
        // The kernel writes a complete FUSE message with each read() call.
        let n = unsafe { read(fuse_fd, req_buf.as_mut_ptr(), req_buf.len()) };
        if n <= 0 {
            if n < 0 {
                let err = io::Error::last_os_error();
                // ENODEV means the filesystem was unmounted.
                if err.raw_os_error() == Some(19) {
                    console_write(&format!(
                        "fuse_mount: bridge[{}]: filesystem unmounted (ENODEV)",
                        bridge_name
                    ));
                } else {
                    console_write(&format!(
                        "fuse_mount: bridge[{}]: read /dev/fuse error: {} (errno={})",
                        bridge_name,
                        err,
                        err.raw_os_error().unwrap_or(-1)
                    ));
                }
            }
            break;
        }
        let req_len = n as usize;

        // Validate the message has at least an InHeader.
        if req_len < FUSE_IN_HEADER_SIZE {
            console_write(&format!(
                "fuse_mount: bridge: short read from /dev/fuse: {} bytes",
                req_len
            ));
            continue;
        }

        // Extract the opcode from the InHeader (offset 4, 4 bytes LE).
        let opcode =
            u32::from_le_bytes([req_buf[4], req_buf[5], req_buf[6], req_buf[7]]);

        // Send the FUSE request to the host server via vsock.
        if let Err(err) = write_all(vsock_fd, &req_buf[..req_len]) {
            console_write(&format!(
                "fuse_mount: bridge[{}]: write to vsock failed: {} (errno={})",
                bridge_name,
                err,
                err.raw_os_error().unwrap_or(-1)
            ));
            break;
        }

        // No-reply operations: the host will not send a response, so skip
        // the response read/write cycle and immediately read the next request.
        if opcode == FUSE_FORGET
            || opcode == FUSE_BATCH_FORGET
            || opcode == FUSE_INTERRUPT
            || opcode == FUSE_DESTROY
        {
            continue;
        }

        // Read the FUSE response from the host.
        // First read the OutHeader (16 bytes) to learn the response length.
        if let Err(err) = read_exact(vsock_fd, &mut resp_buf, FUSE_OUT_HEADER_SIZE) {
            console_write(&format!(
                "fuse_mount: bridge[{}]: read resp header failed: {}",
                bridge_name, err
            ));
            break;
        }

        let resp_len = u32::from_le_bytes([resp_buf[0], resp_buf[1], resp_buf[2], resp_buf[3]])
            as usize;

        if resp_len < FUSE_OUT_HEADER_SIZE || resp_len > MAX_FUSE_MSG {
            console_write(&format!(
                "fuse_mount: bridge[{}]: invalid response length: {}",
                bridge_name, resp_len
            ));
            break;
        }

        // Read the remaining response body.
        let remaining = resp_len - FUSE_OUT_HEADER_SIZE;
        if remaining > 0 {
            if let Err(err) = read_exact(
                vsock_fd,
                &mut resp_buf[FUSE_OUT_HEADER_SIZE..],
                remaining,
            ) {
                console_write(&format!(
                    "fuse_mount: bridge[{}]: read resp body failed: {}",
                    bridge_name, err
                ));
                break;
            }
        }

        // Write the response to /dev/fuse.
        // The kernel expects a complete FUSE response with each write() call.
        if let Err(err) = write_all(fuse_fd, &resp_buf[..resp_len]) {
            console_write(&format!(
                "fuse_mount: bridge[{}]: write to /dev/fuse failed: {} (errno={})",
                bridge_name,
                err,
                err.raw_os_error().unwrap_or(-1)
            ));
            break;
        }
    }

    console_write(&format!("fuse_mount: bridge[{}]: loop exited", bridge_name));
}

// ─── Workaround functions (Windows/NTFS quirks) ─────────────────────────────
// These fix issues with OCI images extracted on Windows (NTFS limitations).
// With virtio-fs + proper FUSE passthrough, symlinks and permissions should be
// preserved correctly, making most of these unnecessary. They are kept as a
// safety net during the transition period.

fn inject_ssh_key_from_cmdline() {
    let cmdline = match read_file("/proc/cmdline") {
        Some(data) => String::from_utf8_lossy(&data).to_string(),
        None => return,
    };

    let mut ssh_key_encoded = None;
    for word in cmdline.split_whitespace() {
        if let Some(val) = word.strip_prefix("nanosb.ssh_key=") {
            ssh_key_encoded = Some(val.to_string());
        }
    }

    let encoded = match ssh_key_encoded {
        Some(k) if !k.is_empty() => k,
        _ => return,
    };

    let ssh_key = encoded.replace(',', " ");
    let key_data = format!("{}\n", ssh_key);

    for dir in &["/mnt/root/.ssh", "/mnt/home/developer/.ssh"] {
        mkdir_p(dir);
        let ak = format!("{}/authorized_keys", dir);
        write_file(&ak, key_data.as_bytes());
        let d = CString::new(*dir).unwrap();
        let f = CString::new(ak.as_str()).unwrap();
        unsafe {
            chmod(d.as_ptr(), 0o700);
            chmod(f.as_ptr(), 0o600);
        }
    }

    let root_ssh = CString::new("/mnt/root/.ssh").unwrap();
    let root_ak = CString::new("/mnt/root/.ssh/authorized_keys").unwrap();
    unsafe {
        chown(root_ssh.as_ptr(), 0, 0);
        chown(root_ak.as_ptr(), 0, 0);
    }
    let dev_ssh = CString::new("/mnt/home/developer/.ssh").unwrap();
    let dev_ak = CString::new("/mnt/home/developer/.ssh/authorized_keys").unwrap();
    unsafe {
        chown(dev_ssh.as_ptr(), 1000, 1000);
        chown(dev_ak.as_ptr(), 1000, 1000);
    }

    console_write("fuse_mount: SSH key injected from kernel cmdline");
}

fn fix_usr_merge_symlinks() {
    for dir in &["bin", "sbin", "lib", "lib64"] {
        let mnt_dir = format!("/mnt/{}", dir);
        let usr_dir = format!("/mnt/usr/{}", dir);
        if !path_exists(&mnt_dir) && path_exists(&usr_dir) {
            let target = format!("usr/{}", dir);
            console_write(&format!(
                "fuse_mount: creating /{} -> {} symlink",
                dir, target
            ));
            do_symlink(&target, &mnt_dir);
        }
    }
    if !path_exists("/mnt/etc") {
        mkdir_p("/mnt/etc");
    }
}

fn fix_lib64() {
    let ld_linux_dst = "/mnt/lib64/ld-linux-x86-64.so.2";
    let candidates = [
        "/mnt/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
        "/mnt/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
        "/mnt/usr/lib64/ld-linux-x86-64.so.2",
        "/mnt/lib64/ld-linux-x86-64.so.2",
    ];
    for src in &candidates {
        if path_exists(src) {
            mkdir_p("/mnt/lib64");
            if *src == ld_linux_dst {
                console_write(&format!("fuse_mount: ld-linux present at {}", src));
                return;
            }
            if copy_file(src, ld_linux_dst) {
                let p = CString::new(ld_linux_dst).unwrap();
                unsafe { chmod(p.as_ptr(), 0o755) };
                console_write(&format!(
                    "fuse_mount: fixed /lib64/ld-linux-x86-64.so.2 from {}",
                    src
                ));
            }
            return;
        }
    }
}

fn ensure_etc_hosts() {
    if !path_exists("/mnt/etc/hosts") {
        mkdir_p("/mnt/etc");
        write_file(
            "/mnt/etc/hosts",
            b"127.0.0.1\tlocalhost\n::1\t\tlocalhost ip6-localhost\n",
        );
    }
}

fn cmdline_has(flag: &str) -> bool {
    if let Some(data) = read_file("/proc/cmdline") {
        let s = String::from_utf8_lossy(&data);
        for word in s.split_whitespace() {
            if word == flag {
                return true;
            }
        }
    }
    false
}


fn exec_busybox(argv: &[&str]) -> bool {
    let pid = unsafe { fork() };
    if pid < 0 {
        console_write(&format!(
            "fuse_mount: fork failed: {}",
            io::Error::last_os_error()
        ));
        return false;
    }
    if pid == 0 {
        let bb = CString::new("/bin/busybox").unwrap();
        let cstrs: Vec<CString> = argv.iter().map(|s| CString::new(*s).unwrap()).collect();
        let mut ptrs: Vec<*const c_char> = cstrs.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        unsafe { execv(bb.as_ptr(), ptrs.as_ptr()) };
        unsafe { _exit(127) };
    }
    let mut status: i32 = 0;
    let r = unsafe { waitpid(pid, &mut status as *mut i32, 0) };
    if r < 0 {
        return false;
    }
    let exited = (status & 0x7f) == 0;
    let code = (status >> 8) & 0xff;
    exited && code == 0
}

/// Extract OCI layers in-guest by streaming tar data from the host over vsock.
///
/// Protocol: guest sends manifest (digests, newline-separated, double-newline
/// terminated). Host replies with `[8-byte LE size][raw tar data]` for each layer.
/// Guest pipes each chunk directly to `tar -x -C /mnt`.
fn extract_layers_in_guest(blobs_vsock_fd: i32) -> bool {
    // Mount tmpfs at /mnt — the in-guest rootfs target.
    mkdir_p("/mnt");
    if !do_mount("tmpfs", "/mnt", "tmpfs", 0, "size=4G,mode=755") {
        console_write("fuse_mount: tmpfs at /mnt failed");
        return false;
    }

    // Read manifest from rootfs FUSE share.
    let manifest_data = match read_file("/fuse-rootfs/.nanosb-layers") {
        Some(d) => d,
        None => {
            console_write("fuse_mount: missing /fuse-rootfs/.nanosb-layers manifest");
            return false;
        }
    };
    let manifest = String::from_utf8_lossy(&manifest_data).into_owned();

    let digests: Vec<&str> = manifest
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    console_write(&format!("fuse_mount: extracting {} layers (streaming)...", digests.len()));

    // Send manifest to host so it knows which tars to stream.
    let mut manifest_msg = String::new();
    for d in &digests {
        manifest_msg.push_str(d);
        manifest_msg.push('\n');
    }
    manifest_msg.push('\n'); // double-newline terminator
    if write_all(blobs_vsock_fd, manifest_msg.as_bytes()).is_err() {
        console_write("fuse_mount: failed to send manifest to host");
        return false;
    }

    // Receive and extract each layer.
    for (i, digest) in digests.iter().enumerate() {
        // Read 8-byte LE file size from host.
        let mut size_buf = [0u8; 8];
        if read_exact(blobs_vsock_fd, &mut size_buf, 8).is_err() {
            console_write(&format!("fuse_mount: failed to read size for layer {}", i + 1));
            return false;
        }
        let file_size = u64::from_le_bytes(size_buf);
        if file_size == 0 {
            console_write(&format!("fuse_mount: host reported error for layer {}", i + 1));
            return false;
        }

        let short = &digest[..12.min(digest.len())];

        // Write the streamed tar data to a temp file, then extract.
        // (busybox tar cannot read from a pipe with seek, so we buffer to a file)
        let tmp_tar = "/tmp/layer.tar";
        {
            let p = CString::new(tmp_tar).unwrap();
            let fd = unsafe { open(p.as_ptr(), O_WRONLY | O_CREAT | O_TRUNC, 0o644u32) };
            if fd < 0 {
                console_write("fuse_mount: failed to open temp tar file");
                return false;
            }

            let mut remaining = file_size;
            let mut buf = [0u8; 256 * 1024]; // 256 KB buffer
            while remaining > 0 {
                let chunk = (remaining as usize).min(buf.len());
                if read_exact(blobs_vsock_fd, &mut buf, chunk).is_err() {
                    console_write(&format!(
                        "fuse_mount: failed to read tar data for layer {} ({} bytes remaining)",
                        i + 1, remaining
                    ));
                    unsafe { close(fd) };
                    return false;
                }
                if write_all(fd, &buf[..chunk]).is_err() {
                    console_write("fuse_mount: failed to write temp tar");
                    unsafe { close(fd) };
                    return false;
                }
                remaining -= chunk as u64;
            }
            unsafe { close(fd) };
        }

        // Extract the temp tar.
        if !exec_busybox(&["tar", "-xf", tmp_tar, "-C", "/mnt"]) {
            console_write(&format!("fuse_mount: tar extract failed for {}", short));
            return false;
        }

        if i < 3 || i == digests.len() - 1 {
            console_write(&format!(
                "fuse_mount: layer {}/{} extracted ({} bytes) {}",
                i + 1,
                digests.len(),
                file_size,
                short
            ));
        }
    }

    // Whiteout pass.
    let whiteout_script = "set -e; find /mnt -name '.wh.*' 2>/dev/null | while read wh; do \
        d=$(dirname \"$wh\"); n=$(basename \"$wh\"); \
        case \"$n\" in \
            .wh..wh..opq) rm -f \"$wh\" ;; \
            *) t=${n#.wh.}; rm -rf \"$d/$t\"; rm -f \"$wh\" ;; \
        esac; \
    done";
    if !exec_busybox(&["sh", "-c", whiteout_script]) {
        console_write("fuse_mount: whiteout pass failed (continuing)");
    }

    // Copy overlay files from the rootfs share.
    if path_exists("/fuse-rootfs/etc/nanosb-mounts") {
        mkdir_p("/mnt/etc");
        let ok = copy_file("/fuse-rootfs/etc/nanosb-mounts", "/mnt/etc/nanosb-mounts");
        console_write(&format!("fuse_mount: copy nanosb-mounts: {}", ok));
    }
    if path_exists("/fuse-rootfs/etc/nanosb-fuse-mounts") {
        mkdir_p("/mnt/etc");
        let ok = copy_file(
            "/fuse-rootfs/etc/nanosb-fuse-mounts",
            "/mnt/etc/nanosb-fuse-mounts",
        );
        console_write(&format!("fuse_mount: copy nanosb-fuse-mounts: {}", ok));
    } else {
        console_write("fuse_mount: /fuse-rootfs/etc/nanosb-fuse-mounts NOT FOUND");
    }

    // Verify the copied config is readable.
    if let Some(data) = read_file("/mnt/etc/nanosb-fuse-mounts") {
        let s = String::from_utf8_lossy(&data);
        console_write(&format!("fuse_mount: /mnt/etc/nanosb-fuse-mounts: {:?}", s.trim()));
    } else {
        console_write("fuse_mount: /mnt/etc/nanosb-fuse-mounts NOT readable after copy");
    }

    console_write("fuse_mount: in-guest extraction complete");
    true
}

fn parse_workspace_mount_line(line: &str) -> Option<(u32, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let (port_s, container_path) = trimmed.split_once(':')?;
    let port = port_s.trim().parse::<u32>().ok()?;
    let container_path = container_path.trim();
    if !container_path.starts_with('/') {
        return None;
    }

    Some((port, container_path.to_string()))
}

/// Run an opendir/lstat/access battery on `mountpoint`, return errno or 0.
fn diag_run_battery(label: &str, mountpoint: &str, pipe_wr: i32) {
    let path = CString::new(mountpoint).unwrap();

    // Test 1: opendir
    let d = unsafe { opendir(path.as_ptr()) };
    let opendir_err = if d.is_null() {
        io::Error::last_os_error().raw_os_error().unwrap_or(99)
    } else {
        unsafe { closedir(d) };
        0
    };

    // Test 2: lstat
    let mut sb: StatBuf = unsafe { std::mem::zeroed() };
    let lstat_ret = unsafe { lstat(path.as_ptr(), &mut sb as *mut StatBuf) };
    let lstat_err = if lstat_ret < 0 {
        io::Error::last_os_error().raw_os_error().unwrap_or(99)
    } else {
        0
    };
    let lstat_mode = if lstat_err == 0 { sb.st_mode } else { 0 };
    let lstat_uid = if lstat_err == 0 { sb.st_uid } else { 0 };
    let lstat_gid = if lstat_err == 0 { sb.st_gid } else { 0 };

    // Test 3: access(R_OK)
    let access_r = unsafe { access(path.as_ptr(), R_OK) };
    let access_r_err = if access_r < 0 {
        io::Error::last_os_error().raw_os_error().unwrap_or(99)
    } else {
        0
    };

    // Test 4: access(R_OK | X_OK)
    let access_rx = unsafe { access(path.as_ptr(), R_OK | X_OK) };
    let access_rx_err = if access_rx < 0 {
        io::Error::last_os_error().raw_os_error().unwrap_or(99)
    } else {
        0
    };

    // Format as one line and send through pipe.
    let line = format!(
        "DIAG-{}: uid={} gid={} opendir_errno={} lstat_errno={} lstat_mode={:o} lstat_uid={} lstat_gid={} access_R_errno={} access_RX_errno={}\n",
        label,
        unsafe { getuid() },
        unsafe { getgid() },
        opendir_err,
        lstat_err,
        lstat_mode,
        lstat_uid,
        lstat_gid,
        access_r_err,
        access_rx_err,
    );
    unsafe { write(pipe_wr, line.as_ptr(), line.len()) };
}

/// Comprehensive FUSE access diagnostic.
///
/// Runs an opendir/lstat/access battery first as ROOT (the current fuse_mount
/// process), then forks a child, drops it to uid/gid 1000, and runs the same
/// battery. Child reports results via a pipe so the parent can log them as
/// root (console writes fail post-setresuid).
fn diag_test_developer_opendir(mountpoint: &str) {
    let mut buf = [0u8; 64];
    let nspath = CString::new("/proc/self/ns/user").unwrap();
    let n = unsafe { readlink(nspath.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    let ns_str = if n > 0 {
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    } else {
        String::from("<unknown>")
    };
    console_write(&format!(
        "fuse_mount: DIAG starting comprehensive test for {} (parent_ns={})",
        mountpoint, ns_str
    ));

    // Create pipe before fork.
    let mut fds: [i32; 2] = [-1, -1];
    let pipe_ret = unsafe { pipe(fds.as_mut_ptr()) };
    if pipe_ret < 0 {
        console_write(&format!(
            "fuse_mount: DIAG pipe() failed: {}",
            io::Error::last_os_error()
        ));
        return;
    }
    let pipe_rd = fds[0];
    let pipe_wr = fds[1];

    // Run battery as ROOT first (parent context).
    diag_run_battery("root", mountpoint, pipe_wr);

    let pid = unsafe { fork() };
    if pid < 0 {
        console_write(&format!(
            "fuse_mount: DIAG fork failed: {}",
            io::Error::last_os_error()
        ));
        unsafe {
            close(pipe_rd);
            close(pipe_wr);
        }
        return;
    }
    if pid == 0 {
        // Child: drop to uid 1000.
        unsafe { close(pipe_rd) };
        let empty_groups: [u32; 0] = [];
        let sg = unsafe { setgroups(0, empty_groups.as_ptr()) };
        let sr_gid = unsafe { setresgid(1000, 1000, 1000) };
        let sr_uid = unsafe { setresuid(1000, 1000, 1000) };
        // Send a cred line so parent can verify the drop happened.
        let cred_line = format!(
            "DIAG-cred: setgroups={} setresgid={} setresuid={} uid={} gid={}\n",
            sg,
            sr_gid,
            sr_uid,
            unsafe { getuid() },
            unsafe { getgid() }
        );
        unsafe { write(pipe_wr, cred_line.as_ptr(), cred_line.len()) };

        // Run battery as uid 1000.
        diag_run_battery("uid1000", mountpoint, pipe_wr);
        unsafe { close(pipe_wr) };
        unsafe { _exit(0) };
    }

    // Parent: close write end, read all child output, then waitpid.
    unsafe { close(pipe_wr) };
    let mut buf = [0u8; 4096];
    let mut total: usize = 0;
    loop {
        let n = unsafe {
            read(
                pipe_rd,
                buf.as_mut_ptr().add(total),
                buf.len() - total,
            )
        };
        if n <= 0 {
            break;
        }
        total += n as usize;
        if total >= buf.len() {
            break;
        }
    }
    unsafe { close(pipe_rd) };
    let mut status: i32 = 0;
    unsafe { waitpid(pid, &mut status as *mut i32, 0) };

    // Log each non-empty line.
    let text = String::from_utf8_lossy(&buf[..total]);
    for line in text.lines() {
        if !line.is_empty() {
            console_write(&format!("fuse_mount: {}", line));
        }
    }
    console_write(&format!(
        "fuse_mount: DIAG complete (child exit_code={})",
        (status >> 8) & 0xff
    ));
}

/// Mount workspace FUSE shares from /mnt/etc/nanosb-fuse-mounts.
///
/// Config format:
///   <port>:<container_path>
/// Example:
///   50010:/workspace
///   50011:/data
fn mount_workspace_fuse_shares(config_path: &str) -> usize {
    let raw = match read_file(config_path) {
        Some(data) => data,
        None => {
            console_write(&format!("fuse_mount: workspace config not found: {}", config_path));
            return 0;
        }
    };
    console_write(&format!(
        "fuse_mount: workspace config loaded ({} bytes)",
        raw.len()
    ));

    let content = String::from_utf8_lossy(&raw).into_owned();
    let mut mounted = 0usize;

    for line in content.lines() {
        let Some((port, container_path)) = parse_workspace_mount_line(line) else {
            continue;
        };

        if container_path == "/" {
            continue;
        }

        let guest_mountpoint = format!("/mnt{}", container_path);
        let _ = exec_busybox(&["mkdir", "-p", &guest_mountpoint]);

        let ws_vsock_fd = vsock_connect(port);
        if ws_vsock_fd < 0 {
            console_write(&format!(
                "fuse_mount: workspace connect failed (port={}, mount={})",
                port, guest_mountpoint
            ));
            continue;
        }

        let ws_fuse_fd = open_fuse_dev_and_mount(&guest_mountpoint);
        if ws_fuse_fd < 0 {
            unsafe { close(ws_vsock_fd) };
            continue;
        }

        let bridge_pid = unsafe { fork() };
        if bridge_pid < 0 {
            console_write(&format!(
                "fuse_mount: workspace bridge fork failed (port={}, mount={})",
                port, guest_mountpoint
            ));
            unsafe {
                close(ws_fuse_fd);
                close(ws_vsock_fd);
            }
            continue;
        }

        if bridge_pid == 0 {
            let bridge_name = format!("workspace-{}", port);
            fuse_bridge_loop(&bridge_name, ws_fuse_fd, ws_vsock_fd);
            unsafe { _exit(0) };
        }

        console_write(&format!(
            "fuse_mount: workspace mounted {} via port {}",
            guest_mountpoint, port
        ));

        // Diagnostic: also log our own user_ns inode for sanity.
        {
            let mut buf = [0u8; 64];
            let nspath = CString::new("/proc/self/ns/user").unwrap();
            let n = unsafe { readlink(nspath.as_ptr(), buf.as_mut_ptr(), buf.len()) };
            if n > 0 {
                let s = String::from_utf8_lossy(&buf[..n as usize]);
                console_write(&format!(
                    "fuse_mount: DIAG mounter: /proc/self/ns/user = {} uid={} gid={}",
                    s, unsafe { getuid() }, unsafe { getgid() }
                ));
            }
        }

        // Give the bridge a moment to handle FUSE_INIT and root LOOKUP.
        unsafe { sleep(1) };

        // Run the developer-uid test in a child process.
        diag_test_developer_opendir(&guest_mountpoint);

        mounted += 1;
    }

    mounted
}

// ─── Main ────────────────────────────────────────────────────────────────────

fn main() {
    // Parse args: fuse_mount <port> <user_command>
    let args: Vec<String> = std::env::args().collect();
    let port: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(50000);
    let user_cmd = args.get(2).map(|s| s.as_str()).unwrap_or("echo 'no command'");

    // Mount basic filesystems.
    do_mount("devtmpfs", "/dev", "devtmpfs", 0, "");
    do_mount("proc", "/proc", "proc", 0, "");
    do_mount("sysfs", "/sys", "sysfs", 0, "");

    // Enable the WSL2 kernel's documented CAP_SYS_ADMIN bypass for FUSE access
    // control. The kernel's fuse_permission() returns -EACCES immediately if
    // fuse_allow_current_process() fails, with no fallback to default_permissions
    // or generic_permission. The `allow_sys_admin_access` module parameter lets
    // processes with CAP_SYS_ADMIN in the initial user namespace bypass this
    // check — necessary so root (init script, agent-gateway PID 1) can access
    // FUSE mounts where the developer (uid 1000) is the declared mount owner.
    //
    // See: linux-msft-wsl-6.6.y/fs/fuse/dir.c:fuse_allow_current_process
    if !write_file("/sys/module/fuse/parameters/allow_sys_admin_access", b"1") {
        console_write(
            "fuse_mount: NOTE: could not enable allow_sys_admin_access \
             (may not exist on this kernel; FUSE access for root may fail)",
        );
    } else {
        console_write("fuse_mount: enabled fuse.allow_sys_admin_access=1");
    }

    // Small delay to let console pipe connect.
    unsafe { sleep(1) };

    let extract_mode = cmdline_has("nanosb.extract_layers=1");
    let fuse_mountpoint = if extract_mode { "/fuse-rootfs" } else { "/mnt" };
    if extract_mode {
        mkdir_p(fuse_mountpoint);
        console_write("fuse_mount: in-guest extraction mode active");
    } else {
        mkdir_p("/mnt");
    }

    console_write(&format!(
        "fuse_mount: connecting to host FUSE server (vsock port {})...",
        port
    ));

    // Connect to the host's FUSE server via AF_VSOCK.
    let vsock_fd = vsock_connect(port);
    if vsock_fd < 0 {
        console_write("fuse_mount: failed to connect to FUSE server, running without rootfs");
    } else {
        // Open /dev/fuse and mount the FUSE filesystem.
        let fuse_fd = open_fuse_dev_and_mount(fuse_mountpoint);
        if fuse_fd < 0 {
            console_write("fuse_mount: FUSE mount failed, running without rootfs");
            unsafe { close(vsock_fd) };
        } else {
            // Fork a child to run the FUSE bridge (relay messages between
            // /dev/fuse and the vsock to the host). The parent continues
            // with the init process.
            let bridge_pid = unsafe { fork() };
            if bridge_pid < 0 {
                console_write("fuse_mount: fork for bridge failed");
                unsafe {
                    close(fuse_fd);
                    close(vsock_fd);
                }
            } else if bridge_pid == 0 {
                // Child: run the FUSE bridge loop.
                fuse_bridge_loop("rootfs", fuse_fd, vsock_fd);
                unsafe { _exit(0) };
            } else {
                // Parent: the bridge is running in the background.
                // Give it a moment to process the FUSE_INIT handshake.
                unsafe { sleep(1) };
                console_write("fuse_mount: FUSE bridge started");

                if extract_mode {
                    // Connect vsock for blob streaming (no FUSE needed).
                    let blobs_vsock_fd = vsock_connect(50002);
                    if blobs_vsock_fd < 0 {
                        console_write("fuse_mount: failed to connect blobs vsock");
                    } else if !extract_layers_in_guest(blobs_vsock_fd) {
                        console_write(
                            "fuse_mount: in-guest extraction failed, running without rootfs",
                        );
                    }
                }
            }
        }
    }

    // Check if rootfs was mounted.
    let has_rootfs = path_exists("/mnt/usr");

    // Set up stdio on /dev/console.
    let console = CString::new("/dev/console").unwrap();
    let console_fd = unsafe { open(console.as_ptr(), O_RDWR) };
    if console_fd >= 0 {
        unsafe {
            dup2(console_fd, 0);
            dup2(console_fd, 1);
            dup2(console_fd, 2);
            if console_fd > 2 {
                close(console_fd);
            }
        }
    }

    if has_rootfs {
        // Mount workspace shares from host (if configured).
        let workspace_mounts = mount_workspace_fuse_shares("/mnt/etc/nanosb-fuse-mounts");
        if workspace_mounts > 0 {
            console_write(&format!(
                "fuse_mount: mounted {} workspace share(s)",
                workspace_mounts
            ));
        }

        // Mount essential filesystems inside /mnt before chroot.
        mkdir_p("/mnt/proc");
        do_mount("proc", "/mnt/proc", "proc", 0, "");
        mkdir_p("/mnt/sys");
        do_mount("sysfs", "/mnt/sys", "sysfs", 0, "");
        mkdir_p("/mnt/dev");
        do_mount("devtmpfs", "/mnt/dev", "devtmpfs", 0, "");
        mkdir_p("/mnt/tmp");
        do_mount("tmpfs", "/mnt/tmp", "tmpfs", 0, "");
        mkdir_p("/mnt/dev/pts");
        do_mount("devpts", "/mnt/dev/pts", "devpts", 0, "");
        mkdir_p("/mnt/run");
        do_mount("tmpfs", "/mnt/run", "tmpfs", 0, "");

        // Mount tmpfs over /etc/ssh so sshd host keys get proper permissions.
        // With FUSE passthrough, permissions are translated correctly from Windows,
        // but sshd still requires strict 0600 on host keys which NTFS can't guarantee.
        mkdir_p("/mnt/etc/ssh");
        do_mount("tmpfs", "/mnt/etc/ssh", "tmpfs", 0, "");
        write_file(
            "/mnt/etc/ssh/sshd_config",
            b"Port 22\nPermitRootLogin yes\nPubkeyAuthentication yes\nPasswordAuthentication no\nStrictModes no\nSubsystem sftp /usr/lib/openssh/sftp-server\n",
        );

        copy_file("/etc/resolv.conf", "/mnt/etc/resolv.conf");

        mkdir_p("/mnt/root/.ssh");
        do_mount("tmpfs", "/mnt/root/.ssh", "tmpfs", 0, "");

        inject_ssh_key_from_cmdline();
        fix_usr_merge_symlinks();
        fix_lib64();

        // Inject busybox as fallback shell.
        mkdir_p("/mnt/bin");
        copy_file("/bin/busybox", "/mnt/bin/.krun-busybox");
        let bb = CString::new("/mnt/bin/.krun-busybox").unwrap();
        unsafe { chmod(bb.as_ptr(), 0o755) };

        ensure_etc_hosts();

        console_write("fuse_mount: chrooting into /mnt");
        let mnt = CString::new("/mnt").unwrap();
        unsafe { chroot(mnt.as_ptr()) };
        let root = CString::new("/").unwrap();
        unsafe { chdir(root.as_ptr()) };
    }

    // Exec user command.
    let bb_path = if has_rootfs {
        "/bin/.krun-busybox"
    } else {
        "/bin/busybox"
    };
    console_write(&format!("fuse_mount: exec {} sh -c '{}'", bb_path, user_cmd));

    let shell = CString::new(bb_path).unwrap();
    let sh = CString::new("sh").unwrap();
    let c_flag = CString::new("-c").unwrap();
    let cmd = CString::new(user_cmd.to_string()).unwrap();

    let argv: Vec<*const c_char> = vec![
        sh.as_ptr(),
        c_flag.as_ptr(),
        cmd.as_ptr(),
        std::ptr::null(),
    ];

    unsafe { execv(shell.as_ptr(), argv.as_ptr()) };
    console_write(&format!(
        "fuse_mount: execv failed: {}",
        io::Error::last_os_error()
    ));
    reboot();
}
