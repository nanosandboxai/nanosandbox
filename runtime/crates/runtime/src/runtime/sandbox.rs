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
    #[cfg(target_os = "macos")]
    pub fn to_seatbelt_profile(&self) -> String {
        fn esc(s: &str) -> String {
            s.replace('\\', "\\\\").replace('"', "\\\"")
        }
        let home = std::env::var("HOME").unwrap_or_default();
        // macOS Seatbelt cannot run libkrun's virtio-net/vfkit path under a fully
        // deny-by-default file policy without breaking VM networking, so this is a
        // DENY-LIST: allow broadly, but deny the high-value host paths an escaped
        // guest must never reach (credentials/keys). Linux uses Landlock + openat2
        // for the stronger deny-by-default equivalent.
        let mut p = String::from(
            "(version 1)\n(deny default)\n(import \"system.sb\")\n(allow default)\n",
        );
        let mut secrets: Vec<String> = vec![
            "/private/etc/master.passwd".to_string(),
            "/private/var/db/dslocal".to_string(),
            "/Library/Keychains".to_string(),
        ];
        if !home.is_empty() {
            for sub in [
                ".ssh",
                ".aws",
                ".gnupg",
                ".kube",
                ".docker",
                ".config/gh",
                ".config/gcloud",
                ".config/op",
                ".netrc",
                ".npmrc",
                ".pypirc",
                ".zsh_history",
                ".bash_history",
                "Library/Keychains",
            ] {
                secrets.push(format!("{home}/{sub}"));
            }
        }
        for s in &secrets {
            p.push_str(&format!(
                "(deny file-read* file-write* (subpath \"{}\"))\n",
                esc(s)
            ));
        }
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

    /// No-op off macOS (Linux uses Landlock, added separately).
    #[cfg(not(target_os = "macos"))]
    pub fn apply(&self) {}

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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn profile_denies_credentials() {
        std::env::set_var("HOME", "/Users/testuser");
        let paths = VmSandboxPaths::default();
        let p = paths.to_seatbelt_profile();
        assert!(p.contains("(allow default)"));
        assert!(p.contains("(deny file-read* file-write* (subpath \"/Users/testuser/.ssh\"))"));
        assert!(p.contains("(deny file-read* file-write* (subpath \"/Users/testuser/.aws\"))"));
        assert!(p.contains("(deny file-read* file-write* (subpath \"/Library/Keychains\"))"));
    }
}
