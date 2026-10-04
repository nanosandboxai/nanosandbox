use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let kernel_path = crate_dir.join("vmlinux.bin");
    let guest_addr_path = crate_dir.join("guest_addr.txt");
    let entry_addr_path = crate_dir.join("entry_addr.txt");

    println!("cargo:rerun-if-env-changed=LIBKRUNFW_WIN_KERNEL");
    println!("cargo:rerun-if-changed={}", kernel_path.display());

    // Priority 1: LIBKRUNFW_WIN_KERNEL env var (local dev override)
    if let Ok(override_path) = env::var("LIBKRUNFW_WIN_KERNEL") {
        let src = PathBuf::from(&override_path);
        if !src.exists() {
            panic!("LIBKRUNFW_WIN_KERNEL={} not found", override_path);
        }
        fs::copy(&src, &kernel_path)
            .unwrap_or_else(|e| panic!("copy kernel from {}: {}", override_path, e));
        return;
    }

    // Priority 2: kernel file present (any size — empty is OK for cargo check)
    if kernel_path.exists() {
        // Ensure stub address files exist
        if !guest_addr_path.exists() {
            fs::write(&guest_addr_path, b"0").ok();
        }
        if !entry_addr_path.exists() {
            fs::write(&entry_addr_path, b"0").ok();
        }
        return;
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Non-Windows: write empty stubs so include_bytes!/include! don't fail
    if target_os != "windows" {
        fs::write(&kernel_path, b"").ok();
        if !guest_addr_path.exists() {
            fs::write(&guest_addr_path, b"0").ok();
        }
        if !entry_addr_path.exists() {
            fs::write(&entry_addr_path, b"0").ok();
        }
        return;
    }

    // Windows without kernel: fail with instructions
    panic!(
        "libkrunfw-win: vmlinux.bin not found at {}\n\n\
         Options:\n\
         1. Set LIBKRUNFW_WIN_KERNEL=/path/to/vmlinux.bin\n\
         2. Download from GitHub Actions (build-kernel.yml artifact)\n\
         3. Build locally: cargo run -p extract_kernel -- <libkrunfw.dylib> <output_dir>\n",
        kernel_path.display()
    );
}
