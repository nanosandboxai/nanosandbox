fn main() {
    // Tell the linker where to find libkrun on macOS (Homebrew)
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-search=native=/opt/homebrew/lib");
    }

    // On Linux, libkrun is typically in standard library paths,
    // but add common non-standard locations just in case.
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-search=native=/usr/local/lib");
    }
}
