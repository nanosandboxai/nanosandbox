#!/bin/sh
# Patch libkrun sources for musl/Alpine compatibility.
#
# Two issues fixed:
#   1. Rust's musl target doesn't support cdylib → switch to staticlib
#   2. The libc crate doesn't expose statx() for musl → use raw syscall
#
# Applied in CI before building libkrun on Alpine. Changes are local to
# the build and don't affect the submodule commit.
set -eu

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LIBKRUN_DIR="${1:-$REPO_ROOT/deps/libkrun}"

echo "=== Patching libkrun for musl ==="

# 1. Change crate-type: cdylib → staticlib
CARGO_TOML="$LIBKRUN_DIR/src/libkrun/Cargo.toml"
echo "  Patching $CARGO_TOML: cdylib → staticlib"
sed 's/crate-type = \["cdylib"\]/crate-type = ["staticlib"]/' "$CARGO_TOML" > "$CARGO_TOML.tmp"
mv "$CARGO_TOML.tmp" "$CARGO_TOML"

# 2. Remove krun_input from workspace members (uses bindgen+clang, segfaults on musl)
WS_CARGO_TOML="$LIBKRUN_DIR/Cargo.toml"
echo "  Patching $WS_CARGO_TOML: remove krun_input from workspace"
sed 's/members = \["src\/libkrun", "src\/krun_input"\]/members = ["src\/libkrun"]/' "$WS_CARGO_TOML" > "$WS_CARGO_TOML.tmp"
mv "$WS_CARGO_TOML.tmp" "$WS_CARGO_TOML"

# 3. Remove krun_display and krun_input optional deps from ALL crates
#    (bindgen+clang runtime segfaults on musl)
echo "  Removing krun_display/krun_input deps from all Cargo.toml files"
for toml in $(find "$LIBKRUN_DIR/src" -name Cargo.toml); do
    if grep -q 'krun_display\|krun_input' "$toml"; then
        echo "    Patching $toml"
        grep -v 'krun_display\|krun_input' "$toml" > "$toml.tmp"
        mv "$toml.tmp" "$toml"
    fi
done

# Note: Cargo.lock is kept — cargo ignores stale entries for removed deps.
# Deleting it would let cargo resolve newer incompatible versions.

# Verify patches applied
echo "  Verifying patches..."
if grep -q 'staticlib' "$CARGO_TOML"; then
    echo "  OK: crate-type = staticlib"
else
    echo "  FAIL: crate-type patch did not apply!"
    exit 1
fi
if ! grep -q 'krun_input' "$WS_CARGO_TOML"; then
    echo "  OK: krun_input removed from workspace"
else
    echo "  FAIL: krun_input still in workspace!"
    cat "$WS_CARGO_TOML"
    exit 1
fi
if ! grep -rq 'krun_display' "$LIBKRUN_DIR/src"/*/Cargo.toml; then
    echo "  OK: krun_display removed from all crates"
else
    echo "  FAIL: krun_display still referenced!"
    grep -rn 'krun_display' "$LIBKRUN_DIR/src"/*/Cargo.toml
    exit 1
fi

# 2. Add musl-compatible statx using raw syscall
#    The libc crate doesn't expose libc::statx, libc::statx struct,
#    STATX_BASIC_STATS, or STATX_MNT_ID for musl targets.
#    We define them ourselves and use libc::syscall(SYS_statx, ...).
PASSTHROUGH="$LIBKRUN_DIR/src/devices/src/virtio/fs/linux/passthrough.rs"
echo "  Patching $PASSTHROUGH: add musl statx compat"

cat > /tmp/musl_statx_patch.py << 'PYTHON'
import sys

filepath = sys.argv[1]
with open(filepath, 'r') as f:
    content = f.read()

# The original function to replace
old_fn = '''fn statx(f: &File) -> io::Result<(libc::stat64, u64)> {
    let mut stx = MaybeUninit::<libc::statx>::zeroed();

    // Safe because this is a constant value and a valid C string.
    let pathname = unsafe { CStr::from_bytes_with_nul_unchecked(EMPTY_CSTR) };

    // Safe because the kernel will only write data in `st` and we check the return
    // value.
    let res = unsafe {
        libc::statx(
            f.as_raw_fd(),
            pathname.as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_BASIC_STATS | libc::STATX_MNT_ID,
            stx.as_mut_ptr(),
        )
    };
    if res >= 0 {
        // Safe because the kernel guarantees that the struct is now fully initialized.
        let stx = unsafe { stx.assume_init() };

        // Unfortunately, we cannot use an initializer to create the stat64 object,
        // because it may contain padding and reserved fields (depending on the
        // architecture), and it does not implement the Default trait.
        // So we take a zeroed struct and set what we can. (Zero in all fields is
        // wrong, but safe.)
        let mut st = unsafe { MaybeUninit::<libc::stat64>::zeroed().assume_init() };

        st.st_dev = libc::makedev(stx.stx_dev_major, stx.stx_dev_minor);
        st.st_ino = stx.stx_ino;
        st.st_mode = stx.stx_mode as _;
        st.st_nlink = stx.stx_nlink as _;
        st.st_uid = stx.stx_uid;
        st.st_gid = stx.stx_gid;
        st.st_rdev = libc::makedev(stx.stx_rdev_major, stx.stx_rdev_minor);
        st.st_size = stx.stx_size as _;
        st.st_blksize = stx.stx_blksize as _;
        st.st_blocks = stx.stx_blocks as _;
        st.st_atime = stx.stx_atime.tv_sec;
        st.st_atime_nsec = stx.stx_atime.tv_nsec as _;
        st.st_mtime = stx.stx_mtime.tv_sec;
        st.st_mtime_nsec = stx.stx_mtime.tv_nsec as _;
        st.st_ctime = stx.stx_ctime.tv_sec;
        st.st_ctime_nsec = stx.stx_ctime.tv_nsec as _;
        Ok((st, stx.stx_mnt_id))
    } else {
        Err(io::Error::last_os_error())
    }
}'''

# Replacement: raw syscall with manually-defined struct for musl compat
new_fn = '''fn statx(f: &File) -> io::Result<(libc::stat64, u64)> {
    // musl compat: the libc crate doesn't expose statx types for musl targets.
    // We define the kernel structs ourselves and use the raw SYS_statx syscall.
    // See: https://github.com/containers/libkrun/issues/431
    #[repr(C)]
    #[derive(Copy, Clone)]
    struct StatxTimestamp {
        tv_sec: i64,
        tv_nsec: u32,
        _reserved: i32,
    }

    #[repr(C)]
    struct Statx {
        stx_mask: u32,
        stx_blksize: u32,
        stx_attributes: u64,
        stx_nlink: u32,
        stx_uid: u32,
        stx_gid: u32,
        stx_mode: u16,
        _spare0: [u16; 1],
        stx_ino: u64,
        stx_size: u64,
        stx_blocks: u64,
        stx_attributes_mask: u64,
        stx_atime: StatxTimestamp,
        stx_btime: StatxTimestamp,
        stx_ctime: StatxTimestamp,
        stx_mtime: StatxTimestamp,
        stx_rdev_major: u32,
        stx_rdev_minor: u32,
        stx_dev_major: u32,
        stx_dev_minor: u32,
        stx_mnt_id: u64,
        stx_dio_mem_align: u32,
        stx_dio_offset_align: u32,
        _spare3: [u64; 12],
    }

    const STATX_BASIC_STATS: u32 = 0x07ff;
    const STATX_MNT_ID: u32 = 0x1000;

    let mut stx = MaybeUninit::<Statx>::zeroed();

    // Safe because this is a constant value and a valid C string.
    let pathname = unsafe { CStr::from_bytes_with_nul_unchecked(EMPTY_CSTR) };

    // Safe because the kernel will only write data in `stx` and we check the return value.
    let res = unsafe {
        libc::syscall(
            libc::SYS_statx,
            f.as_raw_fd(),
            pathname.as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            STATX_BASIC_STATS | STATX_MNT_ID,
            stx.as_mut_ptr(),
        )
    };
    if res >= 0 {
        let stx = unsafe { stx.assume_init() };

        let mut st = unsafe { MaybeUninit::<libc::stat64>::zeroed().assume_init() };

        st.st_dev = libc::makedev(stx.stx_dev_major, stx.stx_dev_minor);
        st.st_ino = stx.stx_ino;
        st.st_mode = stx.stx_mode as _;
        st.st_nlink = stx.stx_nlink as _;
        st.st_uid = stx.stx_uid;
        st.st_gid = stx.stx_gid;
        st.st_rdev = libc::makedev(stx.stx_rdev_major, stx.stx_rdev_minor);
        st.st_size = stx.stx_size as _;
        st.st_blksize = stx.stx_blksize as _;
        st.st_blocks = stx.stx_blocks as _;
        st.st_atime = stx.stx_atime.tv_sec;
        st.st_atime_nsec = stx.stx_atime.tv_nsec as _;
        st.st_mtime = stx.stx_mtime.tv_sec;
        st.st_mtime_nsec = stx.stx_mtime.tv_nsec as _;
        st.st_ctime = stx.stx_ctime.tv_sec;
        st.st_ctime_nsec = stx.stx_ctime.tv_nsec as _;
        Ok((st, stx.stx_mnt_id))
    } else {
        Err(io::Error::last_os_error())
    }
}'''

if old_fn not in content:
    print("WARNING: Could not find exact statx function to patch. The libkrun source may have changed.")
    sys.exit(1)

content = content.replace(old_fn, new_fn)

with open(filepath, 'w') as f:
    f.write(content)

print("  statx patch applied successfully")
PYTHON

python3 /tmp/musl_statx_patch.py "$PASSTHROUGH"
rm -f /tmp/musl_statx_patch.py

echo "=== musl patches applied ==="
