fn main() {
    println!("cargo:rustc-env=RUST_BACKTRACE=1");
    tauri_build::build()
}
