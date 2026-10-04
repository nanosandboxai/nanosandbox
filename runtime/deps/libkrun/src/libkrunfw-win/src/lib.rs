//! libkrunfw-win: provides the same `krunfw_get_kernel` ABI as upstream libkrunfw,
//! with the vmlinux kernel embedded at compile time.
//!
//! On non-Windows, this crate compiles as an empty cdylib stub so the workspace
//! type-checks on Linux/macOS without a kernel binary present.

#[cfg(target_os = "windows")]
mod kernel {
    use std::ffi::c_char;
    use std::sync::Once;

    static KERNEL: &[u8] = include_bytes!("../vmlinux.bin");
    const KERNEL_GUEST_ADDR: u64 = include!("../guest_addr.txt");
    const KERNEL_ENTRY_ADDR: u64 = include!("../entry_addr.txt");

    static INIT: Once = Once::new();
    static mut ALIGNED_PTR: *mut u8 = std::ptr::null_mut();
    static mut ALIGNED_LEN: usize = 0;

    fn aligned_kernel() -> (*mut u8, usize) {
        INIT.call_once(|| {
            let len = KERNEL.len();
            let layout = std::alloc::Layout::from_size_align(len, 4096)
                .expect("kernel layout");
            unsafe {
                let ptr = std::alloc::alloc(layout);
                assert!(!ptr.is_null(), "failed to allocate page-aligned kernel buffer");
                std::ptr::copy_nonoverlapping(KERNEL.as_ptr(), ptr, len);
                ALIGNED_PTR = ptr;
                ALIGNED_LEN = len;
            }
        });
        unsafe { (ALIGNED_PTR, ALIGNED_LEN) }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn krunfw_get_kernel(
        guest_addr: *mut u64,
        entry_addr: *mut u64,
        size: *mut usize,
    ) -> *mut c_char {
        let (ptr, len) = aligned_kernel();
        if !guest_addr.is_null() {
            unsafe { *guest_addr = KERNEL_GUEST_ADDR };
        }
        if !entry_addr.is_null() {
            unsafe { *entry_addr = KERNEL_ENTRY_ADDR };
        }
        if !size.is_null() {
            unsafe { *size = len };
        }
        ptr as *mut c_char
    }
}
