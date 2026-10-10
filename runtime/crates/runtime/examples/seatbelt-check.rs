//! Manual verification that the macOS Seatbelt profile actually confines.
//!
//! Run: `cargo run -p runtime --example seatbelt-check`
//!
//! Applies the VM's deny-default profile with an allowlisted temp directory,
//! then probes both sides: a file inside the allowed directory must be
//! readable, while a canary under `$HOME/.ssh` must be denied. Exits 0 iff both
//! hold. With `NANOSB_SEATBELT=0` the profile is not applied, so the denied
//! probe succeeds and the check exits nonzero — the negative control that
//! proves this check can actually fail.

use runtime::runtime::sandbox::VmSandboxPaths;

fn main() {
    let home = std::env::var("HOME").unwrap_or_default();

    let allow = std::env::temp_dir().join("nanosb_seatbelt_check");
    let _ = std::fs::create_dir_all(&allow);
    let ok_file = allow.join("ok.txt");
    let _ = std::fs::write(&ok_file, "OK");

    let canary_dir = format!("{home}/.ssh");
    let canary = format!("{canary_dir}/seatbelt_canary");
    let _ = std::fs::create_dir_all(&canary_dir);
    let _ = std::fs::write(&canary, "SEATBELT_LEAK");

    let allow_str = allow.to_string_lossy().to_string();
    let paths = VmSandboxPaths {
        rootfs: allow_str.clone(),
        mounts: Vec::new(),
        firmware_dir: Some(allow_str),
        writable_paths: Vec::new(),
    };
    paths.apply();

    let allowed_read_ok = std::fs::read_to_string(&ok_file).is_ok();
    let denied_secret_read = std::fs::read_to_string(&canary).is_err();
    let _ = std::fs::remove_file(&canary);

    println!("allowed_read_ok={allowed_read_ok} denied_secret_read={denied_secret_read}");
    if allowed_read_ok && denied_secret_read {
        std::process::exit(0);
    }
    std::process::exit(1);
}
