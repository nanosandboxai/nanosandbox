// plan9_mount.rs - Init process that mounts HCS Plan9 share as rootfs then execs user command.
//
// Used as /init.krun in the Plan9-mode initrd. It:
//   1. Connects to the host's HCS Plan9 service via AF_VSOCK
//   2. Mounts the 9p share at /mnt using trans=fd
//   3. Chroots into /mnt and execs the user command
//
// Build: CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
//        cargo build -p hcs --bin plan9_mount --target x86_64-unknown-linux-musl --release

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
}

fn console_write(msg: &str) {
    let console = CString::new("/dev/console").unwrap();
    let fd = unsafe { open(console.as_ptr(), 1) }; // O_WRONLY=1
    if fd >= 0 {
        unsafe { write(fd, msg.as_ptr(), msg.len()) };
        unsafe { write(fd, b"\n".as_ptr(), 1) };
        unsafe { close(fd) };
    }
}

fn do_mount(source: &str, target: &str, fstype: &str, data: &str) -> bool {
    let src = CString::new(source).unwrap();
    let tgt = CString::new(target).unwrap();
    let fst = CString::new(fstype).unwrap();
    let dat = CString::new(data).unwrap();

    let ret = unsafe {
        syscall(SYS_MOUNT,
            src.as_ptr() as i64, tgt.as_ptr() as i64,
            fst.as_ptr() as i64, 0i64, dat.as_ptr() as i64)
    };

    if ret != 0 {
        let err = io::Error::last_os_error();
        console_write(&format!("plan9_mount: mount {} on {} failed: {} (errno={})",
            source, target, err, err.raw_os_error().unwrap_or(-1)));
        false
    } else {
        true
    }
}

fn vsock_connect(port: u32) -> i32 {
    let fd = unsafe { socket(AF_VSOCK, SOCK_STREAM, 0) };
    if fd < 0 {
        console_write(&format!("plan9_mount: socket(AF_VSOCK) failed: {}",
            io::Error::last_os_error()));
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
        connect(fd, &addr as *const SockaddrVm as *const u8,
                std::mem::size_of::<SockaddrVm>() as u32)
    };

    if ret < 0 {
        console_write(&format!("plan9_mount: connect port {} failed: {}",
            port, io::Error::last_os_error()));
        unsafe { close(fd) };
        return -1;
    }

    fd
}

fn reboot() -> ! {
    unsafe {
        syscall(SYS_REBOOT, LINUX_REBOOT_MAGIC1, LINUX_REBOOT_MAGIC2,
                LINUX_REBOOT_CMD_RESTART, 0i64);
    }
    loop {}
}

fn read_file(path: &str) -> Option<Vec<u8>> {
    let p = CString::new(path).ok()?;
    let fd = unsafe { open(p.as_ptr(), 0) }; // O_RDONLY
    if fd < 0 { return None; }
    let mut result = Vec::new();
    let mut buf = [0u8; 32768];
    loop {
        let n = unsafe { read(fd, buf.as_mut_ptr(), buf.len()) };
        if n <= 0 { break; }
        result.extend_from_slice(&buf[..n as usize]);
    }
    unsafe { close(fd) };
    if result.is_empty() { return None; }
    Some(result)
}

fn write_file(path: &str, data: &[u8]) -> bool {
    let p = CString::new(path).unwrap();
    // O_WRONLY | O_CREAT | O_TRUNC = 1 | 0o100 | 0o1000 = 0o1101
    let fd = unsafe { open(p.as_ptr(), 0o1101, 0o644u32) };
    if fd < 0 { return false; }
    let mut offset = 0;
    while offset < data.len() {
        let n = unsafe { write(fd, data[offset..].as_ptr(), data.len() - offset) };
        if n <= 0 { break; }
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

fn inject_ssh_key_from_cmdline() {
    let cmdline = match read_file("/proc/cmdline") {
        Some(data) => String::from_utf8_lossy(&data).to_string(),
        None => return,
    };

    // Find nanosb.ssh_key=... param
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

    // Commas back to spaces (encoding from builder.rs)
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

    // Fix ownership
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

    console_write("plan9_mount: SSH key injected from kernel cmdline");
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
    // SYS_symlink = 88
    unsafe { syscall(88i64, tgt.as_ptr() as i64, lnk.as_ptr() as i64) };
}

fn fix_usr_merge_symlinks() {
    // Docker images extracted on Windows lose symlinks like /bin -> usr/bin.
    // Re-create them if /mnt/usr/$dir exists but /mnt/$dir doesn't.
    for dir in &["bin", "sbin", "lib", "lib64"] {
        let mnt_dir = format!("/mnt/{}", dir);
        let usr_dir = format!("/mnt/usr/{}", dir);
        if !path_exists(&mnt_dir) && path_exists(&usr_dir) {
            let target = format!("usr/{}", dir);
            console_write(&format!("plan9_mount: creating /{} -> {} symlink", dir, target));
            do_symlink(&target, &mnt_dir);
        }
    }
    // Ensure /mnt/etc exists
    if !path_exists("/mnt/etc") {
        mkdir_p("/mnt/etc");
    }
}

fn fix_lib64() {
    // On Windows extraction, /lib64 becomes an empty directory instead of a
    // symlink to /lib/x86_64-linux-gnu. Try multiple known locations for ld-linux
    // (varies by base image: debian uses /usr/lib/x86_64-linux-gnu, some images
    // already ship a real /lib64/ld-linux, alpine uses /lib/ld-musl-* — skipped).
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
                // Already at the destination — nothing to do.
                console_write(&format!("plan9_mount: ld-linux present at {}", src));
                return;
            }
            if copy_file(src, ld_linux_dst) {
                let p = CString::new(ld_linux_dst).unwrap();
                unsafe { chmod(p.as_ptr(), 0o755) };
                console_write(&format!("plan9_mount: fixed /lib64/ld-linux-x86-64.so.2 from {}", src));
            } else {
                console_write(&format!("plan9_mount: WARNING: failed to copy ld-linux from {}", src));
            }
            return;
        }
    }
    console_write("plan9_mount: ld-linux-x86-64.so.2 not found in any known location");
}

fn ensure_etc_hosts() {
    if !path_exists("/mnt/etc/hosts") {
        mkdir_p("/mnt/etc");
        write_file("/mnt/etc/hosts", b"127.0.0.1\tlocalhost\n::1\t\tlocalhost ip6-localhost\n");
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

/// Run busybox with the given argv (argv[0] = applet name) and wait for exit.
/// Returns true if the child exited cleanly with status 0.
fn exec_busybox(argv: &[&str]) -> bool {
    let pid = unsafe { fork() };
    if pid < 0 {
        console_write(&format!("plan9_mount: fork failed: {}", io::Error::last_os_error()));
        return false;
    }
    if pid == 0 {
        // Child — exec busybox with the given argv. argv[0] is the applet name
        // (busybox dispatches by argv[0] when invoked as "busybox <applet> ...").
        let bb = CString::new("/bin/busybox").unwrap();
        let cstrs: Vec<CString> = argv.iter().map(|s| CString::new(*s).unwrap()).collect();
        let mut ptrs: Vec<*const c_char> = cstrs.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        unsafe { execv(bb.as_ptr(), ptrs.as_ptr()) };
        // exec failed
        unsafe { _exit(127) };
    }
    let mut status: i32 = 0;
    let r = unsafe { waitpid(pid, &mut status as *mut i32, 0) };
    if r < 0 {
        console_write(&format!("plan9_mount: waitpid failed: {}", io::Error::last_os_error()));
        return false;
    }
    // WIFEXITED && WEXITSTATUS == 0
    let exited = (status & 0x7f) == 0;
    let code = (status >> 8) & 0xff;
    if !exited || code != 0 {
        console_write(&format!("plan9_mount: child exited with status 0x{:x} (code={})", status, code));
        return false;
    }
    true
}

/// Extract OCI layers in-guest from a 9P-shared blobs directory into /mnt (tmpfs).
///
/// Reads `/9p-rootfs/.nanosb-layers` (one digest per line, in order), then for each
/// digest extracts `/blobs/{digest}.tar` into /mnt using busybox tar, then runs a
/// whiteout pass to honor OCI `.wh.*` markers.
fn extract_layers_in_guest() -> bool {
    // Connect 2nd vsock for blobs share (port 50002, see builder.rs)
    let blobs_fd = vsock_connect(50002);
    if blobs_fd < 0 {
        console_write("plan9_mount: failed to connect blobs vsock");
        return false;
    }
    mkdir_p("/blobs");
    let blobs_opts = format!(
        "trans=fd,rfdno={},wfdno={},version=9p2000.L,aname=blobs",
        blobs_fd, blobs_fd
    );
    if !do_mount("blobs", "/blobs", "9p", &blobs_opts) {
        console_write("plan9_mount: blobs 9p mount failed");
        return false;
    }

    // Mount tmpfs at /mnt — the in-guest rootfs target
    mkdir_p("/mnt");
    if !do_mount("tmpfs", "/mnt", "tmpfs", "size=4G,mode=755") {
        console_write("plan9_mount: tmpfs at /mnt failed");
        return false;
    }

    // Read manifest (one layer digest per line, in apply order)
    let manifest_data = match read_file("/9p-rootfs/.nanosb-layers") {
        Some(d) => d,
        None => {
            console_write("plan9_mount: missing /9p-rootfs/.nanosb-layers manifest");
            return false;
        }
    };
    let manifest = String::from_utf8_lossy(&manifest_data).into_owned();

    let extract_start_count = manifest.lines().filter(|l| !l.trim().is_empty()).count();
    console_write(&format!("plan9_mount: extracting {} layers...", extract_start_count));

    for line in manifest.lines() {
        let digest = line.trim();
        if digest.is_empty() { continue; }
        let tar_path = format!("/blobs/{}.tar", digest);
        if !path_exists(&tar_path) {
            console_write(&format!("plan9_mount: layer tar missing: {}", tar_path));
            return false;
        }
        let short = &digest[..12.min(digest.len())];
        console_write(&format!("plan9_mount: tar -xf {} -> /mnt", short));
        if !exec_busybox(&["tar", "-xf", &tar_path, "-C", "/mnt"]) {
            console_write(&format!("plan9_mount: tar extract failed for {}", digest));
            return false;
        }
    }

    // Whiteout pass: OCI uses .wh.<name> to mark deletions and .wh..wh..opq for
    // opaque dirs. Process them so the merged tmpfs reflects the final layer state.
    let whiteout_script = "set -e; find /mnt -name '.wh.*' 2>/dev/null | while read wh; do \
        d=$(dirname \"$wh\"); n=$(basename \"$wh\"); \
        case \"$n\" in \
            .wh..wh..opq) rm -f \"$wh\" ;; \
            *) t=${n#.wh.}; rm -rf \"$d/$t\"; rm -f \"$wh\" ;; \
        esac; \
    done";
    if !exec_busybox(&["sh", "-c", whiteout_script]) {
        console_write("plan9_mount: whiteout pass failed (continuing)");
    }

    // Copy host-side overlay files from the rootfs share onto the extracted rootfs
    // so guest init scripts (nanosb-init.sh) can read them.
    if path_exists("/9p-rootfs/etc/nanosb-mounts") {
        mkdir_p("/mnt/etc");
        copy_file("/9p-rootfs/etc/nanosb-mounts", "/mnt/etc/nanosb-mounts");
    }

    console_write("plan9_mount: in-guest extraction complete");
    true
}


fn main() {
    // Parse args: plan9_mount <port> <share_name> <user_command>
    let args: Vec<String> = std::env::args().collect();
    let port: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(50000);
    let share_name = args.get(2).map(|s| s.as_str()).unwrap_or("rootfs");
    let user_cmd = args.get(3).map(|s| s.as_str()).unwrap_or("echo 'no command'");

    // Mount basic filesystems (may fail with EBUSY if init script already mounted them)
    do_mount("devtmpfs", "/dev", "devtmpfs", "");
    do_mount("proc", "/proc", "proc", "");
    do_mount("sysfs", "/sys", "sysfs", "");

    // Small delay to let console pipe connect
    unsafe { sleep(1) };

    // In-guest extraction mode: the rootfs share carries only a manifest +
    // thin overlay files. Layers come from the blobs share and are extracted
    // by busybox into a tmpfs at /mnt. The 9P rootfs share is moved to
    // /9p-rootfs so the rest of this init can keep treating /mnt as the root.
    let extract_mode = cmdline_has("nanosb.extract_layers=1");
    let nine_p_mount = if extract_mode { "/9p-rootfs" } else { "/mnt" };
    if extract_mode {
        mkdir_p(nine_p_mount);
        console_write("plan9_mount: in-guest extraction mode active");
    }

    console_write(&format!("plan9_mount: connecting to HCS Plan9 service (vsock port {})...", port));

    // Connect to HCS Plan9 service via AF_VSOCK
    let fd = vsock_connect(port);
    if fd < 0 {
        console_write("plan9_mount: failed to connect to Plan9 service, running without rootfs");
    } else {
        // Mount 9p share at nine_p_mount.
        // Key options:
        //   trans=fd      - use the vsock file descriptor as transport
        //   aname=<share> - MUST match the HCS Plan9 share's access_name
        //   version=9p2000.L - Linux 9P2000.L protocol
        let opts = format!(
            "trans=fd,rfdno={},wfdno={},version=9p2000.L,aname={}",
            fd, fd, share_name
        );
        console_write(&format!("plan9_mount: mounting 9p share '{}' at {}", share_name, nine_p_mount));

        if !do_mount(share_name, nine_p_mount, "9p", &opts) {
            console_write("plan9_mount: 9p mount failed, running without rootfs");
            unsafe { close(fd) };
        } else if extract_mode {
            // In extract mode, /mnt itself becomes the tmpfs target into which
            // the OCI layers get unpacked. /9p-rootfs holds the manifest +
            // overlay files and is read-only from our perspective.
            if !extract_layers_in_guest() {
                console_write("plan9_mount: in-guest extraction failed, running without rootfs");
            }
        }
    }

    // Check if rootfs was mounted by looking for /mnt/usr (more reliable than /mnt/bin
    // which may be a symlink lost during Windows extraction)
    let mnt_usr = CString::new("/mnt/usr").unwrap();
    let has_rootfs = unsafe {
        let mut stat_buf = [0u8; 144];
        syscall(4i64, mnt_usr.as_ptr() as i64, stat_buf.as_mut_ptr() as i64) == 0
    };

    // Set up stdio on /dev/console
    let console = CString::new("/dev/console").unwrap();
    let console_fd = unsafe { open(console.as_ptr(), 2) }; // O_RDWR
    if console_fd >= 0 {
        unsafe {
            dup2(console_fd, 0);
            dup2(console_fd, 1);
            dup2(console_fd, 2);
            if console_fd > 2 { close(console_fd); }
        }
    }

    if has_rootfs {
        // Mount essential filesystems inside /mnt before chroot
        // (nanosb-init.sh and sshd need /proc, /sys, /dev, /tmp).
        // OCI images don't ship these mountpoint dirs (runc/containerd create them
        // at container start), so mkdir_p first or mount fails with ENOENT.
        mkdir_p("/mnt/proc");
        do_mount("proc", "/mnt/proc", "proc", "");
        mkdir_p("/mnt/sys");
        do_mount("sysfs", "/mnt/sys", "sysfs", "");
        mkdir_p("/mnt/dev");
        do_mount("devtmpfs", "/mnt/dev", "devtmpfs", "");
        mkdir_p("/mnt/tmp");
        do_mount("tmpfs", "/mnt/tmp", "tmpfs", "");
        mkdir_p("/mnt/dev/pts");
        do_mount("devpts", "/mnt/dev/pts", "devpts", "");
        mkdir_p("/mnt/run");
        do_mount("tmpfs", "/mnt/run", "tmpfs", "");
        // Mount tmpfs over /etc/ssh so ssh-keygen generates host keys with proper
        // permissions. 9P from Windows sets 0777 on all files (NTFS doesn't track
        // Unix perms), but sshd requires host keys to be 0600.
        mkdir_p("/mnt/etc/ssh");
        do_mount("tmpfs", "/mnt/etc/ssh", "tmpfs", "");
        // Copy existing sshd_config from rootfs into tmpfs so sshd can read it
        // (the tmpfs mount hides the 9P version). We need to read the config
        // BEFORE mounting tmpfs, but we already mounted it. Workaround: write a
        // minimal sshd_config that works with busybox-generated keys.
        write_file("/mnt/etc/ssh/sshd_config",
            b"Port 22\nPermitRootLogin yes\nPubkeyAuthentication yes\nPasswordAuthentication no\nStrictModes no\nSubsystem sftp /usr/lib/openssh/sftp-server\n");

        // Copy resolv.conf from initrd (configured by init.krun networking)
        copy_file("/etc/resolv.conf", "/mnt/etc/resolv.conf");

        // Mount tmpfs over /root/.ssh so authorized_keys gets proper permissions
        // (sshd StrictModes requires 0600 on authorized_keys)
        mkdir_p("/mnt/root/.ssh");
        do_mount("tmpfs", "/mnt/root/.ssh", "tmpfs", "");

        // Inject SSH key from kernel cmdline into the rootfs (writes to tmpfs now)
        inject_ssh_key_from_cmdline();

        // Fix usr-merge symlinks (Docker on Windows loses symlinks like /bin -> usr/bin)
        fix_usr_merge_symlinks();

        // Fix /lib64 — on Windows extraction it becomes an empty dir instead of a symlink.
        // If the real ld-linux exists elsewhere, copy it into /lib64.
        fix_lib64();

        // Inject busybox as a static fallback shell (in case /bin/sh has broken dynamic linking).
        // Ensure /mnt/bin exists — may be missing on minimal images where /bin is normally a
        // symlink to /usr/bin that didn't survive Windows extraction and fix_usr_merge_symlinks
        // also missed it (e.g. /usr/bin not present yet at that point).
        mkdir_p("/mnt/bin");
        copy_file("/bin/busybox", "/mnt/bin/.krun-busybox");
        let bb = CString::new("/mnt/bin/.krun-busybox").unwrap();
        unsafe { chmod(bb.as_ptr(), 0o755) };

        // Ensure /etc/hosts has localhost entries
        ensure_etc_hosts();

        console_write("plan9_mount: chrooting into /mnt");
        let mnt = CString::new("/mnt").unwrap();
        unsafe { chroot(mnt.as_ptr()) };
        let root = CString::new("/").unwrap();
        unsafe { chdir(root.as_ptr()) };
    }

    // Exec user command — use busybox as shell (static binary, avoids ld-linux issues)
    let bb_path = if has_rootfs { "/bin/.krun-busybox" } else { "/bin/busybox" };
    console_write(&format!("plan9_mount: exec {} sh -c '{}'", bb_path, user_cmd));

    let shell = CString::new(bb_path).unwrap();
    let sh = CString::new("sh").unwrap();
    let c_flag = CString::new("-c").unwrap();
    let cmd = CString::new(user_cmd.to_string()).unwrap();

    // argv[0] must be "sh" (not the binary path) so busybox selects the sh applet
    let argv: Vec<*const c_char> = vec![
        sh.as_ptr(), c_flag.as_ptr(), cmd.as_ptr(), std::ptr::null()
    ];

    unsafe { execv(shell.as_ptr(), argv.as_ptr()) };
    console_write(&format!("plan9_mount: execv failed: {}", io::Error::last_os_error()));
    reboot();
}
