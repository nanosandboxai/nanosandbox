// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0

//! Userspace IOAPIC for WHPX.
//!
//! WHPX provides LAPIC emulation inside the hypervisor but does **not**
//! emulate the IOAPIC. This module implements a full IOAPIC with:
//!
//! - IOREGSEL / IOWIN register protocol (MMIO at 0xFEC00000)
//! - 24 redirection table entries
//! - Interrupt injection via `WHvRequestInterrupt`
//! - vCPU wake-up via a shared `kick_evt` EventFd

#![cfg(all(target_os = "windows", target_arch = "x86_64"))]

use crate::bus::BusDevice;
use crate::legacy::IrqChipT;
use crate::Error as DeviceError;

use log::{debug, warn};
use utils::eventfd::EventFd;
use windows_sys::Win32::System::Hypervisor::{WHvRequestInterrupt, WHV_INTERRUPT_CONTROL};

/// Standard x86 IOAPIC MMIO base address.
const IOAPIC_BASE: u64 = 0xFEC0_0000;
/// Size of the IOAPIC MMIO region (1 page).
const IOAPIC_SIZE: u64 = 0x1000;
/// Number of redirection table entries.
const NUM_REDIRECTIONS: usize = 24;
/// Default IOAPIC version register: version 0x11 (82093AA standard), 24 entries.
const IOAPIC_VERSION: u32 = 0x0017_0011;

// IOREGSEL register offsets.
const IOREGSEL_OFFSET: u64 = 0x00;
const IOWIN_OFFSET: u64 = 0x10;

// Register indices accessed through IOREGSEL/IOWIN.
const REG_ID: u32 = 0x00;
const REG_VER: u32 = 0x01;
const REG_ARB: u32 = 0x02;
const REG_REDTBL_BASE: u32 = 0x10;

// Redirection table entry bit fields.
const REDIR_MASK_BIT: u64 = 1 << 16;
const REDIR_TRIGGER_MODE_BIT: u64 = 1 << 15;
const REDIR_DEST_MODE_BIT: u64 = 1 << 11;
const REDIR_VECTOR_MASK: u64 = 0xFF;
const REDIR_DELIVERY_MODE_MASK: u64 = 0x700;
const REDIR_DELIVERY_MODE_SHIFT: u64 = 8;
const REDIR_DEST_SHIFT: u64 = 56;
const REDIR_DEST_MASK: u64 = 0xFF << 56;

// WHV_INTERRUPT_TYPE values (from Windows SDK).
const WHV_X64_INTERRUPT_TYPE_FIXED: u64 = 0;
const WHV_X64_INTERRUPT_TYPE_LOWEST_PRIORITY: u64 = 1;
const WHV_X64_INTERRUPT_TYPE_NMI: u64 = 4;

/// WHPX-backed IOAPIC with full redirection table support.
pub struct WhpxIoapic {
    /// Raw WHPX partition handle for calling WHvRequestInterrupt.
    partition_handle: isize,
    /// Currently selected register (set by writing to IOREGSEL at offset 0x00).
    ioregsel: u32,
    /// IOAPIC ID register.
    ioapic_id: u32,
    /// 24 redirection table entries (64-bit each).
    /// Initialized to all-masked (bit 16 set).
    ioredtbl: [u64; NUM_REDIRECTIONS],
    /// Shared with vCPU threads — signaled when an interrupt is injected
    /// to wake vCPUs blocked in HLT.
    kick_evt: EventFd,
}

impl WhpxIoapic {
    /// Create a new WHPX IOAPIC.
    ///
    /// `partition_handle` is the raw `WHV_PARTITION_HANDLE` from `WhpxVm::handle()`.
    /// `kick_evt` is shared with vCPU threads to wake them from HLT.
    /// `ioapic_id` must match the MP table's IOAPIC APIC ID (typically num_cpus + 1).
    pub fn new(partition_handle: isize, kick_evt: EventFd, ioapic_id: u32) -> Self {
        // Initialize all redirection entries as masked.
        let mut ioredtbl = [0u64; NUM_REDIRECTIONS];
        for entry in ioredtbl.iter_mut() {
            *entry = REDIR_MASK_BIT;
        }

        Self {
            partition_handle,
            ioregsel: 0,
            ioapic_id,
            ioredtbl,
            kick_evt,
        }
    }

    /// Read a register selected by ioregsel.
    fn read_register(&self) -> u32 {
        match self.ioregsel {
            REG_ID => self.ioapic_id << 24,
            REG_VER => IOAPIC_VERSION,
            REG_ARB => 0,
            reg if reg >= REG_REDTBL_BASE
                && reg < REG_REDTBL_BASE + (NUM_REDIRECTIONS as u32) * 2 =>
            {
                let index = (reg - REG_REDTBL_BASE) as usize;
                let entry_idx = index / 2;
                if index % 2 == 0 {
                    // Low 32 bits
                    self.ioredtbl[entry_idx] as u32
                } else {
                    // High 32 bits
                    (self.ioredtbl[entry_idx] >> 32) as u32
                }
            }
            _ => {
                warn!("IOAPIC: read unknown register 0x{:02X}", self.ioregsel);
                0
            }
        }
    }

    /// Write a register selected by ioregsel.
    fn write_register(&mut self, value: u32) {
        match self.ioregsel {
            REG_ID => {
                self.ioapic_id = (value >> 24) & 0xF;
                debug!("IOAPIC: set ID = {}", self.ioapic_id);
            }
            REG_VER | REG_ARB => {
                // Read-only registers — ignore writes.
            }
            reg if reg >= REG_REDTBL_BASE
                && reg < REG_REDTBL_BASE + (NUM_REDIRECTIONS as u32) * 2 =>
            {
                let index = (reg - REG_REDTBL_BASE) as usize;
                let entry_idx = index / 2;
                if index % 2 == 0 {
                    // Low 32 bits — preserve high 32.
                    self.ioredtbl[entry_idx] =
                        (self.ioredtbl[entry_idx] & 0xFFFF_FFFF_0000_0000) | (value as u64);
                } else {
                    // High 32 bits — preserve low 32.
                    self.ioredtbl[entry_idx] =
                        (self.ioredtbl[entry_idx] & 0x0000_0000_FFFF_FFFF) | ((value as u64) << 32);
                }
                eprintln!(
                    "IOAPIC: REDTBL[{}] {} = 0x{:016X}",
                    entry_idx,
                    if index % 2 == 0 { "low" } else { "high" },
                    self.ioredtbl[entry_idx]
                );
            }
            _ => {
                warn!(
                    "IOAPIC: write unknown register 0x{:02X} = 0x{value:08X}",
                    self.ioregsel
                );
            }
        }
    }

    /// Inject an interrupt into the guest via WHvRequestInterrupt.
    fn inject_interrupt(&self, entry: u64) {
        let vector = (entry & REDIR_VECTOR_MASK) as u32;
        let delivery_mode = ((entry & REDIR_DELIVERY_MODE_MASK) >> REDIR_DELIVERY_MODE_SHIFT) as u64;
        let dest_mode = if entry & REDIR_DEST_MODE_BIT != 0 { 1u64 } else { 0u64 };
        let trigger_mode = if entry & REDIR_TRIGGER_MODE_BIT != 0 { 1u64 } else { 0u64 };
        let destination = ((entry & REDIR_DEST_MASK) >> REDIR_DEST_SHIFT) as u32;

        // Map IOAPIC delivery mode to WHV_INTERRUPT_TYPE.
        let interrupt_type = match delivery_mode {
            0 => WHV_X64_INTERRUPT_TYPE_FIXED,
            1 => WHV_X64_INTERRUPT_TYPE_LOWEST_PRIORITY,
            4 => WHV_X64_INTERRUPT_TYPE_NMI,
            _ => WHV_X64_INTERRUPT_TYPE_FIXED,
        };

        // Build WHV_INTERRUPT_CONTROL bitfield:
        // bits 1:0 = InterruptType
        // bit 3 = DestinationMode (0=Physical, 1=Logical)
        // bit 4 = TriggerMode (0=Edge, 1=Level)
        let bitfield = interrupt_type | (dest_mode << 3) | (trigger_mode << 4);

        let interrupt_control = WHV_INTERRUPT_CONTROL {
            _bitfield: bitfield,
            Destination: destination,
            Vector: vector,
        };

        let hr = unsafe {
            WHvRequestInterrupt(
                self.partition_handle,
                &interrupt_control as *const _ as *const _,
                std::mem::size_of::<WHV_INTERRUPT_CONTROL>() as u32,
            )
        };

        if hr != 0 {
            warn!(
                "IOAPIC: WHvRequestInterrupt failed: HRESULT=0x{hr:08X} (vector={vector}, dest={destination})"
            );
        } else {
            debug!(
                "IOAPIC: injected interrupt vector={vector} dest={destination} type={interrupt_type}"
            );
        }
    }
}

impl BusDevice for WhpxIoapic {
    fn read(&mut self, _vcpuid: u64, offset: u64, data: &mut [u8]) {
        let value = match offset {
            IOREGSEL_OFFSET => self.ioregsel,
            IOWIN_OFFSET => self.read_register(),
            _ => {
                debug!("IOAPIC: read unknown offset 0x{offset:X}");
                0
            }
        };

        // Write value into data buffer (little-endian), truncated to data.len().
        let bytes = value.to_le_bytes();
        let len = data.len().min(4);
        data[..len].copy_from_slice(&bytes[..len]);
    }

    fn write(&mut self, _vcpuid: u64, offset: u64, data: &[u8]) {
        // Read value from data buffer (little-endian).
        let mut bytes = [0u8; 4];
        let len = data.len().min(4);
        bytes[..len].copy_from_slice(&data[..len]);
        let value = u32::from_le_bytes(bytes);

        match offset {
            IOREGSEL_OFFSET => {
                self.ioregsel = value;
                eprintln!("IOAPIC: IOREGSEL = 0x{value:02X}");
            }
            IOWIN_OFFSET => {
                eprintln!("IOAPIC: IOWIN write 0x{value:08X} (regsel=0x{:02X})", self.ioregsel);
                self.write_register(value);
            }
            _ => {
                debug!("IOAPIC: write unknown offset 0x{offset:X} = 0x{value:08X}");
            }
        }
    }
}

impl IrqChipT for WhpxIoapic {
    fn get_mmio_addr(&self) -> u64 {
        IOAPIC_BASE
    }

    fn get_mmio_size(&self) -> u64 {
        IOAPIC_SIZE
    }

    fn set_irq(
        &self,
        irq_line: Option<u32>,
        interrupt_evt: Option<&EventFd>,
    ) -> Result<(), DeviceError> {
        if let Some(irq) = irq_line {
            if (irq as usize) < NUM_REDIRECTIONS {
                let entry = self.ioredtbl[irq as usize];
                let masked = entry & REDIR_MASK_BIT != 0;

                if !masked {
                    eprintln!("IOAPIC: injecting IRQ {irq}, entry=0x{entry:016X}");
                    self.inject_interrupt(entry);
                    // Wake halted vCPUs so they pick up the injected interrupt.
                    let _ = self.kick_evt.write(1);
                } else {
                    debug!("IOAPIC: IRQ {irq} is masked — not injecting");
                }
            } else {
                warn!("IOAPIC: IRQ line {irq} out of range (max {})", NUM_REDIRECTIONS - 1);
            }
        }

        // Legacy EventFd signaling (for devices that still use it).
        if let Some(evt) = interrupt_evt {
            let _ = evt.write(1);
        }

        Ok(())
    }
}
