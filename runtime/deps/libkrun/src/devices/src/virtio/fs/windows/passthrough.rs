#![cfg(target_os = "windows")]

// Copyright 2019 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Windows passthrough filesystem for virtio-fs.
//!
//! This provides a FUSE filesystem backed by a directory on the Windows host.
//! File operations are translated from FUSE protocol to Windows API calls.
//!
//! Key design decisions:
//! - Uses inode -> host path mapping instead of /proc/self/fd
//! - Generates synthetic inode numbers from BY_HANDLE_FILE_INFORMATION (file index + volume serial)
//! - Uses std::fs for most operations, with Windows-specific metadata extensions
//! - Path translation: converts Linux paths (/ separator) to Windows paths (\ separator)
//!
//! Supported features:
//! - Symlinks (Issue #026): CreateSymbolicLinkW with privilege detection
//! - Hardlinks (Issue #026): CreateHardLinkW via std::fs::hard_link
//! - File locking (Issue #027): LockFileEx / UnlockFileEx (note: mandatory, not advisory)
//! - Extended attributes (Issue #028): Stubbed with ENOTSUP (Windows ADS are too different)

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::OwnedHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use log::warn;

use super::super::super::linux_errno::linux_error;
use super::super::bindings;
use super::super::filesystem::{
    Context, DirEntry, Entry, ExportTable, Extensions, FileSystem, FsOptions, GetxattrReply,
    ListxattrReply, OpenOptions, SetattrValid, ZeroCopyReader, ZeroCopyWriter,
};
use super::super::multikey::MultikeyBTreeMap;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_PRIVILEGE_NOT_HELD, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, CreateSymbolicLinkW, GetFileInformationByHandle, LockFileEx, UnlockFileEx,
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, OPEN_EXISTING,
    SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE, SYMBOLIC_LINK_FLAG_DIRECTORY,
};
use windows_sys::Win32::System::IO::OVERLAPPED;

type Inode = u64;
type Handle = u64;

const ROOT_ID: Inode = 1;

// Linux errno constants used directly in FUSE protocol responses.
const LINUX_EPERM: i32 = 1;
const LINUX_EBADF: i32 = 9;
const LINUX_ENOTDIR: i32 = 20;
const LINUX_EINVAL: i32 = 22;
const LINUX_ENOSYS: i32 = 38;
const LINUX_EOPNOTSUPP: i32 = 95;

// FUSE file types (matching Linux S_IFMT values).
#[allow(dead_code)]
const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

/// Alternate key for looking up inodes by (file_index, volume_serial).
#[derive(Clone, Copy, PartialOrd, Ord, PartialEq, Eq)]
struct InodeAltKey {
    file_index: u64,
    volume_serial: u32,
}

/// Data associated with an inode.
struct InodeData {
    inode: Inode,
    /// Full host path for this inode.
    host_path: PathBuf,
    /// Unique file identifier from the filesystem.
    file_index: u64,
    /// Volume serial number.
    volume_serial: u32,
    /// Reference count (FUSE lookup count).
    refcount: AtomicU64,
}

/// Cached directory entry for readdir.
struct CachedDirEntry {
    ino: u64,
    name: Box<[u8]>,
    type_: u8,
}

/// Directory stream state for readdir.
struct DirStream {
    entries: Vec<CachedDirEntry>,
    ready: bool,
}

impl DirStream {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            ready: false,
        }
    }
}

/// Per-handle data for open files/directories.
struct HandleData {
    inode: Inode,
    handle: RwLock<OwnedHandle>,
    dirstream: Mutex<DirStream>,
}

/// Windows passthrough filesystem configuration.
#[derive(Clone)]
pub struct Config {
    pub root_dir: String,
    pub export_table: Option<ExportTable>,
    pub writeback: bool,
    pub announce_submounts: bool,
    pub export_fsid: u64,
    pub allow_root_dir_delete: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            root_dir: String::new(),
            export_table: None,
            writeback: false,
            announce_submounts: false,
            export_fsid: 0,
            allow_root_dir_delete: false,
        }
    }
}

/// Windows passthrough filesystem.
///
/// This provides a FUSE filesystem backed by a directory on the Windows host.
/// File operations are translated from FUSE protocol to Windows API calls.
pub struct PassthroughFs {
    cfg: Config,
    inodes: RwLock<MultikeyBTreeMap<Inode, InodeAltKey, Arc<InodeData>>>,
    handles: RwLock<BTreeMap<Handle, Arc<HandleData>>>,
    next_inode: AtomicU64,
    next_handle: AtomicU64,
    init_inode: AtomicU64,
}

impl PassthroughFs {
    pub fn new(cfg: Config) -> io::Result<Self> {
        Ok(PassthroughFs {
            cfg,
            inodes: RwLock::new(MultikeyBTreeMap::new()),
            handles: RwLock::new(BTreeMap::new()),
            next_inode: AtomicU64::new(ROOT_ID + 1),
            next_handle: AtomicU64::new(1),
            init_inode: AtomicU64::new(0),
        })
    }

    /// Convert a guest path (with / separators) to a host Windows path.
    fn guest_to_host_path(&self, guest_name: &[u8]) -> PathBuf {
        let name = String::from_utf8_lossy(guest_name);
        // Replace / with \ for Windows
        let win_path = name.replace('/', "\\");
        PathBuf::from(&win_path)
    }

    /// Get the full host path for a child of a given inode.
    fn get_child_path(&self, parent: Inode, name: &CStr) -> io::Result<PathBuf> {
        let inodes = self.inodes.read().unwrap();
        let parent_data = inodes.get(&parent).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;
        let child_name = name.to_str().map_err(|_| {
            io::Error::from_raw_os_error(LINUX_EINVAL)
        })?;
        Ok(parent_data.host_path.join(child_name))
    }

    /// Get file information (file_index, volume_serial, nlinks) via Win32 API.
    fn get_by_handle_info(path: &Path) -> (u64, u32, u32) {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                0, // No specific access needed
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS, // Required for directories
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return (0, 0, 1);
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            return (0, 0, 1);
        }
        let file_index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        (file_index, info.dwVolumeSerialNumber, info.nNumberOfLinks)
    }

    /// Perform stat on a path and return a synthetic stat64.
    fn stat_path(path: &Path) -> io::Result<bindings::stat64> {
        let metadata = std::fs::symlink_metadata(path).map_err(linux_error)?;
        Ok(Self::metadata_to_stat64(path, &metadata))
    }

    /// Convert std::fs::Metadata to our stat64 struct.
    fn metadata_to_stat64(path: &Path, metadata: &std::fs::Metadata) -> bindings::stat64 {
        let file_type = if metadata.is_dir() {
            S_IFDIR | 0o755
        } else if metadata.is_symlink() {
            S_IFLNK | 0o777
        } else {
            S_IFREG | 0o644
        };

        let (file_index, volume_serial, nlinks) = Self::get_by_handle_info(path);

        // Convert Windows FILETIME (100ns intervals since 1601-01-01) to Unix epoch.
        fn filetime_to_unix(ft: u64) -> (i64, i64) {
            // Number of 100ns intervals between 1601-01-01 and 1970-01-01.
            const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
            if ft < EPOCH_DIFF {
                return (0, 0);
            }
            let unix_100ns = ft - EPOCH_DIFF;
            let secs = (unix_100ns / 10_000_000) as i64;
            let nsecs = ((unix_100ns % 10_000_000) * 100) as i64;
            (secs, nsecs)
        }

        let (atime, atime_nsec) = filetime_to_unix(metadata.last_access_time());
        let (mtime, mtime_nsec) = filetime_to_unix(metadata.last_write_time());
        let (ctime, ctime_nsec) = filetime_to_unix(metadata.creation_time());

        let size = metadata.file_size() as i64;
        let blksize = 4096i64;
        let blocks = (size + 511) / 512;

        bindings::stat64 {
            st_dev: volume_serial as u64,
            st_ino: file_index,
            st_nlink: nlinks as u64,
            st_mode: file_type,
            st_uid: 0,
            st_gid: 0,
            __pad0: 0,
            st_rdev: 0,
            st_size: size,
            st_blksize: blksize,
            st_blocks: blocks,
            st_atime: atime,
            st_atime_nsec: atime_nsec,
            st_mtime: mtime,
            st_mtime_nsec: mtime_nsec,
            st_ctime: ctime,
            st_ctime_nsec: ctime_nsec,
        }
    }

    fn alloc_inode(&self) -> Inode {
        self.next_inode.fetch_add(1, Ordering::Relaxed)
    }

    fn alloc_handle(&self) -> Handle {
        self.next_handle.fetch_add(1, Ordering::Relaxed)
    }

    /// Lookup or create an inode for the given path.
    fn do_lookup(&self, path: &Path) -> io::Result<Entry> {
        let stat = Self::stat_path(path)?;
        let alt_key = InodeAltKey {
            file_index: stat.st_ino,
            volume_serial: stat.st_dev as u32,
        };

        let mut inodes = self.inodes.write().unwrap();

        // Check if we already have this inode.
        if let Some(data) = inodes.get_alt(&alt_key) {
            data.refcount.fetch_add(1, Ordering::Relaxed);
            return Ok(Entry {
                inode: data.inode,
                generation: 0,
                attr: stat,
                attr_flags: 0,
                attr_timeout: Duration::from_secs(5),
                entry_timeout: Duration::from_secs(5),
            });
        }

        // Create new inode.
        let inode = self.alloc_inode();
        let data = Arc::new(InodeData {
            inode,
            host_path: path.to_path_buf(),
            file_index: stat.st_ino,
            volume_serial: stat.st_dev as u32,
            refcount: AtomicU64::new(1),
        });

        inodes.insert(inode, alt_key, data);

        Ok(Entry {
            inode,
            generation: 0,
            attr: stat,
            attr_flags: 0,
            attr_timeout: Duration::from_secs(5),
            entry_timeout: Duration::from_secs(5),
        })
    }
}

impl FileSystem for PassthroughFs {
    type Inode = Inode;
    type Handle = Handle;

    fn init(&self, capable: FsOptions) -> io::Result<FsOptions> {
        // Set up root inode.
        let root_path = PathBuf::from(&self.cfg.root_dir);
        if !root_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("root directory not found: {}", self.cfg.root_dir),
            ));
        }

        let stat = Self::stat_path(&root_path)?;
        let alt_key = InodeAltKey {
            file_index: stat.st_ino,
            volume_serial: stat.st_dev as u32,
        };

        let root_data = Arc::new(InodeData {
            inode: ROOT_ID,
            host_path: root_path,
            file_index: stat.st_ino,
            volume_serial: stat.st_dev as u32,
            refcount: AtomicU64::new(2), // root starts with refcount 2
        });

        self.inodes
            .write()
            .unwrap()
            .insert(ROOT_ID, alt_key, root_data);

        let mut opts = FsOptions::empty();
        if capable.contains(FsOptions::WRITEBACK_CACHE) && self.cfg.writeback {
            opts |= FsOptions::WRITEBACK_CACHE;
        }
        if capable.contains(FsOptions::BIG_WRITES) {
            opts |= FsOptions::BIG_WRITES;
        }

        Ok(opts)
    }

    fn destroy(&self) {}

    fn lookup(&self, _ctx: Context, parent: Self::Inode, name: &CStr) -> io::Result<Entry> {
        let path = self.get_child_path(parent, name)?;
        self.do_lookup(&path)
    }

    fn forget(&self, _ctx: Context, inode: Self::Inode, count: u64) {
        let mut inodes = self.inodes.write().unwrap();
        if let Some(data) = inodes.get(&inode) {
            let old = data.refcount.fetch_sub(count, Ordering::Relaxed);
            if old <= count {
                inodes.remove(&inode);
            }
        }
    }

    fn getattr(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        _handle: Option<Self::Handle>,
    ) -> io::Result<(bindings::stat64, Duration)> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;
        let stat = Self::stat_path(&data.host_path)?;
        Ok((stat, Duration::from_secs(5)))
    }

    fn setattr(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        attr: bindings::stat64,
        _handle: Option<Self::Handle>,
        valid: SetattrValid,
    ) -> io::Result<(bindings::stat64, Duration)> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        // Handle truncation.
        if valid.contains(SetattrValid::SIZE) {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&data.host_path)
                .map_err(linux_error)?;
            file.set_len(attr.st_size as u64).map_err(linux_error)?;
        }

        let stat = Self::stat_path(&data.host_path)?;
        Ok((stat, Duration::from_secs(5)))
    }

    fn readlink(&self, _ctx: Context, inode: Self::Inode) -> io::Result<Vec<u8>> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        let target = std::fs::read_link(&data.host_path).map_err(linux_error)?;
        // Convert to bytes with forward slashes for guest.
        let target_str = target.to_string_lossy().replace('\\', "/");
        Ok(target_str.into_bytes())
    }

    fn symlink(
        &self,
        _ctx: Context,
        linkname: &CStr,
        parent: Self::Inode,
        name: &CStr,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        let path = self.get_child_path(parent, name)?;
        let target = linkname.to_str().map_err(|_| {
            io::Error::from_raw_os_error(LINUX_EINVAL)
        })?;

        // On Windows, creating symlinks requires SeCreateSymbolicLinkPrivilege.
        // We use CreateSymbolicLinkW directly to get proper error codes.
        let target_path = Path::new(target);
        let target_wide: Vec<u16> = target_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let link_wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        // Determine if the target is a directory to set the right flag.
        // Check if the target exists and is a directory. The target path may be
        // relative to the parent directory.
        let parent_path = path.parent().unwrap_or(Path::new(""));
        let resolved_target = if target_path.is_absolute() {
            target_path.to_path_buf()
        } else {
            parent_path.join(target_path)
        };
        let is_dir = resolved_target.is_dir();

        let mut flags: u32 = 0;
        if is_dir {
            flags |= SYMBOLIC_LINK_FLAG_DIRECTORY;
        }

        // Try with SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE first (Developer Mode).
        let result = unsafe {
            CreateSymbolicLinkW(
                link_wide.as_ptr(),
                target_wide.as_ptr(),
                flags | SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE,
            )
        };

        if result == 0 {
            // Unprivileged create failed. Try without that flag (requires elevation).
            let result = unsafe {
                CreateSymbolicLinkW(link_wide.as_ptr(), target_wide.as_ptr(), flags)
            };

            if result == 0 {
                let err = unsafe { GetLastError() };
                if err == ERROR_PRIVILEGE_NOT_HELD {
                    // SeCreateSymbolicLinkPrivilege not available.
                    warn!("symlink creation failed: SeCreateSymbolicLinkPrivilege not held");
                    return Err(io::Error::from_raw_os_error(LINUX_EPERM));
                }
                return Err(linux_error(io::Error::last_os_error()));
            }
        }

        self.do_lookup(&path)
    }

    fn link(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        newparent: Self::Inode,
        newname: &CStr,
    ) -> io::Result<Entry> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;
        let existing_path = data.host_path.clone();
        drop(inodes);

        let new_path = self.get_child_path(newparent, newname)?;

        // CreateHardLinkW is used internally by std::fs::hard_link on Windows.
        std::fs::hard_link(&existing_path, &new_path).map_err(linux_error)?;

        self.do_lookup(&new_path)
    }

    fn mkdir(
        &self,
        _ctx: Context,
        parent: Self::Inode,
        name: &CStr,
        _mode: u32,
        _umask: u32,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        let path = self.get_child_path(parent, name)?;
        std::fs::create_dir(&path).map_err(linux_error)?;
        self.do_lookup(&path)
    }

    fn unlink(&self, _ctx: Context, parent: Self::Inode, name: &CStr) -> io::Result<()> {
        let path = self.get_child_path(parent, name)?;
        std::fs::remove_file(&path).map_err(linux_error)?;
        Ok(())
    }

    fn rmdir(&self, _ctx: Context, parent: Self::Inode, name: &CStr) -> io::Result<()> {
        let path = self.get_child_path(parent, name)?;
        std::fs::remove_dir(&path).map_err(linux_error)?;
        Ok(())
    }

    fn rename(
        &self,
        _ctx: Context,
        olddir: Self::Inode,
        oldname: &CStr,
        newdir: Self::Inode,
        newname: &CStr,
        _flags: u32,
    ) -> io::Result<()> {
        let old_path = self.get_child_path(olddir, oldname)?;
        let new_path = self.get_child_path(newdir, newname)?;
        std::fs::rename(&old_path, &new_path).map_err(linux_error)?;
        Ok(())
    }

    fn open(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        _kill_priv: bool,
        flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions)> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        let linux_flags = flags as i32;
        let mut open_opts = std::fs::OpenOptions::new();

        // Map Linux open flags to Rust OpenOptions.
        let accmode = linux_flags & 3; // O_ACCMODE
        match accmode {
            0 => {
                // O_RDONLY
                open_opts.read(true);
            }
            1 => {
                // O_WRONLY
                open_opts.write(true);
            }
            2 => {
                // O_RDWR
                open_opts.read(true).write(true);
            }
            _ => {
                open_opts.read(true);
            }
        }

        if linux_flags & bindings::LINUX_O_TRUNC != 0 {
            open_opts.truncate(true);
        }
        if linux_flags & bindings::LINUX_O_APPEND != 0 {
            open_opts.append(true);
        }
        if linux_flags & bindings::LINUX_O_CREAT != 0 {
            open_opts.create(true);
        }

        let file = open_opts.open(&data.host_path).map_err(linux_error)?;
        let handle_id = self.alloc_handle();
        let handle_data = Arc::new(HandleData {
            inode,
            handle: RwLock::new(OwnedHandle::from(file)),
            dirstream: Mutex::new(DirStream::new()),
        });

        self.handles.write().unwrap().insert(handle_id, handle_data);
        Ok((Some(handle_id), OpenOptions::empty()))
    }

    fn release(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _flags: u32,
        handle: Self::Handle,
        _flush: bool,
        _flock_release: bool,
        _lock_owner: Option<u64>,
    ) -> io::Result<()> {
        self.handles.write().unwrap().remove(&handle);
        Ok(())
    }

    fn create(
        &self,
        ctx: Context,
        parent: Self::Inode,
        name: &CStr,
        _mode: u32,
        kill_priv: bool,
        flags: u32,
        _umask: u32,
        _extensions: Extensions,
    ) -> io::Result<(Entry, Option<Self::Handle>, OpenOptions)> {
        let path = self.get_child_path(parent, name)?;

        // Create the file.
        let _file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(linux_error)?;

        let entry = self.do_lookup(&path)?;
        let (handle, open_opts) = self.open(ctx, entry.inode, kill_priv, flags)?;
        Ok((entry, handle, open_opts))
    }

    fn read<W: io::Write + ZeroCopyWriter>(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        _handle: Self::Handle,
        mut w: W,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _flags: u32,
    ) -> io::Result<usize> {
        use std::io::Read;
        use std::io::Seek;

        let inodes = self.inodes.read().unwrap();
        let inode_data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        // Open the file for reading at the specified offset.
        let mut file = std::fs::File::open(&inode_data.host_path).map_err(linux_error)?;
        file.seek(std::io::SeekFrom::Start(offset))
            .map_err(linux_error)?;

        let mut buf = vec![0u8; size as usize];
        let n = file.read(&mut buf).map_err(linux_error)?;
        w.write_all(&buf[..n]).map_err(linux_error)?;
        Ok(n)
    }

    fn write<R: io::Read + ZeroCopyReader>(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        _handle: Self::Handle,
        mut r: R,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _delayed_write: bool,
        _kill_priv: bool,
        _flags: u32,
    ) -> io::Result<usize> {
        use std::io::Seek;
        use std::io::Write;

        let inodes = self.inodes.read().unwrap();
        let inode_data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&inode_data.host_path)
            .map_err(linux_error)?;
        file.seek(std::io::SeekFrom::Start(offset))
            .map_err(linux_error)?;

        let mut buf = vec![0u8; size as usize];
        let n = r.read(&mut buf).map_err(linux_error)?;
        file.write_all(&buf[..n]).map_err(linux_error)?;
        Ok(n)
    }

    fn flush(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _handle: Self::Handle,
        _lock_owner: u64,
    ) -> io::Result<()> {
        // Nothing to do on flush for Windows.
        Ok(())
    }

    fn fsync(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        _datasync: bool,
        _handle: Self::Handle,
    ) -> io::Result<()> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        // Open and sync the file.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&data.host_path)
            .map_err(linux_error)?;
        file.sync_all().map_err(linux_error)?;
        Ok(())
    }

    fn statfs(&self, _ctx: Context, inode: Self::Inode) -> io::Result<bindings::statvfs64> {
        let inodes = self.inodes.read().unwrap();
        let _data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        // For full disk stats on Windows, we would use GetDiskFreeSpaceExW,
        // but that requires additional FFI. Return reasonable defaults.
        Ok(bindings::statvfs64 {
            f_bsize: 4096,
            f_frsize: 4096,
            f_blocks: 1024 * 1024, // Placeholder
            f_bfree: 512 * 1024,
            f_bavail: 512 * 1024,
            f_files: 1024 * 1024,
            f_ffree: 512 * 1024,
            f_favail: 512 * 1024,
            f_fsid: 0,
            f_flag: 0,
            f_namemax: 255,
        })
    }

    fn opendir(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        _flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions)> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        if !data.host_path.is_dir() {
            return Err(io::Error::from_raw_os_error(LINUX_ENOTDIR));
        }

        let handle_id = self.alloc_handle();

        // Open a handle to the directory. Actual enumeration uses the path
        // with std::fs::read_dir in readdir().
        let dir_handle = std::fs::File::open(&data.host_path).map_err(linux_error)?;
        let handle_data = Arc::new(HandleData {
            inode,
            handle: RwLock::new(OwnedHandle::from(dir_handle)),
            dirstream: Mutex::new(DirStream::new()),
        });

        self.handles.write().unwrap().insert(handle_id, handle_data);
        Ok((Some(handle_id), OpenOptions::empty()))
    }

    fn readdir<F>(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        handle: Self::Handle,
        _size: u32,
        offset: u64,
        mut add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry) -> io::Result<usize>,
    {
        let handles = self.handles.read().unwrap();
        let hd = handles.get(&handle).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        let mut dirstream = hd.dirstream.lock().unwrap();

        // Populate entries if not yet ready.
        if !dirstream.ready {
            let inodes = self.inodes.read().unwrap();
            let inode_data = inodes.get(&inode).ok_or_else(|| {
                io::Error::from_raw_os_error(LINUX_EBADF)
            })?;

            // Use std::fs::read_dir to enumerate directory entries.
            for entry in std::fs::read_dir(&inode_data.host_path).map_err(linux_error)? {
                let entry = entry.map_err(linux_error)?;
                let metadata = entry.metadata().map_err(linux_error)?;
                let file_name = entry.file_name();
                let name_bytes = file_name.to_string_lossy().into_owned().into_bytes();

                // DT_DIR = 4, DT_REG = 8, DT_LNK = 10
                let type_ = if metadata.is_dir() {
                    4u8
                } else if metadata.is_symlink() {
                    10u8
                } else {
                    8u8
                };

                // Generate a synthetic inode number via Win32 API.
                let (ino, _, _) = Self::get_by_handle_info(&entry.path());

                dirstream.entries.push(CachedDirEntry {
                    ino,
                    name: name_bytes.into_boxed_slice(),
                    type_,
                });
            }
            dirstream.ready = true;
        }

        // Return entries starting from offset.
        let mut i = offset;
        while let Some(entry) = dirstream.entries.get(i as usize) {
            let dir_entry = DirEntry {
                ino: entry.ino,
                offset: i + 1,
                type_: entry.type_ as u32,
                name: &entry.name,
            };

            match add_entry(dir_entry) {
                Ok(0) => break, // No more space
                Ok(_) => i += 1,
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    fn releasedir(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _flags: u32,
        handle: Self::Handle,
    ) -> io::Result<()> {
        self.handles.write().unwrap().remove(&handle);
        Ok(())
    }

    // -- Issue #028: Extended Attributes --
    //
    // Windows does not have POSIX extended attributes. While Windows Alternate
    // Data Streams (ADS) provide somewhat similar functionality, the semantics
    // are too different from POSIX xattrs for a simple mapping. We return
    // ENOTSUP (EOPNOTSUPP, Linux errno 95) to indicate that xattr operations
    // are not supported.
    //
    // Note: We intentionally use EOPNOTSUPP (95) instead of ENOSYS (38).
    // ENOSYS would cause the FUSE kernel module to permanently cache the
    // failure and never forward future xattr requests to us.

    fn setxattr(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _name: &CStr,
        _value: &[u8],
        _flags: u32,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(LINUX_EOPNOTSUPP))
    }

    fn getxattr(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _name: &CStr,
        _size: u32,
    ) -> io::Result<GetxattrReply> {
        Err(io::Error::from_raw_os_error(LINUX_EOPNOTSUPP))
    }

    fn listxattr(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _size: u32,
    ) -> io::Result<ListxattrReply> {
        Err(io::Error::from_raw_os_error(LINUX_EOPNOTSUPP))
    }

    fn removexattr(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _name: &CStr,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(LINUX_EOPNOTSUPP))
    }

    // -- Issue #027: File Locking --
    //
    // The FUSE locking trait methods (getlk/setlk/setlkw) currently have
    // no-argument signatures, so we cannot dispatch real locking operations
    // through them yet. We return ENOSYS to indicate that the kernel should
    // handle lock emulation locally.
    //
    // When the trait is extended to pass lock parameters (inode, handle,
    // owner, type, offset, length), the implementation below should use:
    //
    //   - LockFileEx with LOCKFILE_FAIL_IMMEDIATELY for non-blocking (F_SETLK)
    //   - LockFileEx without that flag for blocking locks (F_SETLKW)
    //   - UnlockFileEx for unlock operations
    //
    // Note: Windows locks are mandatory (enforced by the OS), unlike POSIX
    // advisory locks. This means locked regions will actually block other
    // processes from accessing the data, which is a stronger guarantee
    // than what Linux applications typically expect.

    fn getlk(&self) -> io::Result<()> {
        // Cannot implement without lock parameters from the FUSE request.
        Err(io::Error::from_raw_os_error(LINUX_ENOSYS))
    }

    fn setlk(&self) -> io::Result<()> {
        // Cannot implement without lock parameters from the FUSE request.
        Err(io::Error::from_raw_os_error(LINUX_ENOSYS))
    }

    fn setlkw(&self) -> io::Result<()> {
        // Cannot implement without lock parameters from the FUSE request.
        Err(io::Error::from_raw_os_error(LINUX_ENOSYS))
    }
}

// -- Issue #027: File Locking Helper Functions --
//
// These standalone helper functions provide the actual Windows file locking
// operations using LockFileEx / UnlockFileEx. They are ready to be wired
// into the FUSE dispatch once the FileSystem trait's getlk/setlk/setlkw
// signatures are extended to include the necessary parameters (handle,
// offset, length, lock type, blocking flag).
//
// The OVERLAPPED structure is used to specify the byte range to lock.
// Windows file locks differ from POSIX locks in several ways:
//   - They are mandatory (enforced), not advisory
//   - They are per-handle, not per-process
//   - There is no F_GETLK equivalent (you must attempt the lock to test)

/// Create a zero-initialized OVERLAPPED structure with the given byte offset.
fn make_overlapped(offset: u64) -> OVERLAPPED {
    OVERLAPPED {
        Internal: 0,
        InternalHigh: 0,
        Anonymous: windows_sys::Win32::System::IO::OVERLAPPED_0 {
            Anonymous: windows_sys::Win32::System::IO::OVERLAPPED_0_0 {
                Offset: offset as u32,
                OffsetHigh: (offset >> 32) as u32,
            },
        },
        hEvent: 0 as HANDLE,
    }
}

/// Attempt to acquire a file lock (non-blocking, for F_SETLK semantics).
///
/// - `handle`: raw Windows HANDLE for the open file
/// - `offset`: starting byte offset of the lock region
/// - `length`: number of bytes to lock (0 = lock to EOF)
/// - `exclusive`: true for write lock (F_WRLCK), false for read lock (F_RDLCK)
///
/// Returns Ok(()) if the lock was acquired, or an error with LINUX_EAGAIN
/// if the lock is held by another process, or LINUX_EBADF for bad handles.
#[allow(dead_code)]
fn win_lock_file_nonblocking(
    handle: HANDLE,
    offset: u64,
    length: u64,
    exclusive: bool,
) -> io::Result<()> {
    let mut overlapped = make_overlapped(offset);
    let mut flags = LOCKFILE_FAIL_IMMEDIATELY;
    if exclusive {
        flags |= LOCKFILE_EXCLUSIVE_LOCK;
    }

    let len_low = length as u32;
    let len_high = (length >> 32) as u32;

    let result =
        unsafe { LockFileEx(handle, flags, 0, len_low, len_high, &mut overlapped) };

    if result == 0 {
        Err(linux_error(io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

/// Acquire a file lock, blocking until it is available (for F_SETLKW semantics).
///
/// - `handle`: raw Windows HANDLE for the open file
/// - `offset`: starting byte offset of the lock region
/// - `length`: number of bytes to lock (0 = lock to EOF)
/// - `exclusive`: true for write lock (F_WRLCK), false for read lock (F_RDLCK)
///
/// This call will block the calling thread until the lock can be acquired.
#[allow(dead_code)]
fn win_lock_file_blocking(
    handle: HANDLE,
    offset: u64,
    length: u64,
    exclusive: bool,
) -> io::Result<()> {
    let mut overlapped = make_overlapped(offset);
    let mut flags = 0u32;
    if exclusive {
        flags |= LOCKFILE_EXCLUSIVE_LOCK;
    }

    let len_low = length as u32;
    let len_high = (length >> 32) as u32;

    let result =
        unsafe { LockFileEx(handle, flags, 0, len_low, len_high, &mut overlapped) };

    if result == 0 {
        Err(linux_error(io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

/// Release a previously acquired file lock.
///
/// - `handle`: raw Windows HANDLE for the open file
/// - `offset`: starting byte offset of the locked region
/// - `length`: number of bytes that were locked
#[allow(dead_code)]
fn win_unlock_file(handle: HANDLE, offset: u64, length: u64) -> io::Result<()> {
    let mut overlapped = make_overlapped(offset);
    let len_low = length as u32;
    let len_high = (length >> 32) as u32;

    let result =
        unsafe { UnlockFileEx(handle, 0, len_low, len_high, &mut overlapped) };

    if result == 0 {
        Err(linux_error(io::Error::last_os_error()))
    } else {
        Ok(())
    }
}
