//! macOS Seatbelt confinement for the VM-boot subprocess.
//!
//! libkrun's virtiofs server runs **in-process** with the VMM: it has no chroot,
//! no seccomp, and no path confinement of its own. A guest that escapes the
//! shared directory (symlink / `..` / hardlink / TOCTOU — cf. CVE-2026-77179,
//! CVE-2026-47243) would otherwise reach any host path the user can read.
//!
//! This module applies a **deny-default** Seatbelt profile to the
//! `internal-boot-vm` process, allowlisting only the paths a VM genuinely needs
//! (rootfs, declared mounts, firmware libs, control/console sockets, gvproxy).
//! Seatbelt resolves the *final* path, so a symlink from inside a share to
//! `~/.ssh/id_rsa` is denied at the resolved target.
//!
//! On Linux the equivalent is Landlock + `openat2(RESOLVE_IN_ROOT)`.

/// The host paths a VM-boot subprocess may touch, plus its log/socket paths.
#[derive(Debug, Clone, Default)]
pub struct VmSandboxPaths {
    /// Guest root filesystem (shared read-write; guest writes to `/` land here).
    pub rootfs: String,
    /// Declared mounts and extra mounts as `(host_path, read_only)`.
    pub mounts: Vec<(String, bool)>,
    /// Firmware library directory (e.g. `~/.nanosandbox/libs`).
    pub firmware_dir: Option<String>,
    /// Host paths that must be writable (gvproxy/vsock sockets, console log).
    pub writable_paths: Vec<String>,
}

impl VmSandboxPaths {
    /// Build the deny-default Seatbelt profile (Scheme source) for these paths.
    ///
    /// This is a **deny-default** profile: only the paths/operations a VM
    /// genuinely needs are granted. A deny-list layered on `(allow default)` is
    /// not a sandbox (a single missed rule — e.g. `file-mount` — enables the
    /// devfs/LaunchServices escape class), so `(allow default)` must never
    /// reappear here.
    #[cfg(target_os = "macos")]
    pub fn to_seatbelt_profile(&self) -> String {
        fn esc(s: &str) -> String {
            s.replace('\\', "\\\\").replace('"', "\\\"")
        }
        // `system.sb` supplies the baseline system operations libkrun needs
        // (mach services, dyld lookups). `(deny default)` governs everything else.
        let mut p = String::from("(version 1)\n(deny default)\n(import \"system.sb\")\n");

        // In-process VMM/virtiofs: threads + shared memory + sysctl + signals to self.
        p.push_str("(allow process-fork)\n");
        p.push_str("(allow sysctl-read)\n");
        p.push_str("(allow ipc-posix-shm)\n");
        p.push_str("(allow file-read-metadata)\n");
        p.push_str("(allow signal (target self))\n");

        // System libraries loaded by dyld / libkrunfw.
        for sys in [
            "/System/Library",
            "/usr/lib",
            "/usr/share",
            "/Library/Apple",
            "/private/var/db/dyld",
        ] {
            p.push_str(&format!("(allow file-read* (subpath \"{}\"))\n", esc(sys)));
        }

        // Guest root filesystem: guest writes to `/` land here.
        p.push_str(&format!(
            "(allow file-read* file-write* (subpath \"{}\"))\n",
            esc(&self.rootfs)
        ));

        // Declared mounts: RW unless declared read-only.
        for (path, ro) in &self.mounts {
            if *ro {
                p.push_str(&format!("(allow file-read* (subpath \"{}\"))\n", esc(path)));
            } else {
                p.push_str(&format!(
                    "(allow file-read* file-write* (subpath \"{}\"))\n",
                    esc(path)
                ));
            }
        }

        // Firmware directory (libkrunfw dylib).
        if let Some(dir) = &self.firmware_dir {
            p.push_str(&format!("(allow file-read* (subpath \"{}\"))\n", esc(dir)));
        }

        // Control/console sockets and log (file access).
        for w in &self.writable_paths {
            p.push_str(&format!(
                "(allow file-read* file-write* (literal \"{}\"))\n",
                esc(w)
            ));
        }
        // `bind()`-ing a unix socket creates its node, which needs write on the
        // containing directory. libkrun binds its net socket under $TMPDIR
        // (`/private/var/folders/...`) and the control sockets live under /tmp.
        for tmp in ["/private/tmp", "/tmp", "/private/var/folders"] {
            p.push_str(&format!(
                "(allow file-read* file-write* (subpath \"{}\"))\n",
                esc(tmp)
            ));
        }
        // The VMM relays guest traffic to gvproxy over AF_UNIX datagram sockets.
        // Seatbelt's `(local unix-socket)` filter does not cover the datagram
        // bind, so grant network ops broadly: the VM's own network stack is the
        // network boundary here — Seatbelt's job is the *file* confinement above.
        p.push_str("(allow network*)\n");

        // Character devices libkrun touches.
        for dev in ["/dev/null", "/dev/urandom", "/dev/random", "/dev/dtracehelper"] {
            p.push_str(&format!("(allow file-read* (literal \"{}\"))\n", esc(dev)));
        }
        p.push_str("(allow file-write* (literal \"/dev/null\"))\n");
        p
    }

    /// Apply the Seatbelt profile to the current process. Best-effort: a failure
    /// is logged and the process continues (the VM boundary remains the primary
    /// control; this is defense-in-depth).
    #[cfg(target_os = "macos")]
    pub fn apply(&self) {
        // Escape hatch: `NANOSB_SEATBELT=0` disables confinement (e.g. if a future
        // macOS change breaks the profile). Off by default — confinement is on.
        if std::env::var("NANOSB_SEATBELT").as_deref() == Ok("0") {
            eprintln!("nanosb-seatbelt: disabled via NANOSB_SEATBELT=0");
            return;
        }
        let profile = self.to_seatbelt_profile();
        let c_profile = std::ffi::CString::new(profile)
            .expect("seatbelt profile has no interior NUL");
        match unsafe { sandbox_init(c_profile.as_ptr(), 0, std::ptr::null_mut()) } {
            Ok(()) => eprintln!("nanosb-seatbelt: confinement applied"),
            Err(msg) => {
                eprintln!("nanosb-seatbelt: confinement NOT applied: {msg}");
            }
        }
    }

    /// No-op on platforms without a confinement backend.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn apply(&self) {}

    /// Apply Landlock confinement on Linux: restrict this process (the VM-boot
    /// subprocess, which hosts libkrun's virtiofs server) to the guest rootfs,
    /// declared mounts, and firmware directory. Best-effort — a failure is
    /// logged and the process continues. Requires kernel Landlock (5.13+) with
    /// the ABI reported by `landlock_create_ruleset(NULL, 0, VERSION)`.
    ///
    /// NOTE: not yet validated on a Linux host (see the tracking issue).
    #[cfg(target_os = "linux")]
    pub fn apply(&self) {
        if std::env::var("NANOSB_SEATBELT").as_deref() == Ok("0") {
            eprintln!("nanosb-landlock: disabled via NANOSB_SEATBELT=0");
            return;
        }
        match landlock::restrict(self) {
            Ok(()) => eprintln!("nanosb-landlock: confinement applied"),
            Err(e) => eprintln!("nanosb-landlock: confinement NOT applied: {e}"),
        }
    }

    /// Build these paths from a boot request's rootfs/mounts/sockets.
    pub fn from_parts(
        rootfs: &str,
        mounts: impl Iterator<Item = (String, bool)>,
        firmware_dir: Option<String>,
        writable: impl Iterator<Item = String>,
    ) -> Self {
        Self {
            rootfs: rootfs.to_string(),
            mounts: mounts.collect(),
            firmware_dir,
            writable_paths: writable.collect(),
        }
    }
}

// Raw binding to the (deprecated but functional) macOS Seatbelt API.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    #[link_name = "sandbox_init"]
    fn sandbox_init_sys(
        profile: *const std::ffi::c_char,
        flags: u64,
        errorbuf: *mut *mut std::ffi::c_char,
    ) -> std::ffi::c_int;
    fn sandbox_free_error(errorbuf: *mut std::ffi::c_char);
}

#[cfg(target_os = "macos")]
unsafe fn sandbox_init(
    profile: *const std::ffi::c_char,
    flags: u64,
    errorbuf: *mut *mut std::ffi::c_char,
) -> Result<(), String> {
    let rc = sandbox_init_sys(profile, flags, errorbuf);
    if rc == 0 {
        Ok(())
    } else {
        let msg = if errorbuf.is_null() || (*errorbuf).is_null() {
            format!("sandbox_init returned {rc}")
        } else {
            let s = std::ffi::CStr::from_ptr(*errorbuf)
                .to_string_lossy()
                .to_string();
            sandbox_free_error(*errorbuf);
            s
        };
        Err(msg)
    }
}

// Linux confinement via Landlock using raw syscalls (no external crate).
// Access-flag values and syscall numbers are from the kernel UAPI
// (`linux/landlock.h`); the ABI version gates which bits the kernel accepts.
#[cfg(target_os = "linux")]
mod landlock {
    use super::VmSandboxPaths;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const NR_CREATE_RULESET: libc::c_long = 444;
    const NR_ADD_RULE: libc::c_long = 445;
    const NR_RESTRICT_SELF: libc::c_long = 446;
    const CREATE_RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: u32 = 1;

    const ACCESS_FS_EXECUTE: u64 = 1 << 0;
    const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
    const ACCESS_FS_READ_FILE: u64 = 1 << 2;
    const ACCESS_FS_READ_DIR: u64 = 1 << 3;
    const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
    const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
    const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
    const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
    const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
    const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
    const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
    const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
    const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
    const ACCESS_FS_REFER: u64 = 1 << 13;
    const ACCESS_FS_TRUNCATE: u64 = 1 << 14;

    const FS_ALL: u64 = ACCESS_FS_EXECUTE
        | ACCESS_FS_WRITE_FILE
        | ACCESS_FS_READ_FILE
        | ACCESS_FS_READ_DIR
        | ACCESS_FS_REMOVE_DIR
        | ACCESS_FS_REMOVE_FILE
        | ACCESS_FS_MAKE_CHAR
        | ACCESS_FS_MAKE_DIR
        | ACCESS_FS_MAKE_REG
        | ACCESS_FS_MAKE_SOCK
        | ACCESS_FS_MAKE_FIFO
        | ACCESS_FS_MAKE_BLOCK
        | ACCESS_FS_MAKE_SYM
        | ACCESS_FS_REFER
        | ACCESS_FS_TRUNCATE;

    const FS_READ: u64 = ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    fn create_ruleset(handled: u64) -> Result<i32, String> {
        let attr = RulesetAttr {
            handled_access_fs: handled,
        };
        let fd = unsafe {
            libc::syscall(
                NR_CREATE_RULESET,
                &attr as *const RulesetAttr,
                std::mem::size_of::<RulesetAttr>(),
                0u32,
            )
        };
        if fd < 0 {
            return Err(format!(
                "create_ruleset: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(fd as i32)
    }

    fn add_path_rule(ruleset_fd: i32, path: &str, allowed: u64) -> Result<(), String> {
        let c = CString::new(path.as_bytes()).map_err(|_| "path has NUL".to_string())?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(format!("open {path}: {}", std::io::Error::last_os_error()));
        }
        let attr = PathBeneathAttr {
            allowed_access: allowed,
            parent_fd: fd,
        };
        let rc = unsafe {
            libc::syscall(
                NR_ADD_RULE,
                ruleset_fd,
                RULE_PATH_BENEATH,
                &attr as *const PathBeneathAttr,
                0u32,
            )
        };
        unsafe { libc::close(fd) };
        if rc < 0 {
            return Err(format!(
                "add_rule {path}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    pub fn restrict(paths: &VmSandboxPaths) -> Result<(), String> {
        let v = unsafe {
            libc::syscall(
                NR_CREATE_RULESET,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                CREATE_RULESET_VERSION,
            )
        };
        if v < 0 {
            return Err(format!(
                "landlock unsupported: {}",
                std::io::Error::last_os_error()
            ));
        }

        // Mask handled bits the running kernel's ABI does not know.
        let mut handled = FS_ALL;
        if v < 3 {
            handled &= !ACCESS_FS_TRUNCATE;
        }
        if v < 2 {
            handled &= !ACCESS_FS_REFER;
        }

        let ruleset = create_ruleset(handled)?;
        for ro in [
            "/usr", "/lib", "/lib64", "/etc", "/proc", "/sys", "/dev", "/tmp", "/run",
        ] {
            let _ = add_path_rule(ruleset, ro, FS_READ);
        }
        let _ = add_path_rule(ruleset, &paths.rootfs, handled);
        for (p, readonly) in &paths.mounts {
            let acc = if *readonly { FS_READ } else { handled };
            let _ = add_path_rule(ruleset, p, acc);
        }
        if let Some(dir) = &paths.firmware_dir {
            let _ = add_path_rule(ruleset, dir, FS_READ);
        }
        for w in &paths.writable_paths {
            let _ = add_path_rule(ruleset, w, FS_READ | ACCESS_FS_WRITE_FILE);
        }

        let rc = unsafe { libc::syscall(NR_RESTRICT_SELF, ruleset, 0u32) };
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(ruleset) };
        if rc < 0 {
            return Err(format!("restrict_self: {err}"));
        }
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn profile_is_deny_default_and_scoped() {
        let paths = VmSandboxPaths {
            rootfs: "/tmp/rootfs".to_string(),
            mounts: vec![
                ("/tmp/ws".to_string(), false),
                ("/tmp/ro".to_string(), true),
            ],
            firmware_dir: Some("/Users/testuser/.nanosandbox/libs".to_string()),
            writable_paths: vec!["/tmp/gvproxy.sock".to_string()],
        };
        let p = paths.to_seatbelt_profile();
        assert!(p.contains("(deny default)"));
        assert!(
            !p.contains("(allow default)"),
            "a deny-list on (allow default) is not a sandbox"
        );
        assert!(p.contains("(allow file-read* file-write* (subpath \"/tmp/rootfs\"))"));
        assert!(p.contains("(allow file-read* file-write* (subpath \"/tmp/ws\"))"));
        assert!(p.contains("(allow file-read* (subpath \"/tmp/ro\"))"));
        assert!(!p.contains("file-write* (subpath \"/tmp/ro\")"));
        assert!(p.contains("(allow file-read* (subpath \"/Users/testuser/.nanosandbox/libs\"))"));
        assert!(p.contains("(allow network*)"));
    }
}
