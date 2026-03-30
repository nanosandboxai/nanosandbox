use std::path::Path;

fn is_musl() -> bool {
    std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl")
}

fn link_lib(name: &str) {
    if is_musl() {
        // On musl, libkrun is built as a static library (staticlib)
        // because Rust's musl target doesn't support cdylib.
        // staticlib bundles Rust std — allow duplicate symbols when
        // linking into another Rust binary.
        println!("cargo:rustc-link-lib=static={name}");
        println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition");
    } else {
        println!("cargo:rustc-link-lib=dylib={name}");
    }
}

fn main() {
    // Strategy:
    // 1. Check for locally-built libkrun from submodule (deps/libkrun/)
    // 2. Fall back to pkg-config for system-installed libkrun
    // 3. Fall back to known platform-specific paths

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_root = Path::new(&manifest_dir).parent().unwrap().parent().unwrap();

    // Check for submodule build output
    let submodule_lib = workspace_root.join("deps/libkrun/target/release");
    if submodule_lib.exists() {
        println!("cargo:rustc-link-search=native={}", submodule_lib.display());
        link_lib("krun");
        println!("cargo:rerun-if-changed={}", submodule_lib.display());
        return;
    }

    // Try pkg-config
    if pkg_config::probe_library("libkrun").is_ok() {
        return;
    }

    // Platform-specific fallback paths
    #[cfg(target_os = "macos")]
    {
        let paths = ["/opt/homebrew/lib", "/usr/local/lib"];
        for p in &paths {
            if Path::new(p).join("libkrun.dylib").exists() {
                println!("cargo:rustc-link-search=native={}", p);
                link_lib("krun");
                return;
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let paths = [
            "/usr/lib",
            "/usr/lib64",
            "/usr/local/lib",
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib/aarch64-linux-gnu",
        ];
        for p in &paths {
            if Path::new(p).join("libkrun.so").exists()
                || (is_musl() && Path::new(p).join("libkrun.a").exists())
            {
                println!("cargo:rustc-link-search=native={}", p);
                link_lib("krun");
                return;
            }
        }
    }

    // Environment variable override
    if let Ok(path) = std::env::var("NANOSANDBOX_LIBKRUN_PATH") {
        let p = Path::new(&path);
        if let Some(dir) = p.parent() {
            println!("cargo:rustc-link-search=native={}", dir.display());
        }
        link_lib("krun");
        return;
    }

    // If nothing found, emit a warning but don't fail the build.
    // The nanosandbox crate can still use libloading as a runtime fallback.
    println!("cargo:warning=libkrun not found at build time. Runtime dynamic loading will be used as fallback.");
}
