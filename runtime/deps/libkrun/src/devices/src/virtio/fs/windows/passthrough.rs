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
//! - File locking: currently not wired by FileSystem trait signatures
//! - Extended attributes: persisted in NTFS ADS side streams

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::io;
use std::io::{Read, Seek, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::fs::OpenOptionsExt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use log::{info, warn};

/// Returns true when NANOSB_FUSE_TRACE is set to a non-empty value other than "0"/"false".
/// Gates verbose passthrough tracing used to diagnose readdir/lookup divergence.
fn fuse_trace_enabled() -> bool {
    static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        match std::env::var("NANOSB_FUSE_TRACE") {
            Ok(v) => {
                let v = v.trim().to_ascii_lowercase();
                !v.is_empty() && v != "0" && v != "false" && v != "no" && v != "off"
            }
            Err(_) => false,
        }
    })
}

/// Hex-encode a byte slice for trace logs.
fn hex_bytes(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        use std::fmt::Write as _;
        let _ = write!(&mut s, "{:02x}", x);
    }
    s
}

/// Hex-encode an OsStr as its UTF-16 wide form (Windows-native).
fn hex_wide(os: &std::ffi::OsStr) -> String {
    let wide: Vec<u16> = os.encode_wide().collect();
    let mut s = String::with_capacity(wide.len() * 4 + 2);
    for w in wide {
        use std::fmt::Write as _;
        let _ = write!(&mut s, "{:04x}", w);
    }
    s
}

macro_rules! ftrace {
    ($($arg:tt)*) => {
        if fuse_trace_enabled() {
            info!(target: "fuse_trace", $($arg)*);
        }
    };
}

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
    CreateFileW, CreateSymbolicLinkW, GetDiskFreeSpaceExW, GetFileInformationByHandle,
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE,
    SYMBOLIC_LINK_FLAG_DIRECTORY,
};

type Inode = u64;
type Handle = u64;

const ROOT_ID: Inode = 1;

// Linux errno constants used directly in FUSE protocol responses.
const LINUX_EPERM: i32 = 1;
const LINUX_ENOENT: i32 = 2;
const LINUX_ENXIO: i32 = 6;
const LINUX_E2BIG: i32 = 7;
const LINUX_EBADF: i32 = 9;
const LINUX_EACCES: i32 = 13;
const LINUX_EEXIST: i32 = 17;
const LINUX_ENOTDIR: i32 = 20;
const LINUX_EINVAL: i32 = 22;
const LINUX_ENOSYS: i32 = 38;
const LINUX_EOPNOTSUPP: i32 = 95;
const LINUX_F_OK: i32 = 0;
const LINUX_X_OK: i32 = 1;
const LINUX_W_OK: i32 = 2;
const LINUX_R_OK: i32 = 4;
const LINUX_SEEK_SET: u32 = 0;
const LINUX_SEEK_CUR: u32 = 1;
const LINUX_SEEK_END: u32 = 2;
const LINUX_SEEK_DATA: u32 = 3;
const LINUX_SEEK_HOLE: u32 = 4;
const LINUX_FALLOC_FL_KEEP_SIZE: u32 = 0x01;
const LINUX_FALLOC_FL_PUNCH_HOLE: u32 = 0x02;
const LINUX_FALLOC_FL_ZERO_RANGE: u32 = 0x10;
const LINUX_XATTR_SIZE_MAX: usize = 65536;

// FUSE file types (matching Linux S_IFMT values).
const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const ATTR_STREAM_NAME: &str = "nanosb.attrs";
const XATTR_INDEX_STREAM_NAME: &str = "nanosb.xattrs";
const LINUX_ENODATA: i32 = 61;
const LINUX_ERANGE: i32 = 34;

/// Alternate key for looking up inodes by (file_index, volume_serial).
#[derive(Clone, Copy, PartialOrd, Ord, PartialEq, Eq)]
struct InodeAltKey {
    file_index: u64,
    volume_serial: u32,
}

/// Data associated with an inode.
struct InodeData {
    inode: Inode,
    /// Full host path for this inode. Wrapped in RwLock so it can be updated
    /// when the underlying host file is renamed (e.g., atomic write via
    /// tmp+rename pattern used by Node.js `fs.writeFile`). Without this update,
    /// subsequent open()/read()/write() would use the stale tmp path and fail
    /// with ENOENT even though FUSE lookup succeeds via stat_path on the new
    /// name.
    host_path: RwLock<PathBuf>,
    /// Unique file identifier from the filesystem.
    file_index: u64,
    /// Volume serial number.
    volume_serial: u32,
    /// Reference count (FUSE lookup count).
    refcount: AtomicU64,
}

impl InodeData {
    /// Clone the current host path (cheap snapshot for read-only ops).
    fn path_buf(&self) -> PathBuf {
        self.host_path.read().unwrap().clone()
    }
    /// Overwrite the host path (used by rename()).
    fn set_path(&self, new_path: PathBuf) {
        *self.host_path.write().unwrap() = new_path;
    }
}

/// Cached directory entry for readdir.
struct CachedDirEntry {
    ino: u64,
    name: Box<[u8]>,
    type_: u8,
    host_path: PathBuf,
}

/// Returns true when NANOSB_FUSE_VERBOSE_PROBE is set to a non-empty value
/// other than "0"/"false".
fn fuse_verbose_probe_enabled() -> bool {
    static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        match std::env::var("NANOSB_FUSE_VERBOSE_PROBE") {
            Ok(v) => {
                let v = v.trim().to_ascii_lowercase();
                !v.is_empty() && v != "0" && v != "false" && v != "no" && v != "off"
            }
            Err(_) => false,
        }
    })
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
    handle: RwLock<File>,
    dirstream: Mutex<DirStream>,
}

#[derive(Clone, Copy, Default)]
struct AttrOverride {
    uid: Option<u32>,
    gid: Option<u32>,
    mode: Option<u32>,
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
    attr_overrides: RwLock<BTreeMap<Inode, AttrOverride>>,
}

impl PassthroughFs {
    pub fn new(cfg: Config) -> io::Result<Self> {
        Ok(PassthroughFs {
            cfg,
            inodes: RwLock::new(MultikeyBTreeMap::new()),
            handles: RwLock::new(BTreeMap::new()),
            next_inode: AtomicU64::new(ROOT_ID + 1),
            next_handle: AtomicU64::new(1),
            attr_overrides: RwLock::new(BTreeMap::new()),
        })
    }

    fn attr_stream_path(path: &Path) -> PathBuf {
        let mut stream_path = path.as_os_str().to_os_string();
        stream_path.push(":");
        stream_path.push(ATTR_STREAM_NAME);
        PathBuf::from(stream_path)
    }

    fn named_stream_path(path: &Path, stream_name: &str) -> PathBuf {
        let mut stream_path = path.as_os_str().to_os_string();
        stream_path.push(":");
        stream_path.push(stream_name);
        PathBuf::from(stream_path)
    }

    fn xattr_name_hex(name: &[u8]) -> String {
        let mut out = String::with_capacity(name.len() * 2);
        for b in name {
            use std::fmt::Write as _;
            let _ = write!(&mut out, "{:02x}", b);
        }
        out
    }

    fn xattr_name_from_hex(hex: &str) -> Option<Vec<u8>> {
        if hex.len() % 2 != 0 {
            return None;
        }
        let mut out = Vec::with_capacity(hex.len() / 2);
        let bytes = hex.as_bytes();
        for i in (0..bytes.len()).step_by(2) {
            let hi = (bytes[i] as char).to_digit(16)?;
            let lo = (bytes[i + 1] as char).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
        }
        Some(out)
    }

    fn xattr_stream_name(name: &[u8]) -> String {
        format!("nanosb.xattr.{}", Self::xattr_name_hex(name))
    }

    fn xattr_stream_path(path: &Path, name: &[u8]) -> PathBuf {
        Self::named_stream_path(path, &Self::xattr_stream_name(name))
    }

    fn read_xattr_index(path: &Path) -> io::Result<Vec<Vec<u8>>> {
        let index_path = Self::named_stream_path(path, XATTR_INDEX_STREAM_NAME);
        match std::fs::read_to_string(index_path) {
            Ok(raw) => Ok(raw
                .lines()
                .filter_map(Self::xattr_name_from_hex)
                .collect::<Vec<_>>()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(linux_error(e)),
        }
    }

    fn write_xattr_index(path: &Path, names: &[Vec<u8>]) -> io::Result<()> {
        let index_path = Self::named_stream_path(path, XATTR_INDEX_STREAM_NAME);
        let payload = names
            .iter()
            .map(|n| Self::xattr_name_hex(n))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(index_path, payload.as_bytes()).map_err(linux_error)
    }

    /// Parse the per-inode ADS payload into an AttrOverride.
    ///
    /// Format: `uid:gid:mode` where each component may be `-` to denote
    /// "not explicitly set". Historical payloads always emit three integers
    /// (no `-`), which is fine as long as the writer only filled in fields
    /// that were actually overridden. See `write_attr_override_to_ads` for
    /// details on the historical bug that wrote a default mode `0o644` on
    /// chown-only updates and how we defensively recover from it.
    fn parse_attr_override(raw: &str) -> Option<AttrOverride> {
        let mut parts = raw.trim().split(':');
        let uid = match parts.next()? {
            "-" | "" => None,
            s => Some(s.parse::<u32>().ok()?),
        };
        let gid = match parts.next()? {
            "-" | "" => None,
            s => Some(s.parse::<u32>().ok()?),
        };
        let mode = match parts.next() {
            Some("-") | Some("") | None => None,
            Some(s) => Some(s.parse::<u32>().ok()? & 0o7777),
        };
        if uid.is_none() && gid.is_none() && mode.is_none() {
            return None;
        }
        Some(AttrOverride { uid, gid, mode })
    }

    /// Persist an AttrOverride to NTFS ADS for the given path.
    ///
    /// Only fields that are `Some(_)` are written as integers; unset fields
    /// are emitted as `-`. Previous versions of this function emitted a
    /// default `0o644` whenever `mode` was None, which corrupted directory
    /// permissions on plain `chown`: the FUSE server would later read back
    /// the override and clobber the natural directory mode (`0o755`) with
    /// `0o644`, breaking traversal for non-root users.
    fn write_attr_override_to_ads(path: &Path, attrs: AttrOverride) -> io::Result<()> {
        let stream_path = Self::attr_stream_path(path);
        fn field(v: Option<u32>) -> String {
            match v {
                Some(x) => x.to_string(),
                None => "-".to_string(),
            }
        }
        let mode_field = match attrs.mode {
            Some(m) => (m & 0o7777).to_string(),
            None => "-".to_string(),
        };
        let payload = format!("{}:{}:{}", field(attrs.uid), field(attrs.gid), mode_field);
        std::fs::write(stream_path, payload.as_bytes()).map_err(linux_error)
    }

    fn read_attr_override_from_ads(path: &Path) -> io::Result<Option<AttrOverride>> {
        let stream_path = Self::attr_stream_path(path);
        match std::fs::read_to_string(stream_path) {
            Ok(raw) => Ok(Self::parse_attr_override(&raw)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(linux_error(e)),
        }
    }

    fn get_or_load_attr_override(&self, inode: Inode, path: &Path) -> Option<AttrOverride> {
        if let Some(attrs) = self.attr_overrides.read().unwrap().get(&inode).copied() {
            return Some(attrs);
        }

        match Self::read_attr_override_from_ads(path) {
            Ok(Some(attrs)) => {
                self.attr_overrides.write().unwrap().insert(inode, attrs);
                Some(attrs)
            }
            Ok(None) => None,
            Err(e) => {
                warn!(
                    "failed to read attr override for {}: {}",
                    path.display(),
                    e
                );
                None
            }
        }
    }

    fn get_parent_attr_override(&self, parent: Inode) -> Option<AttrOverride> {
        let parent_path = self
            .inodes
            .read()
            .unwrap()
            .get(&parent)
            .map(|d| d.path_buf())?;
        self.get_or_load_attr_override(parent, &parent_path)
    }

    fn apply_path_attr_overrides(
        &self,
        inode: Inode,
        path: &Path,
        parent_attrs: Option<AttrOverride>,
        mut stat: bindings::stat64,
    ) -> bindings::stat64 {
        if let Some(attrs) = self.get_or_load_attr_override(inode, path) {
            if let Some(uid) = attrs.uid {
                stat.st_uid = uid;
            }
            if let Some(gid) = attrs.gid {
                stat.st_gid = gid;
            }
            if let Some(mode) = attrs.mode {
                // Recovery for legacy ADS corruption: older builds wrote
                // mode=0o644 as a default whenever chown ran without an
                // explicit chmod. For directories that produces 0o40644
                // (no x-bit), breaking traversal for non-root users on
                // `/workspace` after the init-time chown -R. If we see
                // exactly that signature on a directory, drop the override
                // and trust the natural mode from the host filesystem.
                let is_dir = (stat.st_mode & S_IFMT) == S_IFDIR;
                let legacy_dir_bug = is_dir && (mode & 0o7777) == 0o644;
                if !legacy_dir_bug {
                    stat.st_mode = (stat.st_mode & S_IFMT) | (mode & 0o7777);
                }
            }
            return stat;
        }

        if let Some(parent_attrs) = parent_attrs {
            if let Some(uid) = parent_attrs.uid {
                stat.st_uid = uid;
            }
            if let Some(gid) = parent_attrs.gid {
                stat.st_gid = gid;
            }
        }

        stat
    }

    fn set_attr_override(&self, inode: Inode, path: &Path, attr: bindings::stat64, valid: SetattrValid) {
        let mut override_map = self.attr_overrides.write().unwrap();
        let entry = override_map.entry(inode).or_default();

        if valid.contains(SetattrValid::UID) {
            entry.uid = Some(attr.st_uid);
        }
        if valid.contains(SetattrValid::GID) {
            entry.gid = Some(attr.st_gid);
        }
        if valid.contains(SetattrValid::MODE) {
            entry.mode = Some(attr.st_mode);
        }

        if let Err(e) = Self::write_attr_override_to_ads(path, *entry) {
            warn!(
                "failed to persist attr override for {}: {}",
                path.display(),
                e
            );
        }
    }

    fn set_owner_mode_override(&self, inode: Inode, path: &Path, uid: u32, gid: u32, mode: u32) {
        let attrs = AttrOverride {
            uid: Some(uid),
            gid: Some(gid),
            mode: Some(mode & 0o7777),
        };
        self.attr_overrides.write().unwrap().insert(inode, attrs);
        if let Err(e) = Self::write_attr_override_to_ads(path, attrs) {
            warn!(
                "failed to persist owner/mode override for {}: {}",
                path.display(),
                e
            );
        }
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
        Ok(parent_data.path_buf().join(child_name))
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
            // Default to root-owned (0/0). The WSL2 guest kernel's
            // acl_permission_check rejects ownership-by-uid-1000 even when the
            // calling process has fsuid==1000 (verified by /proc/self/status
            // diagnostic). Falling back to "other" mode bits via uid=0/gid=0
            // and mode 0o755 grants developer the r-x it needs. Specific
            // overrides for files with persisted ADS attrs still apply via
            // apply_path_attr_overrides() below.
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

    fn check_access(st: &bindings::stat64, ctx: Context, mask: u32) -> bool {
        let mode = mask as i32 & (LINUX_R_OK | LINUX_W_OK | LINUX_X_OK);
        if mode == LINUX_F_OK {
            return true;
        }

        if (mode & LINUX_R_OK) != 0
            && ctx.uid != 0
            && (st.st_uid != ctx.uid || st.st_mode & 0o400 == 0)
            && (st.st_gid != ctx.gid || st.st_mode & 0o040 == 0)
            && st.st_mode & 0o004 == 0
        {
            return false;
        }

        if (mode & LINUX_W_OK) != 0
            && ctx.uid != 0
            && (st.st_uid != ctx.uid || st.st_mode & 0o200 == 0)
            && (st.st_gid != ctx.gid || st.st_mode & 0o020 == 0)
            && st.st_mode & 0o002 == 0
        {
            return false;
        }

        if (mode & LINUX_X_OK) != 0
            && (ctx.uid != 0 || st.st_mode & 0o111 == 0)
            && (st.st_uid != ctx.uid || st.st_mode & 0o100 == 0)
            && (st.st_gid != ctx.gid || st.st_mode & 0o010 == 0)
            && st.st_mode & 0o001 == 0
        {
            return false;
        }

        true
    }

    fn apply_attr_override_to_stat(mut stat: bindings::stat64, attrs: AttrOverride) -> bindings::stat64 {
        if let Some(uid) = attrs.uid {
            stat.st_uid = uid;
        }
        if let Some(gid) = attrs.gid {
            stat.st_gid = gid;
        }
        if let Some(mode) = attrs.mode {
            let is_dir = (stat.st_mode & S_IFMT) == S_IFDIR;
            let legacy_dir_bug = is_dir && (mode & 0o7777) == 0o644;
            if !legacy_dir_bug {
                stat.st_mode = (stat.st_mode & S_IFMT) | (mode & 0o7777);
            }
        }
        stat
    }

    fn stat_path_with_effective_attrs(
        &self,
        path: &Path,
        parent: Option<Inode>,
    ) -> io::Result<bindings::stat64> {
        let mut stat = Self::stat_path(path)?;

        if let Ok(Some(attrs)) = Self::read_attr_override_from_ads(path) {
            return Ok(Self::apply_attr_override_to_stat(stat, attrs));
        }

        if let Some(parent_attrs) = parent.and_then(|p| self.get_parent_attr_override(p)) {
            if let Some(uid) = parent_attrs.uid {
                stat.st_uid = uid;
            }
            if let Some(gid) = parent_attrs.gid {
                stat.st_gid = gid;
            }
        }

        Ok(stat)
    }

    fn inode_path_and_stat(&self, inode: Inode) -> io::Result<(PathBuf, bindings::stat64)> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes
            .get(&inode)
            .ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?;
        let path = data.path_buf();
        drop(inodes);

        let stat = Self::stat_path(&path)?;
        let stat = self.apply_path_attr_overrides(inode, &path, None, stat);
        Ok((path, stat))
    }

    fn require_parent_delete_perms(&self, ctx: Context, parent: Inode) -> io::Result<bindings::stat64> {
        let (_, parent_stat) = self.inode_path_and_stat(parent)?;
        if Self::check_access(&parent_stat, ctx, (LINUX_W_OK | LINUX_X_OK) as u32) {
            Ok(parent_stat)
        } else {
            Err(io::Error::from_raw_os_error(LINUX_EACCES))
        }
    }

    fn enforce_sticky_delete(
        parent_stat: &bindings::stat64,
        child_stat: &bindings::stat64,
        ctx: Context,
    ) -> io::Result<()> {
        let sticky = parent_stat.st_mode & 0o1000 != 0;
        if sticky
            && ctx.uid != 0
            && ctx.uid != parent_stat.st_uid
            && ctx.uid != child_stat.st_uid
        {
            return Err(io::Error::from_raw_os_error(LINUX_EPERM));
        }
        Ok(())
    }

    fn write_zero_range(file: &mut File, offset: u64, length: u64) -> io::Result<()> {
        if length == 0 {
            return Ok(());
        }

        let mut remaining = length;
        let mut pos = offset;
        let zero = vec![0u8; 1024 * 1024];

        while remaining > 0 {
            let chunk = std::cmp::min(remaining, zero.len() as u64) as usize;
            file.seek(std::io::SeekFrom::Start(pos)).map_err(linux_error)?;
            file.write_all(&zero[..chunk]).map_err(linux_error)?;
            pos = pos.saturating_add(chunk as u64);
            remaining -= chunk as u64;
        }

        Ok(())
    }

    fn hydrate_dirstream_entries(&self, inode: Inode, dirstream: &mut DirStream) -> io::Result<()> {
        if dirstream.ready {
            return Ok(());
        }

        let inodes = self.inodes.read().unwrap();
        let inode_data = inodes
            .get(&inode)
            .ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?;
        let host_path = inode_data.path_buf();
        drop(inodes);
        ftrace!(
            "readdir: hydrate inode={} host_path={:?} host_path_wide={}",
            inode,
            host_path,
            hex_wide(host_path.as_os_str())
        );

        for entry in std::fs::read_dir(&host_path).map_err(linux_error)? {
            let entry = entry.map_err(linux_error)?;
            let metadata = entry.metadata().map_err(linux_error)?;
            let file_name = entry.file_name();
            let name_bytes = match file_name.to_str() {
                Some(s) => s.as_bytes().to_vec(),
                None => {
                    let wide: Vec<u16> = file_name.encode_wide().collect();
                    String::from_utf16_lossy(&wide).into_bytes()
                }
            };
            let entry_path = entry.path();
            ftrace!(
                "readdir: entry path={:?} name_wide={} name_lossy_hex={}",
                entry_path,
                hex_wide(file_name.as_os_str()),
                hex_bytes(&name_bytes)
            );

            let type_ = if metadata.is_dir() {
                4u8
            } else if metadata.is_symlink() {
                10u8
            } else {
                8u8
            };

            let (ino, _, _) = Self::get_by_handle_info(&entry_path);

            dirstream.entries.push(CachedDirEntry {
                ino,
                name: name_bytes.into_boxed_slice(),
                type_,
                host_path: entry_path,
            });
        }
        dirstream.ready = true;
        Ok(())
    }

    /// Lookup or create an inode for the given path.
    fn do_lookup(&self, path: &Path, parent: Option<Inode>) -> io::Result<Entry> {
        let stat = match Self::stat_path(path) {
            Ok(s) => s,
            Err(e) => {
                ftrace!(
                    "do_lookup: stat_path FAILED parent={:?} path={:?} path_wide={} errno={:?}",
                    parent,
                    path,
                    hex_wide(path.as_os_str()),
                    e.raw_os_error()
                );
                return Err(e);
            }
        };
        ftrace!(
            "do_lookup: stat_path OK parent={:?} path_wide={} ino={} dev={} mode=0{:o} size={}",
            parent,
            hex_wide(path.as_os_str()),
            stat.st_ino,
            stat.st_dev,
            stat.st_mode,
            stat.st_size
        );
        let parent_attrs = parent.and_then(|p| self.get_parent_attr_override(p));
        let alt_key = InodeAltKey {
            file_index: stat.st_ino,
            volume_serial: stat.st_dev as u32,
        };

        let mut inodes = self.inodes.write().unwrap();

        // Check if we already have this inode.
        if let Some(data) = inodes.get_alt(&alt_key) {
            ftrace!(
                "do_lookup: cache HIT alt_key=({}, {}) inode={} path_wide={}",
                alt_key.file_index,
                alt_key.volume_serial,
                data.inode,
                hex_wide(path.as_os_str())
            );
            data.refcount.fetch_add(1, Ordering::Relaxed);
            let stat = self.apply_path_attr_overrides(data.inode, path, parent_attrs, stat);
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
        ftrace!(
            "do_lookup: cache MISS alt_key=({}, {}) new_inode={} path_wide={}",
            alt_key.file_index,
            alt_key.volume_serial,
            inode,
            hex_wide(path.as_os_str())
        );
        let data = Arc::new(InodeData {
            inode,
            host_path: RwLock::new(path.to_path_buf()),
            file_index: stat.st_ino,
            volume_serial: stat.st_dev as u32,
            refcount: AtomicU64::new(1),
        });

        inodes.insert(inode, alt_key, data);

        Ok(Entry {
            inode,
            generation: 0,
            attr: self.apply_path_attr_overrides(inode, path, parent_attrs, stat),
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
            host_path: RwLock::new(root_path),
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
        if capable.contains(FsOptions::DO_READDIRPLUS) {
            opts |= FsOptions::DO_READDIRPLUS;
        }
        if capable.contains(FsOptions::READDIRPLUS_AUTO) {
            opts |= FsOptions::READDIRPLUS_AUTO;
        }

        Ok(opts)
    }

    fn destroy(&self) {}

    fn lookup(&self, _ctx: Context, parent: Self::Inode, name: &CStr) -> io::Result<Entry> {
        let name_bytes = name.to_bytes();
        ftrace!(
            "lookup: parent={} name_utf8={:?} name_hex={}",
            parent,
            String::from_utf8_lossy(name_bytes),
            hex_bytes(name_bytes)
        );
        let path = match self.get_child_path(parent, name) {
            Ok(p) => p,
            Err(e) => {
                ftrace!(
                    "lookup: get_child_path failed parent={} name_hex={} errno={:?}",
                    parent,
                    hex_bytes(name_bytes),
                    e.raw_os_error()
                );
                return Err(e);
            }
        };
        let result = self.do_lookup(&path, Some(parent));
        if let Err(ref e) = result {
            ftrace!(
                "lookup: ERROR parent={} name_hex={} path={:?} path_wide={} errno={:?}",
                parent,
                hex_bytes(name_bytes),
                path,
                hex_wide(path.as_os_str()),
                e.raw_os_error()
            );
            // Side-by-side dump: list parent and print every readdir name in hex.
            if fuse_verbose_probe_enabled() {
                if let Some(parent_path) = self
                    .inodes
                    .read()
                    .unwrap()
                    .get(&parent)
                    .map(|d| d.path_buf())
                {
                    match std::fs::read_dir(&parent_path) {
                        Ok(rd) => {
                            let mut shown = 0usize;
                            for entry in rd.flatten() {
                                let n = entry.file_name();
                                ftrace!(
                                    "lookup: readdir-probe parent={:?} name_wide={} name_lossy_hex={}",
                                    parent_path,
                                    hex_wide(n.as_os_str()),
                                    hex_bytes(n.to_string_lossy().as_bytes())
                                );
                                shown += 1;
                                if shown >= 64 {
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            ftrace!(
                                "lookup: readdir-probe FAILED parent={:?} errno={:?}",
                                parent_path,
                                e.raw_os_error()
                            );
                        }
                    }
                }
            }
        } else {
            ftrace!(
                "lookup: OK parent={} name_hex={} path_wide={}",
                parent,
                hex_bytes(name_bytes),
                hex_wide(path.as_os_str())
            );
        }
        result
    }

    fn forget(&self, _ctx: Context, inode: Self::Inode, count: u64) {
        let mut inodes = self.inodes.write().unwrap();
        if let Some(data) = inodes.get(&inode) {
            let old = data.refcount.fetch_sub(count, Ordering::Relaxed);
            if old <= count {
                inodes.remove(&inode);
                self.attr_overrides.write().unwrap().remove(&inode);
            }
        }
    }

    fn batch_forget(&self, _ctx: Context, requests: Vec<(Self::Inode, u64)>) {
        let mut inodes = self.inodes.write().unwrap();
        let mut attr_overrides = self.attr_overrides.write().unwrap();

        for (inode, count) in requests {
            if let Some(data) = inodes.get(&inode) {
                let old = data.refcount.fetch_sub(count, Ordering::Relaxed);
                if old <= count {
                    inodes.remove(&inode);
                    attr_overrides.remove(&inode);
                }
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
        let host_path = data.path_buf();
        let stat = Self::stat_path(&host_path)?;
        let stat = self.apply_path_attr_overrides(inode, &host_path, None, stat);
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

        let host_path = data.path_buf();

        // Handle truncation.
        if valid.contains(SetattrValid::SIZE) {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&host_path)
                .map_err(linux_error)?;
            file.set_len(attr.st_size as u64).map_err(linux_error)?;
        }

        if valid.intersects(SetattrValid::UID | SetattrValid::GID | SetattrValid::MODE) {
            self.set_attr_override(inode, &host_path, attr, valid);
        }

        let stat = Self::stat_path(&host_path)?;
        let stat = self.apply_path_attr_overrides(inode, &host_path, None, stat);
        Ok((stat, Duration::from_secs(5)))
    }

    fn readlink(&self, _ctx: Context, inode: Self::Inode) -> io::Result<Vec<u8>> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        let target = std::fs::read_link(&data.path_buf()).map_err(linux_error)?;
        // Convert to bytes with forward slashes for guest.
        let target_str = target.to_string_lossy().replace('\\', "/");
        Ok(target_str.into_bytes())
    }

    fn symlink(
        &self,
        ctx: Context,
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

        let entry = self.do_lookup(&path, Some(parent))?;
        self.set_owner_mode_override(entry.inode, &path, ctx.uid, ctx.gid, 0o777);
        Ok(entry)
    }

    fn link(
        &self,
        ctx: Context,
        inode: Self::Inode,
        newparent: Self::Inode,
        newname: &CStr,
    ) -> io::Result<Entry> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;
        let existing_path = data.path_buf();
        drop(inodes);

        let new_path = self.get_child_path(newparent, newname)?;

        // CreateHardLinkW is used internally by std::fs::hard_link on Windows.
        std::fs::hard_link(&existing_path, &new_path).map_err(linux_error)?;

        let entry = self.do_lookup(&new_path, Some(newparent))?;
        let source_stat = Self::stat_path(&existing_path)?;
        self.set_owner_mode_override(
            entry.inode,
            &new_path,
            if ctx.uid == 0 { source_stat.st_uid } else { ctx.uid },
            if ctx.gid == 0 { source_stat.st_gid } else { ctx.gid },
            source_stat.st_mode & 0o7777,
        );
        Ok(entry)
    }

    fn mknod(
        &self,
        ctx: Context,
        parent: Self::Inode,
        name: &CStr,
        mode: u32,
        _rdev: u32,
        umask: u32,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        let path = self.get_child_path(parent, name)?;
        let file_type = mode & S_IFMT;
        if file_type != 0 && file_type != S_IFREG {
            return Err(io::Error::from_raw_os_error(LINUX_EOPNOTSUPP));
        }

        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(linux_error)?;

        let entry = self.do_lookup(&path, Some(parent))?;
        self.set_owner_mode_override(entry.inode, &path, ctx.uid, ctx.gid, mode & !umask & 0o7777);
        Ok(entry)
    }

    fn mkdir(
        &self,
        ctx: Context,
        parent: Self::Inode,
        name: &CStr,
        mode: u32,
        umask: u32,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        let path = self.get_child_path(parent, name)?;
        std::fs::create_dir(&path).map_err(linux_error)?;
        let entry = self.do_lookup(&path, Some(parent))?;
        self.set_owner_mode_override(entry.inode, &path, ctx.uid, ctx.gid, mode & !umask & 0o7777);
        Ok(entry)
    }

    fn unlink(&self, ctx: Context, parent: Self::Inode, name: &CStr) -> io::Result<()> {
        let parent_stat = self.require_parent_delete_perms(ctx, parent)?;
        let path = self.get_child_path(parent, name)?;
        let child_stat = self.stat_path_with_effective_attrs(&path, Some(parent))?;
        Self::enforce_sticky_delete(&parent_stat, &child_stat, ctx)?;
        std::fs::remove_file(&path).map_err(linux_error)?;
        Ok(())
    }

    fn rmdir(&self, ctx: Context, parent: Self::Inode, name: &CStr) -> io::Result<()> {
        let parent_stat = self.require_parent_delete_perms(ctx, parent)?;
        let path = self.get_child_path(parent, name)?;
        let child_stat = self.stat_path_with_effective_attrs(&path, Some(parent))?;
        Self::enforce_sticky_delete(&parent_stat, &child_stat, ctx)?;
        std::fs::remove_dir(&path).map_err(linux_error)?;
        Ok(())
    }

    fn rename(
        &self,
        ctx: Context,
        olddir: Self::Inode,
        oldname: &CStr,
        newdir: Self::Inode,
        newname: &CStr,
        _flags: u32,
    ) -> io::Result<()> {
        let old_path = self.get_child_path(olddir, oldname)?;
        let new_path = self.get_child_path(newdir, newname)?;
        let old_parent_stat = self.require_parent_delete_perms(ctx, olddir)?;
        let new_parent_stat = self.require_parent_delete_perms(ctx, newdir)?;
        let old_child_stat = self.stat_path_with_effective_attrs(&old_path, Some(olddir))?;
        Self::enforce_sticky_delete(&old_parent_stat, &old_child_stat, ctx)?;
        match self.stat_path_with_effective_attrs(&new_path, Some(newdir)) {
            Ok(new_child_stat) => {
                Self::enforce_sticky_delete(&new_parent_stat, &new_child_stat, ctx)?;
            }
            Err(e) if e.raw_os_error() == Some(LINUX_ENOENT) => {}
            Err(e) => return Err(e),
        }
        ftrace!(
            "rename: olddir={} oldname={:?} newdir={} newname={:?} old_path_wide={} new_path_wide={}",
            olddir,
            String::from_utf8_lossy(oldname.to_bytes()),
            newdir,
            String::from_utf8_lossy(newname.to_bytes()),
            hex_wide(old_path.as_os_str()),
            hex_wide(new_path.as_os_str())
        );
        std::fs::rename(&old_path, &new_path).map_err(|e| {
            ftrace!(
                "rename: FAILED old_wide={} new_wide={} errno={:?}",
                hex_wide(old_path.as_os_str()),
                hex_wide(new_path.as_os_str()),
                e.raw_os_error()
            );
            linux_error(e)
        })?;

        // CRITICAL: update the cached host_path on the affected inode so that
        // subsequent open()/read()/write()/fsync() operations resolve against
        // the new path. Without this, atomic-write patterns (Node.js
        // `fs.writeFile`: create tmp -> write -> rename tmp -> target) leave
        // the inode pointing at the now-vanished tmp filename, producing
        // ENOENT on every subsequent op despite FUSE lookup succeeding.
        match Self::stat_path(&new_path) {
            Ok(stat) => {
                let alt_key = InodeAltKey {
                    file_index: stat.st_ino,
                    volume_serial: stat.st_dev as u32,
                };
                let inodes = self.inodes.read().unwrap();
                if let Some(data) = inodes.get_alt(&alt_key) {
                    let old = data.path_buf();
                    data.set_path(new_path.clone());
                    ftrace!(
                        "rename: inode {} path updated old_wide={} new_wide={}",
                        data.inode,
                        hex_wide(old.as_os_str()),
                        hex_wide(new_path.as_os_str())
                    );
                } else {
                    ftrace!(
                        "rename: no cached inode for alt_key=({}, {}) — skip path fixup",
                        alt_key.file_index,
                        alt_key.volume_serial
                    );
                }
            }
            Err(e) => {
                // If we can't stat the destination, log and continue; the next
                // lookup will refresh state. Don't fail the rename itself.
                ftrace!(
                    "rename: post-rename stat_path failed new_wide={} errno={:?}",
                    hex_wide(new_path.as_os_str()),
                    e.raw_os_error()
                );
            }
        }

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

        open_opts
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);

        let file = open_opts.open(&data.path_buf()).map_err(linux_error)?;
        let handle_id = self.alloc_handle();
        let handle_data = Arc::new(HandleData {
            inode,
            handle: RwLock::new(file),
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
        mode: u32,
        kill_priv: bool,
        flags: u32,
        umask: u32,
        _extensions: Extensions,
    ) -> io::Result<(Entry, Option<Self::Handle>, OpenOptions)> {
        let name_bytes = name.to_bytes();
        ftrace!(
            "create: parent={} name_utf8={:?} name_hex={} mode=0{:o} flags=0x{:x} umask=0o{:o}",
            parent,
            String::from_utf8_lossy(name_bytes),
            hex_bytes(name_bytes),
            mode,
            flags,
            umask
        );
        let path = self.get_child_path(parent, name)?;
        ftrace!(
            "create: resolved path={:?} path_wide={}",
            path,
            hex_wide(path.as_os_str())
        );

        // Create the file.
        let _file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| {
                ftrace!(
                    "create: OpenOptions(create_new) FAILED path_wide={} errno={:?}",
                    hex_wide(path.as_os_str()),
                    e.raw_os_error()
                );
                linux_error(e)
            })?;

        let entry = self.do_lookup(&path, Some(parent))?;
        ftrace!(
            "create: do_lookup OK inode={} ino={} mode=0{:o}",
            entry.inode,
            entry.attr.st_ino,
            entry.attr.st_mode
        );
        self.set_owner_mode_override(entry.inode, &path, ctx.uid, ctx.gid, mode & !umask & 0o7777);
        let (handle, open_opts) = self.open(ctx, entry.inode, kill_priv, flags)?;
        Ok((entry, handle, open_opts))
    }

    fn read<W: io::Write + ZeroCopyWriter>(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        handle: Self::Handle,
        mut w: W,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _flags: u32,
    ) -> io::Result<usize> {
        use std::io::Read;
        use std::io::Seek;

        let handles = self.handles.read().unwrap();
        let handle_data = handles.get(&handle).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        let mut file = handle_data.handle.read().unwrap().try_clone().map_err(linux_error)?;
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
        _inode: Self::Inode,
        handle: Self::Handle,
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

        let handles = self.handles.read().unwrap();
        let handle_data = handles.get(&handle).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;

        let mut file = handle_data.handle.write().unwrap().try_clone().map_err(linux_error)?;
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
        datasync: bool,
        handle: Self::Handle,
    ) -> io::Result<()> {
        let handles = self.handles.read().unwrap();
        let handle_data = handles
            .get(&handle)
            .filter(|hd| hd.inode == inode)
            .ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?
            .clone();
        drop(handles);

        let file = handle_data.handle.read().unwrap();
        if datasync {
            file.sync_data().map_err(linux_error)?;
        } else {
            file.sync_all().map_err(linux_error)?;
        }
        Ok(())
    }

    fn fsyncdir(
        &self,
        ctx: Context,
        inode: Self::Inode,
        datasync: bool,
        handle: Self::Handle,
    ) -> io::Result<()> {
        self.fsync(ctx, inode, datasync, handle)
    }

    fn statfs(&self, _ctx: Context, inode: Self::Inode) -> io::Result<bindings::statvfs64> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| {
            io::Error::from_raw_os_error(LINUX_EBADF)
        })?;
        let mut root_path = data.path_buf();
        drop(inodes);

        if !root_path.is_dir() {
            if let Some(parent) = root_path.parent() {
                root_path = parent.to_path_buf();
            }
        }

        let wide: Vec<u16> = root_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut avail_bytes: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut free_bytes: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut avail_bytes,
                &mut total_bytes,
                &mut free_bytes,
            )
        };
        if ok == 0 {
            return Err(linux_error(io::Error::last_os_error()));
        }

        let block_size = 4096u64;
        Ok(bindings::statvfs64 {
            f_bsize: block_size,
            f_frsize: block_size,
            f_blocks: total_bytes / block_size,
            f_bfree: free_bytes / block_size,
            f_bavail: avail_bytes / block_size,
            f_files: 0,
            f_ffree: 0,
            f_favail: 0,
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

        let host_path = data.path_buf();
        if !host_path.is_dir() {
            return Err(io::Error::from_raw_os_error(LINUX_ENOTDIR));
        }

        let handle_id = self.alloc_handle();

        // Open a handle to the directory. Actual enumeration uses the path
        // with std::fs::read_dir in readdir().
        let dir_handle = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&host_path)
            .map_err(linux_error)?;
        let handle_data = Arc::new(HandleData {
            inode,
            handle: RwLock::new(dir_handle),
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
        self.hydrate_dirstream_entries(inode, &mut dirstream)?;

        // Return entries starting from offset.
        ftrace!(
            "readdir: emit inode={} handle={} offset={} cached_entries={}",
            inode,
            handle,
            offset,
            dirstream.entries.len()
        );
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

    fn setxattr(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        name: &CStr,
        value: &[u8],
        flags: u32,
    ) -> io::Result<()> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?;
        let host_path = data.path_buf();
        drop(inodes);
        let xname = name.to_bytes();
        let stream_path = Self::xattr_stream_path(&host_path, xname);
        let exists = stream_path.exists();

        if (flags as i32) & bindings::LINUX_XATTR_CREATE != 0 && exists {
            return Err(io::Error::from_raw_os_error(LINUX_EEXIST));
        }
        if (flags as i32) & bindings::LINUX_XATTR_REPLACE != 0 && !exists {
            return Err(io::Error::from_raw_os_error(LINUX_ENODATA));
        }
        if value.len() > LINUX_XATTR_SIZE_MAX {
            return Err(io::Error::from_raw_os_error(LINUX_E2BIG));
        }

        std::fs::write(&stream_path, value).map_err(linux_error)?;

        let mut names = Self::read_xattr_index(&host_path)?;
        if !names.iter().any(|n| n.as_slice() == xname) {
            names.push(xname.to_vec());
            Self::write_xattr_index(&host_path, &names)?;
        }

        Ok(())
    }

    fn readdirplus<F>(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        handle: Self::Handle,
        _size: u32,
        offset: u64,
        mut add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry, Entry) -> io::Result<usize>,
    {
        let handles = self.handles.read().unwrap();
        let hd = handles
            .get(&handle)
            .ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?;

        let mut dirstream = hd.dirstream.lock().unwrap();
        self.hydrate_dirstream_entries(inode, &mut dirstream)?;

        let mut i = offset;
        while let Some(entry) = dirstream.entries.get(i as usize) {
            let dir_entry = DirEntry {
                ino: entry.ino,
                offset: i + 1,
                type_: entry.type_ as u32,
                name: &entry.name,
            };
            let lookup = self.do_lookup(&entry.host_path, Some(inode))?;

            match add_entry(dir_entry, lookup) {
                Ok(0) => break,
                Ok(_) => i += 1,
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    fn access(&self, ctx: Context, inode: Self::Inode, mask: u32) -> io::Result<()> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes
            .get(&inode)
            .ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?;
        let host_path = data.path_buf();
        drop(inodes);

        let stat = Self::stat_path(&host_path)?;
        let stat = self.apply_path_attr_overrides(inode, &host_path, None, stat);
        if Self::check_access(&stat, ctx, mask) {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(LINUX_EACCES))
        }
    }

    fn getxattr(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        name: &CStr,
        size: u32,
    ) -> io::Result<GetxattrReply> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?;
        let host_path = data.path_buf();
        drop(inodes);
        let stream_path = Self::xattr_stream_path(&host_path, name.to_bytes());
        let value = std::fs::read(&stream_path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                io::Error::from_raw_os_error(LINUX_ENODATA)
            } else {
                linux_error(e)
            }
        })?;

        if size == 0 {
            return Ok(GetxattrReply::Count(value.len() as u32));
        }
        if size < value.len() as u32 {
            return Err(io::Error::from_raw_os_error(LINUX_ERANGE));
        }

        Ok(GetxattrReply::Value(value))
    }

    fn listxattr(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        size: u32,
    ) -> io::Result<ListxattrReply> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?;
        let host_path = data.path_buf();
        drop(inodes);
        let names = Self::read_xattr_index(&host_path)?;

        let mut packed = Vec::new();
        for name in names {
            packed.extend_from_slice(&name);
            packed.push(0);
        }

        if size == 0 {
            return Ok(ListxattrReply::Count(packed.len() as u32));
        }
        if size < packed.len() as u32 {
            return Err(io::Error::from_raw_os_error(LINUX_ERANGE));
        }

        Ok(ListxattrReply::Names(packed))
    }

    fn removexattr(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        name: &CStr,
    ) -> io::Result<()> {
        let inodes = self.inodes.read().unwrap();
        let data = inodes.get(&inode).ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?;
        let host_path = data.path_buf();
        drop(inodes);
        let xname = name.to_bytes();
        let stream_path = Self::xattr_stream_path(&host_path, xname);
        std::fs::remove_file(&stream_path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                io::Error::from_raw_os_error(LINUX_ENODATA)
            } else {
                linux_error(e)
            }
        })?;

        let mut names = Self::read_xattr_index(&host_path)?;
        names.retain(|n| n.as_slice() != xname);
        Self::write_xattr_index(&host_path, &names)?;
        Ok(())
    }

    fn fallocate(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        handle: Self::Handle,
        mode: u32,
        offset: u64,
        length: u64,
    ) -> io::Result<()> {
        let unsupported = mode
            & !(LINUX_FALLOC_FL_KEEP_SIZE | LINUX_FALLOC_FL_PUNCH_HOLE | LINUX_FALLOC_FL_ZERO_RANGE);
        if unsupported != 0 {
            return Err(io::Error::from_raw_os_error(LINUX_EOPNOTSUPP));
        }
        if (mode & LINUX_FALLOC_FL_PUNCH_HOLE) != 0 && (mode & LINUX_FALLOC_FL_KEEP_SIZE) == 0 {
            return Err(io::Error::from_raw_os_error(LINUX_EINVAL));
        }

        let handles = self.handles.read().unwrap();
        let handle_data = handles
            .get(&handle)
            .filter(|hd| hd.inode == inode)
            .ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?
            .clone();
        drop(handles);

        let mut file = handle_data.handle.write().unwrap().try_clone().map_err(linux_error)?;
        let end = offset.saturating_add(length);
        let size = file.metadata().map_err(linux_error)?.len();

        if (mode & LINUX_FALLOC_FL_PUNCH_HOLE) != 0 {
            if offset >= size {
                return Ok(());
            }
            let punch_end = std::cmp::min(end, size);
            return Self::write_zero_range(&mut file, offset, punch_end.saturating_sub(offset));
        }

        if (mode & LINUX_FALLOC_FL_ZERO_RANGE) != 0 {
            let zero_end = if (mode & LINUX_FALLOC_FL_KEEP_SIZE) != 0 {
                std::cmp::min(end, size)
            } else {
                end
            };
            if zero_end > offset {
                Self::write_zero_range(&mut file, offset, zero_end.saturating_sub(offset))?;
            }
            if (mode & LINUX_FALLOC_FL_KEEP_SIZE) == 0 && end > size {
                file.set_len(end).map_err(linux_error)?;
            }
            return Ok(());
        }

        if (mode & LINUX_FALLOC_FL_KEEP_SIZE) == 0 && end > size {
            file.set_len(end).map_err(linux_error)?;
        }
        Ok(())
    }

    fn lseek(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        handle: Self::Handle,
        offset: u64,
        whence: u32,
    ) -> io::Result<u64> {
        let handles = self.handles.read().unwrap();
        let handle_data = handles
            .get(&handle)
            .filter(|hd| hd.inode == inode)
            .ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?
            .clone();
        drop(handles);

        let mut file = handle_data.handle.write().unwrap();
        let size = file.metadata().map_err(linux_error)?.len();
        match whence {
            LINUX_SEEK_SET => file
                .seek(std::io::SeekFrom::Start(offset))
                .map_err(linux_error),
            LINUX_SEEK_CUR => file
                .seek(std::io::SeekFrom::Current(offset as i64))
                .map_err(linux_error),
            LINUX_SEEK_END => file
                .seek(std::io::SeekFrom::End(offset as i64))
                .map_err(linux_error),
            LINUX_SEEK_DATA => {
                if offset >= size {
                    Err(io::Error::from_raw_os_error(LINUX_ENXIO))
                } else {
                    Ok(offset)
                }
            }
            LINUX_SEEK_HOLE => {
                if offset > size {
                    Err(io::Error::from_raw_os_error(LINUX_ENXIO))
                } else {
                    Ok(size)
                }
            }
            _ => Err(io::Error::from_raw_os_error(LINUX_EINVAL)),
        }
    }

    fn copyfilerange(
        &self,
        _ctx: Context,
        inode_in: Self::Inode,
        handle_in: Self::Handle,
        offset_in: u64,
        inode_out: Self::Inode,
        handle_out: Self::Handle,
        offset_out: u64,
        len: u64,
        flags: u64,
    ) -> io::Result<usize> {
        if flags != 0 {
            return Err(io::Error::from_raw_os_error(LINUX_EINVAL));
        }
        if len == 0 {
            return Ok(0);
        }
        if inode_in == inode_out {
            let in_end = offset_in.saturating_add(len);
            let out_end = offset_out.saturating_add(len);
            if offset_in < out_end && offset_out < in_end {
                return Err(io::Error::from_raw_os_error(LINUX_EINVAL));
            }
        }

        let same_volume = match (self.inode_path_and_stat(inode_in), self.inode_path_and_stat(inode_out)) {
            (Ok((_, st_in)), Ok((_, st_out))) => st_in.st_dev == st_out.st_dev,
            _ => false,
        };

        let handles = self.handles.read().unwrap();
        let input = handles
            .get(&handle_in)
            .filter(|hd| hd.inode == inode_in)
            .ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?
            .clone();
        let output = handles
            .get(&handle_out)
            .filter(|hd| hd.inode == inode_out)
            .ok_or_else(|| io::Error::from_raw_os_error(LINUX_EBADF))?
            .clone();
        drop(handles);

        let mut src = input.handle.read().unwrap().try_clone().map_err(linux_error)?;
        let mut dst = output.handle.write().unwrap().try_clone().map_err(linux_error)?;
        src.seek(std::io::SeekFrom::Start(offset_in)).map_err(linux_error)?;
        dst.seek(std::io::SeekFrom::Start(offset_out)).map_err(linux_error)?;

        let mut remaining = len;
        let mut total: usize = 0;
        let mut buf = vec![0u8; if same_volume { 4 * 1024 * 1024 } else { 1024 * 1024 }];
        while remaining > 0 {
            let chunk = std::cmp::min(remaining, buf.len() as u64) as usize;
            let n = src.read(&mut buf[..chunk]).map_err(linux_error)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).map_err(linux_error)?;
            total += n;
            remaining -= n as u64;
        }

        Ok(total)
    }

    // -- File locking --
    //
    // The current trait signatures do not pass lock parameters, so lock
    // operations are intentionally unimplemented here.

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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx(uid: u32, gid: u32) -> Context {
        Context { uid, gid, pid: 1234 }
    }

    fn test_stat(uid: u32, gid: u32, mode: u32) -> bindings::stat64 {
        bindings::stat64 {
            st_dev: 0,
            st_ino: 0,
            st_nlink: 1,
            st_mode: S_IFREG | (mode & 0o7777),
            st_uid: uid,
            st_gid: gid,
            __pad0: 0,
            st_rdev: 0,
            st_size: 0,
            st_blksize: 4096,
            st_blocks: 0,
            st_atime: 0,
            st_atime_nsec: 0,
            st_mtime: 0,
            st_mtime_nsec: 0,
            st_ctime: 0,
            st_ctime_nsec: 0,
        }
    }

    fn test_dir_stat(uid: u32, gid: u32, mode: u32) -> bindings::stat64 {
        bindings::stat64 {
            st_mode: S_IFDIR | (mode & 0o7777),
            ..test_stat(uid, gid, mode)
        }
    }

    #[test]
    fn access_owner_permissions_are_applied() {
        let st = test_stat(1000, 100, 0o640);
        assert!(PassthroughFs::check_access(&st, test_ctx(1000, 999), LINUX_R_OK as u32));
        assert!(PassthroughFs::check_access(&st, test_ctx(1000, 999), LINUX_W_OK as u32));
        assert!(!PassthroughFs::check_access(&st, test_ctx(1000, 999), LINUX_X_OK as u32));
    }

    #[test]
    fn access_group_and_other_permissions_are_applied() {
        let st = test_stat(2000, 200, 0o654);
        assert!(PassthroughFs::check_access(&st, test_ctx(3000, 200), LINUX_R_OK as u32));
        assert!(!PassthroughFs::check_access(&st, test_ctx(3000, 200), LINUX_W_OK as u32));
        assert!(PassthroughFs::check_access(&st, test_ctx(4000, 500), LINUX_R_OK as u32));
        assert!(!PassthroughFs::check_access(&st, test_ctx(4000, 500), LINUX_W_OK as u32));
    }

    #[test]
    fn access_root_execute_requires_any_execute_bit() {
        let no_exec = test_stat(1000, 1000, 0o666);
        let has_exec = test_stat(1000, 1000, 0o661);

        assert!(!PassthroughFs::check_access(&no_exec, test_ctx(0, 0), LINUX_X_OK as u32));
        assert!(PassthroughFs::check_access(&has_exec, test_ctx(0, 0), LINUX_X_OK as u32));
    }

    #[test]
    fn sticky_delete_denies_non_owner() {
        let parent = test_dir_stat(1000, 1000, 0o1777);
        let child = test_stat(2000, 2000, 0o644);
        let err = PassthroughFs::enforce_sticky_delete(&parent, &child, test_ctx(3000, 3000))
            .unwrap_err();
        assert_eq!(err.raw_os_error(), Some(LINUX_EPERM));
    }

    #[test]
    fn sticky_delete_allows_file_or_dir_owner() {
        let parent = test_dir_stat(1000, 1000, 0o1777);
        let child = test_stat(2000, 2000, 0o644);
        assert!(PassthroughFs::enforce_sticky_delete(&parent, &child, test_ctx(1000, 1)).is_ok());
        assert!(PassthroughFs::enforce_sticky_delete(&parent, &child, test_ctx(2000, 1)).is_ok());
    }
}

