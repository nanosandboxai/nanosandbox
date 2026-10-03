// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0

//! Windows-compatible guest memory implementation using VirtualAlloc.
//!
//! The upstream vm-memory crate's `GuestMemoryMmap` only works on Unix (uses mmap).
//! This module provides a VirtualAlloc-based alternative that implements the same
//! vm-memory traits (`GuestMemory`, `GuestMemoryRegion`, etc.).
//!
//! Usage: On Windows, `create_guest_memory()` returns a `GuestMemoryMmap` replacement.

#![cfg(target_os = "windows")]

use std::io;
use std::ptr;
use std::slice;
use std::sync::Arc;

use vm_memory::{
    bitmap::{Bitmap, BitmapSlice, WithBitmapSlice},
    guest_memory, volatile_memory, Address, AtomicAccess, Bytes, GuestAddress, GuestMemory,
    GuestMemoryRegion, GuestUsize, MemoryRegionAddress, VolatileMemory, VolatileSlice,
};
use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};

/// A memory region backed by VirtualAlloc.
pub struct WinMemoryRegion {
    base: *mut u8,
    size: usize,
    guest_base: GuestAddress,
}

// SAFETY: The memory region pointer is owned and managed through VirtualAlloc.
unsafe impl Send for WinMemoryRegion {}
unsafe impl Sync for WinMemoryRegion {}

impl WinMemoryRegion {
    /// Allocate a new memory region at the specified guest address.
    pub fn new(guest_base: GuestAddress, size: usize) -> io::Result<Self> {
        let base = unsafe {
            VirtualAlloc(ptr::null_mut(), size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE)
        };

        if base.is_null() {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            base: base as *mut u8,
            size,
            guest_base,
        })
    }

    /// Get the host virtual address for this region.
    pub fn as_ptr(&self) -> *mut u8 {
        self.base
    }
}

impl Drop for WinMemoryRegion {
    fn drop(&mut self) {
        if !self.base.is_null() {
            unsafe {
                VirtualFree(self.base as *mut _, 0, MEM_RELEASE);
            }
        }
    }
}

// Implement Bitmap traits for WinMemoryRegion so it can serve as its own bitmap.
// This is a no-op bitmap (no dirty tracking).
impl<'a> WithBitmapSlice<'a> for WinMemoryRegion {
    type S = ();
}

impl Bitmap for WinMemoryRegion {
    fn mark_dirty(&self, _offset: usize, _len: usize) {}
    fn dirty_at(&self, _offset: usize) -> bool {
        true
    }
    fn slice_at(&self, _offset: usize) -> () {}
}

impl Bytes<MemoryRegionAddress> for WinMemoryRegion {
    type E = guest_memory::Error;

    fn write(&self, buf: &[u8], addr: MemoryRegionAddress) -> Result<usize, Self::E> {
        let offset = addr.raw_value() as usize;
        if offset >= self.size {
            return Err(guest_memory::Error::InvalidGuestAddress(GuestAddress(
                offset as u64,
            )));
        }
        let len = buf.len().min(self.size - offset);
        // SAFETY: We verified offset is within bounds.
        unsafe {
            ptr::copy_nonoverlapping(buf.as_ptr(), self.base.add(offset), len);
        }
        Ok(len)
    }

    fn read(&self, buf: &mut [u8], addr: MemoryRegionAddress) -> Result<usize, Self::E> {
        let offset = addr.raw_value() as usize;
        if offset >= self.size {
            return Err(guest_memory::Error::InvalidGuestAddress(GuestAddress(
                offset as u64,
            )));
        }
        let len = buf.len().min(self.size - offset);
        // SAFETY: We verified offset is within bounds.
        unsafe {
            ptr::copy_nonoverlapping(self.base.add(offset), buf.as_mut_ptr(), len);
        }
        Ok(len)
    }

    fn write_slice(&self, buf: &[u8], addr: MemoryRegionAddress) -> Result<(), Self::E> {
        let len = self.write(buf, addr)?;
        if len != buf.len() {
            return Err(guest_memory::Error::PartialBuffer {
                expected: buf.len(),
                completed: len,
            });
        }
        Ok(())
    }

    fn read_slice(&self, buf: &mut [u8], addr: MemoryRegionAddress) -> Result<(), Self::E> {
        let len = self.read(buf, addr)?;
        if len != buf.len() {
            return Err(guest_memory::Error::PartialBuffer {
                expected: buf.len(),
                completed: len,
            });
        }
        Ok(())
    }

    fn read_exact_from<F: io::Read>(
        &self,
        addr: MemoryRegionAddress,
        src: &mut F,
        count: usize,
    ) -> Result<(), Self::E> {
        let offset = addr.raw_value() as usize;
        if offset.checked_add(count).map_or(true, |end| end > self.size) {
            return Err(guest_memory::Error::InvalidGuestAddress(GuestAddress(
                offset as u64,
            )));
        }
        // SAFETY: We verified the range is within bounds.
        let buf = unsafe { slice::from_raw_parts_mut(self.base.add(offset), count) };
        src.read_exact(buf)
            .map_err(guest_memory::Error::IOError)
    }

    fn read_from<F: io::Read>(
        &self,
        addr: MemoryRegionAddress,
        src: &mut F,
        count: usize,
    ) -> Result<usize, Self::E> {
        let offset = addr.raw_value() as usize;
        if offset >= self.size {
            return Err(guest_memory::Error::InvalidGuestAddress(GuestAddress(
                offset as u64,
            )));
        }
        let len = count.min(self.size - offset);
        let buf = unsafe { slice::from_raw_parts_mut(self.base.add(offset), len) };
        src.read(buf).map_err(guest_memory::Error::IOError)
    }

    fn write_all_to<F: io::Write>(
        &self,
        addr: MemoryRegionAddress,
        dst: &mut F,
        count: usize,
    ) -> Result<(), Self::E> {
        let offset = addr.raw_value() as usize;
        if offset.checked_add(count).map_or(true, |end| end > self.size) {
            return Err(guest_memory::Error::InvalidGuestAddress(GuestAddress(
                offset as u64,
            )));
        }
        let buf = unsafe { slice::from_raw_parts(self.base.add(offset), count) };
        dst.write_all(buf)
            .map_err(guest_memory::Error::IOError)
    }

    fn write_to<F: io::Write>(
        &self,
        addr: MemoryRegionAddress,
        dst: &mut F,
        count: usize,
    ) -> Result<usize, Self::E> {
        let offset = addr.raw_value() as usize;
        if offset >= self.size {
            return Err(guest_memory::Error::InvalidGuestAddress(GuestAddress(
                offset as u64,
            )));
        }
        let len = count.min(self.size - offset);
        let buf = unsafe { slice::from_raw_parts(self.base.add(offset), len) };
        dst.write(buf).map_err(guest_memory::Error::IOError)
    }

    fn store<T: AtomicAccess>(
        &self,
        val: T,
        addr: MemoryRegionAddress,
        order: std::sync::atomic::Ordering,
    ) -> Result<(), Self::E> {
        let offset = addr.raw_value() as usize;
        if offset + std::mem::size_of::<T>() > self.size {
            return Err(guest_memory::Error::InvalidGuestAddress(GuestAddress(
                offset as u64,
            )));
        }
        // SAFETY: Verified bounds. AtomicAccess guarantees alignment/access safety.
        let ptr = unsafe { self.base.add(offset) };
        // Write the value atomically using volatile write.
        // AtomicAccess types are plain data types (u8, u16, u32, u64).
        unsafe {
            ptr::write_volatile(ptr as *mut T, val);
        }
        let _ = order;
        Ok(())
    }

    fn load<T: AtomicAccess>(
        &self,
        addr: MemoryRegionAddress,
        order: std::sync::atomic::Ordering,
    ) -> Result<T, Self::E> {
        let offset = addr.raw_value() as usize;
        if offset + std::mem::size_of::<T>() > self.size {
            return Err(guest_memory::Error::InvalidGuestAddress(GuestAddress(
                offset as u64,
            )));
        }
        let ptr = unsafe { self.base.add(offset) };
        let val = unsafe { ptr::read_volatile(ptr as *const T) };
        let _ = order;
        Ok(val)
    }
}

impl VolatileMemory for WinMemoryRegion {
    type B = ();

    fn len(&self) -> usize {
        self.size
    }

    fn get_slice(
        &self,
        offset: usize,
        count: usize,
    ) -> volatile_memory::Result<VolatileSlice> {
        let end = offset
            .checked_add(count)
            .ok_or(volatile_memory::Error::Overflow {
                base: offset,
                offset: count,
            })?;
        if end > self.size {
            return Err(volatile_memory::Error::OutOfBounds { addr: end });
        }
        // SAFETY: We verified the range is within bounds.
        Ok(unsafe { VolatileSlice::new(self.base.add(offset), count) })
    }
}

impl GuestMemoryRegion for WinMemoryRegion {
    type B = WinMemoryRegion;

    fn len(&self) -> GuestUsize {
        self.size as GuestUsize
    }

    fn start_addr(&self) -> GuestAddress {
        self.guest_base
    }

    fn bitmap(&self) -> &Self::B {
        self
    }

    fn get_host_address(&self, addr: MemoryRegionAddress) -> guest_memory::Result<*mut u8> {
        let offset = addr.raw_value() as usize;
        if offset >= self.size {
            return Err(guest_memory::Error::InvalidGuestAddress(GuestAddress(
                offset as u64,
            )));
        }
        Ok(unsafe { self.base.add(offset) })
    }
}

/// A collection of WinMemoryRegions implementing GuestMemory.
pub struct GuestMemoryWindows {
    regions: Vec<Arc<WinMemoryRegion>>,
}

impl GuestMemoryWindows {
    /// Create guest memory from a list of (guest_address, size) pairs.
    pub fn from_ranges(ranges: &[(GuestAddress, usize)]) -> io::Result<Self> {
        let mut regions = Vec::with_capacity(ranges.len());
        for (addr, size) in ranges {
            regions.push(Arc::new(WinMemoryRegion::new(*addr, *size)?));
        }
        // Sort by guest address.
        regions.sort_by_key(|r| r.guest_base);
        Ok(Self { regions })
    }
}

impl GuestMemory for GuestMemoryWindows {
    type R = WinMemoryRegion;

    fn num_regions(&self) -> usize {
        self.regions.len()
    }

    fn find_region(&self, addr: GuestAddress) -> Option<&Self::R> {
        for region in &self.regions {
            if addr >= region.guest_base
                && addr.raw_value() < region.guest_base.raw_value() + region.size as u64
            {
                return Some(region.as_ref());
            }
        }
        None
    }

    fn iter(&self) -> impl Iterator<Item = &Self::R> {
        self.regions.iter().map(|r| r.as_ref())
    }

    fn try_access<F>(
        &self,
        count: usize,
        addr: GuestAddress,
        mut f: F,
    ) -> guest_memory::Result<usize>
    where
        F: FnMut(usize, usize, MemoryRegionAddress, &Self::R) -> guest_memory::Result<usize>,
    {
        let mut cur = addr;
        let mut total = 0usize;
        while total < count {
            let region = self.find_region(cur).ok_or(
                guest_memory::Error::InvalidGuestAddress(cur),
            )?;
            let offset = cur.raw_value() - region.start_addr().raw_value();
            let len = std::cmp::min(
                count - total,
                GuestMemoryRegion::len(region) as usize - offset as usize,
            );
            let result = f(total, len, MemoryRegionAddress(offset), region)?;
            total += result;
            if result == 0 || result < len {
                break;
            }
            cur = cur
                .checked_add(result as u64)
                .ok_or(guest_memory::Error::InvalidGuestAddress(cur))?;
        }
        Ok(total)
    }
}
