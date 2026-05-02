// Copyright 2024 The libkrun-win Authors
// SPDX-License-Identifier: Apache-2.0

//! Generate a Linux initrd (raw newc cpio) containing the full rootfs for HCS VMs.
//!
//! On Linux/macOS, libkrun uses virtio-fs to share the rootfs directory with
//! the guest kernel. On Windows, HCS doesn't support virtio-fs and the WSL
//! kernel blocks 9p mounts, so we pack the entire rootfs into the initrd.
//! The kernel unpacks it as the root filesystem — same result, different
//! mechanism.

use std::io::Write;
use std::path::{Path, PathBuf};

use log::info;

/// Execution configuration to embed in the initrd as config files.
///
/// init.krun reads these files at boot instead of parsing the kernel cmdline,
/// which avoids quoting issues with env vars and multi-word arguments.
pub struct ExecConfig {
    /// Path to the binary to execute inside the guest (e.g. "/bin/echo").
    pub exec_path: String,
    /// Working directory inside the guest (e.g. "/").
    pub workdir: String,
    /// Environment variables as `KEY=VALUE` strings.
    pub env: Vec<String>,
    /// Command arguments (may include argv[0]).
    pub args: Vec<String>,
}

/// Locate a dependency binary (busybox, vsock_proxy, …) on disk.
///
/// install-deps writes deps under `~/.nanosandbox/libs/` so we check there
/// first. Older layouts and source builds are still honored as fallbacks:
///   1. `~/.nanosandbox/libs/<name>`   (current install layout — install.ps1)
///   2. `~/.nanosandbox/<name>`         (legacy flat layout)
///   3. `<dir-of-current-exe>/<name>`   (portable / dev install)
///   4. `<hcs-crate>/../../<name>`      (cargo source build, repo root)
fn find_dep_bin(name: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // ~/.nanosandbox/libs/<name>  and  ~/.nanosandbox/<name>
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    if let Some(home) = home {
        candidates.push(home.join(".nanosandbox").join("libs").join(name));
        candidates.push(home.join(".nanosandbox").join(name));
    }

    // Next to the current executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
        }
    }

    // Repo source layout (deps/libkrun/src/hcs → repo root).
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join(name),
    );

    candidates
        .into_iter()
        .find(|p| p.exists() && p.is_file())
}

/// Network configuration to embed in the initrd for guest-side setup.
pub struct NetConfig {
    /// Guest IP address (e.g. "172.28.0.2").
    pub ip: String,
    /// Subnet prefix length (e.g. 16).
    pub prefix_len: u8,
    /// Gateway IP address (e.g. "172.28.0.1").
    pub gateway: String,
    /// DNS server(s).
    pub dns: Vec<String>,
}

/// Generate an initrd containing the full rootfs directory, exec config files,
/// and a generated `/init.krun` script.
///
/// If `exec_config` is provided, writes config files to `/etc/krun/` in the
/// initrd. init.krun reads these at boot. If not provided, init.krun falls
/// back to parsing KRUN_INIT/KRUN_WORKDIR from the kernel cmdline.
///
/// If the rootfs lacks `/bin/busybox`, attempts to inject one from the repo's
/// `busybox` file (a static Linux ELF binary).
///
/// Returns the path to the generated initrd file.
pub fn generate_initrd_from_rootfs(
    rootfs_path: &Path,
    exec_config: Option<&ExecConfig>,
    net_config: Option<&NetConfig>,
) -> std::io::Result<PathBuf> {
    let temp_dir = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into())).join(".nanosandbox").join("tmp");
    let _ = std::fs::create_dir_all(&temp_dir);
    let initrd_path = temp_dir.join(format!("libkrun-initrd-{}.img", std::process::id()));

    let start = std::time::Instant::now();
    let mut cpio = Vec::new();
    let mut ino: u32 = 1;

    // Root directory entry
    cpio_entry(&mut cpio, ino, 0o040755, ".", &[], 0, 0);
    ino += 1;

    // Walk the rootfs and add every entry
    let mut file_count = 0u64;
    let mut total_bytes = 0u64;
    walk_dir(
        rootfs_path,
        rootfs_path,
        &mut cpio,
        &mut ino,
        &mut file_count,
        &mut total_bytes,
    )?;

    // Auto-inject busybox if not present in rootfs.
    // init.krun needs it for mount, --install, and basic commands.
    //
    // Handle merged-usr layout: /bin may be a symlink to usr/bin (or a broken
    // symlink file on Windows). Check both locations for busybox.
    let bin_is_symlink_to_usr = {
        let bin_path = rootfs_path.join("bin");
        if bin_path.is_file() {
            // Broken symlink file on Windows — check if it points to usr/bin
            std::fs::read_to_string(&bin_path)
                .ok()
                .map_or(false, |t| t.trim() == "usr/bin")
        } else {
            false
        }
    };
    let has_busybox = rootfs_path.join("bin").join("busybox").exists()
        || (bin_is_symlink_to_usr && rootfs_path.join("usr/bin/busybox").exists());
    if !has_busybox {
        if let Some(busybox_src) = find_dep_bin("busybox") {
            let data = std::fs::read(&busybox_src)?;
            // If /bin is a symlink to usr/bin (merged-usr), inject into usr/bin/
            // so it's accessible via both /bin/busybox and /usr/bin/busybox.
            let inject_dir = if bin_is_symlink_to_usr {
                "usr/bin"
            } else {
                if !rootfs_path.join("bin").exists() {
                    cpio_entry(&mut cpio, ino, 0o040755, "bin", &[], 0, 0);
                    ino += 1;
                }
                "bin"
            };
            cpio_entry(
                &mut cpio,
                ino,
                0o0100755,
                &format!("{}/busybox", inject_dir),
                &data,
                0,
                0,
            );
            ino += 1;
            // Also inject /bin/sh symlink if missing
            let sh_path = format!("{}/sh", inject_dir);
            let has_sh = rootfs_path.join(&sh_path).exists()
                || (bin_is_symlink_to_usr && rootfs_path.join("usr/bin/sh").exists());
            if !has_sh {
                cpio_entry(
                    &mut cpio,
                    ino,
                    0o0120777,
                    &sh_path,
                    b"busybox",
                    0,
                    0,
                );
                ino += 1;
            }
            info!(
                "initrd: injected busybox from {} into {} ({} bytes)",
                busybox_src.display(),
                inject_dir,
                data.len()
            );
        } else {
            info!("initrd: WARNING - no busybox in rootfs and none found in deps search paths");
        }
    }

    // Inject vsock_proxy binary for HvSocket host communication (Windows).
    // This tiny static binary listens on AF_VSOCK port 50001 and proxies
    // connections to the agent-gateway at TCP 127.0.0.1:8080.
    if let Some(vsock_proxy_src) = find_dep_bin("vsock_proxy") {
        let data = std::fs::read(&vsock_proxy_src)?;
        cpio_entry(&mut cpio, ino, 0o0100755, "bin/vsock_proxy", &data, 0, 0);
        ino += 1;
        info!(
            "initrd: injected vsock_proxy from {} ({} bytes)",
            vsock_proxy_src.display(),
            data.len()
        );
    } else {
        info!("initrd: WARNING - vsock_proxy not found in deps search paths");
    }

    // Write exec config as a shell script in /etc/krun/cmd.
    // This avoids all quoting/word-splitting issues — the script properly
    // quotes each argument and env var.
    if let Some(cfg) = exec_config {
        if !rootfs_path.join("etc").exists() {
            cpio_entry(&mut cpio, ino, 0o040755, "etc", &[], 0, 0);
            ino += 1;
        }
        cpio_entry(&mut cpio, ino, 0o040755, "etc/krun", &[], 0, 0);
        ino += 1;

        // Generate a shell script that sets env, cd, and execs the command.
        // Use busybox ash to avoid dynamic linking issues with dash/bash.
        let mut cmd_script = String::from("#!/bin/sh\nexport PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n");
        for env_var in &cfg.env {
            let trimmed = env_var.trim();
            if trimmed.is_empty() || !trimmed.contains('=') || trimmed.starts_with('=') {
                continue;
            }
            // Filter out env vars with names invalid for POSIX shells
            // (e.g. "CommonProgramFiles(x86)" from Windows).
            let var_name = trimmed.split('=').next().unwrap_or("");
            if var_name.is_empty()
                || !var_name.as_bytes()[0].is_ascii_alphabetic() && var_name.as_bytes()[0] != b'_'
                || !var_name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                continue;
            }
            cmd_script.push_str(&format!("export '{}'\n", shell_escape(trimmed)));
        }
        cmd_script.push_str(&format!("cd '{}' 2>/dev/null\n", shell_escape(&cfg.workdir)));
        // Build the exec line with each arg properly quoted
        cmd_script.push_str(&format!("exec '{}'", shell_escape(&cfg.exec_path)));
        for arg in &cfg.args {
            cmd_script.push_str(&format!(" '{}'", shell_escape(arg)));
        }
        cmd_script.push('\n');

        cpio_entry(
            &mut cpio,
            ino,
            0o0100755,
            "etc/krun/cmd",
            cmd_script.as_bytes(),
            0,
            0,
        );
        ino += 1;

        info!(
            "initrd: exec config: {} (workdir={}, {} env vars, {} args)",
            cfg.exec_path,
            cfg.workdir,
            cfg.env.len(),
            cfg.args.len()
        );
    }

    // Write network config as /etc/krun/net (shell script sourced by init.krun).
    if let Some(net) = net_config {
        // Ensure etc/krun directory exists (may already exist from exec_config)
        if exec_config.is_none() {
            if !rootfs_path.join("etc").exists() {
                cpio_entry(&mut cpio, ino, 0o040755, "etc", &[], 0, 0);
                ino += 1;
            }
            cpio_entry(&mut cpio, ino, 0o040755, "etc/krun", &[], 0, 0);
            ino += 1;
        }

        let mut net_script = String::from("#!/bin/busybox sh\n");
        net_script.push_str("# Network configuration (HCN NAT)\n");
        // Wait for the NIC to appear — Hyper-V netvsc driver may take a moment
        net_script.push_str(
            "for i in $(seq 1 30); do\n\
             \x20 if [ -d /sys/class/net/eth0 ]; then break; fi\n\
             \x20 sleep 0.1 2>/dev/null || usleep 100000 2>/dev/null\n\
             done\n",
        );
        net_script.push_str(&format!(
            "ip addr add {}/{} dev eth0 2>/dev/null || \\\n\
             \x20 ifconfig eth0 {} netmask 255.255.0.0 2>/dev/null\n",
            net.ip, net.prefix_len, net.ip
        ));
        net_script.push_str("ip link set eth0 up 2>/dev/null || ifconfig eth0 up 2>/dev/null\n");
        net_script.push_str(&format!(
            "ip route add default via {} 2>/dev/null || \\\n\
             \x20 route add default gw {} 2>/dev/null\n",
            net.gateway, net.gateway
        ));
        // DNS
        if !net.dns.is_empty() {
            net_script.push_str("mkdir -p /etc 2>/dev/null\n");
            for dns in &net.dns {
                net_script.push_str(&format!(
                    "echo 'nameserver {}' >> /etc/resolv.conf\n",
                    dns
                ));
            }
        } else {
            // Default DNS
            net_script.push_str(
                "mkdir -p /etc 2>/dev/null\n\
                 echo 'nameserver 8.8.8.8' > /etc/resolv.conf\n\
                 echo 'nameserver 8.8.4.4' >> /etc/resolv.conf\n",
            );
        }

        cpio_entry(
            &mut cpio,
            ino,
            0o0100755,
            "etc/krun/net",
            net_script.as_bytes(),
            0,
            0,
        );
        ino += 1;

        info!(
            "initrd: net config: ip={}/{} gw={}",
            net.ip, net.prefix_len, net.gateway
        );
    }

    // Inject /init.krun — the init script that reads config and execs.
    let init_script = INIT_KRUN_SCRIPT;
    cpio_entry(
        &mut cpio,
        ino,
        0o0100755,
        "init.krun",
        init_script.as_bytes(),
        0,
        0,
    );
    ino += 1;

    // Ensure essential directories exist for the init script
    for dir in &["proc", "sys", "dev", "tmp"] {
        let dir_path = rootfs_path.join(dir);
        if !dir_path.exists() {
            cpio_entry(&mut cpio, ino, 0o040755, dir, &[], 0, 0);
            ino += 1;
        }
    }

    // Trailer
    cpio_entry(&mut cpio, 0, 0, "TRAILER!!!", &[], 0, 0);

    info!(
        "initrd: packed {} files ({:.1} MB uncompressed) in {:.1}s",
        file_count,
        cpio.len() as f64 / 1_048_576.0,
        start.elapsed().as_secs_f64()
    );

    // Write raw cpio (no gzip). HCS passes InitRdPath directly to the kernel
    // which expects to find the newc cpio magic (070701) at the start. Gzip'd
    // archives are not recognized and fall back to the old ramdisk path.
    std::fs::write(&initrd_path, &cpio)?;
    Ok(initrd_path)
}

/// Recursively walk a directory and add all entries to the cpio archive.
fn walk_dir(
    base: &Path,
    dir: &Path,
    cpio: &mut Vec<u8>,
    ino: &mut u32,
    file_count: &mut u64,
    total_bytes: &mut u64,
) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("initrd: skipping {}: {}", dir.display(), e);
            return Ok(());
        }
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(base).unwrap_or(&path);
        let name = rel.to_string_lossy().replace('\\', "/");

        // Skip empty names
        if name.is_empty() {
            continue;
        }

        let ft = entry.file_type()?;

        if ft.is_symlink() {
            // Symlink: store target as data
            let target = std::fs::read_link(&path)?;
            let target_str = target.to_string_lossy().replace('\\', "/");
            cpio_entry(cpio, *ino, 0o0120777, &name, target_str.as_bytes(), 0, 0);
            *ino += 1;
            *file_count += 1;
        } else if ft.is_dir() {
            cpio_entry(cpio, *ino, 0o040755, &name, &[], 0, 0);
            *ino += 1;
            *file_count += 1;
            walk_dir(base, &path, cpio, ino, file_count, total_bytes)?;
        } else if ft.is_file() {
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("initrd: WARN: skipping {} (metadata: {})", path.display(), e);
                    continue;
                }
            };
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("initrd: WARN: skipping {} (read: {})", path.display(), e);
                    continue;
                }
            };

            // Detect broken symlinks from Docker image extraction on Windows.
            // Windows cannot represent Unix symlinks, so Docker image extractors
            // write symlinks as small regular files containing the target path.
            // Detect these and emit proper cpio symlinks so the Linux kernel
            // unpacks them correctly.
            //
            // Heuristic: file is small (<256B), not ELF, not a shebang script,
            // content is a single line that looks like a Unix path (only path
            // characters, no spaces).
            if data.len() < 256
                && !data.starts_with(b"\x7fELF")
                && !data.starts_with(b"#!")
            {
                let target = std::str::from_utf8(&data)
                    .unwrap_or("")
                    .trim_end_matches(|c: char| c == '\n' || c == '\r' || c == '\0');
                if !target.is_empty()
                    && !target.contains('\n')
                    && !target.contains(' ')
                    && target.contains('/')
                    && target.bytes().all(|b| b.is_ascii_alphanumeric()
                        || b == b'/' || b == b'.' || b == b'-' || b == b'_' || b == b'+')
                {
                    // Emit as a symlink. The content looks like a Unix path
                    // (only path chars, single line, contains '/', small file).
                    // We can't fully verify targets on Windows because they may
                    // chain through other broken symlinks.
                    cpio_entry(cpio, *ino, 0o0120777, &name, target.as_bytes(), 0, 0);
                    *ino += 1;
                    *file_count += 1;
                    continue;
                }
            }

            let mode = if is_executable(&path, &meta) {
                0o0100755
            } else if name.contains("ssh_host_") && !name.ends_with(".pub") {
                // SSH host private keys must be mode 0600 or sshd refuses to start.
                0o0100600
            } else {
                0o0100644
            };
            *total_bytes += data.len() as u64;
            cpio_entry(cpio, *ino, mode, &name, &data, 0, 0);
            *ino += 1;
            *file_count += 1;
        }
        // Skip device nodes, sockets, etc. — not needed in VM rootfs
    }

    Ok(())
}

/// Check if a file should be marked executable.
fn is_executable(path: &Path, _meta: &std::fs::Metadata) -> bool {
    // On Windows we can't check Unix permissions, so use heuristics:
    // - ELF binaries (start with \x7fELF)
    // - Shell scripts (start with #!)
    // - Files in bin/sbin directories
    // - Files with no extension in bin-like paths
    if let Some(parent) = path.parent() {
        let p = parent.to_string_lossy();
        if p.contains("bin") || p.contains("sbin") || p.contains("libexec") {
            return true;
        }
    }

    // Check file extension
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        if ext == "sh" || ext == "py" || ext == "pl" || ext == "rb" {
            return true;
        }
    }

    // Check magic bytes
    if let Ok(f) = std::fs::File::open(path) {
        use std::io::Read;
        let mut magic = [0u8; 4];
        let mut reader = std::io::BufReader::new(f);
        if reader.read_exact(&mut magic).is_ok() {
            // ELF magic
            if magic == [0x7f, b'E', b'L', b'F'] {
                return true;
            }
            // Shebang
            if magic[0] == b'#' && magic[1] == b'!' {
                return true;
            }
        }
    }

    false
}

/// Shell init script injected into the initrd as /init.krun.
///
/// This script:
/// 1. Mounts /proc, /sys, /dev, /tmp
/// 2. Installs busybox symlinks for a working PATH
/// 3. Reads exec config from /etc/krun/ files (preferred)
/// 4. Falls back to parsing /proc/cmdline if files are missing
/// 5. Exports environment variables
/// 6. cd to workdir and exec the command with args
const INIT_KRUN_SCRIPT: &str = r#"#!/bin/busybox sh
# /init.krun — HCS VM init (replaces libkrunfw init on Linux/macOS)
# Uses busybox ash (statically linked) to avoid dynamic linker issues.
# Reads exec config from /etc/krun/ files (written by initrd generator).
# Falls back to kernel cmdline parsing if files are missing.

BB=/bin/busybox

$BB echo "init.krun: starting"

# Mount essential filesystems
$BB mount -t proc proc /proc 2>/dev/null
$BB mount -t sysfs sysfs /sys 2>/dev/null
$BB mount -t devtmpfs devtmpfs /dev 2>/dev/null
$BB mount -t tmpfs tmpfs /tmp 2>/dev/null
$BB mkdir -p /dev/pts 2>/dev/null
$BB mount -t devpts devpts /dev/pts 2>/dev/null

# Install busybox symlinks for a working PATH
$BB --install -s /bin 2>/dev/null
$BB --install -s /usr/bin 2>/dev/null
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

echo "init.krun: busybox installed, checking /bin/sh: $(ls -la /bin/sh 2>&1)"

# Configure networking if /etc/krun/net exists (HCN NAT)
if [ -f /etc/krun/net ]; then
  echo "init.krun: sourcing /etc/krun/net"
  . /etc/krun/net
fi

# Inject SSH public key with proper ownership.
# The cpio unpacks all files as UID 0 (root). For sshd to accept pubkey
# auth for non-root users, authorized_keys must be owned by that user.
# nanosb-init.sh also does this, but we do it here as a safety net.
if [ -f /etc/krun/ssh_pubkey ]; then
  echo "init.krun: injecting SSH key"
  mkdir -p /root/.ssh 2>/dev/null
  cp /etc/krun/ssh_pubkey /root/.ssh/authorized_keys
  chmod 600 /root/.ssh/authorized_keys
  chown 0:0 /root/.ssh /root/.ssh/authorized_keys 2>/dev/null
  # Inject for developer user (UID 1000) if the user exists
  if grep -q '^developer:' /etc/passwd 2>/dev/null; then
    mkdir -p /home/developer/.ssh 2>/dev/null
    cp /etc/krun/ssh_pubkey /home/developer/.ssh/authorized_keys
    chmod 700 /home/developer/.ssh
    chmod 600 /home/developer/.ssh/authorized_keys
    chown -R 1000:1000 /home/developer/.ssh 2>/dev/null
  fi
fi

# Start vsock proxy for HvSocket host communication.
if [ -x /bin/vsock_proxy ]; then
  /bin/vsock_proxy >/dev/console 2>&1 &
  sleep 0.2
  # Add a default route so the kernel has a path for outbound packets.
  # Without this, connecting to external IPs returns ENETUNREACH before
  # iptables REDIRECT can capture the packet.
  ip addr add 10.0.0.1/32 dev lo 2>/dev/null
  ip route add default via 10.0.0.1 dev lo 2>/dev/null
  # Use iptables-nft (nftables built-in in WSL kernel; legacy iptables is modular
  # and unavailable with nomodule). Fall back to iptables for non-WSL kernels.
  for _ipt in iptables-nft iptables; do
    if type "$_ipt" >/dev/null 2>&1; then
      "$_ipt" -t nat -A OUTPUT -p tcp ! -d 127.0.0.0/8 -j REDIRECT --to-port 1080 2>/dev/null
      break
    fi
  done
  # Override DNS to use vsock_proxy DNS forwarder on 127.0.0.1:53.
  # /etc/krun/net may have set nameserver to 8.8.8.8, but that's unreachable
  # without a real network — UDP DNS can't be captured by iptables REDIRECT.
  mkdir -p /etc 2>/dev/null
  echo "nameserver 127.0.0.1" > /etc/resolv.conf
  echo "init.krun: vsock_proxy started, DNS via 127.0.0.1, iptables REDIRECT to :1080"
fi

# If /etc/krun/cmd exists, it's a generated script with properly quoted
# env vars, workdir, and command. Just exec it.
if [ -x /etc/krun/cmd ]; then
  exec /etc/krun/cmd
  echo "init.krun: ERROR - exec /etc/krun/cmd returned $?"
fi

# Fallback: parse kernel cmdline (for backward compat / manual testing)
KRUN_INIT=""
KRUN_WORKDIR="/"
ARGS=""
read -r CMDLINE < /proc/cmdline
found_dashdash=0
for token in $CMDLINE; do
  if [ "$found_dashdash" = "1" ]; then
    if [ -z "$ARGS" ]; then
      ARGS="$token"
    else
      ARGS="$ARGS $token"
    fi
    continue
  fi
  if [ "$token" = "--" ]; then
    found_dashdash=1
    continue
  fi
  case "$token" in
    KRUN_INIT=*) KRUN_INIT="${token#KRUN_INIT=}" ;;
    KRUN_WORKDIR=*) KRUN_WORKDIR="${token#KRUN_WORKDIR=}" ;;
  esac
done

# Default: try common init paths
if [ -z "$KRUN_INIT" ]; then
  if [ -x /sbin/init ]; then
    KRUN_INIT=/sbin/init
  elif [ -x /usr/local/bin/agent-gateway ]; then
    KRUN_INIT=/usr/local/bin/agent-gateway
  else
    exec /bin/sh
  fi
fi

cd "$KRUN_WORKDIR" 2>/dev/null

# Exec the command
if [ -n "$ARGS" ]; then
  exec $KRUN_INIT $ARGS
else
  exec $KRUN_INIT
fi
"#;

/// Generate a minimal boot initrd (~3MB) containing only busybox, init.krun,
/// and config files. The actual rootfs is mounted via Plan9 (9p) share.
///
/// This avoids packing the entire rootfs into the initrd, which doesn't scale
/// for large images (the WSL2 kernel's initramfs unpacker runs out of memory
/// for rootfs images >500MB).
pub fn generate_boot_initrd(
    exec_config: Option<&ExecConfig>,
    net_config: Option<&NetConfig>,
) -> std::io::Result<PathBuf> {
    let temp_dir = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into())).join(".nanosandbox").join("tmp");
    let _ = std::fs::create_dir_all(&temp_dir);
    let initrd_path = temp_dir.join(format!("libkrun-initrd-{}.img", std::process::id()));

    let start = std::time::Instant::now();
    let mut cpio = Vec::new();
    let mut ino: u32 = 1;

    // Root directory
    cpio_entry(&mut cpio, ino, 0o040755, ".", &[], 0, 0);
    ino += 1;

    // Essential directories
    for dir in &["bin", "proc", "sys", "dev", "tmp", "mnt", "etc", "etc/krun"] {
        cpio_entry(&mut cpio, ino, 0o040755, dir, &[], 0, 0);
        ino += 1;
    }

    // Inject busybox
    if let Some(busybox_src) = find_dep_bin("busybox") {
        let data = std::fs::read(&busybox_src)?;
        cpio_entry(&mut cpio, ino, 0o0100755, "bin/busybox", &data, 0, 0);
        ino += 1;
        // /bin/sh symlink
        cpio_entry(&mut cpio, ino, 0o0120777, "bin/sh", b"busybox", 0, 0);
        ino += 1;
        info!(
            "boot initrd: injected busybox from {} ({} bytes)",
            busybox_src.display(),
            data.len()
        );
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "busybox not found in deps search paths (~/.nanosandbox/libs/, etc.)",
        ));
    }

    // Write exec config
    if let Some(cfg) = exec_config {
        let mut cmd_script = String::from("#!/bin/sh\nexport PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n");
        for env_var in &cfg.env {
            // Skip empty or malformed env vars (must contain KEY=VALUE)
            let trimmed = env_var.trim();
            if trimmed.is_empty() || !trimmed.contains('=') || trimmed.starts_with('=') {
                continue;
            }
            // Filter out env vars with names invalid for POSIX shells
            // (e.g. "CommonProgramFiles(x86)" from Windows).
            let var_name = trimmed.split('=').next().unwrap_or("");
            if var_name.is_empty()
                || !var_name.as_bytes()[0].is_ascii_alphabetic() && var_name.as_bytes()[0] != b'_'
                || !var_name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                continue;
            }
            cmd_script.push_str(&format!("export '{}'\n", shell_escape(trimmed)));
        }
        cmd_script.push_str(&format!("cd '{}' 2>/dev/null\n", shell_escape(&cfg.workdir)));
        cmd_script.push_str(&format!("exec '{}'", shell_escape(&cfg.exec_path)));
        for arg in &cfg.args {
            cmd_script.push_str(&format!(" '{}'", shell_escape(arg)));
        }
        cmd_script.push('\n');

        eprintln!("hcs: /etc/krun/cmd:\n{}", cmd_script);
        cpio_entry(&mut cpio, ino, 0o0100755, "etc/krun/cmd", cmd_script.as_bytes(), 0, 0);
        ino += 1;
    }

    // Write network config
    if let Some(net) = net_config {
        let mut net_script = String::from("#!/bin/busybox sh\n");
        net_script.push_str("# Network configuration (HCN NAT)\n");
        net_script.push_str(
            "for i in $(seq 1 30); do\n\
             \x20 if [ -d /sys/class/net/eth0 ]; then break; fi\n\
             \x20 sleep 0.1 2>/dev/null || usleep 100000 2>/dev/null || sleep 1\n\
             done\n",
        );
        net_script.push_str(&format!(
            "ip addr add {}/{} dev eth0 2>/dev/null\n\
             ip link set eth0 up 2>/dev/null\n\
             ip route add default via {} dev eth0 2>/dev/null\n",
            net.ip, net.prefix_len, net.gateway
        ));
        if !net.dns.is_empty() {
            net_script.push_str("mkdir -p /etc 2>/dev/null\n");
            for dns in &net.dns {
                net_script.push_str(&format!("echo 'nameserver {}' >> /etc/resolv.conf\n", dns));
            }
        } else {
            net_script.push_str(
                "mkdir -p /etc 2>/dev/null\n\
                 echo 'nameserver 8.8.8.8' > /etc/resolv.conf\n\
                 echo 'nameserver 8.8.4.4' >> /etc/resolv.conf\n",
            );
        }
        cpio_entry(&mut cpio, ino, 0o0100755, "etc/krun/net", net_script.as_bytes(), 0, 0);
        ino += 1;
    }

    // Inject vsock_proxy binary (AF_VSOCK proxy for inbound + outbound networking)
    if let Some(vsock_proxy_src) = find_dep_bin("vsock_proxy") {
        let data = std::fs::read(&vsock_proxy_src)?;
        cpio_entry(&mut cpio, ino, 0o0100755, "bin/vsock_proxy", &data, 0, 0);
        ino += 1;
        info!(
            "boot initrd: injected vsock_proxy from {} ({} bytes)",
            vsock_proxy_src.display(),
            data.len()
        );
    } else {
        info!("boot initrd: vsock_proxy not found in deps search paths, outbound networking will not work in Plan9 mode");
    }

    // Inject plan9_mount binary if available (uses AF_VSOCK for 9p mount —
    // required because the WSL2 kernel lacks the 9pnet_hyperv transport module).
    // Looked up via the same dep search as busybox/vsock_proxy so installed
    // nanosb (which has no `target/` dir) finds it under ~/.nanosandbox/libs/.
    let has_plan9_mount = if let Some(plan9_mount_src) = find_dep_bin("plan9_mount") {
        let data = std::fs::read(&plan9_mount_src)?;
        cpio_entry(&mut cpio, ino, 0o0100755, "bin/plan9_mount", &data, 0, 0);
        ino += 1;
        info!(
            "boot initrd: injected plan9_mount from {} ({} bytes)",
            plan9_mount_src.display(),
            data.len()
        );
        true
    } else {
        info!("boot initrd: plan9_mount not found in deps search paths, using shell-only 9p init (will likely fail on WSL kernel)");
        false
    };

    // Inject init.krun — if plan9_mount binary is present, use it for vsock-based
    // 9p mount; otherwise fall back to shell-based mount (requires 9pnet_hyperv).
    let init_script = if has_plan9_mount {
        INIT_KRUN_9P_VSOCK_SCRIPT
    } else {
        INIT_KRUN_9P_SCRIPT
    };
    cpio_entry(&mut cpio, ino, 0o0100755, "init.krun", init_script.as_bytes(), 0, 0);
    ino += 1;

    // Trailer
    cpio_entry(&mut cpio, 0, 0, "TRAILER!!!", &[], 0, 0);

    std::fs::write(&initrd_path, &cpio)?;

    info!(
        "boot initrd: {:.1} MB, generated in {:.1}s",
        cpio.len() as f64 / 1_048_576.0,
        start.elapsed().as_secs_f64()
    );

    Ok(initrd_path)
}

/// Init script for 9p-rootfs mode using plan9_mount binary (vsock transport).
/// plan9_mount connects to the HCS Plan9 service via AF_VSOCK, mounts 9p with
/// trans=fd, chroots into the rootfs, and execs the user command.
///
/// Since plan9_mount chroots into /mnt after mounting, files from the initrd
/// (/etc/krun/cmd) are not accessible. We read the cmd content before calling
/// plan9_mount and pass it inline as the user_command argument.
const INIT_KRUN_9P_VSOCK_SCRIPT: &str = r#"#!/bin/busybox sh
# /init.krun — HCS VM init (9p rootfs mode, vsock transport)
# Uses plan9_mount binary for AF_VSOCK-based 9p mount.

BB=/bin/busybox

$BB echo "init.krun: starting (9p vsock mode)"

# plan9_mount handles devtmpfs/proc/sysfs mounting internally,
# but we need to mount them first to read /etc/krun/cmd and set up networking.
$BB mount -t proc proc /proc 2>/dev/null
$BB mount -t sysfs sysfs /sys 2>/dev/null
$BB mount -t devtmpfs devtmpfs /dev 2>/dev/null
$BB mount -t tmpfs tmpfs /tmp 2>/dev/null
$BB mkdir -p /dev/pts 2>/dev/null

# Install busybox symlinks
$BB --install -s /bin 2>/dev/null
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

# Bring up loopback (required for vsock_proxy to connect to 127.0.0.1)
ip link set lo up 2>/dev/null

# Configure DNS to use local vsock proxy
mkdir -p /etc 2>/dev/null
echo "nameserver 127.0.0.1" > /etc/resolv.conf

# Legacy networking (HCN NAT — disabled, kept for fallback)
if [ -f /etc/krun/net ]; then
  echo "init.krun: configuring legacy network"
  . /etc/krun/net
fi

# Start vsock proxy for HvSocket host communication.
# Must run AFTER /etc/krun/net so vsock proxy overrides DNS and routes.
if [ -x /bin/vsock_proxy ]; then
  /bin/vsock_proxy >/dev/console 2>&1 &
  sleep 0.2
  # Add a default route so the kernel has a path for outbound packets.
  # Without this, connecting to external IPs returns ENETUNREACH before
  # iptables REDIRECT can capture the packet.
  ip addr add 10.0.0.1/32 dev lo 2>/dev/null
  ip route add default via 10.0.0.1 dev lo 2>/dev/null
  # Use iptables-nft (nftables built-in in WSL kernel; legacy iptables is modular
  # and unavailable with nomodule). Fall back to iptables for non-WSL kernels.
  for _ipt in iptables-nft iptables; do
    if type "$_ipt" >/dev/null 2>&1; then
      "$_ipt" -t nat -A OUTPUT -p tcp ! -d 127.0.0.0/8 -j REDIRECT --to-port 1080 2>/dev/null
      break
    fi
  done
  # Override DNS to use vsock_proxy DNS forwarder on 127.0.0.1:53.
  # /etc/krun/net may have set nameserver to 8.8.8.8, but that's unreachable
  # without a real network — UDP DNS can't be captured by iptables REDIRECT.
  echo "nameserver 127.0.0.1" > /etc/resolv.conf
  echo "init.krun: vsock_proxy started, DNS via 127.0.0.1, iptables REDIRECT to :1080"
fi

# Read the exec command from /etc/krun/cmd (written by initrd generator).
# After plan9_mount chroots into the 9p rootfs, the initrd's /etc/krun/cmd
# won't be accessible, so we extract the exec line and pass it inline.
USER_CMD="/bin/sh"
if [ -f /etc/krun/cmd ]; then
  # The cmd script has: cd 'dir' + exec 'prog' 'arg1' ...
  # Extract just the exec line (last line starting with exec)
  EXEC_LINE=$(grep "^exec " /etc/krun/cmd | tail -1)
  if [ -n "$EXEC_LINE" ]; then
    # Remove the 'exec ' prefix and single quotes to get the raw command
    USER_CMD=$(echo "$EXEC_LINE" | sed "s/^exec //; s/'//g")
  fi
fi

echo "init.krun: user command: $USER_CMD"
echo "init.krun: launching plan9_mount (vsock port 50000)..."

# plan9_mount connects AF_VSOCK to port 50000 (HCS Plan9 server),
# mounts 9p share "rootfs" at /mnt with trans=fd, chroots into /mnt, execs USER_CMD.
exec /bin/plan9_mount 50000 rootfs "$USER_CMD"
"#;

/// Init script for 9p-rootfs mode. Mounts Plan9 share at /mnt, then
/// switch_root into it and exec the command from /etc/krun/cmd.
const INIT_KRUN_9P_SCRIPT: &str = r#"#!/bin/busybox sh
# /init.krun — HCS VM init (9p rootfs mode)
# Mounts the rootfs from a Plan9 share, then switch_root into it.

BB=/bin/busybox

$BB echo "init.krun: starting (9p rootfs mode)"

# Mount essential filesystems
$BB mount -t proc proc /proc 2>/dev/null
$BB mount -t sysfs sysfs /sys 2>/dev/null
$BB mount -t devtmpfs devtmpfs /dev 2>/dev/null
$BB mount -t tmpfs tmpfs /tmp 2>/dev/null
$BB mkdir -p /dev/pts 2>/dev/null
$BB mount -t devpts devpts /dev/pts 2>/dev/null

# Install busybox symlinks
$BB --install -s /bin 2>/dev/null
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

# Bring up loopback (required for vsock_proxy to connect to 127.0.0.1)
ip link set lo up 2>/dev/null

# Configure DNS to use local vsock proxy
mkdir -p /etc 2>/dev/null
echo "nameserver 127.0.0.1" > /etc/resolv.conf

# Legacy networking (HCN NAT — disabled)
if [ -f /etc/krun/net ]; then
  echo "init.krun: configuring legacy network"
  . /etc/krun/net
fi

# Start vsock proxy for HvSocket host communication.
# Must run AFTER /etc/krun/net so vsock proxy overrides DNS and routes.
if [ -x /bin/vsock_proxy ]; then
  /bin/vsock_proxy >/dev/console 2>&1 &
  sleep 0.2
  # Add a default route so the kernel has a path for outbound packets.
  # Without this, connecting to external IPs returns ENETUNREACH before
  # iptables REDIRECT can capture the packet.
  ip addr add 10.0.0.1/32 dev lo 2>/dev/null
  ip route add default via 10.0.0.1 dev lo 2>/dev/null
  # Use iptables-nft (nftables built-in in WSL kernel; legacy iptables is modular
  # and unavailable with nomodule). Fall back to iptables for non-WSL kernels.
  for _ipt in iptables-nft iptables; do
    if type "$_ipt" >/dev/null 2>&1; then
      "$_ipt" -t nat -A OUTPUT -p tcp ! -d 127.0.0.0/8 -j REDIRECT --to-port 1080 2>/dev/null
      break
    fi
  done
  # Override DNS to use vsock_proxy DNS forwarder on 127.0.0.1:53.
  # /etc/krun/net may have set nameserver to 8.8.8.8, but that's unreachable
  # without a real network — UDP DNS can't be captured by iptables REDIRECT.
  echo "nameserver 127.0.0.1" > /etc/resolv.conf
  echo "init.krun: vsock_proxy started, DNS via 127.0.0.1, iptables REDIRECT to :1080"
fi

# Mount the rootfs via Plan9 (9p) share
echo "init.krun: mounting 9p rootfs..."
mkdir -p /mnt 2>/dev/null
# Diagnostics: check available 9p transports and VMBus devices
echo "init.krun: 9p modules:"
ls /sys/module/9p* 2>/dev/null || echo "  (none)"
echo "init.krun: VMBus devices:"
ls /sys/bus/vmbus/devices/ 2>/dev/null | head -5 || echo "  (none)"
echo "init.krun: virtio devices:"
ls /sys/bus/virtio/devices/ 2>/dev/null | head -5 || echo "  (none)"

for TRANS in hyperv virtio ""; do
  if [ -n "$TRANS" ]; then
    OPTS="trans=$TRANS,version=9p2000.L,msize=262144"
  else
    OPTS="version=9p2000.L"
  fi
  echo "init.krun: trying mount -t 9p -o $OPTS rootfs /mnt"
  mount -t 9p -o "$OPTS" rootfs /mnt 2>&1
  RC=$?
  if [ $RC -eq 0 ]; then
    break
  fi
  echo "init.krun: failed (rc=$RC)"
done

if [ $RC -ne 0 ]; then
  echo "init.krun: ERROR - could not mount 9p rootfs"
  echo "init.krun: available filesystems:"
  cat /proc/filesystems 2>/dev/null
  echo "init.krun: falling back to initrd-only mode"
  # If /etc/krun/cmd exists in the initrd, run it directly
  if [ -x /etc/krun/cmd ]; then
    exec /etc/krun/cmd
  fi
  exec /bin/sh
fi

echo "init.krun: 9p rootfs mounted at /mnt"
ls /mnt/ 2>/dev/null | head -5

# Copy config files into the rootfs
mkdir -p /mnt/etc/krun 2>/dev/null
if [ -f /etc/krun/cmd ]; then
  cp /etc/krun/cmd /mnt/etc/krun/cmd
  chmod +x /mnt/etc/krun/cmd
fi
if [ -f /etc/krun/net ]; then
  cp /etc/krun/net /mnt/etc/krun/net
fi
# Copy resolv.conf if we configured networking
if [ -f /etc/resolv.conf ]; then
  mkdir -p /mnt/etc 2>/dev/null
  cp /etc/resolv.conf /mnt/etc/resolv.conf
fi

# Ensure /etc/hosts has localhost entries
if [ ! -f /mnt/etc/hosts ] || ! grep -q '127.0.0.1' /mnt/etc/hosts 2>/dev/null; then
  mkdir -p /mnt/etc 2>/dev/null
  printf '127.0.0.1\tlocalhost\n::1\t\tlocalhost ip6-localhost\n' > /mnt/etc/hosts
fi

# Parse kernel cmdline for SSH key
CMDLINE=$(cat /proc/cmdline)
get_param() {
  local result=""
  for word in $CMDLINE; do
    case "$word" in
      "$1="*) result="${word#*=}" ;;
    esac
  done
  echo "$result"
}

NANOSB_SSH=$(get_param nanosb.ssh_key)

# Inject SSH key (comma-separated from cmdline -> spaces)
if [ -n "$NANOSB_SSH" ]; then
  SSH_KEY=$(echo "$NANOSB_SSH" | tr ',' ' ')
  for d in /mnt/root/.ssh /mnt/home/developer/.ssh; do
    mkdir -p "$d"
    echo "$SSH_KEY" > "$d/authorized_keys"
    chmod 700 "$d"
    chmod 600 "$d/authorized_keys"
  done
  chown -R 0:0 /mnt/root/.ssh 2>/dev/null
  chown -R 1000:1000 /mnt/home/developer/.ssh 2>/dev/null
  echo "init.krun: SSH key injected"
fi

# Ensure essential mount points exist in the rootfs
mkdir -p /mnt/proc /mnt/sys /mnt/dev /mnt/tmp /mnt/dev/pts 2>/dev/null

# Fix missing usr-merge symlinks (broken during Windows extraction of OCI layers)
for d in bin sbin lib lib64; do
  if [ ! -e /mnt/$d ] && [ -d /mnt/usr/$d ]; then
    echo "init.krun: creating /$d -> usr/$d symlink"
    ln -s usr/$d /mnt/$d
  fi
done
if [ ! -d /mnt/etc ]; then
  mkdir -p /mnt/etc
  echo "init.krun: created /etc"
fi

# Move mount points into rootfs for switch_root
mount --move /proc /mnt/proc 2>/dev/null || true
mount --move /sys /mnt/sys 2>/dev/null || true
mount --move /dev /mnt/dev 2>/dev/null || true
mount --move /tmp /mnt/tmp 2>/dev/null || true

# switch_root into the 9p rootfs and exec the command
echo "init.krun: switch_root to /mnt"
if [ -x /mnt/etc/krun/cmd ]; then
  exec switch_root /mnt /etc/krun/cmd
fi

# Fallback: try common init paths
if [ -x /mnt/usr/local/bin/nanosb-init.sh ]; then
  exec switch_root /mnt /usr/local/bin/nanosb-init.sh
fi
if [ -x /mnt/usr/local/bin/agent-gateway ]; then
  exec switch_root /mnt /usr/local/bin/agent-gateway
fi
if [ -x /mnt/sbin/init ]; then
  exec switch_root /mnt /sbin/init
fi

echo "init.krun: no command found, dropping to shell"
exec switch_root /mnt /bin/sh
"#;

/// Init script for direct root boot (no initrd). This script lives on the
/// ext4 rootfs itself. It reads all config from kernel cmdline params
/// (nanosb.ip, nanosb.gw, nanosb.prefix, nanosb.dns, nanosb.ssh_key).
/// This eliminates initrd unpacking, /dev/sda wait, and pivot_root overhead.
pub const INIT_KRUN_DIRECT_SCRIPT: &str = r#"#!/busybox sh
# /init.krun — Direct root boot init (no initrd, no pivot)
# Uses /busybox (statically linked, injected into ext4) as the shell interpreter
# because Docker images extracted on Windows lose /lib64 symlinks, making
# dynamically-linked /bin/sh fail. Config from kernel cmdline nanosb.* params.

BB=/busybox

# Ensure essential mount points exist (may be missing from Windows extraction)
$BB mkdir -p /dev /proc /sys /tmp /dev/pts /bin /sbin /lib

# Fix usr-merge symlinks lost during OCI extraction on Windows.
# Debian/Ubuntu use /bin -> /usr/bin etc. Windows drops these symlinks.
for d in bin sbin lib lib64; do
  if [ ! -e /$d ] || [ "$($BB ls -A /$d 2>/dev/null | $BB wc -l)" = "0" ]; then
    if [ -d /usr/$d ]; then
      $BB rm -rf /$d
      $BB ln -s usr/$d /$d
    fi
  fi
done

$BB mount -t proc proc /proc 2>/dev/null
$BB mount -t sysfs sysfs /sys 2>/dev/null
$BB mount -t devtmpfs devtmpfs /dev 2>/dev/null
$BB mount -t tmpfs tmpfs /tmp 2>/dev/null
$BB mkdir -p /dev/pts 2>/dev/null
$BB mount -t devpts devpts /dev/pts 2>/dev/null

# Install busybox symlinks so standard commands work
$BB --install -s /bin 2>/dev/null
$BB --install -s /usr/bin 2>/dev/null
$BB --install -s /sbin 2>/dev/null
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

# Fix broken /lib64 symlink (Docker on Windows loses symlinks)
if [ ! -f /lib64/ld-linux-x86-64.so.2 ] && [ -f /lib/x86_64-linux-gnu/ld-linux-x86-64.so.2 ]; then
  rm -rf /lib64 2>/dev/null
  ln -s /lib/x86_64-linux-gnu /lib64
fi

# Bring up loopback
ip link set lo up 2>/dev/null

# Parse kernel cmdline params
CMDLINE=$(cat /proc/cmdline)
get_param() {
  local result=""
  for word in $CMDLINE; do
    case "$word" in
      "$1="*) result="${word#*=}" ;;
    esac
  done
  echo "$result"
}

NANOSB_IP=$(get_param nanosb.ip)
NANOSB_GW=$(get_param nanosb.gw)
NANOSB_PREFIX=$(get_param nanosb.prefix)
NANOSB_DNS=$(get_param nanosb.dns)
NANOSB_SSH=$(get_param nanosb.ssh_key)

echo "init.krun: booting (direct root)"

# Configure DNS to use local vsock proxy (resolves via host)
mkdir -p /etc 2>/dev/null
echo "nameserver 127.0.0.1" > /etc/resolv.conf

# Legacy HCN NAT network config (disabled — using HvSocket proxies now)
if [ -n "$NANOSB_IP" ]; then
  echo "init.krun: configuring legacy network (HCN)"
  for i in $(seq 1 30); do
    NIC=$(ls /sys/class/net/ 2>/dev/null | grep -v lo | head -1)
    [ -n "$NIC" ] && break
    sleep 0.1 2>/dev/null || usleep 100000 2>/dev/null || sleep 1
  done
  if [ -n "$NIC" ]; then
    ip addr add "$NANOSB_IP/$NANOSB_PREFIX" dev "$NIC" 2>/dev/null
    ip link set "$NIC" up 2>/dev/null
    ip route add default via "$NANOSB_GW" 2>/dev/null
  fi
fi

# Ensure /etc/hosts
if [ ! -f /etc/hosts ] || ! grep -q '127.0.0.1' /etc/hosts 2>/dev/null; then
  mkdir -p /etc 2>/dev/null
  printf '127.0.0.1\tlocalhost\n::1\t\tlocalhost ip6-localhost\n' > /etc/hosts
fi

# Inject SSH key (comma-separated from cmdline → spaces)
if [ -n "$NANOSB_SSH" ]; then
  SSH_KEY=$(echo "$NANOSB_SSH" | tr ',' ' ')
  for d in /root/.ssh /home/developer/.ssh; do
    mkdir -p "$d"
    echo "$SSH_KEY" > "$d/authorized_keys"
    chmod 700 "$d"
    chmod 600 "$d/authorized_keys"
  done
  chown -R 0:0 /root/.ssh 2>/dev/null
  chown -R 1000:1000 /home/developer/.ssh 2>/dev/null
  echo "init.krun: SSH key injected"
fi

echo "init.krun: setup complete, finding exec target"

# Start vsock proxy for HvSocket host communication.
# Provides: inbound gateway/SSH, outbound DNS (UDP :53) and TCP (:1080) relays.
# Note: vsock_proxy is shipped in the rootfs (Plan 9 share) at /vsock_proxy.
if [ -x /vsock_proxy ]; then
  /vsock_proxy >/dev/console 2>&1 &
  sleep 0.2
  # Create a dummy interface with a default route so the kernel has a path
  # for outbound packets. Without this, connecting to external IPs returns
  # ENETUNREACH before iptables REDIRECT can capture the packet.
  # Add a default route so the kernel has a path for outbound packets.
  # Without this, connecting to external IPs returns ENETUNREACH before
  # iptables REDIRECT can capture the packet.
  ip addr add 10.0.0.1/32 dev lo 2>/dev/null
  ip route add default via 10.0.0.1 dev lo 2>/dev/null
  # Redirect all outbound TCP (except localhost) through the vsock TCP proxy.
  # The proxy uses SO_ORIGINAL_DST to recover the real destination.
  # Use iptables-nft (nftables built-in in WSL kernel; legacy iptables is modular
  # and unavailable with nomodule). Fall back to iptables for non-WSL kernels.
  for _ipt in iptables-nft iptables; do
    if type "$_ipt" >/dev/null 2>&1; then
      "$_ipt" -t nat -A OUTPUT -p tcp ! -d 127.0.0.0/8 -j REDIRECT --to-port 1080 2>/dev/null
      break
    fi
  done
  echo "init.krun: vsock_proxy started, iptables REDIRECT to :1080"
fi

# Determine what to exec
# nanosb.exec is a base64-encoded shell script from kernel cmdline
NANOSB_EXEC=$(get_param nanosb.exec)
if [ -n "$NANOSB_EXEC" ]; then
  echo "$NANOSB_EXEC" | base64 -d > /tmp/krun_cmd 2>/dev/null
  chmod 755 /tmp/krun_cmd
  exec /tmp/krun_cmd
elif [ -x /etc/krun/cmd ]; then
  exec /etc/krun/cmd
elif [ -x /usr/local/bin/nanosb-init.sh ]; then
  echo "init.krun: exec nanosb-init.sh"
  exec /usr/local/bin/nanosb-init.sh
elif [ -x /usr/local/bin/agent-gateway ]; then
  echo "init.krun: exec agent-gateway"
  exec /usr/local/bin/agent-gateway
elif [ -x /sbin/init ]; then
  exec /sbin/init
else
  exec /bin/sh
fi
"#;

/// Escape a string for use in single-quoted shell context.
/// Single quotes are replaced with `'\''` (end quote, escaped quote, start quote).
fn shell_escape(s: &str) -> String {
    s.replace('\'', "'\\''")
}

fn cpio_entry(
    archive: &mut Vec<u8>,
    ino: u32,
    mode: u32,
    name: &str,
    data: &[u8],
    rdev_major: u32,
    rdev_minor: u32,
) {
    let namesize = name.len() + 1;
    let hdr = format!(
        "070701{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}",
        ino,
        mode,
        0, // uid
        0, // gid
        1, // nlink
        0, // mtime
        data.len(),
        0, // devmajor
        0, // devminor
        rdev_major,
        rdev_minor,
        namesize,
        0, // check
    );
    archive.extend_from_slice(hdr.as_bytes());
    archive.extend_from_slice(name.as_bytes());
    archive.push(0);
    let pad = (4 - ((110 + namesize) % 4)) % 4;
    archive.extend(std::iter::repeat(0u8).take(pad));
    archive.extend_from_slice(data);
    let pad = (4 - (data.len() % 4)) % 4;
    archive.extend(std::iter::repeat(0u8).take(pad));
}
