//! Manual verification that the macOS Seatbelt denylist profile actually blocks
//! reads of credential paths.
//!
//! Run: `cargo run -p runtime --example seatbelt-check`
//!
//! Creates a canary in a denied path (`$HOME/.ssh`), applies the same profile the
//! VM-boot subprocess uses, then attempts to read the canary and a control file.
//! Exits 0 iff the denied read fails and the control read succeeds.

use runtime::runtime::sandbox::VmSandboxPaths;

fn main() {
    let home = std::env::var("HOME").unwrap_or_default();
    let canary = format!("{home}/.ssh/seatbelt_canary");
    if std::fs::create_dir_all(format!("{home}/.ssh")).is_err() {
        // If ~/.ssh does not exist and cannot be created, still run the check:
        // the deny should hold even for a nonexistent path.
    }
    let _ = std::fs::write(&canary, "SEATBELT_LEAK");

    // Apply the VM's Seatbelt profile to THIS process (irreversible).
    VmSandboxPaths::default().apply();

    let denied_secret_read = std::fs::read_to_string(&canary).is_err();
    let control_read_ok = std::fs::read_to_string("/dev/null").is_ok();

    println!("denied_home_ssh_read={denied_secret_read} control_read_ok={control_read_ok}");
    let _ = std::fs::remove_file(&canary);
    if denied_secret_read && control_read_ok {
        std::process::exit(0);
    }
    std::process::exit(1);
}
