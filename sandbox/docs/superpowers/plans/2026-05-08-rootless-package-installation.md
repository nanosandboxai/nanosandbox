# Rootless Package Installation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable code agents to install dev tools without root by confining all installs to `/home/developer/.local/`, hardening the VM with seccomp, reduced capabilities, read-only rootfs, and additional resource limits.

**Architecture:** Three repos change in parallel — `runtime` (OCI security hardening), `agents-registry` (Dockerfile, init script, gateway env vars), and `sandbox` (config defaults). The runtime changes enforce 10 security layers at the OCI spec level. The agents-registry changes remove sudo, create the user-local install prefix, and add the apt-get wrapper. The sandbox repo holds the spec/plan docs.

**Tech Stack:** Rust (runtime OCI generation), Go (agent-gateway), Shell (Dockerfile, init script, wrapper scripts)

**Spec:** `docs/superpowers/specs/2026-05-08-rootless-package-installation-design.md`

---

## File Map

### runtime repo (`feat/rootless-package-installation`)

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/runtime/src/oci.rs` | Modify | Seccomp profile, capability reduction, read-only rootfs, tmpfs mounts, rlimits, PID limit |

### agents-registry repo (`feat/rootless-package-installation`)

| File | Action | Responsibility |
|------|--------|----------------|
| `docker/Dockerfile.base` | Modify | Remove sudo, strip suid, create .local/ tree, create apt wrappers, init dpkg db |
| `docker/nanosb-init.sh` | Modify | Prepare .local/ dir tree on boot, ensure dpkg db |
| `docker/rootless-apt-wrapper.sh` | Create | apt-get/apt wrapper using unshare + user-local prefix |
| `agent-gateway/main.go` | Modify | Add rootless env vars (PIP_USER, NPM_CONFIG_PREFIX, etc.), update PATH |

### sandbox repo (`feat/rootless-package-installation`)

| File | Action | Responsibility |
|------|--------|----------------|
| `docs/superpowers/specs/2026-05-08-rootless-package-installation-design.md` | Already created | Design spec |
| `docs/superpowers/plans/2026-05-08-rootless-package-installation.md` | Already created | This plan |

---

## Task 1: Seccomp Profile in OCI Config

**Repo:** `runtime` (branch `feat/rootless-package-installation`)
**Files:**
- Modify: `crates/runtime/src/oci.rs:168-222` (generate_linux_config function)

- [ ] **Step 1: Write the failing test**

Add to `crates/runtime/src/oci.rs` inside the `mod tests` block at line 397:

```rust
#[test]
fn test_seccomp_profile() {
    let config = SandboxConfig::builder()
        .name("test-sandbox")
        .image("alpine:latest")
        .cpus(1)
        .memory_mb(256)
        .build();

    let oci_config = generate_config(&config, Path::new("rootfs"));
    let seccomp = &oci_config["linux"]["seccomp"];

    // Must have seccomp section
    assert!(!seccomp.is_null(), "seccomp section must exist");

    // Default action must be ALLOW (blocklist approach)
    assert_eq!(seccomp["defaultAction"], "SCMP_ACT_ALLOW");

    // Must block dangerous syscalls
    let syscalls = seccomp["syscalls"].as_array().expect("syscalls must be array");
    assert!(!syscalls.is_empty(), "must have blocked syscalls");

    // Verify specific blocked syscalls
    let blocked: Vec<&str> = syscalls.iter()
        .filter(|s| s["action"] == "SCMP_ACT_ERRNO")
        .flat_map(|s| s["names"].as_array().unwrap_or(&vec![]).iter())
        .filter_map(|n| n.as_str())
        .collect();

    assert!(blocked.contains(&"kexec_load"), "kexec_load must be blocked");
    assert!(blocked.contains(&"ptrace"), "ptrace must be blocked");
    assert!(blocked.contains(&"bpf"), "bpf must be blocked");
    assert!(blocked.contains(&"io_uring_setup"), "io_uring_setup must be blocked");
    assert!(blocked.contains(&"reboot"), "reboot must be blocked");
    assert!(blocked.contains(&"init_module"), "init_module must be blocked");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/janvaca/devdone-labs/runtime && cargo test -p runtime test_seccomp_profile -- --nocapture`
Expected: FAIL — seccomp section is null

- [ ] **Step 3: Add generate_seccomp function and wire it into generate_linux_config**

Add the `generate_seccomp` function before `generate_linux_config` in `crates/runtime/src/oci.rs`:

```rust
/// Generate seccomp profile blocking dangerous syscalls
fn generate_seccomp() -> serde_json::Value {
    json!({
        "defaultAction": "SCMP_ACT_ALLOW",
        "architectures": ["SCMP_ARCH_X86_64", "SCMP_ARCH_AARCH64", "SCMP_ARCH_X86"],
        "syscalls": [
            {
                "names": [
                    "kexec_load",
                    "kexec_file_load",
                    "init_module",
                    "finit_module",
                    "delete_module",
                    "reboot",
                    "pivot_root",
                    "swapon",
                    "swapoff",
                    "acct",
                    "settimeofday",
                    "clock_settime",
                    "clock_adjtime",
                    "adjtimex",
                    "add_key",
                    "keyctl",
                    "request_key",
                    "ptrace",
                    "userfaultfd",
                    "perf_event_open",
                    "bpf",
                    "io_uring_setup",
                    "io_uring_enter",
                    "io_uring_register",
                    "lookup_dcookie",
                    "mbind",
                    "move_pages",
                    "migrate_pages",
                    "personality",
                    "vm86",
                    "vm86old",
                    "modify_ldt",
                    "open_by_handle_at",
                    "name_to_handle_at"
                ],
                "action": "SCMP_ACT_ERRNO",
                "errnoRet": 1
            }
        ]
    })
}
```

Then modify `generate_linux_config` to include it. Change the json! block at line 181 to add `"seccomp"` after `"readonlyPaths"`:

In the `json!({})` block inside `generate_linux_config`, add after the `"readonlyPaths"` array (after line 221):

```rust
        "seccomp": generate_seccomp()
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/janvaca/devdone-labs/runtime && cargo test -p runtime test_seccomp_profile -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
cd /Users/janvaca/devdone-labs/runtime
git add crates/runtime/src/oci.rs
git commit -m "feat: add seccomp profile blocking dangerous syscalls

Block 34 dangerous syscalls including ptrace, bpf, io_uring,
kexec, reboot, and kernel module operations.
Uses blocklist approach (SCMP_ACT_ALLOW default) since code
agents need broad syscall access for dev tools.

Ref: nanosandboxai/sandbox#8"
```

---

## Task 2: Reduce Capabilities and Drop Ambient/Inheritable

**Repo:** `runtime` (branch `feat/rootless-package-installation`)
**Files:**
- Modify: `crates/runtime/src/oci.rs:259-284` (generate_capabilities function)

- [ ] **Step 1: Write the failing test**

Add to `crates/runtime/src/oci.rs` inside the `mod tests` block:

```rust
#[test]
fn test_reduced_capabilities() {
    let config = SandboxConfig::builder()
        .name("test-sandbox")
        .image("alpine:latest")
        .cpus(1)
        .memory_mb(256)
        .build();

    let oci_config = generate_config(&config, Path::new("rootfs"));
    let caps = &oci_config["process"]["capabilities"];

    // Must have exactly 7 capabilities in bounding set
    let bounding = caps["bounding"].as_array().expect("bounding must be array");
    assert_eq!(bounding.len(), 7, "must have exactly 7 capabilities, got {}", bounding.len());

    // Verify kept capabilities
    let cap_strs: Vec<&str> = bounding.iter().filter_map(|c| c.as_str()).collect();
    assert!(cap_strs.contains(&"CAP_CHOWN"));
    assert!(cap_strs.contains(&"CAP_FSETID"));
    assert!(cap_strs.contains(&"CAP_SETGID"));
    assert!(cap_strs.contains(&"CAP_SETUID"));
    assert!(cap_strs.contains(&"CAP_NET_BIND_SERVICE"));
    assert!(cap_strs.contains(&"CAP_KILL"));
    assert!(cap_strs.contains(&"CAP_AUDIT_WRITE"));

    // Verify dropped capabilities
    assert!(!cap_strs.contains(&"CAP_DAC_OVERRIDE"), "CAP_DAC_OVERRIDE must be dropped");
    assert!(!cap_strs.contains(&"CAP_FOWNER"), "CAP_FOWNER must be dropped");
    assert!(!cap_strs.contains(&"CAP_NET_RAW"), "CAP_NET_RAW must be dropped");
    assert!(!cap_strs.contains(&"CAP_MKNOD"), "CAP_MKNOD must be dropped");
    assert!(!cap_strs.contains(&"CAP_SYS_CHROOT"), "CAP_SYS_CHROOT must be dropped");

    // Ambient and inheritable must be empty
    let ambient = caps["ambient"].as_array().expect("ambient must be array");
    let inheritable = caps["inheritable"].as_array().expect("inheritable must be array");
    assert!(ambient.is_empty(), "ambient capabilities must be empty");
    assert!(inheritable.is_empty(), "inheritable capabilities must be empty");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/janvaca/devdone-labs/runtime && cargo test -p runtime test_reduced_capabilities -- --nocapture`
Expected: FAIL — currently has 14 capabilities and non-empty ambient/inheritable

- [ ] **Step 3: Update generate_capabilities function**

Replace the entire `generate_capabilities` function at line 259:

```rust
/// Generate capabilities configuration
///
/// Reduced capability set (7 caps, down from 14). Dangerous capabilities
/// like CAP_DAC_OVERRIDE, CAP_FOWNER, CAP_NET_RAW, CAP_MKNOD, CAP_SYS_CHROOT,
/// CAP_SETFCAP, and CAP_SETPCAP are dropped. Ambient and inheritable sets
/// are empty to prevent capability inheritance by child processes.
fn generate_capabilities() -> serde_json::Value {
    let caps = vec![
        "CAP_CHOWN",
        "CAP_FSETID",
        "CAP_SETGID",
        "CAP_SETUID",
        "CAP_NET_BIND_SERVICE",
        "CAP_KILL",
        "CAP_AUDIT_WRITE",
    ];

    json!({
        "bounding": caps,
        "effective": caps,
        "permitted": caps,
        "inheritable": [],
        "ambient": []
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/janvaca/devdone-labs/runtime && cargo test -p runtime test_reduced_capabilities -- --nocapture`
Expected: PASS

- [ ] **Step 5: Run all existing tests to check for regressions**

Run: `cd /Users/janvaca/devdone-labs/runtime && cargo test -p runtime -- --nocapture`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
cd /Users/janvaca/devdone-labs/runtime
git add crates/runtime/src/oci.rs
git commit -m "feat: reduce capabilities from 14 to 7, clear ambient/inheritable

Drop CAP_DAC_OVERRIDE, CAP_FOWNER, CAP_NET_RAW, CAP_MKNOD,
CAP_SYS_CHROOT, CAP_SETFCAP, CAP_SETPCAP.
Clear ambient and inheritable sets so child processes don't
inherit capabilities.

Ref: nanosandboxai/sandbox#8"
```

---

## Task 3: Read-Only Root Filesystem + tmpfs Mounts

**Repo:** `runtime` (branch `feat/rootless-package-installation`)
**Files:**
- Modify: `crates/runtime/src/oci.rs:46-48` (readonly flag)
- Modify: `crates/runtime/src/oci.rs:66-119` (generate_mounts — add tmpfs overlays)

- [ ] **Step 1: Write the failing test**

Add to `crates/runtime/src/oci.rs` inside the `mod tests` block:

```rust
#[test]
fn test_readonly_rootfs() {
    let config = SandboxConfig::builder()
        .name("test-sandbox")
        .image("alpine:latest")
        .cpus(1)
        .memory_mb(256)
        .build();

    let oci_config = generate_config(&config, Path::new("rootfs"));

    // Root filesystem must be read-only
    assert_eq!(oci_config["root"]["readonly"], true, "rootfs must be read-only");
}

#[test]
fn test_tmpfs_writable_overlays() {
    let config = SandboxConfig::builder()
        .name("test-sandbox")
        .image("alpine:latest")
        .cpus(1)
        .memory_mb(256)
        .build();

    let oci_config = generate_config(&config, Path::new("rootfs"));
    let mounts = oci_config["mounts"].as_array().expect("mounts must be array");

    let destinations: Vec<&str> = mounts
        .iter()
        .filter_map(|m| m["destination"].as_str())
        .collect();

    // Must have writable tmpfs overlays for runtime-writable paths
    assert!(destinations.contains(&"/run"), "/run tmpfs must exist");
    assert!(destinations.contains(&"/var/log"), "/var/log tmpfs must exist");

    // Verify /tmp has size limit
    let tmp_mount = mounts.iter().find(|m| m["destination"] == "/tmp").unwrap();
    let tmp_opts: Vec<&str> = tmp_mount["options"].as_array().unwrap()
        .iter().filter_map(|o| o.as_str()).collect();
    assert!(tmp_opts.iter().any(|o| o.starts_with("size=")), "/tmp must have size limit");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /Users/janvaca/devdone-labs/runtime && cargo test -p runtime test_readonly_rootfs test_tmpfs_writable_overlays -- --nocapture`
Expected: FAIL — readonly is false, no /run or /var/log mounts

- [ ] **Step 3: Change readonly to true**

In `crates/runtime/src/oci.rs`, change line 48:

```rust
            "readonly": true
```

- [ ] **Step 4: Add tmpfs mounts for writable areas**

In `generate_mounts`, add these mounts after the `/tmp` mount (after line 118, before the closing `];`):

```rust
        // Writable overlays for read-only rootfs
        json!({
            "destination": "/run",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "mode=755", "size=67108864"]
        }),
        json!({
            "destination": "/var/log",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "noexec", "mode=755", "size=33554432"]
        }),
```

Also update the `/tmp` mount (line 113-118) to add a size limit:

Change:
```rust
        json!({
            "destination": "/tmp",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "mode=1777"]
        }),
```
To:
```rust
        json!({
            "destination": "/tmp",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "mode=1777", "size=268435456"]
        }),
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd /Users/janvaca/devdone-labs/runtime && cargo test -p runtime -- --nocapture`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
cd /Users/janvaca/devdone-labs/runtime
git add crates/runtime/src/oci.rs
git commit -m "feat: read-only root filesystem with tmpfs writable overlays

Set rootfs readonly: true. Add tmpfs mounts for /run (64MB) and
/var/log (32MB). Add 256MB size limit to /tmp.
System dirs are now truly immutable, not just permission-protected.

Ref: nanosandboxai/sandbox#8"
```

---

## Task 4: Enhanced Resource Limits

**Repo:** `runtime` (branch `feat/rootless-package-installation`)
**Files:**
- Modify: `crates/runtime/src/oci.rs:37-43` (rlimits)
- Modify: `crates/runtime/src/oci.rs:179` (pids_limit)

- [ ] **Step 1: Write the failing test**

Add to `crates/runtime/src/oci.rs` inside the `mod tests` block:

```rust
#[test]
fn test_enhanced_resource_limits() {
    let config = SandboxConfig::builder()
        .name("test-sandbox")
        .image("alpine:latest")
        .cpus(1)
        .memory_mb(256)
        .build();

    let oci_config = generate_config(&config, Path::new("rootfs"));

    // Check rlimits
    let rlimits = oci_config["process"]["rlimits"].as_array().expect("rlimits must be array");
    let rlimit_types: Vec<&str> = rlimits.iter().filter_map(|r| r["type"].as_str()).collect();

    assert!(rlimit_types.contains(&"RLIMIT_NOFILE"), "must have RLIMIT_NOFILE");
    assert!(rlimit_types.contains(&"RLIMIT_NPROC"), "must have RLIMIT_NPROC");
    assert!(rlimit_types.contains(&"RLIMIT_FSIZE"), "must have RLIMIT_FSIZE");
    assert!(rlimit_types.contains(&"RLIMIT_AS"), "must have RLIMIT_AS");

    // Check PID cgroup limit is 512
    let pids_limit = oci_config["linux"]["resources"]["pids"]["limit"].as_i64().unwrap();
    assert_eq!(pids_limit, 512, "PID limit must be 512");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/janvaca/devdone-labs/runtime && cargo test -p runtime test_enhanced_resource_limits -- --nocapture`
Expected: FAIL — missing RLIMIT_NPROC, RLIMIT_FSIZE, RLIMIT_AS; PID limit is 256

- [ ] **Step 3: Update rlimits and PID limit**

In `crates/runtime/src/oci.rs`, replace the rlimits block (lines 37-43):

```rust
            "rlimits": [
                {
                    "type": "RLIMIT_NOFILE",
                    "hard": 1024,
                    "soft": 1024
                },
                {
                    "type": "RLIMIT_NPROC",
                    "hard": 512,
                    "soft": 512
                },
                {
                    "type": "RLIMIT_FSIZE",
                    "hard": 1073741824_i64,
                    "soft": 1073741824_i64
                },
                {
                    "type": "RLIMIT_AS",
                    "hard": 8589934592_i64,
                    "soft": 8589934592_i64
                }
            ],
```

Change line 179 from:
```rust
    let pids_limit = 256_i64;
```
To:
```rust
    let pids_limit = 512_i64;
```

- [ ] **Step 4: Run all tests**

Run: `cd /Users/janvaca/devdone-labs/runtime && cargo test -p runtime -- --nocapture`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
cd /Users/janvaca/devdone-labs/runtime
git add crates/runtime/src/oci.rs
git commit -m "feat: enhanced resource limits — NPROC, FSIZE, AS; PID 256→512

Add RLIMIT_NPROC (512), RLIMIT_FSIZE (1GB), RLIMIT_AS (8GB).
Increase PID cgroup limit from 256 to 512 for apt-get/unshare.

Ref: nanosandboxai/sandbox#8"
```

---

## Task 5: Remove sudo and Strip suid Binaries in Dockerfile

**Repo:** `agents-registry` (branch `feat/rootless-package-installation`)
**Files:**
- Modify: `docker/Dockerfile.base:19-42`

- [ ] **Step 1: Remove sudo from apt-get install**

In `docker/Dockerfile.base`, change the system dependencies block (lines 19-31). Remove `sudo \` from line 29:

```dockerfile
# System dependencies (agents + networking + SSH access)
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    ca-certificates \
    git \
    iproute2 \
    iptables \
    nftables \
    bash \
    procps \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /root/.ssh \
    && chmod 700 /root/.ssh
```

- [ ] **Step 2: Remove sudoers line and strip suid/sgid binaries**

In `docker/Dockerfile.base`, replace the user creation block (lines 37-42):

```dockerfile
# Create non-root user for running code agents.
# Agents like Claude Code refuse --dangerously-skip-permissions as root.
# No sudo — agents install packages via rootless wrappers (unshare + user prefix).
RUN usermod -l developer -d /home/developer -m node \
    && groupmod -n developer node \
    && mkdir -p /home/developer/.ssh \
    && chmod 700 /home/developer/.ssh \
    && chown -R developer:developer /home/developer \
    && find / -perm /6000 -type f -exec chmod a-s {} + 2>/dev/null || true
```

- [ ] **Step 3: Build the image to verify it works**

Run: `cd /Users/janvaca/devdone-labs/agents-registry && docker build -f docker/Dockerfile.base -t nanosb-base-test . 2>&1 | tail -5`
Expected: Successfully built (or successfully tagged)

- [ ] **Step 4: Verify sudo is removed and no suid binaries exist**

Run:
```bash
docker run --rm nanosb-base-test sh -c "which sudo 2>/dev/null && echo 'FAIL: sudo found' || echo 'OK: sudo not found'"
docker run --rm nanosb-base-test sh -c "find / -perm /4000 -type f 2>/dev/null | head -5; echo 'suid check done'"
```
Expected: "OK: sudo not found" and no suid binaries listed

- [ ] **Step 5: Commit**

```bash
cd /Users/janvaca/devdone-labs/agents-registry
git add docker/Dockerfile.base
git commit -m "feat: remove sudo, strip suid/sgid binaries

Remove sudo package and sudoers.d/developer. Strip suid/sgid bits
from all binaries. No privilege escalation paths remain.

Ref: nanosandboxai/sandbox#8"
```

---

## Task 6: Create User-Local Install Prefix and apt Wrapper

**Repo:** `agents-registry` (branch `feat/rootless-package-installation`)
**Files:**
- Create: `docker/rootless-apt-wrapper.sh`
- Modify: `docker/Dockerfile.base`

- [ ] **Step 1: Create the apt-get wrapper script**

Create `docker/rootless-apt-wrapper.sh`:

```sh
#!/bin/sh
# Rootless apt-get/apt wrapper for nanosandbox code agents.
# Uses user namespace (unshare --user) to satisfy dpkg UID checks.
# All state and installed files go to /home/developer/.local/.
#
# This wrapper is placed at /usr/local/bin/apt-get and /usr/local/bin/apt,
# taking PATH precedence over /usr/bin/apt-get and /usr/bin/apt.

PREFIX="/home/developer/.local"

# Audit log
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $(basename "$0") $*" \
    >> "$PREFIX/var/log/pkg-install.log" 2>/dev/null

# Resolve the real binary name (apt-get or apt) based on how we were invoked
REAL_BIN="/usr/bin/$(basename "$0")"

exec unshare --user --map-root-user -- \
    "$REAL_BIN" \
    -o Dir::State="$PREFIX/var/lib/apt" \
    -o Dir::Cache="$PREFIX/cache/apt" \
    -o Dir::Etc="$PREFIX/etc/apt" \
    -o Dir::Log="$PREFIX/var/log" \
    -o DPkg::Options::="--instdir=$PREFIX/usr" \
    -o DPkg::Options::="--admindir=$PREFIX/var/lib/dpkg" \
    "$@"
```

- [ ] **Step 2: Add install prefix setup and wrapper installation to Dockerfile**

In `docker/Dockerfile.base`, add after the developer user creation block (after the `find / -perm /6000` line) and before the agent-gateway COPY:

```dockerfile
# Set up rootless package installation prefix under developer's home.
# All apt-get installs go to ~/.local/usr/, all state to ~/.local/var/.
RUN mkdir -p /home/developer/.local/usr/bin \
    /home/developer/.local/usr/lib \
    /home/developer/.local/usr/include \
    /home/developer/.local/usr/share \
    /home/developer/.local/var/lib/apt/lists/partial \
    /home/developer/.local/var/lib/dpkg/info \
    /home/developer/.local/var/lib/dpkg/updates \
    /home/developer/.local/var/lib/dpkg/triggers \
    /home/developer/.local/var/log \
    /home/developer/.local/cache/apt/archives/partial \
    /home/developer/.local/etc/apt/apt.conf.d \
    /home/developer/.local/etc/apt/sources.list.d \
    /home/developer/.local/etc/apt/preferences.d \
    /home/developer/.local/bin \
    /home/developer/.local/lib \
    /home/developer/.npm-global/bin \
    /home/developer/.npm-global/lib/node_modules \
    /home/developer/go/bin \
    && cp /var/lib/dpkg/status /home/developer/.local/var/lib/dpkg/status \
    && cp /var/lib/dpkg/available /home/developer/.local/var/lib/dpkg/available \
    && cp /etc/apt/sources.list /home/developer/.local/etc/apt/sources.list 2>/dev/null \
    || cp /etc/apt/sources.list.d/*.sources /home/developer/.local/etc/apt/sources.list.d/ 2>/dev/null || true \
    && chown -R developer:developer /home/developer/.local /home/developer/.npm-global /home/developer/go

# Install rootless apt-get/apt wrappers (take PATH precedence over /usr/bin/)
COPY docker/rootless-apt-wrapper.sh /usr/local/bin/apt-get
COPY docker/rootless-apt-wrapper.sh /usr/local/bin/apt
RUN chmod +x /usr/local/bin/apt-get /usr/local/bin/apt
```

- [ ] **Step 3: Build and test the wrapper**

Run:
```bash
cd /Users/janvaca/devdone-labs/agents-registry
docker build -f docker/Dockerfile.base -t nanosb-base-test . 2>&1 | tail -5
```
Expected: Successfully built

Run:
```bash
docker run --rm nanosb-base-test sh -c "ls -la /usr/local/bin/apt-get /usr/local/bin/apt"
docker run --rm nanosb-base-test sh -c "ls -la /home/developer/.local/var/lib/dpkg/status"
docker run --rm nanosb-base-test sh -c "cat /usr/local/bin/apt-get | head -3"
```
Expected: Wrapper scripts exist, dpkg status copied, wrapper content visible

- [ ] **Step 4: Commit**

```bash
cd /Users/janvaca/devdone-labs/agents-registry
git add docker/rootless-apt-wrapper.sh docker/Dockerfile.base
git commit -m "feat: user-local install prefix and rootless apt wrapper

Create /home/developer/.local/ directory tree for all package
installs. Add apt-get/apt wrapper using unshare --user that
redirects all apt/dpkg state to the user-local prefix.
Initialize dpkg database from system copy.

Ref: nanosandboxai/sandbox#8"
```

---

## Task 7: Update nanosb-init.sh for Install Prefix Preparation

**Repo:** `agents-registry` (branch `feat/rootless-package-installation`)
**Files:**
- Modify: `docker/nanosb-init.sh`

- [ ] **Step 1: Add install prefix preparation section**

In `docker/nanosb-init.sh`, add a new section after the agent state symlinks section (after line 187, before the SSH key setup section):

```sh
# ---------------------------------------------------------------
# 1d. Prepare rootless package installation prefix
# ---------------------------------------------------------------
# Ensure the .local/ directory tree exists and has correct ownership.
# The Dockerfile pre-creates this, but virtiofs mounts or first-boot
# conditions may require re-initialization.
PKG_PREFIX="/home/developer/.local"
if [ ! -f "$PKG_PREFIX/var/lib/dpkg/status" ]; then
    echo "nanosb-init: initializing package install prefix"
    mkdir -p "$PKG_PREFIX/usr/bin" \
        "$PKG_PREFIX/usr/lib" \
        "$PKG_PREFIX/usr/include" \
        "$PKG_PREFIX/usr/share" \
        "$PKG_PREFIX/var/lib/apt/lists/partial" \
        "$PKG_PREFIX/var/lib/dpkg/info" \
        "$PKG_PREFIX/var/lib/dpkg/updates" \
        "$PKG_PREFIX/var/lib/dpkg/triggers" \
        "$PKG_PREFIX/var/log" \
        "$PKG_PREFIX/cache/apt/archives/partial" \
        "$PKG_PREFIX/etc/apt/apt.conf.d" \
        "$PKG_PREFIX/etc/apt/sources.list.d" \
        "$PKG_PREFIX/etc/apt/preferences.d" \
        "$PKG_PREFIX/bin" \
        "$PKG_PREFIX/lib" \
        2>/dev/null || true
    cp /var/lib/dpkg/status "$PKG_PREFIX/var/lib/dpkg/status" 2>/dev/null || true
    cp /var/lib/dpkg/available "$PKG_PREFIX/var/lib/dpkg/available" 2>/dev/null || true
    cp /etc/apt/sources.list "$PKG_PREFIX/etc/apt/sources.list" 2>/dev/null || true
    cp /etc/apt/sources.list.d/*.sources "$PKG_PREFIX/etc/apt/sources.list.d/" 2>/dev/null || true
    echo "nanosb-init: package install prefix ready"
fi
chown -R developer:developer "$PKG_PREFIX" 2>/dev/null || true
```

- [ ] **Step 2: Commit**

```bash
cd /Users/janvaca/devdone-labs/agents-registry
git add docker/nanosb-init.sh
git commit -m "feat: init script prepares rootless package install prefix

Ensure .local/ directory tree is initialized on first boot.
Copies dpkg database and apt sources if not already present.

Ref: nanosandboxai/sandbox#8"
```

---

## Task 8: Update agent-gateway Environment Variables and PATH

**Repo:** `agents-registry` (branch `feat/rootless-package-installation`)
**Files:**
- Modify: `agent-gateway/main.go:332-355`

- [ ] **Step 1: Update PATH and add rootless env vars**

In `agent-gateway/main.go`, replace lines 342-345 (the HOME/USER/PATH/TERM block):

```go
	cmdEnv = setEnv(cmdEnv, "HOME", "/home/developer")
	cmdEnv = setEnv(cmdEnv, "USER", "developer")
	cmdEnv = ensureEnv(cmdEnv, "PATH", "/home/developer/.local/usr/bin:/home/developer/.local/bin:/home/developer/.cargo/bin:/home/developer/go/bin:/home/developer/.npm-global/bin:/home/developer/.local/share/gem/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
	cmdEnv = ensureEnv(cmdEnv, "TERM", "dumb")
	// Rootless package manager configuration: all installs go to user-local prefix.
	cmdEnv = ensureEnv(cmdEnv, "PIP_USER", "1")
	cmdEnv = ensureEnv(cmdEnv, "NPM_CONFIG_PREFIX", "/home/developer/.npm-global")
	cmdEnv = ensureEnv(cmdEnv, "GEM_HOME", "/home/developer/.local/share/gem")
	cmdEnv = ensureEnv(cmdEnv, "GOPATH", "/home/developer/go")
	cmdEnv = ensureEnv(cmdEnv, "CARGO_HOME", "/home/developer/.cargo")
	cmdEnv = ensureEnv(cmdEnv, "RUSTUP_HOME", "/home/developer/.rustup")
	cmdEnv = ensureEnv(cmdEnv, "LD_LIBRARY_PATH", "/home/developer/.local/usr/lib:/home/developer/.local/lib")
```

- [ ] **Step 2: Build agent-gateway to verify compilation**

Run:
```bash
cd /Users/janvaca/devdone-labs/agents-registry/agent-gateway
go build -o /dev/null . 2>&1
```
Expected: No errors

- [ ] **Step 3: Commit**

```bash
cd /Users/janvaca/devdone-labs/agents-registry
git add agent-gateway/main.go
git commit -m "feat: rootless env vars — PATH, PIP_USER, NPM_CONFIG_PREFIX, etc.

Update PATH to include user-local bin dirs first.
Set PIP_USER=1, NPM_CONFIG_PREFIX, GEM_HOME, GOPATH, CARGO_HOME,
RUSTUP_HOME, LD_LIBRARY_PATH for rootless package installation.

Ref: nanosandboxai/sandbox#8"
```

---

## Task 9: Push All Branches

**Repos:** all three

- [ ] **Step 1: Push runtime branch**

```bash
cd /Users/janvaca/devdone-labs/runtime
git push -u origin feat/rootless-package-installation
```

- [ ] **Step 2: Push agents-registry branch**

```bash
cd /Users/janvaca/devdone-labs/agents-registry
git push -u origin feat/rootless-package-installation
```

- [ ] **Step 3: Push sandbox branch (spec + plan docs)**

```bash
cd /Users/janvaca/devdone-labs/sandbox
git add docs/superpowers/specs/2026-05-08-rootless-package-installation-design.md \
        docs/superpowers/plans/2026-05-08-rootless-package-installation.md
git commit -m "docs: rootless package installation design spec and implementation plan

Ref: nanosandboxai/sandbox#8"
git push -u origin feat/rootless-package-installation
```
