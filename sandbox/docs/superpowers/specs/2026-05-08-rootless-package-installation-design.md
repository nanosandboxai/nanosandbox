# Rootless Package Installation for Code Agents

**Date:** 2026-05-08
**Issue:** https://github.com/nanosandboxai/sandbox/issues/8
**Status:** Draft

## Problem

Code agents running inside nanosandbox microVMs cannot install development dependencies
(Python, Go, Rust, gcc, etc.) because:

1. The `developer` user (UID 1000) does not have root access
2. `apt-get` fails with `Permission denied` when writing to `/var/lib/apt/lists/partial`
3. No languages are pre-installed in the base image by design
4. The current workaround (`sudo NOPASSWD:ALL`) grants unrestricted root, violating the security model

## Goals

- Code agents can install any development tool at runtime without root or sudo
- All installed packages live under `/home/developer/` (agent's own filesystem)
- `/workspace` remains clean — only project source code, nothing installed
- System directories (`/usr`, `/etc`, `/var`) are immutable to the agent
- No privilege escalation paths exist (no sudo, no suid, no su)
- Apply every available security hardening layer (seccomp, capabilities, read-only rootfs, network policy, resource limits)

## Non-Goals

- Pre-installing languages in the base image (agents install what they need)
- Supporting system-level packages (databases, systemd services)
- Modifying the hypervisor or kernel security model

---

## Security Architecture (Defense in Depth — 10 Layers)

```
Layer  1: Hardware VM       — libkrun microVM (KVM/HVF hypervisor boundary)
Layer  2: Kernel hardening  — CONFIG_MODULES=n, no kexec, no ftrace, no swap
Layer  3: Seccomp profile   — Block dangerous syscalls (NEW)
Layer  4: Capabilities      — Reduced from 14 to 7, no ambient caps (NEW)
Layer  5: Read-only rootfs  — System dirs truly immutable via ro root (NEW)
Layer  6: Filesystem jail   — Agent confined to /home/developer/ only
Layer  7: Privilege removal  — No sudo, no suid, no su, noNewPrivileges
Layer  8: Resource limits   — PID, memory, CPU, file size, address space, open files (ENHANCED)
Layer  9: Network policy    — Egress restricted to package registries (NEW)
Layer 10: Audit             — Package installation logged
```

---

## Layer 1: Hardware VM Isolation (Existing — No Changes)

- libkrun microVM with dedicated kernel per sandbox
- KVM (Linux) / HVF (macOS) / WHPX (Windows) hypervisor boundary
- Single-tenant, disposable VM
- Any kernel exploit is contained within the VM

## Layer 2: Kernel Hardening (Existing — No Changes)

Already configured in `libkrunfw` kernel configs:

```
CONFIG_MODULES=n          — No runtime module loading
CONFIG_KEXEC=n            — Cannot replace running kernel
CONFIG_FTRACE=n           — No kernel function tracing
CONFIG_SWAP=n             — No swap (prevents data leakage)
CONFIG_USER_NS=y          — Needed for unshare wrapper
CONFIG_SECCOMP=y          — Syscall filtering
CONFIG_SECCOMP_FILTER=y   — BPF-based seccomp
CONFIG_AUDIT=y            — Audit framework available
```

LSM stack: `lockdown,yama,loadpin,safesetid,integrity,selinux,bpf`

## Layer 3: Seccomp Profile (NEW)

Block syscalls that a code agent never needs. Applied in `oci.rs` under `linux.seccomp`.

**Default action:** `SCMP_ACT_ALLOW` (allowlist is impractical for dev tools; blocklist dangerous calls)

**Blocked syscalls:**

| Syscall | Why blocked |
|---------|-------------|
| `kexec_load`, `kexec_file_load` | Kernel replacement |
| `init_module`, `finit_module`, `delete_module` | Module loading (also CONFIG disabled) |
| `reboot` | VM reboot |
| `mount`, `umount2` | Mount manipulation outside init (note: allowed via unshare for apt wrapper) |
| `pivot_root` | Root filesystem change |
| `swapon`, `swapoff` | Swap manipulation (also CONFIG disabled) |
| `acct` | Process accounting manipulation |
| `settimeofday`, `clock_settime`, `clock_adjtime`, `adjtimex` | Time manipulation |
| `add_key`, `keyctl`, `request_key` | Kernel keyring access |
| `ptrace` | Process debugging/injection |
| `userfaultfd` | Frequently used in kernel exploits |
| `perf_event_open` | Kernel performance counters (info leak) |
| `bpf` | eBPF program loading (exploit vector) |
| `io_uring_setup`, `io_uring_enter`, `io_uring_register` | io_uring subsystem (CVE-heavy) |
| `lookup_dcookie` | Kernel profiling info leak |
| `mbind`, `move_pages`, `migrate_pages` | NUMA memory manipulation |
| `personality` | Change execution domain (bypass ASLR) |
| `vm86`, `vm86old` | Legacy x86 virtual 8086 mode |
| `modify_ldt` | Modify local descriptor table |
| `open_by_handle_at` | Bypass path-based access control |
| `name_to_handle_at` | File handle operations (container escape vector) |

**Note on `mount`:** The apt-get wrapper uses `unshare --user --map-root-user` which
creates a user namespace. Inside that namespace, `mount` is permitted by the kernel for
the namespace creator. The seccomp block on `mount` applies to the main namespace only.
However, since seccomp filters are inherited by child processes, we must handle this
carefully: the wrapper script itself invokes `unshare` which internally does the namespace
setup before the seccomp filter takes effect. Alternative: use `SCMP_ACT_ERRNO` for mount
with an exception for `MS_BIND` operations, or omit mount from the blocklist and rely on
the read-only rootfs + filesystem permissions instead.

**Recommended approach:** Do NOT block `mount` in seccomp (the read-only rootfs + permissions
already prevent unauthorized mounts). Block all other syscalls listed above.

## Layer 4: Capability Reduction (NEW)

**Current:** 14 capabilities in all 5 sets (bounding, effective, inheritable, permitted, ambient)

**New:** 7 capabilities, ambient and inheritable sets EMPTY

| Capability | Keep | Reason |
|------------|------|--------|
| `CAP_CHOWN` | YES | Package installation needs chown |
| `CAP_DAC_OVERRIDE` | **DROP** | Bypasses ALL file permission checks |
| `CAP_FSETID` | YES | Needed for extracting tarballs |
| `CAP_FOWNER` | **DROP** | Bypasses ownership checks |
| `CAP_MKNOD` | **DROP** | Create device files — never needed |
| `CAP_NET_RAW` | **DROP** | Raw sockets (ping, packet sniffing) |
| `CAP_SETGID` | YES | User namespace (unshare) needs it |
| `CAP_SETUID` | YES | User namespace (unshare) needs it |
| `CAP_SETFCAP` | **DROP** | Set file capabilities — blocked by noNewPrivileges anyway |
| `CAP_SETPCAP` | **DROP** | Modify capability sets |
| `CAP_NET_BIND_SERVICE` | YES | Dev servers may bind ports < 1024 |
| `CAP_SYS_CHROOT` | **DROP** | chroot not needed |
| `CAP_KILL` | YES | Signal management for processes |
| `CAP_AUDIT_WRITE` | YES | Audit logging |

**Capability sets:**
```json
{
    "bounding":    ["CAP_CHOWN","CAP_FSETID","CAP_SETGID","CAP_SETUID","CAP_NET_BIND_SERVICE","CAP_KILL","CAP_AUDIT_WRITE"],
    "effective":   ["CAP_CHOWN","CAP_FSETID","CAP_SETGID","CAP_SETUID","CAP_NET_BIND_SERVICE","CAP_KILL","CAP_AUDIT_WRITE"],
    "permitted":   ["CAP_CHOWN","CAP_FSETID","CAP_SETGID","CAP_SETUID","CAP_NET_BIND_SERVICE","CAP_KILL","CAP_AUDIT_WRITE"],
    "inheritable": [],
    "ambient":     []
}
```

**Why empty ambient/inheritable:** Ambient capabilities are inherited by ALL child processes
even without setuid. By clearing them, child processes (agent commands) start with no
special capabilities unless explicitly granted. This follows the principle of least privilege.

## Layer 5: Read-Only Root Filesystem (NEW)

**Change in `oci.rs`:**
```json
"root": {
    "path": "rootfs",
    "readonly": true
}
```

**Writable areas via tmpfs overlays:**

| Path | Type | Size | Purpose |
|------|------|------|---------|
| `/home/developer` | virtiofs or tmpfs | — | Agent home (packages, config, state) |
| `/workspace` | virtiofs | — | Git project (mounted from host) |
| `/tmp` | tmpfs | 256MB | Temporary files |
| `/run` | tmpfs | 64MB | Runtime state (PID files, sockets) |
| `/var/log` | tmpfs | 32MB | Log files |
| `/dev/shm` | tmpfs | 64MB | Shared memory (existing) |
| `/etc/resolv.conf` | bind mount (rw) | — | DNS config (agent-gateway writes this) |
| `/etc/dropbear` | tmpfs | 1MB | SSH host keys (9P mode) |
| `/root/.ssh` | tmpfs | 1MB | Root SSH keys (init only) |
| `/home/developer/.ssh` | tmpfs | 1MB | Developer SSH keys |

**Everything else is read-only.** The agent literally cannot modify `/usr`, `/etc` (except
resolv.conf), `/var`, `/lib`, `/sbin`, or any other system directory.

## Layer 6: Filesystem Jail (NEW)

The agent is confined to `/home/developer/` for all writes:

```
/home/developer/                    ← agent's home (writable)
├── .local/                         ← ALL installed packages
│   ├── bin/                        ← installed binaries (pip scripts, etc.)
│   ├── lib/                        ← installed libraries
│   ├── usr/                        ← apt-get --instdir target
│   │   ├── bin/                    ← apt-installed binaries
│   │   ├── lib/                    ← apt-installed libraries
│   │   ├── include/                ← headers
│   │   └── share/                  ← man pages, docs
│   ├── var/
│   │   ├── lib/
│   │   │   ├── apt/               ← apt package state + lists
│   │   │   └── dpkg/              ← dpkg database
│   │   └── log/
│   │       └── pkg-install.log    ← audit log
│   ├── cache/
│   │   └── apt/                   ← downloaded .deb files
│   │       └── archives/
│   └── etc/
│       └── apt/                   ← custom sources.list, apt.conf
│           ├── sources.list
│           └── apt.conf
├── .cargo/bin/                     ← cargo install
├── .npm-global/                    ← npm install -g
│   ├── bin/
│   └── lib/node_modules/
├── go/bin/                         ← go install
├── .rustup/                        ← rustup toolchains
├── .nanosb-state/                  ← agent session state (existing)
│
└── workspace/ → /workspace         ← git project (virtiofs mount)
    ├── src/                        ← source code — ONLY this gets committed
    ├── package.json
    └── ...

/usr/                               ← READ-ONLY (rootfs)
/etc/                               ← READ-ONLY (rootfs, except resolv.conf)
/var/                               ← READ-ONLY (rootfs)
/lib/                               ← READ-ONLY (rootfs)
/sbin/                              ← READ-ONLY (rootfs)
```

## Layer 7: Privilege Removal (NEW)

**Remove from Dockerfile.base:**
- `sudo` package (purge entirely)
- `/etc/sudoers.d/developer` file
- All suid/sgid bits: `find / -perm /6000 -exec chmod a-s {} + 2>/dev/null || true`
- `su` binary: `chmod a-s /usr/bin/su` (or remove)

**Existing (no changes needed):**
- `noNewPrivileges: true` in OCI config — blocks setuid/setgid escalation
- Agent-gateway drops to UID 1000 via `SysProcAttr.Credential`

**Verification:** No binary on the filesystem has suid/sgid bits set.

## Layer 8: Resource Limits (ENHANCED)

**OCI rlimits — current vs new:**

| Limit | Current | New | Purpose |
|-------|---------|-----|---------|
| `RLIMIT_NOFILE` | 1024 / 1024 | 1024 / 1024 | Max open file descriptors |
| `RLIMIT_NPROC` | — | 512 / 512 | Max processes per user (NEW) |
| `RLIMIT_FSIZE` | — | 1GB / 1GB | Max file size — prevents disk exhaustion (NEW) |
| `RLIMIT_AS` | — | 8GB / 8GB | Max address space — prevents memory exhaustion (NEW) |

**OCI cgroup limits — current vs new:**

| Limit | Current | New | Purpose |
|-------|---------|-----|---------|
| Memory | config.memory_mb | no change | Hard memory limit |
| Swap | = memory (disabled) | no change | Swap disabled |
| CPU quota | cpus * 100000 | no change | CPU hard limit |
| PIDs | 256 | **512** | Increased for apt-get + unshare + dpkg |

**tmpfs size limits (NEW):**

| Mount | Size | Purpose |
|-------|------|---------|
| `/tmp` | 256MB | Temp files (was unlimited) |
| `/dev/shm` | 64MB | Shared memory (was 64MB, no change) |
| `/dev` | 64MB | Device nodes (was 64MB, no change) |
| `/run` | 64MB | Runtime state (NEW) |
| `/var/log` | 32MB | Logs (NEW) |
| `/etc/dropbear` | 1MB | SSH host keys (NEW) |
| `/root/.ssh` | 1MB | Root SSH (NEW) |
| `/home/developer/.ssh` | 1MB | Dev SSH (NEW) |

## Layer 9: Network Egress Policy (NEW)

Restrict outbound network connections to known package registries and essential services.

**Allowed destinations:**

| Category | Domains |
|----------|---------|
| Debian packages | `deb.debian.org`, `security.debian.org`, `cdn-fastly.deb.debian.org` |
| Node.js | `registry.npmjs.org`, `nodejs.org` |
| Python | `pypi.org`, `files.pythonhosted.org` |
| Rust | `crates.io`, `static.crates.io`, `static.rust-lang.org` |
| Go | `proxy.golang.org`, `sum.golang.org`, `storage.googleapis.com` |
| Ruby | `rubygems.org` |
| GitHub | `github.com`, `raw.githubusercontent.com`, `objects.githubusercontent.com` |
| DNS | `8.8.8.8`, `1.1.1.1`, `8.8.4.4` |
| General | `api.anthropic.com`, `api.openai.com` (agent API access) |

**Implementation options:**

1. **Agent-gateway level (recommended):** Configure network scope via sandbox config.
   Add an `allowed_egress_domains` list to `SandboxConfig`. Agent-gateway or the VM
   runtime enforces this via DNS filtering or iptables rules set during init.

2. **VM-level nftables:** Add rules in `nanosb-init.sh` that restrict outbound to
   resolved IPs of allowed domains. Requires periodic DNS refresh.

3. **Proxy-based:** Route all HTTP(S) through a forward proxy that enforces domain allowlists.
   Heaviest but most reliable.

**Note:** This is a policy layer. Some agents need arbitrary API access (e.g., calling
a user's backend service). This should be configurable per-sandbox via `sandbox.yml`:

```yaml
sandboxes:
  my-agent:
    network:
      egress_policy: restricted    # or "unrestricted" (default for backward compat)
      allowed_domains:
        - api.myservice.com        # additional allowed domains
```

**Default:** `unrestricted` for backward compatibility. Users opt-in to `restricted` mode.

## Layer 10: Audit (NEW)

**Package installation audit log:**
The apt-get wrapper logs every invocation to `~/.local/var/log/pkg-install.log`:
```
2026-05-08T12:34:56Z apt-get install python3 golang-go
2026-05-08T12:35:12Z apt-get install build-essential
```

**Agent-gateway audit:**
Agent-gateway already logs all exec requests via `log.Printf("[agent-gateway] exec: ...")`.
No changes needed for command-level audit.

---

## Execution Model

```
agent-gateway (root, PID 1)
  └── SSH session (developer, UID 1000)
       └── claude-code / codex / goose (developer, UID 1000)
            │
            ├── apt-get install python3
            │   └── /usr/local/bin/apt-get (wrapper)
            │       └── unshare --user --map-root-user
            │           └── /usr/bin/apt-get -o Dir::State=... -o Dir::Cache=...
            │               └── installs to /home/developer/.local/usr/
            │
            ├── pip install flask
            │   └── direct (PIP_USER=1 → ~/.local/lib/python3/)
            │
            ├── npm install -g typescript
            │   └── direct (NPM_CONFIG_PREFIX=~/.npm-global)
            │
            ├── cargo install ripgrep
            │   └── direct (~/.cargo/bin/)
            │
            └── ANY other tool
                └── direct (most install to user dirs by default)
                    OR fails gracefully (no root to damage system)
```

### apt-get Wrapper Detail

**Wrapper (`/usr/local/bin/apt-get`):**
```sh
#!/bin/sh
# Rootless apt-get wrapper for nanosandbox code agents
# Uses user namespace to satisfy dpkg UID checks
# All state and installs go to /home/developer/.local/

PREFIX="/home/developer/.local"

# Audit log
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) apt-get $*" >> "$PREFIX/var/log/pkg-install.log" 2>/dev/null

exec unshare --user --map-root-user -- \
    /usr/bin/apt-get \
    -o Dir::State="$PREFIX/var/lib/apt" \
    -o Dir::Cache="$PREFIX/cache/apt" \
    -o Dir::Etc="$PREFIX/etc/apt" \
    -o Dir::Log="$PREFIX/var/log" \
    -o DPkg::Options::="--instdir=$PREFIX/usr" \
    -o DPkg::Options::="--admindir=$PREFIX/var/lib/dpkg" \
    "$@"
```

An identical wrapper is created for `/usr/local/bin/apt`.

The real binaries remain at `/usr/bin/apt-get` and `/usr/bin/apt`.
The wrappers in `/usr/local/bin/` take PATH precedence.

### Environment Variables

Set in agent-gateway `streamCommand()` for ALL spawned processes:

```
# User-local install paths (make all package managers rootless by default)
PIP_USER=1
NPM_CONFIG_PREFIX=/home/developer/.npm-global
GEM_HOME=/home/developer/.local/share/gem
GOPATH=/home/developer/go

# Extended PATH (user-installed binaries take precedence)
PATH=/home/developer/.local/usr/bin:/home/developer/.local/bin:/home/developer/.cargo/bin:/home/developer/go/bin:/home/developer/.npm-global/bin:/home/developer/.local/share/gem/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
```

### Init Script Changes (nanosb-init.sh)

Add a new section before agent-gateway exec:

1. Create `.local/` directory tree with all subdirs
2. Initialize dpkg database: copy `/var/lib/dpkg/status` and `/var/lib/dpkg/available` to `~/.local/var/lib/dpkg/`
3. Create `sources.list` in `~/.local/etc/apt/` (copy from `/etc/apt/sources.list`)
4. Create `apt.conf` with custom dir overrides
5. Set ownership: `chown -R developer:developer /home/developer/.local`

---

## Changes by Repository

### 1. agents-registry (`feat/rootless-package-installation`)

**`docker/Dockerfile.base`:**
- Remove `sudo` from apt-get install line
- Remove `echo "developer ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/developer`
- Strip all suid/sgid: `find / -perm /6000 -exec chmod a-s {} + 2>/dev/null || true`
- Create `.local/` directory tree (bin, lib, usr, var, cache, etc) owned by developer
- Create apt-get and apt wrapper scripts at `/usr/local/bin/`
- Initialize dpkg database in `~/.local/var/lib/dpkg/`
- Create apt.conf and sources.list in `~/.local/etc/apt/`
- Set LD_LIBRARY_PATH or create ldconfig entry for `~/.local/usr/lib/`

**`docker/nanosb-init.sh`:**
- Add section to verify/repair `.local/` directory tree after virtiofs mount
- Ensure dpkg database is initialized on first boot

**`agent-gateway/main.go`:**
- Update PATH in `setEnv`/`ensureEnv` to include all user-local bin directories
- Add env vars: `PIP_USER`, `NPM_CONFIG_PREFIX`, `GEM_HOME`, `GOPATH`
- Set `LD_LIBRARY_PATH` to include `~/.local/usr/lib`

### 2. runtime (`feat/rootless-package-installation`)

**`crates/runtime/src/oci.rs`:**

*Seccomp profile:*
- Add `linux.seccomp` section with `SCMP_ACT_ALLOW` default and `SCMP_ACT_ERRNO` for blocked syscalls
- Block: `kexec_load`, `kexec_file_load`, `init_module`, `finit_module`, `delete_module`,
  `reboot`, `pivot_root`, `swapon`, `swapoff`, `acct`, `settimeofday`, `clock_settime`,
  `clock_adjtime`, `adjtimex`, `add_key`, `keyctl`, `request_key`, `ptrace`, `userfaultfd`,
  `perf_event_open`, `bpf`, `io_uring_setup`, `io_uring_enter`, `io_uring_register`,
  `lookup_dcookie`, `mbind`, `move_pages`, `migrate_pages`, `personality`, `vm86`,
  `vm86old`, `modify_ldt`, `open_by_handle_at`, `name_to_handle_at`

*Capabilities:*
- Reduce from 14 to 7: keep `CAP_CHOWN`, `CAP_FSETID`, `CAP_SETGID`, `CAP_SETUID`,
  `CAP_NET_BIND_SERVICE`, `CAP_KILL`, `CAP_AUDIT_WRITE`
- Clear `inheritable` and `ambient` sets (set to empty arrays)

*Read-only rootfs:*
- Change `"readonly": false` to `"readonly": true`
- Add tmpfs mounts: `/run` (64MB), `/var/log` (32MB)
- Add bind mount (rw) for `/etc/resolv.conf`
- Add size limit to `/tmp`: `size=256m`
- Add tmpfs mounts for SSH: `/etc/dropbear` (1MB), `/root/.ssh` (1MB), `/home/developer/.ssh` (1MB)

*Resource limits:*
- Add `RLIMIT_NPROC`: 512/512
- Add `RLIMIT_FSIZE`: 1073741824/1073741824 (1GB)
- Add `RLIMIT_AS`: 8589934592/8589934592 (8GB)
- Increase PID cgroup limit from 256 to 512

### 3. sandbox (`feat/rootless-package-installation`)

**`crates/sandbox/src/config/mod.rs` or gateway bootstrap:**
- Update default PATH to include user-local bin directories
- Add rootless package manager env vars to default environment
- Add `egress_policy` and `allowed_domains` to network config (optional, for Layer 9)

**Spec document:**
- `docs/superpowers/specs/2026-05-08-rootless-package-installation-design.md` (this file)

---

## Security Summary Matrix

| Attack Vector | Mitigation | Layer |
|---------------|-----------|-------|
| Kernel exploit via syscall | Seccomp blocks dangerous syscalls | 3 |
| Kernel exploit via user namespace | Contained by hypervisor | 1 |
| Kernel module loading | CONFIG_MODULES=n + seccomp block | 2, 3 |
| Privilege escalation via setuid | noNewPrivileges + suid stripped | 7 |
| Privilege escalation via sudo | sudo removed entirely | 7 |
| Privilege escalation via capabilities | Reduced to 7, no ambient/inheritable | 4 |
| File system tampering | Read-only rootfs | 5 |
| Write outside home dir | Filesystem permissions + ro rootfs | 5, 6 |
| Fork bomb | PID limit 512 + RLIMIT_NPROC | 8 |
| Memory exhaustion | Cgroup memory limit + RLIMIT_AS | 8 |
| Disk exhaustion | RLIMIT_FSIZE + tmpfs size limits | 8 |
| File descriptor exhaustion | RLIMIT_NOFILE 1024 | 8 |
| Process env var snooping | hidepid=2 on /proc | existing |
| /proc information leaks | maskedPaths + readonlyPaths | existing |
| Data exfiltration via network | Egress policy (opt-in) | 9 |
| ptrace-based injection | Seccomp blocks ptrace | 3 |
| io_uring exploits | Seccomp blocks io_uring | 3 |
| eBPF exploits | Seccomp blocks bpf | 3 |
| Container escape via mount | Read-only rootfs + no CAP_SYS_ADMIN | 4, 5 |
| Container escape via open_by_handle_at | Seccomp blocks it | 3 |
| Time manipulation (log tampering) | Seccomp blocks clock_settime | 3 |

---

## Testing Plan

### 1. Functional Tests

- `apt-get update && apt-get install -y python3` works as developer, installs to `~/.local/usr/`
- `pip install flask` installs to `~/.local/lib/python3/`
- `npm install -g typescript` installs to `~/.npm-global/`
- `cargo install ripgrep` installs to `~/.cargo/bin/`
- `go install golang.org/x/tools/gopls@latest` installs to `~/go/bin/`
- Installed binaries are in PATH and executable
- `/workspace` contains no installed package artifacts
- `git status` in `/workspace` is clean after installations

### 2. Security Tests

**Privilege escalation:**
- `sudo` → command not found
- `su` → operation not permitted (suid stripped)
- `find / -perm /4000 2>/dev/null` → empty (no suid binaries)
- `id -u` → 1000 (not root)

**Filesystem immutability:**
- `touch /usr/bin/test` → read-only filesystem
- `touch /etc/test` → read-only filesystem
- `touch /var/test` → read-only filesystem
- `touch /usr/local/bin/test` → read-only filesystem
- `touch /home/developer/test` → success (writable)
- `touch /tmp/test` → success (writable tmpfs)

**Seccomp enforcement:**
- `unshare --pid` → operation not permitted (if blocked) or succeeds (pid ns allowed)
- Python: `import ctypes; ctypes.CDLL(None).reboot(...)` → blocked by seccomp
- Python: `import ctypes; ctypes.CDLL(None).ptrace(...)` → blocked by seccomp

**Capability verification:**
- `cat /proc/self/status | grep Cap` → verify reduced capability set
- `ping 8.8.8.8` → fails (CAP_NET_RAW dropped)
- `mknod /tmp/test-dev c 1 3` → fails (CAP_MKNOD dropped)

**Resource limits:**
- Fork bomb: `:(){ :|:& };:` → killed at 512 processes
- Large file: `dd if=/dev/zero of=/tmp/big bs=1M count=2048` → fails at 1GB
- File descriptors: open > 1024 files → fails

### 3. Isolation Tests

- Package installed in one sandbox not visible in another
- Two concurrent sandboxes do not interfere
- VM destruction cleans up all state

### 4. Network Tests (when egress policy enabled)

- `curl https://pypi.org` → success (allowed)
- `curl https://example.com` → blocked (not in allowlist)
- `apt-get update` → success (deb.debian.org allowed)
