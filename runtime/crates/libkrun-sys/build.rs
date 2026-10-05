//! Build script for libkrun-sys.
//!
//! Links the prebuilt libkrun static library (libkrun.a) into the final binary.
//! The library must be built first via `runtime/scripts/build-libkrun.sh`.
//!
//! Environment variables:
//!   LIBKRUN_LIB_DIR  — Directory containing libkrun.a (default: $HOME/.nanosandbox/lib)
//!
//! If the library is not found, this script prints a helpful error message
//! pointing to the build script.

use std::path::PathBuf;

fn main() {
    // Determine the library directory
    let lib_dir = if let Ok(dir) = std::env::var("LIBKRUN_LIB_DIR") {
        PathBuf::from(dir)
    } else {
        let home = std::env::var("HOME").expect("HOME must be set");
        PathBuf::from(home).join(".nanosandbox").join("lib")
    };

    let libkrun_a = lib_dir.join("libkrun.a");

    // Always register rerun triggers — including when the artifact is missing —
    // so cargo re-runs this script once the library appears.
    println!("cargo:rerun-if-changed={}", libkrun_a.display());
    println!("cargo:rerun-if-env-changed=LIBKRUN_LIB_DIR");

    if !libkrun_a.exists() {
        // Print a helpful error and continue — cargo will fail at link time
        // if the library is truly needed (e.g., for the final binary).
        // This allows `cargo check` on libkrun-sys alone to succeed.
        println!("cargo:warning=libkrun.a not found at {}", libkrun_a.display());
        println!("cargo:warning=Run: runtime/scripts/build-libkrun.sh");
        println!("cargo:warning=Or set: LIBKRUN_LIB_DIR=/path/to/libkrun.a/dir");
        return;
    }

    // Tell cargo to link the static library
    println!("cargo:rustc-link-lib=static=krun");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    // On macOS, libkrun uses Hypervisor.framework
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-lib=framework=Hypervisor");
}
