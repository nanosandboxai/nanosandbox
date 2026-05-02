#!/bin/sh
# nanosb-init.sh — PID 1 init script for nanosandbox VMs
#
# Starts sshd for direct access, then execs agent-gateway.
#
# Networking is NOT configured here — it is handled by the VM runtime:
#   - Linux/macOS: libkrun VMM configures gvproxy virtio-net
#   - Windows HCS: init.krun configures vsock_proxy + iptables REDIRECT
#
# NOTE: Do NOT use "set -e" here. This is an init script (PID 1) —
# if it exits for ANY reason, the VM shuts down. Every command must
# be individually guarded with "|| true" or explicit error handling.

# ---------------------------------------------------------------
# 0. Deduplicate: libkrun's init.c spawns this script twice
#    (once directly, once via a forked /init.krun copy). Use an
#    atomic mkdir lock to ensure only the first instance proceeds.
# ---------------------------------------------------------------
if ! mkdir /tmp/.nanosb-init-lock 2>/dev/null; then
    # Another instance already holds the lock — sleep forever.
    # Do NOT exit: PID 1 (/init.krun) may depend on its direct
    # child staying alive; exiting would shut down the VM.
    while true; do sleep 3600; done
fi

echo "nanosb-init: starting (v20)"

# ---------------------------------------------------------------
# 0b. Outbound proxy routing (Windows HCS)
# ---------------------------------------------------------------
# When vsock_proxy is running (started by init.krun before switch_root),
# all outbound TCP must be routed through it via NAT REDIRECT to port 1080.
#
# The WSL kernel has nftables built-in (=y) but iptables as modules (=m).
# Since we boot with nomodule, only raw nft commands work reliably.
# Fall back to iptables-nft / iptables for non-WSL kernels.
if pidof vsock_proxy >/dev/null 2>&1; then
    REDIRECT_OK=false

    # Try nft (direct nftables API — no kernel modules needed).
    # WSL kernel: NFT_NAT=y (dnat built-in), NFT_REDIR=m (redirect needs module).
    # Use dnat instead of redirect since we boot with nomodule.
    if command -v nft >/dev/null 2>&1 || [ -x /usr/sbin/nft ]; then
        NFT=$(command -v nft 2>/dev/null || echo /usr/sbin/nft)
        if $NFT add table ip nanosb 2>/dev/null \
           && $NFT add chain ip nanosb output '{ type nat hook output priority -100 ; policy accept ; }' 2>/dev/null \
           && $NFT -f - 2>/dev/null <<'NFTRULE'
add rule ip nanosb output ip daddr != 127.0.0.0/8 ip protocol tcp dnat to 127.0.0.1:1080
NFTRULE
        then
            REDIRECT_OK=true
            echo "nanosb-init: nft DNAT to vsock_proxy 127.0.0.1:1080"
        else
            echo "nanosb-init: nft dnat failed, trying iptables fallback"
        fi
    fi

    # Fallback: iptables-nft or iptables (for kernels with iptables modules loaded)
    if [ "$REDIRECT_OK" = "false" ]; then
        for ipt in iptables-nft /usr/sbin/iptables-nft iptables /usr/sbin/iptables; do
            if command -v "$ipt" >/dev/null 2>&1; then
                IPT_ERR=$($ipt -t nat -A OUTPUT -p tcp ! -d 127.0.0.0/8 -j REDIRECT --to-port 1080 2>&1)
                if [ $? -eq 0 ]; then
                    REDIRECT_OK=true
                    echo "nanosb-init: iptables REDIRECT to vsock_proxy :1080 (via $ipt)"
                else
                    echo "nanosb-init: $ipt failed: $IPT_ERR"
                fi
                break
            fi
        done
    fi

    if [ "$REDIRECT_OK" = "false" ]; then
        echo "nanosb-init: WARNING: could not set NAT REDIRECT, outbound TCP will not work"
    fi
fi

# ---------------------------------------------------------------
# 0c. Detect 9P rootfs mode (Windows HCS)
# ---------------------------------------------------------------
# When the rootfs is a Plan9 share from a Windows host, NTFS doesn't
# track Unix permissions. All files appear as 0777 through 9P.
# sshd requires strict permissions on host keys and authorized_keys.
# We detect this via a kernel cmdline flag set by the Windows runtime.
NANOSB_9P_MODE=false
if grep -q 'nanosb.9p_rootfs=1' /proc/cmdline 2>/dev/null; then
    NANOSB_9P_MODE=true
    echo "nanosb-init: 9P rootfs mode detected (Windows HCS)"
fi

# Helper: parse a key=value parameter from /proc/cmdline
get_cmdline_param() {
    local key="$1"
    cat /proc/cmdline 2>/dev/null | tr ' ' '\n' | grep "^${key}=" | cut -d= -f2-
}

# ---------------------------------------------------------------
# 1. Mount virtiofs shared directories
# ---------------------------------------------------------------
# The host writes /etc/nanosb-mounts with lines: "<tag> <mountpoint>"
# Each line corresponds to a virtiofs device registered via krun_add_virtiofs.
if [ -f /etc/nanosb-mounts ]; then
    while read -r tag mountpoint; do
        [ -z "$tag" ] && continue
        mkdir -p "$mountpoint" 2>/dev/null || true
        # Use agent-gateway --mount for direct syscall.Mount() — util-linux's
        # mount binary refuses even for root in libkrun micro-VMs.
        if /usr/local/bin/agent-gateway --mount "$tag" "$mountpoint" virtiofs 2>&1; then
            echo "nanosb-init: mounted $tag -> $mountpoint"
        else
            echo "nanosb-init: mount $tag -> $mountpoint FAILED"
        fi
    done < /etc/nanosb-mounts
    echo "nanosb-init: virtiofs mounts done"
fi

# ---------------------------------------------------------------
# 1b. Link agent state dirs into /workspace/.nanosb-state/
# ---------------------------------------------------------------
# Agent session state (conversation history, config) is stored inside
# the workspace clone at .nanosb-state/ so it persists across VM
# restarts via VirtioFS. Symlink each agent's home state dir there.
if [ -d /workspace ]; then
    STATE_DIR="/workspace/.nanosb-state"
    mkdir -p "$STATE_DIR" 2>/dev/null || true

    # Claude Code: ~/.claude/ (conversations, project memory, settings)
    # Codex:       ~/.codex/  (sessions, config.toml, AGENTS.md)
    # Cursor:      ~/.cursor/ (mcp.json, chat state)
    for dir in .claude .codex .cursor; do
        mkdir -p "$STATE_DIR/$dir" 2>/dev/null || true
        if [ -e "/home/developer/$dir" ] && [ ! -L "/home/developer/$dir" ]; then
            rm -rf "/home/developer/$dir" 2>/dev/null || true
        fi
        ln -sfn "$STATE_DIR/$dir" "/home/developer/$dir" 2>/dev/null || true
    done

    # Goose: ~/.config/goose/ (sessions, config.yaml)
    mkdir -p "$STATE_DIR/.config/goose" 2>/dev/null || true
    mkdir -p /home/developer/.config 2>/dev/null || true
    if [ -e /home/developer/.config/goose ] && [ ! -L /home/developer/.config/goose ]; then
        rm -rf /home/developer/.config/goose 2>/dev/null || true
    fi
    ln -sfn "$STATE_DIR/.config/goose" /home/developer/.config/goose 2>/dev/null || true

    chown -R developer:developer "$STATE_DIR" 2>/dev/null || true
    echo "nanosb-init: agent state symlinks created (workspace-backed)"
fi

# ---------------------------------------------------------------
# 2. Start dropbear (background) — enables SSH health check + access
# ---------------------------------------------------------------
# dropbear is used instead of openssh-server to avoid privilege separation
# issues on Linux virtiofs: openssh privsep requires /run/sshd owned by root,
# but virtiofs exposes all files with the host user's UID so chown fails.
# dropbear has no privsep requirement and works uniformly on all platforms.
if [ -x /usr/sbin/dropbear ]; then

    if [ "$NANOSB_9P_MODE" = "true" ]; then
        # 9P mode (Windows HCS): NTFS doesn't track Unix permissions.
        # dropbear stores host keys in /etc/dropbear — mount a tmpfs so
        # the auto-generated keys (-R) get proper 0600 permissions.
        mkdir -p /etc/dropbear 2>/dev/null || true
        mount -t tmpfs tmpfs /etc/dropbear 2>/dev/null || true

        # Inject SSH public key from kernel cmdline (set by Windows runtime)
        NANOSB_SSH_KEY=$(get_cmdline_param nanosb.ssh_key)
        if [ -n "$NANOSB_SSH_KEY" ]; then
            SSH_KEY=$(echo "$NANOSB_SSH_KEY" | tr ',' ' ')
            mkdir -p /root/.ssh 2>/dev/null || true
            mount -t tmpfs tmpfs /root/.ssh 2>/dev/null || true
            echo "$SSH_KEY" > /root/.ssh/authorized_keys
            chmod 600 /root/.ssh/authorized_keys 2>/dev/null || true
            echo "nanosb-init: SSH key injected from kernel cmdline"
        fi
        echo "nanosb-init: 9P dropbear setup complete"
    else
        # Normal mode (virtiofs / macOS / Linux)
        chown 0:0 /root 2>/dev/null || true
        chown -R 0:0 /root/.ssh 2>/dev/null || true
    fi

    # Copy authorized_keys to developer user — TUI connects as 'developer'
    # because agents like Claude Code refuse --dangerously-skip-permissions as root.
    if [ -f /root/.ssh/authorized_keys ]; then
        mkdir -p /home/developer/.ssh 2>/dev/null || true
        if [ "$NANOSB_9P_MODE" = "true" ]; then
            mount -t tmpfs tmpfs /home/developer/.ssh 2>/dev/null || true
        fi
        cp /root/.ssh/authorized_keys /home/developer/.ssh/authorized_keys 2>/dev/null || true
        chown -R developer:developer /home/developer/.ssh 2>/dev/null || true
        chmod 700 /home/developer/.ssh 2>/dev/null || true
        chmod 600 /home/developer/.ssh/authorized_keys 2>/dev/null || true
    fi

    # Unlock developer account (Debian locks accounts with '!' in /etc/shadow).
    usermod -p '*' developer 2>/dev/null || true

    # Fix ownership of developer home and workspace.
    chown -R developer:developer /home/developer 2>/dev/null || true
    chown -R developer:developer /workspace 2>/dev/null || true

    # Start dropbear: -R auto-generates host keys, -E logs to stderr, -p 22
    if /usr/sbin/dropbear -R -E -p 22 2>&1; then
        echo "nanosb-init: dropbear started"
    else
        echo "nanosb-init: ERROR: dropbear failed to start (exit $?)" >&2
    fi
fi

# ---------------------------------------------------------------
# 3. Start agent-gateway (foreground) — handles agent API + MCP
# ---------------------------------------------------------------
if [ -x /usr/local/bin/agent-gateway ]; then
    echo "nanosb-init: starting agent-gateway"
    # On Windows HCS (9P mode), networking is handled by init.krun's
    # vsock_proxy + iptables REDIRECT — agent-gateway must skip eth0
    # setup. On Linux/macOS (libkrun + gvproxy virtio-net), agent-gateway
    # owns eth0 bring-up and DHCP-style static IP assignment, so it must
    # NOT skip — otherwise eth0 stays DOWN and gvproxy can't ARP the guest.
    if [ "$NANOSB_9P_MODE" = "true" ]; then
        exec /usr/local/bin/agent-gateway --skip-network-init
    else
        exec /usr/local/bin/agent-gateway
    fi
else
    echo "nanosb-init: agent-gateway not found, entering hold mode"
    # Keep the VM alive so SSH access still works for debugging.
    while true; do sleep 3600; done
fi
