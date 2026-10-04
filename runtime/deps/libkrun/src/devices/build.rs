use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

fn build_default_init() -> PathBuf {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let libkrun_root = manifest_dir.join("../..");
    let init_src = libkrun_root.join("init/init.c");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let init_bin = out_dir.join("init");

    println!("cargo:rerun-if-env-changed=CC_LINUX");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=TIMESYNC");
    println!("cargo:rerun-if-changed={}", init_src.display());
    println!(
        "cargo:rerun-if-changed={}",
        libkrun_root.join("init/jsmn.h").display()
    );

    let mut init_cc_flags = vec!["-O2", "-static", "-Wall"];
    if std::env::var_os("TIMESYNC").as_deref() == Some(OsStr::new("1")) {
        init_cc_flags.push("-D__TIMESYNC__");
    }

    let cc_value = std::env::var("CC_LINUX")
        .or_else(|_| std::env::var("CC"))
        .unwrap_or_else(|_| "cc".to_string());
    let mut cc_parts = cc_value.split_ascii_whitespace();
    let cc = cc_parts.next().expect("CC_LINUX/CC must not be empty");
    let status = Command::new(cc)
        .args(cc_parts)
        .args(&init_cc_flags)
        .arg("-o")
        .arg(&init_bin)
        .arg(&init_src)
        .status()
        .unwrap_or_else(|e| panic!("failed to execute {cc}: {e}"));

    if !status.success() {
        panic!("failed to compile init/init.c: {status}");
    }
    init_bin
}

fn find_prebuilt_init() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let libkrun_root = manifest_dir.join("../..");

    // Try architecture-specific prebuilt first, then generic
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let candidates = [
        libkrun_root.join(format!("init/prebuilt/init-{arch}")),
        libkrun_root.join("init/prebuilt/init"),
    ];

    for candidate in &candidates {
        if candidate.exists() && std::fs::metadata(candidate).map(|m| m.len() > 0).unwrap_or(false)
        {
            return Some(candidate.clone());
        }
    }
    None
}

fn main() {
    let init_binary_path = std::env::var_os("KRUN_INIT_BINARY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // On macOS, init.c requires Linux headers — use prebuilt binary
            if cfg!(target_os = "macos") {
                if let Some(prebuilt) = find_prebuilt_init() {
                    return prebuilt;
                }
                panic!(
                    "Cannot compile init.c on macOS (requires Linux headers). \
                     Either set KRUN_INIT_BINARY_PATH or place a prebuilt init binary in \
                     deps/libkrun/init/prebuilt/init-aarch64 (or init-x86_64)."
                );
            }
            // The init binary is a Linux ELF that runs inside the guest VM.
            // On non-Linux hosts, it must be cross-compiled. If no cross-compiler
            // is available, create a placeholder so the build succeeds (the actual
            // init binary must be supplied at runtime via KRUN_INIT_BINARY_PATH).
            let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
            if target_os == "linux" {
                let init_path = build_default_init();
                // SAFETY: The build script is single threaded.
                unsafe { std::env::set_var("KRUN_INIT_BINARY_PATH", &init_path) };
                init_path
            } else {
                // Try to cross-compile with CC_LINUX; if not set, use a placeholder.
                if std::env::var("CC_LINUX").is_ok() {
                    let init_path = build_default_init();
                    unsafe { std::env::set_var("KRUN_INIT_BINARY_PATH", &init_path) };
                    init_path
                } else {
                    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
                    let placeholder = out_dir.join("init");
                    std::fs::write(&placeholder, b"PLACEHOLDER_INIT").ok();
                    placeholder
                }
            }
        });
    println!(
        "cargo:rustc-env=KRUN_INIT_BINARY_PATH={}",
        init_binary_path.display()
    );
    println!("cargo:rerun-if-env-changed=KRUN_INIT_BINARY_PATH");
}
