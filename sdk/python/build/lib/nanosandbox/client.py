"""Protocol client for the nanosandbox helper (nanosb-protocol v1.0.0)."""

from __future__ import annotations

import base64
import json
import os
import socket
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional

PROTOCOL_VERSION = "1.0.0"
CLIENT_NAME = "nanosandbox-python"


class NanosbError(Exception):
    """A structured error returned by the helper."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message

    def is_not_found(self) -> bool:
        return self.code == "not_found"


def _b64(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def _unb64(text: str) -> bytes:
    return base64.b64decode(text)


class _Conn:
    """One negotiated connection (dial -> hello -> request -> response)."""

    def __init__(self, path: str, client_name: str = CLIENT_NAME) -> None:
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._sock.connect(path)
        self._reader = self._sock.makefile("rb")
        self.hello = self._handshake(client_name)

    def _handshake(self, client_name: str) -> Dict[str, Any]:
        self.send(
            {
                "type": "hello",
                "client": client_name,
                "sdk_version": PROTOCOL_VERSION,
                "protocol_min": PROTOCOL_VERSION,
                "protocol_max": PROTOCOL_VERSION,
            }
        )
        resp = self.recv()
        if resp.get("type") != "hello_ok":
            raise NanosbError(resp.get("code", "internal"), resp.get("message", "bad handshake"))
        if resp.get("protocol") != PROTOCOL_VERSION:
            raise NanosbError(
                "incompatible_version",
                f"helper speaks {resp.get('protocol')}, client needs {PROTOCOL_VERSION}",
            )
        return resp

    def send(self, message: Dict[str, Any]) -> None:
        line = json.dumps(message, separators=(",", ":")) + "\n"
        self._sock.sendall(line.encode("utf-8"))

    def recv(self) -> Dict[str, Any]:
        line = self._reader.readline()
        if not line:
            raise NanosbError("transport", "connection closed")
        return json.loads(line.decode("utf-8"))

    def call(self, message: Dict[str, Any]) -> Dict[str, Any]:
        self.send(message)
        resp = self.recv()
        if resp.get("type") == "error":
            raise NanosbError(resp.get("code", "internal"), resp.get("message", ""))
        return resp

    def close(self) -> None:
        try:
            self._reader.close()
        finally:
            self._sock.close()

    def __enter__(self) -> "_Conn":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()


@dataclass
class SandboxBuilder:
    """Fluent builder for a sandbox creation spec."""

    image: str
    name: Optional[str] = None
    cpus: Optional[int] = None
    memory_mb: Optional[int] = None
    env: Dict[str, str] = field(default_factory=dict)
    mounts: List[Dict[str, Any]] = field(default_factory=list)
    network: Dict[str, Any] = field(default_factory=dict)
    workdir: Optional[str] = None
    timeout_secs: Optional[int] = None
    user: Optional[str] = None
    home: Optional[str] = None
    command: Optional[str] = None
    command_args: List[str] = field(default_factory=list)
    labels: Dict[str, str] = field(default_factory=dict)
    ephemeral: bool = False
    replace: bool = False
    _detached: bool = True
    _hostname: Optional[str] = None
    _secret_env: Dict[str, str] = field(default_factory=dict)

    def with_name(self, name: str) -> "SandboxBuilder":
        self.name = name
        return self

    def with_resources(self, cpus: int, memory_mb: int) -> "SandboxBuilder":
        self.cpus = cpus
        self.memory_mb = memory_mb
        return self

    def with_env(self, key: str, value: str) -> "SandboxBuilder":
        self.env[key] = value
        return self

    def with_label(self, key: str, value: str) -> "SandboxBuilder":
        self.labels[key] = value
        return self

    def mount(self, host_path: str, guest_path: str, readonly: bool = False) -> "SandboxBuilder":
        self.mounts.append(
            {
                "host_path": host_path,
                "guest_path": guest_path,
                "readonly": readonly,
                "mount_type": "virtiofs",
            }
        )
        return self

    def publish_tcp(self, host_port: int, guest_port: int) -> "SandboxBuilder":
        self.network.setdefault("enabled", True)
        self.network.setdefault("publish", []).append(
            {"host_port": host_port, "container_port": guest_port, "protocol": "tcp"}
        )
        return self

    def publish_udp(self, host_port: int, guest_port: int) -> "SandboxBuilder":
        self.network.setdefault("enabled", True)
        self.network.setdefault("publish", []).append(
            {"host_port": host_port, "container_port": guest_port, "protocol": "udp"}
        )
        return self

    def detached(self, detached: bool) -> "SandboxBuilder":
        self._detached = detached
        return self

    def hostname(self, hostname: str) -> "SandboxBuilder":
        self._hostname = hostname
        return self

    def mount_file(self, host_path: str, guest_path: str) -> "SandboxBuilder":
        self.mounts.append(
            {
                "host_path": host_path,
                "guest_path": guest_path,
                "readonly": False,
                "mount_type": "virtiofs",
                "file": True,
            }
        )
        return self

    def mount_tmpfs(self, guest_path: str) -> "SandboxBuilder":
        self.mounts.append(
            {
                "host_path": "",
                "guest_path": guest_path,
                "readonly": False,
                "mount_type": "tmpfs",
                "tmpfs": True,
            }
        )
        return self

    def mount_options(self, options: List[str]) -> "SandboxBuilder":
        if self.mounts:
            self.mounts[-1]["options"] = list(options)
        return self

    def mount_owner(self, uid: int, gid: int) -> "SandboxBuilder":
        if self.mounts:
            self.mounts[-1]["uid"] = uid
            self.mounts[-1]["gid"] = gid
        return self

    def secret_env(self, key: str, secret_key: str) -> "SandboxBuilder":
        self._secret_env[key] = secret_key
        return self

    def build(self) -> Dict[str, Any]:
        spec: Dict[str, Any] = {"image": self.image, "detached": self._detached}
        for key in ("name", "cpus", "memory_mb", "workdir", "timeout_secs", "user", "home", "command"):
            value = getattr(self, key)
            if value is not None:
                spec[key] = value
        if self._hostname is not None:
            spec["hostname"] = self._hostname
        if self.env:
            spec["env"] = self.env
        if self.mounts:
            spec["mounts"] = self.mounts
        if self.network:
            spec["network"] = self.network
        if self.command_args:
            spec["command_args"] = self.command_args
        if self.labels:
            spec["labels"] = self.labels
        if self.ephemeral:
            spec["ephemeral"] = True
        if self.replace:
            spec["replace"] = True
        if self._secret_env:
            spec["secret_env"] = self._secret_env
        return spec


@dataclass
class VolumeBuilder:
    """Fluent builder for a volume creation spec."""

    name: str
    dir: Optional[str] = None
    disk_image: Optional[str] = None
    fstype: Optional[str] = None
    quota_bytes: Optional[int] = None

    def with_dir(self, dir: str) -> "VolumeBuilder":
        self.dir = dir
        return self

    def disk_image(self, image: str) -> "VolumeBuilder":
        self.disk_image = image
        return self

    def fstype(self, fstype: str) -> "VolumeBuilder":
        self.fstype = fstype
        return self

    def volume_quota(self, bytes: int) -> "VolumeBuilder":
        self.quota_bytes = bytes
        return self

    def build(self) -> Dict[str, Any]:
        spec: Dict[str, Any] = {"name": self.name}
        for key in ("dir", "disk_image", "fstype", "quota_bytes"):
            value = getattr(self, key)
            if value is not None:
                spec[key] = value
        return spec


class Sandbox:
    """A handle bound to one sandbox."""

    def __init__(self, client: "SandboxClient", name: str) -> None:
        self._client = client
        self._name = name

    @property
    def name(self) -> str:
        return self._name

    # lifecycle ---------------------------------------------------------
    def state(self) -> str:
        return self._client._call({"type": "status", "name": self._name})["state"]

    def info(self) -> Dict[str, Any]:
        return self._client.get(self._name)

    def start(self) -> None:
        self._client._call({"type": "start", "name": self._name})

    def stop(self, force: bool = False) -> None:
        self._client._call({"type": "stop", "name": self._name, "force": force})

    def kill(self) -> None:
        self._client._call({"type": "kill", "name": self._name})

    def remove(self, force: bool = False) -> None:
        self._client._call({"type": "remove", "name": self._name, "force": force})

    def pause(self) -> None:
        self._client._call({"type": "pause", "name": self._name})

    def resume(self) -> None:
        self._client._call({"type": "resume", "name": self._name})

    def touch(self) -> None:
        self._client._call({"type": "touch", "name": self._name})

    def drain(self) -> None:
        self._client._call({"type": "drain", "name": self._name})

    def modify(self, spec: Dict[str, Any]) -> None:
        self._client._call({"type": "modify", "name": self._name, "spec": spec})

    def wait_until_stopped(self, timeout_secs: float = 30.0) -> str:
        deadline = time.monotonic() + timeout_secs
        while True:
            st = self.state()
            if st == "stopped":
                return st
            if st == "error":
                raise NanosbError("internal", "sandbox entered error state while waiting")
            if time.monotonic() >= deadline:
                raise NanosbError(
                    "timeout",
                    f"sandbox '{self._name}' did not stop within {timeout_secs}s",
                )
            time.sleep(0.1)

    # execution ---------------------------------------------------------
    def exec(self, command: str, args: Optional[List[str]] = None) -> Dict[str, Any]:
        return self.exec_with({"command": command, "args": args or []})

    def exec_with(self, request: Dict[str, Any]) -> Dict[str, Any]:
        req = dict(request)
        req["type"] = "exec"
        req["name"] = self._name
        req.setdefault("args", [])
        return self._client._call(req)

    def shell(self, script: str) -> Dict[str, Any]:
        return self._client._call({"type": "shell", "name": self._name, "script": script})

    def exec_stream(self, request: Dict[str, Any], on_event: Callable[[Dict[str, Any]], None]) -> int:
        req = dict(request)
        req["type"] = "exec_stream"
        req["name"] = self._name
        req.setdefault("args", [])
        code = -1
        with _Conn(self._client._socket) as conn:
            conn.send(req)
            ack = conn.recv()
            if ack.get("type") == "error":
                raise NanosbError(ack.get("code", "internal"), ack.get("message", ""))
            while True:
                resp = conn.recv()
                if resp.get("type") != "guest":
                    continue
                event = resp.get("event")
                if event == "exit":
                    code = resp.get("code", -1)
                    on_event(resp)
                    break
                if event == "error":
                    on_event(resp)
                    raise NanosbError("internal", resp.get("message", ""))
                on_event(resp)
        return code

    def shell_stream(self, script: str, on_event: Callable[[Dict[str, Any]], None]) -> int:
        return self.exec_stream({"command": script, "shell": True}, on_event)

    def attach(self, on_event: Callable[[Dict[str, Any]], None]) -> None:
        with _Conn(self._client._socket) as conn:
            conn.send({"type": "attach", "name": self._name})
            ack = conn.recv()
            if ack.get("type") == "error":
                raise NanosbError(ack.get("code", "internal"), ack.get("message", ""))
            while True:
                resp = conn.recv()
                if resp.get("type") != "guest":
                    continue
                on_event(resp)
                if resp.get("event") in ("exit", "error"):
                    break

    def attach_session(self) -> "AttachSession":
        conn = _Conn(self._client._socket)
        conn.send({"type": "attach", "name": self._name})
        ack = conn.recv()
        if ack.get("type") == "error":
            conn.close()
            raise NanosbError(ack.get("code", "internal"), ack.get("message", ""))
        return AttachSession(conn)

    # observability -----------------------------------------------------
    def metrics(self) -> Dict[str, Any]:
        return self._client._call({"type": "metrics", "name": self._name})

    def metrics_stream(
        self, interval_ms: Optional[int], on_event: Callable[[Dict[str, Any]], None]
    ) -> None:
        req: Dict[str, Any] = {"type": "metrics_stream", "name": self._name}
        if interval_ms is not None:
            req["interval_ms"] = interval_ms
        with _Conn(self._client._socket) as conn:
            conn.send(req)
            ack = conn.recv()
            if ack.get("type") == "error":
                raise NanosbError(ack.get("code", "internal"), ack.get("message", ""))
            while True:
                resp = conn.recv()
                if resp.get("type") != "event":
                    continue
                if resp.get("event") == "done":
                    break
                if resp.get("event") == "metrics":
                    on_event(resp)

    def logs(self, tail: Optional[int] = None) -> List[Dict[str, Any]]:
        req: Dict[str, Any] = {"type": "logs", "name": self._name, "follow": False}
        if tail is not None:
            req["tail"] = tail
        return self._client._call(req)["lines"]

    def logs_with(
        self,
        tail: Optional[int] = None,
        cursor: Optional[int] = None,
        source: Optional[str] = None,
    ) -> List[Dict[str, Any]]:
        req: Dict[str, Any] = {"type": "logs", "name": self._name, "follow": False}
        if tail is not None:
            req["tail"] = tail
        if cursor is not None:
            req["cursor"] = cursor
        if source is not None:
            req["source"] = source
        return self._client._call(req)["lines"]

    def logs_follow(self, on_line: Callable[[Dict[str, Any]], None], tail: Optional[int] = None) -> None:
        req: Dict[str, Any] = {"type": "logs", "name": self._name, "follow": True}
        if tail is not None:
            req["tail"] = tail
        with _Conn(self._client._socket) as conn:
            conn.send(req)
            ack = conn.recv()
            if ack.get("type") == "error":
                raise NanosbError(ack.get("code", "internal"), ack.get("message", ""))
            while True:
                resp = conn.recv()
                if resp.get("type") != "event":
                    continue
                if resp.get("event") == "done":
                    break
                if resp.get("event") == "log":
                    on_line(resp)

    def logs_follow_with(
        self,
        on_line: Callable[[Dict[str, Any]], None],
        tail: Optional[int] = None,
        cursor: Optional[int] = None,
        source: Optional[str] = None,
    ) -> None:
        req: Dict[str, Any] = {"type": "logs", "name": self._name, "follow": True}
        if tail is not None:
            req["tail"] = tail
        if cursor is not None:
            req["cursor"] = cursor
        if source is not None:
            req["source"] = source
        with _Conn(self._client._socket) as conn:
            conn.send(req)
            ack = conn.recv()
            if ack.get("type") == "error":
                raise NanosbError(ack.get("code", "internal"), ack.get("message", ""))
            while True:
                resp = conn.recv()
                if resp.get("type") != "event":
                    continue
                if resp.get("event") == "done":
                    break
                if resp.get("event") == "log":
                    on_line(resp)

    # filesystem --------------------------------------------------------
    def fs_read(self, path: str, offset: Optional[int] = None, length: Optional[int] = None) -> bytes:
        req: Dict[str, Any] = {"type": "fs_read", "name": self._name, "path": path}
        if offset is not None:
            req["offset"] = offset
        if length is not None:
            req["length"] = length
        return _unb64(self._client._call(req)["data_base64"])

    def fs_read_range(self, path: str, offset: int, length: int) -> bytes:
        return _unb64(
            self._client._call(
                {"type": "fs_read", "name": self._name, "path": path, "offset": offset, "length": length}
            )["data_base64"]
        )

    def fs_read_stream(
        self, path: str, chunk_size: int, on_chunk: Callable[[bytes], None]
    ) -> None:
        chunk_size = max(chunk_size, 1)
        offset = 0
        while True:
            chunk = self.fs_read_range(path, offset, chunk_size)
            if not chunk:
                break
            on_chunk(chunk)
            if len(chunk) < chunk_size:
                break
            offset += len(chunk)

    def fs_write_stream(self, path: str, data: bytes, chunk_size: int) -> None:
        with _Conn(self._client._socket) as conn:
            conn.send({"type": "fs_write_stream", "name": self._name, "path": path})
            ack = conn.recv()
            if ack.get("type") == "error":
                raise NanosbError(ack.get("code", "internal"), ack.get("message", ""))
            chunk_size = max(chunk_size, 1)
            chunks = [data[i : i + chunk_size] for i in range(0, len(data), chunk_size)]
            if not chunks:
                chunks = [b""]
            for i, chunk in enumerate(chunks):
                conn.call(
                    {"type": "fs_data", "data_base64": _b64(chunk), "eof": i == len(chunks) - 1}
                )

    def fs_read_to_string(self, path: str) -> str:
        return self._client._call(
            {"type": "fs_read_to_string", "name": self._name, "path": path}
        )["content"]

    def fs_write(self, path: str, data: bytes, append: bool = False) -> None:
        self._client._call(
            {
                "type": "fs_write",
                "name": self._name,
                "path": path,
                "data_base64": _b64(data),
                "append": append,
            }
        )

    def fs_list(self, path: str) -> List[Dict[str, Any]]:
        return self._client._call({"type": "fs_list", "name": self._name, "path": path})["entries"]

    def fs_stat(self, path: str) -> Dict[str, Any]:
        return self._client._call({"type": "fs_stat", "name": self._name, "path": path})

    def fs_exists(self, path: str) -> bool:
        return self._client._call({"type": "fs_exists", "name": self._name, "path": path})["exists"]

    def fs_mkdir(self, path: str, recursive: bool = False) -> None:
        self._client._call(
            {"type": "fs_mkdir", "name": self._name, "path": path, "recursive": recursive}
        )

    def fs_remove(self, path: str) -> None:
        self._client._call({"type": "fs_remove", "name": self._name, "path": path})

    def fs_remove_dir(self, path: str, recursive: bool = False) -> None:
        self._client._call(
            {"type": "fs_remove_dir", "name": self._name, "path": path, "recursive": recursive}
        )

    def fs_copy(self, src: str, dst: str) -> None:
        self._client._call({"type": "fs_copy", "name": self._name, "from": src, "to": dst})

    def fs_rename(self, src: str, dst: str) -> None:
        self._client._call({"type": "fs_rename", "name": self._name, "from": src, "to": dst})

    def fs_copy_from_host(self, host_path: str, guest_path: str) -> None:
        self._client._call(
            {"type": "fs_copy_from_host", "name": self._name, "host_path": host_path, "guest_path": guest_path}
        )

    def fs_copy_to_host(self, guest_path: str, host_path: str) -> None:
        self._client._call(
            {"type": "fs_copy_to_host", "name": self._name, "host_path": host_path, "guest_path": guest_path}
        )

    # storage / secrets / snapshots ------------------------------------
    def volume_create(self, name: str, dir: Optional[str] = None) -> Dict[str, Any]:
        return self._client._call(
            {"type": "volume_create", "name": self._name, "volume": {"name": name, "dir": dir}}
        )

    def volume_list(self) -> List[Dict[str, Any]]:
        return self._client._call({"type": "volume_list", "name": self._name})["volumes"]

    def volume_get(self, volume: str) -> Dict[str, Any]:
        return self._client._call({"type": "volume_get", "name": self._name, "volume": volume})

    def volume_remove(self, volume: str) -> None:
        self._client._call({"type": "volume_remove", "name": self._name, "volume": volume})

    def volume_fs(self, name: str) -> str:
        info = self.volume_get(name)
        if info.get("dir"):
            return info["dir"]
        home = os.environ.get("NANOSB_HOME") or os.path.join(
            os.path.expanduser("~"), ".nanosandbox"
        )
        return os.path.join(home, "volumes", name)

    def mount_add(self, mount: Dict[str, Any]) -> None:
        self._client._call({"type": "mount_add", "name": self._name, "mount": mount})

    def secret_set(self, key: str, value: str) -> None:
        self._client._call({"type": "secret_set", "name": self._name, "key": key, "value": value})

    def secret_list(self) -> List[Dict[str, Any]]:
        return self._client._call({"type": "secret_list", "name": self._name})["secrets"]

    def secret_remove(self, key: str) -> None:
        self._client._call({"type": "secret_remove", "name": self._name, "key": key})

    def snapshot_create(self, dest: Optional[str] = None) -> Dict[str, Any]:
        return self._client._call(
            {"type": "snapshot_create", "name": self._name, "dest": dest, "include_memory": False}
        )

    def snapshot_restore(self, snapshot: str, name: str) -> None:
        self._client._call({"type": "snapshot_restore", "snapshot": snapshot, "name": name})

    def snapshot_fork(self, snapshot: str, name: str) -> None:
        self._client._call({"type": "snapshot_fork", "snapshot": snapshot, "name": name})

    def snapshot_remove(self, snapshot: str) -> None:
        self._client._call({"type": "snapshot_remove", "snapshot": snapshot})


class AttachSession:
    """A live attach session: drive console input/resize and read guest events
    on the same connection. `read_events` returns `None` once the stream ends."""

    def __init__(self, conn: _Conn) -> None:
        self._conn = conn

    def send_input(self, data: bytes) -> None:
        text = data.decode("utf-8", errors="replace")
        self._conn.call({"type": "attach_input", "data": text})

    def resize(self, cols: int, rows: int) -> None:
        self._conn.call({"type": "attach_resize", "cols": cols, "rows": rows})

    def read_events(self) -> Optional[Dict[str, Any]]:
        while True:
            resp = self._conn.recv()
            if resp.get("type") != "guest":
                continue
            if resp.get("event") in ("exit", "error"):
                return None
            return resp

    def close(self) -> None:
        self._conn.close()


class SandboxClient:
    """A protocol client. Each call dials and re-negotiates (stateless)."""

    def __init__(self, socket_path: str) -> None:
        self._socket = socket_path
        with _Conn(socket_path) as conn:
            self._caps: List[str] = conn.hello.get("capabilities", [])

    @classmethod
    def connect(cls, socket_path: str) -> "SandboxClient":
        return cls(socket_path)

    def capabilities(self) -> List[str]:
        return list(self._caps)

    def supports(self, capability: str) -> bool:
        return capability in self._caps

    def _call(self, message: Dict[str, Any]) -> Dict[str, Any]:
        with _Conn(self._socket) as conn:
            return conn.call(message)

    def ping(self) -> str:
        return self._call({"type": "ping"})["version"]

    def list(self, all: bool = False, label: Optional[str] = None) -> List[Dict[str, Any]]:
        req: Dict[str, Any] = {"type": "list", "all": all}
        if label is not None:
            req["label"] = label
        return self._call(req)["sandboxes"]

    def list_with_label(self, label: str, all: bool = False) -> List[Dict[str, Any]]:
        return self._call({"type": "list", "all": all, "label": label})["sandboxes"]

    def list_page(
        self,
        limit: Optional[int] = None,
        cursor: Optional[str] = None,
        label: Optional[str] = None,
    ) -> Dict[str, Any]:
        req: Dict[str, Any] = {"type": "list", "all": True}
        if limit is not None:
            req["limit"] = limit
        if cursor is not None:
            req["cursor"] = cursor
        if label is not None:
            req["label"] = label
        return self._call(req)

    def snapshots(self) -> List[Dict[str, Any]]:
        return self._call({"type": "snapshot_list"})["snapshots"]

    def get(self, name: str) -> Dict[str, Any]:
        return self._call({"type": "get", "name": name})

    def create(self, spec: Dict[str, Any]) -> Sandbox:
        self._call({"type": "create", "spec": spec})
        return Sandbox(self, spec["name"])

    def create_with(self, builder: SandboxBuilder) -> Sandbox:
        spec = builder.build()
        if "name" not in spec:
            raise ValueError("spec.name is required to create")
        return self.create(spec)

    def sandbox(self, name: str) -> Sandbox:
        return Sandbox(self, name)
