#!/usr/bin/env bash
# Re-apply structural patches to deps/libkrun after an upstream sync.
#
# These patches are required for libkrun to build as an rlib within our
# Cargo workspace (instead of its original standalone cdylib build).
#
# Usage: bash scripts/ci/patch-libkrun-workspace.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LIBKRUN_DIR="$REPO_ROOT/deps/libkrun"

echo "Patching libkrun for workspace integration..."

# 1. Remove [workspace] block from root Cargo.toml (conflicts with our workspace)
if grep -q '^\[workspace\]' "$LIBKRUN_DIR/Cargo.toml" 2>/dev/null; then
    echo "  - Removing [workspace] from deps/libkrun/Cargo.toml"
    # Remove [workspace] and its members/exclude arrays until next section or EOF
    python3 -c "
import re, sys
with open('$LIBKRUN_DIR/Cargo.toml', 'r') as f:
    content = f.read()
# Remove [workspace] section (everything from [workspace] to next [section] or EOF)
content = re.sub(r'\[workspace\][^\[]*', '', content, flags=re.DOTALL)
with open('$LIBKRUN_DIR/Cargo.toml', 'w') as f:
    f.write(content.strip() + '\n')
"
fi

# 2. Change crate-type from cdylib to rlib
LIBKRUN_CARGO="$LIBKRUN_DIR/src/libkrun/Cargo.toml"
if grep -q 'crate-type.*cdylib' "$LIBKRUN_CARGO" 2>/dev/null; then
    echo "  - Changing crate-type from cdylib to rlib"
    sed -i.bak 's/crate-type = \["cdylib"\]/crate-type = ["rlib"]/' "$LIBKRUN_CARGO"
    rm -f "$LIBKRUN_CARGO.bak"
fi

# 3. Clean up build.rs — remove soname/install_name directives (cdylib-only)
BUILD_RS="$LIBKRUN_DIR/src/libkrun/build.rs"
if [ -f "$BUILD_RS" ] && grep -q 'soname\|install_name' "$BUILD_RS" 2>/dev/null; then
    echo "  - Cleaning build.rs soname/install_name directives"
    cat > "$BUILD_RS" << 'BUILDRS'
fn main() {
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-lib=framework=Hypervisor");
}
BUILDRS
fi

# 4. Pin vm-memory to =0.16.2 in all crates that use it
echo "  - Pinning vm-memory to =0.16.2"
for cargo_toml in $(find "$LIBKRUN_DIR/src" -name "Cargo.toml"); do
    if grep -q 'vm-memory' "$cargo_toml" 2>/dev/null; then
        # Replace any vm-memory version spec with exact pin
        sed -i.bak 's/vm-memory = { version = "[^"]*"/vm-memory = { version = "=0.16.2"/g' "$cargo_toml"
        rm -f "$cargo_toml.bak"
    fi
done

# 5. Pin linux-loader to =0.13.1 (last version compatible with vm-memory 0.16)
# 0.13.2 upgraded to vm-memory 0.17, causing trait mismatch with libkrun's 0.16 pin.
# The mismatch only surfaces on x86_64 builds because the failing code paths are
# gated behind #[cfg(target_arch = "x86_64")] in vmm/src/builder.rs.
echo "  - Pinning linux-loader to =0.13.1"
for cargo_toml in $(find "$LIBKRUN_DIR/src" -name "Cargo.toml"); do
    if grep -q 'linux-loader' "$cargo_toml" 2>/dev/null; then
        sed -i.bak 's/linux-loader = { version = "[^"]*"/linux-loader = { version = "=0.13.1"/g' "$cargo_toml"
        rm -f "$cargo_toml.bak"
    fi
done

# 5. Ensure feature propagation in libkrun Cargo.toml
# The features must propagate to vmm and devices sub-crates
if ! grep -q 'net = \["vmm/net"' "$LIBKRUN_CARGO" 2>/dev/null; then
    echo "  - Ensuring feature propagation (net, blk, etc.)"
    echo "  WARNING: Feature propagation may need manual review after upstream sync"
fi

echo "Done. Run 'cargo check -p nanosandbox' to verify."
