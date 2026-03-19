#!/bin/sh
# nanosb-init.sh — PID 1 init script for nanosandbox VMs
#
# Configures networking (if not already done by libkrun's VMM),
# starts sshd for direct access, then execs agent-gateway.
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

echo "nanosb-init: starting (v7-dedup)"

# ---------------------------------------------------------------
# 1. Configure networking (gvproxy virtio-net)
# ---------------------------------------------------------------
# libkrun's VMM may have already configured the network interface
# via gvproxy. Only add the address if eth0 doesn't already have one.
if command -v ip >/dev/null 2>&1; then
    ip link set eth0 up 2>/dev/null || true
    if ! ip addr show eth0 2>/dev/null | grep -q 'inet '; then
        ip addr add 192.168.127.2/24 dev eth0 2>/dev/null || true
    fi
    if ! ip route show 2>/dev/null | grep -q 'default'; then
        ip route add default via 192.168.127.1 dev eth0 2>/dev/null || true
    fi
elif command -v ifconfig >/dev/null 2>&1; then
    ifconfig eth0 192.168.127.2 netmask 255.255.255.0 up 2>/dev/null || true
    route add default gw 192.168.127.1 2>/dev/null || true
fi

# DNS — gvproxy's built-in DNS is at the gateway IP
mkdir -p /etc 2>/dev/null || true
echo "nameserver 192.168.127.1" > /etc/resolv.conf 2>/dev/null || true

echo "nanosb-init: networking configured"

# ---------------------------------------------------------------
# 2. Mount virtiofs shared directories
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
# 2b. Link agent state dirs into /workspace/.nanosb-state/
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
# 3. Start sshd (background) — enables SSH health check + access
# ---------------------------------------------------------------
if [ -x /usr/sbin/sshd ]; then
    # Generate host keys if missing (first boot)
    ssh-keygen -A 2>/dev/null || true

    # Fix ownership: virtiofs passes through host UIDs, so rootfs files
    # and directories appear owned by the macOS user (e.g. UID 501)
    # instead of root. sshd StrictModes requires /run/sshd, /root,
    # /root/.ssh, and authorized_keys to all be owned by root (UID 0).
    mkdir -p /run/sshd 2>/dev/null || true
    chown 0:0 /run /run/sshd 2>/dev/null || true
    chmod 0755 /run/sshd 2>/dev/null || true
    chown 0:0 /root 2>/dev/null || true
    chown -R 0:0 /root/.ssh 2>/dev/null || true

    # Copy SSH authorized_keys from root to developer user so that
    # the TUI can connect as 'developer' (non-root) for agent CLIs.
    # Agents like Claude Code refuse --dangerously-skip-permissions as root.
    if [ -f /root/.ssh/authorized_keys ]; then
        mkdir -p /home/developer/.ssh 2>/dev/null || true
        cp /root/.ssh/authorized_keys /home/developer/.ssh/authorized_keys 2>/dev/null || true
        chown -R developer:developer /home/developer/.ssh 2>/dev/null || true
        chmod 700 /home/developer/.ssh 2>/dev/null || true
        chmod 600 /home/developer/.ssh/authorized_keys 2>/dev/null || true
    fi

    # Fix ownership of developer's entire home directory.
    # virtiofs passes through host UIDs (e.g. macOS UID 501), so all files
    # from the rootfs appear with wrong ownership inside the VM.
    chown -R developer:developer /home/developer 2>/dev/null || true

    # Ensure /workspace is writable by the developer user
    chown -R developer:developer /workspace 2>/dev/null || true

    /usr/sbin/sshd 2>/dev/null || echo "nanosb-init: warning: sshd failed to start"
    echo "nanosb-init: sshd started"
fi

# ---------------------------------------------------------------
# 4. Start agent-gateway (foreground) — handles agent API + MCP
# ---------------------------------------------------------------
if [ -x /usr/local/bin/agent-gateway ]; then
    echo "nanosb-init: starting agent-gateway"
    exec /usr/local/bin/agent-gateway --skip-network-init
else
    echo "nanosb-init: agent-gateway not found, entering hold mode"
    # Keep the VM alive so SSH access still works for debugging.
    while true; do sleep 3600; done
fi
