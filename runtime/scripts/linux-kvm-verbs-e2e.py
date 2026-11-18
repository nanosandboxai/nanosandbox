#!/usr/bin/env python3
"""Live E2E for Epic 9 shim-facing verbs (C2/C3/C4) against a real Linux/KVM VM.

Drives the per-sandbox control socket (NDJSON protocol) of a running nanosandbox
supervisor and asserts:
  C2: exec_stream with tty:true accepts live stdin (exec_input) and a resize,
      and the guest process reads it;
  C3: blocking `wait` returns the workload's real exit code once terminal;
  C4: `kill` with an unsupported signal returns a typed `unsupported`; SIGTERM
      is accepted;
plus that the hello handshake advertises wait/exec_pty capabilities and no longer
advertises the reserved `signal` capability.
"""
import json
import os
import socket
import subprocess
import sys
import time

HOME = os.path.expanduser("~")
SANDBOXES = os.path.join(HOME, ".nanosandbox", "sandboxes")
BIN = os.path.join(HOME, "nanosandbox", "target", "debug", "nanosb")
IMAGE = "docker.io/library/alpine:latest"
FAILS = []


def check(ok, msg):
    print(("PASS: " if ok else "FAIL: ") + msg, flush=True)
    if not ok:
        FAILS.append(msg)


def ctrl_path(name):
    return os.path.join(SANDBOXES, name, "control.sock")


def wait_path(path, timeout=30):
    end = time.time() + timeout
    while time.time() < end:
        if os.path.exists(path):
            return path
        time.sleep(0.1)
    raise RuntimeError(f"no socket {path} after {timeout}s")


class Conn:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(path)
        self.buf = b""

    def send(self, obj):
        self.s.sendall((json.dumps(obj) + "\n").encode())

    def recv(self, timeout=30):
        self.s.settimeout(timeout)
        while b"\n" not in self.buf:
            chunk = self.s.recv(65536)
            if not chunk:
                return None
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        if not line.strip():
            return self.recv(timeout)
        return json.loads(line)

    def close(self):
        self.s.close()


def start(name, cmd, exec_channel=False):
    subprocess.run([BIN, "rm", "-f", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    env = dict(os.environ)
    env["PATH"] = os.path.join(HOME, ".nanosandbox", "bin") + ":" + env.get("PATH", "")
    args = [BIN, "run", "--name", name]
    if exec_channel:
        args.append("--exec")
    args += [IMAGE] + list(cmd)
    return subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)


def stop(name):
    subprocess.run([BIN, "rm", "-f", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def hello(conn):
    conn.send({"type": "hello", "client": "live-e2e", "sdk_version": "0",
               "protocol_min": "1.0.0", "protocol_max": "1.0.0"})
    return conn.recv()


# ── Test A: exec TTY + live stdin + kill(signal) ────────────────────────────
print("== sandbox e2eexec (exec agent as PID 1) ==", flush=True)
pa = start("e2eexec", [], exec_channel=True)
c = None
try:
    path = wait_path(ctrl_path("e2eexec"))
    wait_path(os.path.join(SANDBOXES, "e2eexec", "exec.sock"), timeout=30)
    c = Conn(path)
    h = hello(c)
    caps = set(h.get("capabilities", []))
    check("signal" not in caps and "wait" in caps and "exec_pty" in caps,
          f"hello advertises wait/exec_pty and not signal (got {sorted(caps)})")

    # C2: tty exec_stream with live stdin + resize
    c.send({"type": "exec_stream", "command": "sh", "args": ["-c", "read x; echo got:$x"],
            "tty": True, "cols": 120, "rows": 40})
    ack = c.recv()
    check(ack.get("type") == "ok", f"exec_stream(tty) acked (got {ack})")
    c.send({"type": "exec_input", "data": "ping\n"})
    c.send({"type": "exec_resize", "cols": 100, "rows": 30})
    stdout, exit_code, saw_input_ack = "", None, False
    for _ in range(300):
        fr = c.recv(timeout=15)
        if fr is None:
            break
        if fr.get("type") == "ok" and not saw_input_ack:
            saw_input_ack = True
        elif fr.get("type") == "guest":
            ev = fr.get("event")
            if ev == "stdout":
                stdout += fr.get("data", "")
            elif ev == "exit":
                exit_code = fr.get("code")
                break
            elif ev == "error":
                break
    check(saw_input_ack, "exec_input accepted (ok ack)")
    check("got:ping" in stdout, f"guest read live stdin -> {stdout!r}")
    check(exit_code == 0, f"tty exec exit code 0 (got {exit_code})")

    # C4: unsupported signal -> typed unsupported (fresh connection, so the
    # ended exec stream's input reader cannot consume the frame)
    c.close()
    c = None
    ck = Conn(path)
    hello(ck)
    ck.send({"type": "kill", "signal": 10})  # SIGUSR1
    r = ck.recv()
    check(r.get("type") == "error" and r.get("code") == "unsupported",
          f"kill(unsupported signal) -> unsupported (got {r})")
    ck.close()
except Exception as e:
    check(False, f"Test A error: {e}")

# C4: graceful SIGTERM stops the sandbox
try:
    c2 = Conn(ctrl_path("e2eexec"))
    hello(c2)
    c2.send({"type": "kill", "signal": 15})
    r = c2.recv()
    check(r.get("type") == "ok", f"kill(SIGTERM) acked (got {r})")
    c2.close()
except Exception as e:
    check(False, f"kill(SIGTERM) failed: {e}")
try:
    pa.wait(timeout=40)
    check(pa.returncode == 0, f"run exited 0 after SIGTERM (got {pa.returncode})")
except subprocess.TimeoutExpired:
    check(False, "run did not exit after SIGTERM")
stop("e2eexec")

# ── Test B: blocking wait returns the real exit code ────────────────────────
print("== sandbox e2ewait (workload exits 42) ==", flush=True)
pb = start("e2ewait", ["/bin/sh", "-c", "sleep 3; exit 42"], exec_channel=False)
try:
    path = wait_path(ctrl_path("e2ewait"))
    pc = Conn(path)
    hello(pc)
    pc.send({"type": "wait", "timeout_secs": 30})
    r = pc.recv(timeout=40)
    check(r is not None and r.get("type") == "wait" and r.get("exit_code") == 42
          and r.get("state") == "stopped",
          f"wait -> terminal exit_code 42 (got {r})")
    pc.close()
except Exception as e:
    check(False, f"Test B error: {e}")
try:
    pb.wait(timeout=30)
except subprocess.TimeoutExpired:
    pass
stop("e2ewait")

print(f"== live E2E: {len(FAILS)} failure(s) ==", flush=True)
sys.exit(1 if FAILS else 0)
