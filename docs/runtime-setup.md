# Runtime setup (macOS arm64)

Nanosandbox runs each agent in a microVM (libkrun). The CLI/TUI and the
protocol helper (`nanosb-runtime`) need three runtime artifacts:

| Artifact | Role | Default location |
|---|---|---|
| `nanosb-runtime` | the slim helper daemon (boots/controls VMs, serves the protocol) | on `PATH`, or `$NANOSB_HOME/bin/` |
| `gvproxy` | user-mode networking sidecar | `$NANOSB_HOME/bin/gvproxy`, `PATH`, or `/usr/local/bin` |
| `libkrunfw.5.dylib` | kernel firmware `dlopen`ed at boot | `$NANOSB_HOME/libs/` or system lib dirs |

`$NANOSB_HOME` defaults to `~/.nanosandbox` and overrides every lookup above.

## Toolchain prerequisites

- `cargo check` / `cargo test` for the workspace need **no libclang**: the
  `libkrun-sys` build script links the static library only when
  `$LIBKRUN_LIB_DIR/libkrun.a` (default `~/.nanosandbox/lib/libkrun.a`) exists.
- Building libkrun itself (`runtime/scripts/build-libkrun.sh`) **does** need
  libclang for bindgen: `libclang-dev` on Linux with
  `LIBCLANG_PATH="$(llvm-config --prefix)/lib"`, or `brew install llvm` with
  `LIBCLANG_PATH="$(brew --prefix llvm)/lib"` on macOS. The script resolves
  this automatically but fails loudly if `libclang.so` is absent.

## Install from source

```bash
# 1. Runtime deps (libkrunfw + gvproxy) into ~/.nanosandbox
./install-deps/install.sh

# 2. Build the binary / helper
cargo build --release -p nanosb-cli
cargo build --release -p nanosb-cli --bin nanosb-runtime \
    --no-default-features --features runtime-host

# 3. Verify
./target/release/nanosb doctor     # 4/4 checks
```

## Bundled packages (WS9)

`scripts/package-sdk.sh` produces a self-contained package per SDK under
`dist/nanosandbox-sdk-<lang>/`:

```
dist/nanosandbox-sdk-<lang>/
├── bin/nanosb-runtime          # pre-signed helper
├── bin/gvproxy                 # networking sidecar
├── libs/libkrunfw.5.dylib      # kernel firmware
├── sdk/<lang>/                 # the SDK sources
├── install.sh                  # lays the runtime into $NANOSB_HOME
└── README.md
```

```bash
scripts/package-sdk.sh               # all SDKs
scripts/package-sdk.sh rust python   # a subset
DIST=/tmp/out scripts/package-sdk.sh # custom output dir
```

### Signing

The helper is code-signed **at package build time** (`codesign`, ad-hoc by
default; use a Developer ID identity for distribution). `install.sh` performs a
**lazy re-sign**: if the copied helper's signature is invalid (for example after
a quarantine xattr is attached during download), it strips the quarantine flag
and re-signs so the `com.apple.security.hypervisor` entitlement is present.

### `NANOSB_HOME`

Every path resolves relative to `$NANOSB_HOME` (default `~/.nanosandbox`):

```bash
NANOSB_HOME=/opt/nanosb ./dist/nanosandbox-sdk-rust/install.sh
NANOSB_HOME=/opt/nanosb nanosb doctor
```

This is also how CI/Linux runs use a prebuilt runtime instead of the bundled one.

## SDK runtime bundle

The language SDKs auto-provision the runtime: `ManagedRuntime` runs
`scripts/sdk/ensure-runtime.sh`, which downloads the **SDK runtime bundle**
`nanosandbox-runtime-<os>-<arch>.tar.gz` from
`$NANOSB_RUNTIME_URL/v$NANOSB_VERSION/` and unpacks it into `$NANOSB_HOME`:

```
bin/nanosb-runtime      # the slim protocol helper
bin/gvproxy             # networking sidecar
libs/libkrunfw*.dylib   # kernel firmware (libkrunfw*.so* on Linux)
guest-bin/exec-agent    # in-guest exec agent
entitlements.plist      # macOS hypervisor entitlement
```

The release pipeline builds one bundle per platform (macOS arm64, Linux x86_64)
from already-built pieces (helper from `target/release/nanosb-runtime`,
gvproxy + libkrunfw + exec-agent from `$NANOSB_SRC_HOME`, `entitlements.plist`
from the repo root) and attaches it to each GitHub release, along with a
`.sha256` sidecar:

```bash
scripts/sdk/build-runtime-bundle.sh 0.2.0 --out dist
# → dist/nanosandbox-runtime-darwin-arm64.tar.gz (+ .sha256)
```

### Interop with `scripts/install.sh`

`scripts/install.sh` and the SDK provisioner install the **same runtime** into
the **same `$NANOSB_HOME`** (default `~/.nanosandbox`) in the exact bundle
layout above. Running `install.sh` installs the helper (`bin/nanosb-runtime`),
`gvproxy`, `libkrunfw`, and `exec-agent`, so the SDK's
`ensure-runtime.sh` finds its runtime already present and exits 0; conversely,
running the SDK provisioner produces the files `install.sh` relies on. Either
installation satisfies the other.