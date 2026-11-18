"""Unit tests for the Python SDK using an in-process fake NDJSON helper."""

import json
import os
import shutil
import socket
import sys
import tempfile
import threading
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from nanosandbox import NanosbError, SandboxBuilder, SandboxClient, VolumeBuilder  # noqa: E402
from nanosandbox.client import PROTOCOL_VERSION  # noqa: E402


class FakeHelper:
    """A scripted NDJSON helper on a temp Unix socket."""

    def __init__(self) -> None:
        self._dir = tempfile.mkdtemp(prefix="nanosb-py-")
        self.path = os.path.join(self._dir, "helper.sock")
        self._listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._listener.bind(self.path)
        self._listener.listen(16)
        self._queue = []
        self._lock = threading.Lock()
        self._last = None
        self._stop = False
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def push(self, obj) -> None:
        with self._lock:
            self._queue.append(json.dumps(obj, separators=(",", ":")))

    def _pop(self):
        with self._lock:
            return self._queue.pop(0) if self._queue else None

    def last_request(self):
        with self._lock:
            return self._last

    def _serve(self) -> None:
        self._listener.settimeout(0.2)
        while not self._stop:
            try:
                conn, _ = self._listener.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            threading.Thread(target=self._handle, args=(conn,), daemon=True).start()

    @staticmethod
    def _is_streaming(req) -> bool:
        rtype = req.get("type")
        if rtype in ("exec_stream", "attach", "metrics_stream"):
            return True
        if rtype == "logs" and req.get("follow"):
            return True
        return False

    @staticmethod
    def _is_terminal(line: str) -> bool:
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            return False
        if obj.get("type") == "guest":
            return obj.get("event") in ("exit", "error")
        if obj.get("type") == "event":
            return obj.get("event") == "done"
        return False

    def _handle(self, conn) -> None:
        reader = conn.makefile("rb")
        while True:
            line = reader.readline()
            if not line:
                break
            req = json.loads(line.decode())
            if req.get("type") != "hello":
                with self._lock:
                    self._last = req
            if req.get("type") == "hello":
                resp = {
                    "type": "hello_ok",
                    "protocol": PROTOCOL_VERSION,
                    "helper_version": "test",
                    "capabilities": ["lifecycle", "exec", "fs", "logs", "metrics"],
                }
                conn.sendall((json.dumps(resp) + "\n").encode())
                continue
            streaming = self._is_streaming(req)
            scripted = self._pop()
            if scripted is None:
                scripted = json.dumps({"type": "error", "code": "internal", "message": "no script"})
            conn.sendall((scripted + "\n").encode())
            if streaming and not self._is_terminal(scripted):
                # Flush the rest of the scripted stream without waiting for
                # another request line (the client only sends one for streams).
                while True:
                    nxt = self._pop()
                    if nxt is None:
                        break
                    conn.sendall((nxt + "\n").encode())
                    if self._is_terminal(nxt):
                        break
        conn.close()

    def close(self) -> None:
        self._stop = True
        try:
            self._listener.close()
        except OSError:
            pass
        shutil.rmtree(self._dir, ignore_errors=True)


class ClientTest(unittest.TestCase):
    def setUp(self) -> None:
        self.helper = FakeHelper()

    def tearDown(self) -> None:
        self.helper.close()

    def test_handshake_and_capabilities(self) -> None:
        client = SandboxClient.connect(self.helper.path)
        self.assertIn("lifecycle", client.capabilities())
        self.assertTrue(client.supports("exec"))

    def test_ping(self) -> None:
        self.helper.push({"type": "pong", "version": "9"})
        client = SandboxClient.connect(self.helper.path)
        self.assertEqual(client.ping(), "9")

    def test_list(self) -> None:
        self.helper.push(
            {"type": "list", "sandboxes": [{"name": "a", "state": "running", "image": "alpine"}]}
        )
        client = SandboxClient.connect(self.helper.path)
        self.assertEqual(client.list()[0]["name"], "a")

    def test_exec(self) -> None:
        self.helper.push(
            {"type": "exec_result", "exit_code": 0, "stdout": "hi\n", "stderr": "", "duration_ms": 1}
        )
        client = SandboxClient.connect(self.helper.path)
        out = client.sandbox("a").exec("echo", ["hi"])
        self.assertEqual(out["stdout"], "hi\n")

    def test_error_is_typed(self) -> None:
        self.helper.push({"type": "error", "code": "not_found", "message": "nope"})
        client = SandboxClient.connect(self.helper.path)
        with self.assertRaises(NanosbError) as ctx:
            client.get("missing")
        self.assertTrue(ctx.exception.is_not_found())

    def test_fs_write_then_read(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})
        self.helper.push({"type": "fs_data", "data_base64": "aGVsbG8=", "eof": True})
        client = SandboxClient.connect(self.helper.path)
        box = client.sandbox("a")
        box.fs_write("/tmp/x", b"hello")
        self.assertEqual(box.fs_read("/tmp/x"), b"hello")

    def test_exec_stream(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})
        self.helper.push({"type": "guest", "event": "stdout", "data": "a"})
        self.helper.push({"type": "guest", "event": "exit", "code": 0})
        client = SandboxClient.connect(self.helper.path)
        events = []
        code = client.sandbox("a").exec_stream({"command": "x"}, events.append)
        self.assertEqual(code, 0)
        self.assertEqual(events[0]["data"], "a")

    def test_builder(self) -> None:
        spec = (
            SandboxBuilder("alpine")
            .with_name("demo")
            .with_resources(2, 512)
            .publish_tcp(8080, 80)
            .with_label("team", "sdk")
            .build()
        )
        self.assertEqual(spec["name"], "demo")
        self.assertEqual(spec["network"]["publish"][0]["protocol"], "tcp")
        self.assertEqual(spec["labels"]["team"], "sdk")

    def test_builder_mount_and_secret_methods(self) -> None:
        spec = (
            SandboxBuilder("alpine")
            .detached(False)
            .hostname("box")
            .mount_file("/host/f", "/guest/f")
            .mount_tmpfs("/tmp")
            .mount_options(["noexec", "nosuid"])
            .mount_owner(1000, 1000)
            .secret_env("API_KEY", "api-key")
            .build()
        )
        self.assertFalse(spec["detached"])
        self.assertEqual(spec["hostname"], "box")
        self.assertTrue(spec["mounts"][0]["file"])
        self.assertTrue(spec["mounts"][1]["tmpfs"])
        self.assertEqual(spec["mounts"][1]["mount_type"], "tmpfs")
        self.assertEqual(spec["mounts"][1]["options"], ["noexec", "nosuid"])
        self.assertEqual(spec["mounts"][1]["uid"], 1000)
        self.assertEqual(spec["mounts"][1]["gid"], 1000)
        self.assertEqual(spec["secret_env"]["API_KEY"], "api-key")

    def test_volume_builder(self) -> None:
        vol = VolumeBuilder("data").with_dir("/tmp/data").volume_quota(1024).build()
        self.assertEqual(vol["name"], "data")
        self.assertEqual(vol["dir"], "/tmp/data")
        self.assertEqual(vol["quota_bytes"], 1024)

    def test_modify(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})
        client = SandboxClient.connect(self.helper.path)
        client.sandbox("a").modify({"idle_timeout_secs": 300, "labels": {"team": "sdk"}})

    def test_wait_until_stopped(self) -> None:
        self.helper.push({"type": "wait", "exit_code": 0, "exited_at_ms": 1000, "state": "stopped"})
        client = SandboxClient.connect(self.helper.path)
        state = client.sandbox("a").wait_until_stopped(5)
        self.assertEqual(state, "stopped")

    def test_wait_returns_triple(self) -> None:
        self.helper.push({"type": "wait", "exit_code": 7, "exited_at_ms": 1234, "state": "stopped"})
        client = SandboxClient.connect(self.helper.path)
        result = client.sandbox("a").wait()
        self.assertEqual(result, (7, 1234, "stopped"))

    def test_wait_sends_timeout_secs(self) -> None:
        self.helper.push({"type": "wait", "exit_code": 0, "exited_at_ms": 1, "state": "stopped"})
        client = SandboxClient.connect(self.helper.path)
        client.sandbox("a").wait(5)
        self.assertEqual(self.helper.last_request()["timeout_secs"], 5)

    def test_kill_with_sends_signal(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})
        client = SandboxClient.connect(self.helper.path)
        client.sandbox("a").kill_with(15)
        self.assertEqual(self.helper.last_request()["signal"], 15)

    def test_restart_sends_type_and_name(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})
        client = SandboxClient.connect(self.helper.path)
        client.sandbox("a").restart()
        req = self.helper.last_request()
        self.assertEqual(req["type"], "restart")
        self.assertEqual(req["name"], "a")

    def test_wait_error_raises(self) -> None:
        self.helper.push({"type": "error", "code": "not_found", "message": "nope"})
        client = SandboxClient.connect(self.helper.path)
        with self.assertRaises(NanosbError) as ctx:
            client.sandbox("a").wait()
        self.assertTrue(ctx.exception.is_not_found())

    def test_shell_stream(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})
        self.helper.push({"type": "guest", "event": "stdout", "data": "hi"})
        self.helper.push({"type": "guest", "event": "exit", "code": 0})
        client = SandboxClient.connect(self.helper.path)
        out = []
        code = client.sandbox("a").shell_stream("echo hi", out.append)
        self.assertEqual(code, 0)
        self.assertEqual(out[0]["data"], "hi")

    def test_attach_session_drives_input_resize_and_reads_events(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})  # attach ack
        self.helper.push({"type": "ok", "version": "1"})  # send_input ack
        self.helper.push({"type": "ok", "version": "1"})  # resize ack
        self.helper.push({"type": "guest", "event": "stdout", "data": "hi"})
        self.helper.push({"type": "guest", "event": "exit", "code": 0})
        client = SandboxClient.connect(self.helper.path)
        session = client.sandbox("a").attach_session()
        try:
            session.send_input(b"x")
            session.resize(80, 24)
            out = []
            while True:
                ev = session.read_events()
                if ev is None:
                    break
                out.append(ev["data"])
            self.assertEqual(out, ["hi"])
        finally:
            session.close()

    def test_metrics_stream(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})
        self.helper.push(
            {
                "type": "event",
                "event": "metrics",
                "cpu_percent": 1.5,
                "memory_used_bytes": 10,
                "memory_total_bytes": 100,
                "disk_used_bytes": 0,
                "disk_total_bytes": 0,
                "net_rx_bytes": 0,
                "net_tx_bytes": 0,
                "timestamp_ms": 0,
            }
        )
        self.helper.push({"type": "event", "event": "done"})
        client = SandboxClient.connect(self.helper.path)
        samples = []
        client.sandbox("a").metrics_stream(100, samples.append)
        self.assertEqual(len(samples), 1)
        self.assertEqual(samples[0]["cpu_percent"], 1.5)

    def test_logs_with_cursor_and_source(self) -> None:
        self.helper.push(
            {
                "type": "logs",
                "lines": [{"cursor": 5, "timestamp_ms": 0, "source": "stdout", "data": "x"}],
                "next_cursor": 6,
            }
        )
        client = SandboxClient.connect(self.helper.path)
        lines = client.sandbox("a").logs_with(tail=10, cursor=5, source="stdout")
        self.assertEqual(len(lines), 1)
        self.assertEqual(lines[0]["cursor"], 5)

    def test_logs_follow_with_cursor_and_source(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})
        self.helper.push(
            {"type": "event", "event": "log", "cursor": 5, "timestamp_ms": 0, "source": "stdout", "data": "x"}
        )
        self.helper.push({"type": "event", "event": "done"})
        client = SandboxClient.connect(self.helper.path)
        out = []
        client.sandbox("a").logs_follow_with(out.append, tail=10, cursor=5, source="stdout")
        self.assertEqual([line["data"] for line in out], ["x"])

    def test_fs_read_range(self) -> None:
        self.helper.push({"type": "fs_data", "data_base64": "aGVsbG8=", "eof": True})
        client = SandboxClient.connect(self.helper.path)
        self.assertEqual(client.sandbox("a").fs_read_range("/tmp/x", 0, 5), b"hello")

    def test_fs_read_stream_chunks(self) -> None:
        self.helper.push({"type": "fs_data", "data_base64": "aGVsbG8=", "eof": True})
        self.helper.push({"type": "fs_data", "data_base64": "", "eof": True})
        client = SandboxClient.connect(self.helper.path)
        out = bytearray()
        client.sandbox("a").fs_read_stream("/tmp/x", 3, out.extend)
        self.assertEqual(bytes(out), b"hello")

    def test_fs_write_stream(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})  # opening ack
        self.helper.push({"type": "ok", "version": "1"})  # chunk 1 ack
        self.helper.push({"type": "ok", "version": "1"})  # chunk 2 ack
        client = SandboxClient.connect(self.helper.path)
        client.sandbox("a").fs_write_stream("/tmp/big.bin", b"chunk1chunk2", 6)

    def test_volume_get_remove_and_fs(self) -> None:
        self.helper.push({"type": "volume", "name": "data", "dir": "/tmp/vol-data", "used_bytes": 0})
        self.helper.push({"type": "volume", "name": "data", "dir": "/tmp/vol-data", "used_bytes": 0})
        self.helper.push({"type": "ok", "version": "1"})
        client = SandboxClient.connect(self.helper.path)
        box = client.sandbox("a")
        self.assertEqual(box.volume_get("data")["name"], "data")
        self.assertEqual(box.volume_fs("data"), "/tmp/vol-data")
        box.volume_remove("data")

    def test_mount_add(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})
        client = SandboxClient.connect(self.helper.path)
        client.sandbox("a").mount_add(
            {"host_path": "data", "guest_path": "/mnt/data", "mount_type": "bind"}
        )

    def test_secret_remove(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})
        client = SandboxClient.connect(self.helper.path)
        client.sandbox("a").secret_remove("API_KEY")

    def test_snapshot_restore_fork_remove(self) -> None:
        self.helper.push({"type": "ok", "version": "1"})
        self.helper.push({"type": "ok", "version": "1"})
        self.helper.push({"type": "ok", "version": "1"})
        client = SandboxClient.connect(self.helper.path)
        box = client.sandbox("a")
        box.snapshot_restore("snap-1", "box")
        box.snapshot_fork("snap-1", "forked")
        box.snapshot_remove("snap-1")

    def test_list_page_returns_next_cursor(self) -> None:
        self.helper.push(
            {
                "type": "list",
                "sandboxes": [{"name": "a", "state": "running", "image": "alpine"}],
                "next_cursor": "abc",
            }
        )
        client = SandboxClient.connect(self.helper.path)
        page = client.list_page(limit=10, cursor="prev", label="team=sdk")
        self.assertEqual(len(page["sandboxes"]), 1)
        self.assertEqual(page["next_cursor"], "abc")

    def test_list_with_label(self) -> None:
        self.helper.push(
            {"type": "list", "sandboxes": [{"name": "a", "state": "running", "image": "alpine"}]}
        )
        client = SandboxClient.connect(self.helper.path)
        self.assertEqual(client.list_with_label("team=sdk")[0]["name"], "a")

    def test_snapshots(self) -> None:
        self.helper.push(
            {
                "type": "snapshots",
                "snapshots": [{"id": "snap-1", "sandbox": "box", "created_at_ms": 1000}],
            }
        )
        client = SandboxClient.connect(self.helper.path)
        self.assertEqual(client.snapshots()[0]["id"], "snap-1")


if __name__ == "__main__":
    unittest.main()
