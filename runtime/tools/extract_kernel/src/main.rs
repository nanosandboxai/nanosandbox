//! Extract vmlinux kernel binary from libkrunfw.
//!
//! Two modes:
//!   1. `extract_kernel [output_dir]`
//!      On macOS/Linux: loads libkrunfw.dylib/.so via dlopen, calls krunfw_get_kernel
//!
//!   2. `extract_kernel --elf <path-to-libkrunfw.so> [output_dir]`
//!      On any platform: parses the ELF .so statically, extracts KERNEL_BUNDLE data
//!      and reads guest_addr/entry_addr from the krunfw_get_kernel disassembly.
//!
//! Outputs:
//!   - vmlinux.bin: raw kernel binary
//!   - vmlinux_meta.json: { guest_addr, entry_addr, size }
//!   - guest_addr.txt: hex constant for include!()
//!   - entry_addr.txt: hex constant for include!()

use goblin::elf::Elf;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize)]
struct KernelMeta {
    guest_addr: u64,
    entry_addr: u64,
    size: usize,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Parse arguments
    let mut elf_path: Option<String> = None;
    let mut output_dir = PathBuf::from(".");
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--elf" {
            i += 1;
            elf_path = Some(args.get(i).expect("--elf requires a path argument").clone());
        } else {
            output_dir = PathBuf::from(&args[i]);
        }
        i += 1;
    }

    if let Some(elf) = elf_path {
        extract_from_elf(&elf, &output_dir);
    } else {
        #[cfg(not(target_os = "windows"))]
        extract_from_dlopen(&output_dir);

        #[cfg(target_os = "windows")]
        {
            eprintln!("On Windows, use --elf <path-to-libkrunfw.so> to parse the ELF directly.");
            eprintln!("Download from: https://github.com/containers/libkrunfw/releases");
            std::process::exit(1);
        }
    }
}

/// Extract kernel by parsing the ELF .so file statically (works on any platform).
fn extract_from_elf(so_path: &str, output_dir: &PathBuf) {
    println!("Parsing ELF: {so_path}");

    let data = std::fs::read(so_path).expect("Failed to read .so file");
    let elf = Elf::parse(&data).expect("Failed to parse ELF");

    // Find KERNEL_BUNDLE symbol (check both strtab and dynstrtab)
    let kernel_sym = elf
        .syms
        .iter()
        .find(|sym| {
            elf.strtab.get_at(sym.st_name).unwrap_or("") == "KERNEL_BUNDLE"
        })
        .or_else(|| {
            elf.dynsyms.iter().find(|sym| {
                elf.dynstrtab.get_at(sym.st_name).unwrap_or("") == "KERNEL_BUNDLE"
            })
        })
        .expect("KERNEL_BUNDLE symbol not found in ELF");

    let kernel_size = kernel_sym.st_size as usize;
    println!("  KERNEL_BUNDLE: vaddr=0x{:X}, size={} ({:.1} MB)",
        kernel_sym.st_value, kernel_size, kernel_size as f64 / 1024.0 / 1024.0);

    // Convert virtual address to file offset using program headers
    let kernel_offset = vaddr_to_offset(&elf, kernel_sym.st_value)
        .expect("Could not map KERNEL_BUNDLE vaddr to file offset");
    println!("  File offset: 0x{kernel_offset:X}");

    let kernel_data = &data[kernel_offset..kernel_offset + kernel_size];

    // Find krunfw_get_kernel to extract guest_addr and entry_addr.
    // These are hardcoded in the function as immediate values.
    // We look for the function symbol to get its code, then scan for mov instructions.
    let (guest_addr, entry_addr) = find_kernel_addresses(&elf, &data);
    println!("  guest_addr: 0x{guest_addr:X}");
    println!("  entry_addr: 0x{entry_addr:X}");

    write_outputs(output_dir, kernel_data, guest_addr, entry_addr);
}

/// Find guest_addr and entry_addr by examining the krunfw_get_kernel function.
///
/// The function stores immediate values via `mov qword ptr [rdi], imm` instructions.
/// We scan the function bytes for the x86_64 pattern: 48 C7 07 (mov [rdi], imm32)
/// and REX.W mov patterns.
fn find_kernel_addresses(elf: &Elf, data: &[u8]) -> (u64, u64) {
    let func_sym = elf
        .syms
        .iter()
        .find(|sym| {
            elf.strtab.get_at(sym.st_name).unwrap_or("") == "krunfw_get_kernel"
        })
        .or_else(|| {
            elf.dynsyms.iter().find(|sym| {
                elf.dynstrtab.get_at(sym.st_name).unwrap_or("") == "krunfw_get_kernel"
            })
        })
        .expect("krunfw_get_kernel symbol not found");

    let func_offset = vaddr_to_offset(elf, func_sym.st_value)
        .expect("Could not map krunfw_get_kernel to file offset");
    let func_size = func_sym.st_size as usize;
    let func_bytes = &data[func_offset..func_offset + func_size.max(256)];

    println!("  krunfw_get_kernel: vaddr=0x{:X}, size={}, file_offset=0x{:X}",
        func_sym.st_value, func_size, func_offset);

    // Scan for x86_64 mov instructions that store immediate values to memory.
    // Pattern 1: 48 C7 07 xx xx xx xx = mov qword [rdi], sign-extended imm32
    // Pattern 2: 48 C7 06 xx xx xx xx = mov qword [rsi], sign-extended imm32
    // Pattern 3: 48 C7 02 xx xx xx xx = mov qword [rdx], sign-extended imm32
    // Also: 48 C7 47 xx = mov qword [rdi+disp8], imm32
    //
    // We collect all immediate values that look like addresses.
    let mut candidate_values: Vec<u64> = Vec::new();

    let mut pos = 0;
    while pos + 7 <= func_bytes.len() {
        // REX.W prefix (0x48 or 0x49)
        if func_bytes[pos] == 0x48 && func_bytes[pos + 1] == 0xC7 {
            let modrm = func_bytes[pos + 2];
            let rm = modrm & 0x07;
            let mod_field = (modrm >> 6) & 0x03;

            let imm_offset = match mod_field {
                0x00 if rm != 0x04 && rm != 0x05 => Some(3), // [reg]
                0x01 if rm != 0x04 => Some(4),               // [reg+disp8]
                0x02 if rm != 0x04 => Some(7),               // [reg+disp32]
                _ => None,
            };

            if let Some(off) = imm_offset {
                if pos + off + 4 <= func_bytes.len() {
                    let imm = u32::from_le_bytes([
                        func_bytes[pos + off],
                        func_bytes[pos + off + 1],
                        func_bytes[pos + off + 2],
                        func_bytes[pos + off + 3],
                    ]);
                    // Sign-extend to 64-bit
                    let val = imm as i32 as i64 as u64;
                    if val >= 0x100000 && val < 0x1_0000_0000 {
                        candidate_values.push(val);
                    }
                }
            }
        }

        // Also check for movabs (REX.W + B8+rd = 48 B8..BF followed by 8 bytes)
        if func_bytes[pos] == 0x48 && func_bytes[pos + 1] >= 0xB8 && func_bytes[pos + 1] <= 0xBF {
            if pos + 10 <= func_bytes.len() {
                let val = u64::from_le_bytes([
                    func_bytes[pos + 2], func_bytes[pos + 3],
                    func_bytes[pos + 4], func_bytes[pos + 5],
                    func_bytes[pos + 6], func_bytes[pos + 7],
                    func_bytes[pos + 8], func_bytes[pos + 9],
                ]);
                if val >= 0x100000 && val < 0x1_0000_0000 {
                    candidate_values.push(val);
                }
            }
        }

        pos += 1;
    }

    println!("  Candidate address values found: {:?}",
        candidate_values.iter().map(|v| format!("0x{v:X}")).collect::<Vec<_>>());

    // Heuristic: guest_addr is typically the smallest value (load address),
    // entry_addr is slightly larger (load address + entry offset)
    if candidate_values.len() >= 2 {
        candidate_values.sort();
        candidate_values.dedup();
        let guest_addr = candidate_values[0];
        let entry_addr = candidate_values[1];
        return (guest_addr, entry_addr);
    } else if candidate_values.len() == 1 {
        // Sometimes guest_addr == entry_addr
        let addr = candidate_values[0];
        return (addr, addr);
    }

    // Fallback: known values for libkrunfw 5.x x86_64
    eprintln!("WARNING: Could not find addresses in krunfw_get_kernel, using defaults for x86_64");
    (0x1000000, 0x1000000)
}

/// Convert ELF virtual address to file offset using program headers.
fn vaddr_to_offset(elf: &Elf, vaddr: u64) -> Option<usize> {
    for ph in &elf.program_headers {
        if ph.p_type == goblin::elf::program_header::PT_LOAD
            && vaddr >= ph.p_vaddr
            && vaddr < ph.p_vaddr + ph.p_memsz
        {
            return Some((ph.p_offset + (vaddr - ph.p_vaddr)) as usize);
        }
    }
    None
}

/// Write all output files.
fn write_outputs(output_dir: &PathBuf, kernel_data: &[u8], guest_addr: u64, entry_addr: u64) {
    std::fs::create_dir_all(output_dir).ok();

    // vmlinux.bin
    let bin_path = output_dir.join("vmlinux.bin");
    std::fs::write(&bin_path, kernel_data).expect("Failed to write vmlinux.bin");
    println!("Wrote: {} ({} bytes)", bin_path.display(), kernel_data.len());

    // vmlinux_meta.json
    let meta = KernelMeta {
        guest_addr,
        entry_addr,
        size: kernel_data.len(),
    };
    let meta_path = output_dir.join("vmlinux_meta.json");
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap())
        .expect("Failed to write vmlinux_meta.json");
    println!("Wrote: {}", meta_path.display());

    // guest_addr.txt / entry_addr.txt (for Rust include!())
    let ga_path = output_dir.join("guest_addr.txt");
    std::fs::write(&ga_path, format!("0x{:X}", guest_addr))
        .expect("Failed to write guest_addr.txt");
    println!("Wrote: {}", ga_path.display());

    let ea_path = output_dir.join("entry_addr.txt");
    std::fs::write(&ea_path, format!("0x{:X}", entry_addr))
        .expect("Failed to write entry_addr.txt");
    println!("Wrote: {}", ea_path.display());

    println!("\nKernel extraction complete!");
    println!("  guest_addr = 0x{guest_addr:X}");
    println!("  entry_addr = 0x{entry_addr:X}");
    println!("  size       = {} bytes ({:.1} MB)", kernel_data.len(), kernel_data.len() as f64 / 1024.0 / 1024.0);
}

/// Extract kernel by loading the native library (macOS/Linux only).
#[cfg(not(target_os = "windows"))]
fn extract_from_dlopen(output_dir: &PathBuf) {
    use libloading::{Library, Symbol};
    use std::ffi::c_char;

    #[cfg(target_os = "macos")]
    let candidates = &[
        "/opt/homebrew/lib/libkrunfw.dylib",
        "/usr/local/lib/libkrunfw.dylib",
        "/opt/homebrew/lib/libkrunfw.5.dylib",
    ];

    #[cfg(target_os = "linux")]
    let candidates = &[
        "libkrunfw.so.5",
        "/usr/lib/libkrunfw.so.5",
        "/usr/lib64/libkrunfw.so.5",
        "/usr/local/lib/libkrunfw.so.5",
    ];

    let lib = candidates
        .iter()
        .find_map(|path| {
            println!("Trying: {path}");
            unsafe { Library::new(path) }.ok()
        })
        .expect("Could not find libkrunfw. Install it or use --elf <path>");

    println!("Loaded libkrunfw successfully.");

    let get_kernel: Symbol<unsafe extern "C" fn(*mut u64, *mut u64, *mut usize) -> *mut c_char> =
        unsafe { lib.get(b"krunfw_get_kernel") }.expect("Symbol krunfw_get_kernel not found");

    let mut guest_addr: u64 = 0;
    let mut entry_addr: u64 = 0;
    let mut size: usize = 0;

    let kernel_ptr = unsafe { get_kernel(&mut guest_addr, &mut entry_addr, &mut size) };
    assert!(!kernel_ptr.is_null(), "krunfw_get_kernel returned NULL");
    assert!(size > 0, "kernel size is 0");

    let kernel_data = unsafe { std::slice::from_raw_parts(kernel_ptr as *const u8, size) };
    write_outputs(output_dir, kernel_data, guest_addr, entry_addr);
}
