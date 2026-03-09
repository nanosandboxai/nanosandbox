#!/bin/sh
# nanosb-init.sh — PID 1 init script for nanosandbox VMs
#
# Configures networking (if not already done by libkrun's VMM),
# starts sshd for direct access, then execs agent-gateway.
#
# NOTE: Do NOT use "set -e" here. This is an init script (PID 1) —
# if it exits for ANY reason, the VM shuts down. Every command must
# be individually guarded with "|| true" or explicit error handling.

echo "nanosb-init: starting (v3)"

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
# 2. Start sshd (background) — enables SSH health check + access
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

    /usr/sbin/sshd 2>/dev/null || echo "nanosb-init: warning: sshd failed to start"
    echo "nanosb-init: sshd started"
fi

# ---------------------------------------------------------------
# 3. Start agent-gateway (foreground) — handles agent API + MCP
# ---------------------------------------------------------------
if [ -x /usr/local/bin/agent-gateway ]; then
    echo "nanosb-init: starting agent-gateway"
    exec /usr/local/bin/agent-gateway --skip-network-init
else
    echo "nanosb-init: agent-gateway not found, entering hold mode"
    # Keep the VM alive so SSH access still works for debugging.
    while true; do sleep 3600; done
fi
