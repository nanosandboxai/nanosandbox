#!/usr/bin/env python3
"""Conformance driver for the Python SDK.

Runs every scenario in `sdk/conformance/scenarios/` against the
`nanosb-conformance` harness and evaluates the scenario `assert` blocks per
`sdk/conformance/SPEC.md`. Exits non-zero if any scenario fails.

Usage:
    python3 sdk/python/conformance/run.py
    NANOSB_CONFORMANCE_BIN=/path/to/nanosb-conformance python3 sdk/python/conformance/run.py
"""

from __future__ import annotations

import base64
import json
import os
import signal
import subprocess
import sys
from typing import Any, Dict, List, Optional

_HERE = os.path.dirname(os.path.abspath(__file__))
_SDK_ROOT = os.path.dirname(_HERE)  # sdk/python
_REPO_ROOT = os.path.dirname(os.path.dirname(_SDK_ROOT))  # repo root
_SCENARIOS_DIR = os.path.join(_REPO_ROOT, "sdk", "conformance", "scenarios")

if _SDK_ROOT not in sys.path:
    sys.path.insert(0, _SDK_ROOT)

from nanosandbox import NanosbError, SandboxClient  # noqa: E402
from nanosandbox.client import _Conn  # noqa: E402

# A generous per-scenario wall-clock budget so a wedged stream cannot hang CI.
_SCENARIO_BUDGET_SECS = 60


class _BudgetExceeded(Exception):
    pass


def _alarm_handler(signum: int, frame: Any) -> None:
    raise _BudgetExceeded("scenario exceeded time budget")


def _b64(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def _unb64(text: str) -> bytes:
    return base64.b64decode(text)


class DriverState:
    """Accumulated observations from driving a scenario's steps."""

    def __init__(self) -> None:
        self.acked = set()
        self.errors: List[tuple] = []
        self.exit_code: Optional[int] = None
        self.stdout = ""
        self.stderr = ""
        self.get_result: Optional[Dict[str, Any]] = None
        self.list_result: Optional[List[Dict[str, Any]]] = None
        self.fs_entries: Optional[List[Dict[str, Any]]] = None
        self.fs_stat: Optional[Dict[str, Any]] = None
        self.fs_exists: Optional[bool] = None
        self.fs_read_bytes: Optional[bytes] = None
        self.fs_content: Optional[str] = None
        self.chunks_acked = 0
        self.first_logs: Optional[Dict[str, Any]] = None
        self.resume_logs: Optional[Dict[str, Any]] = None
        self.follow_lines: List[Dict[str, Any]] = []
        self.metrics: Optional[Dict[str, Any]] = None
        self.metrics_samples: List[Dict[str, Any]] = []
        self.volume_created: Optional[Dict[str, Any]] = None
        self.volumes: Optional[List[Dict[str, Any]]] = None
        self.volume_get: Optional[Dict[str, Any]] = None
        self.secrets: Optional[List[Dict[str, Any]]] = None
        self.snapshot_created: Optional[Dict[str, Any]] = None
        self.snapshots: Optional[List[Dict[str, Any]]] = None
        self.attach_events: List[Dict[str, Any]] = []
        self.attach_exit_code: Optional[int] = None
        self.ping_ok = False
        self.wait_result: Optional[tuple] = None
        self.timeout_code: Optional[str] = None
        self.unsupported_observed = False


def _req_from_expect(expect: Dict[str, Any]) -> Dict[str, Any]:
    """The request fields for a verb, minus the `type`/`name` routing keys."""
    return {k: v for k, v in expect.items() if k not in ("type", "name")}


def drive_step(client: SandboxClient, state: DriverState, expect: Dict[str, Any]) -> None:
    """Invoke the SDK operation mapped from `expect.type` (SPEC §5)."""
    etype = expect["type"]
    name = expect.get("name")
    # snapshot_remove is not sandbox-scoped (no `name`); any handle works.
    box = client.sandbox(name) if name is not None else client.sandbox("")
    try:
        if etype == "create":
            client.create(expect["spec"])
            state.acked.add("create")
        elif etype == "get":
            state.get_result = client.get(name)
        elif etype == "list":
            state.list_result = client.list(all=expect.get("all", False))
        elif etype == "start":
            box.start()
            state.acked.add("start")
        elif etype == "stop":
            box.stop(force=expect.get("force", False))
            state.acked.add("stop")
        elif etype == "kill":
            if "signal" in expect:
                box.kill_with(expect["signal"])
            else:
                box.kill()
            state.acked.add("kill")
        elif etype == "wait":
            state.wait_result = box.wait(expect.get("timeout_secs"))
            state.acked.add("wait")
        elif etype == "restart":
            box.restart()
            state.acked.add("restart")
        elif etype == "remove":
            box.remove(force=expect.get("force", False))
            state.acked.add("remove")
        elif etype == "drain":
            box.drain()
            state.acked.add("drain")
        elif etype == "touch":
            box.touch()
            state.acked.add("touch")
        elif etype == "pause":
            box.pause()
            state.acked.add("pause")
        elif etype == "resume":
            box.resume()
            state.acked.add("resume")
        elif etype == "modify":
            box.modify(expect["spec"])
            state.acked.add("modify")
        elif etype == "status":
            box.state()
        elif etype == "ping":
            client.ping()
            state.ping_ok = True
        elif etype == "exec":
            result = box.exec_with(_req_from_expect(expect))
            state.exit_code = result["exit_code"]
            state.stdout = result.get("stdout", "")
            state.stderr = result.get("stderr", "")
        elif etype == "exec_stream":
            code = box.exec_stream(_req_from_expect(expect), _exec_stream_event(state))
            state.exit_code = code
        elif etype == "shell":
            result = box.shell(expect["script"])
            state.exit_code = result["exit_code"]
            state.stdout = result.get("stdout", "")
            state.stderr = result.get("stderr", "")
        elif etype == "attach":
            box.attach(state.attach_events.append)
            state.attach_exit_code = next(
                (e.get("code") for e in state.attach_events if e.get("event") == "exit"), None
            )
        elif etype == "attach_input":
            client._call({"type": "attach_input", "data": expect["data"]})
            state.acked.add("input")
        elif etype == "attach_resize":
            client._call({"type": "attach_resize", "cols": expect["cols"], "rows": expect["rows"]})
            state.acked.add("resize")
        elif etype == "fs_read":
            state.fs_read_bytes = box.fs_read(expect["path"])
        elif etype == "fs_read_to_string":
            state.fs_content = box.fs_read_to_string(expect["path"])
        elif etype == "fs_write":
            box.fs_write(expect["path"], _unb64(expect["data_base64"]))
            state.acked.add("write")
        elif etype == "fs_list":
            state.fs_entries = box.fs_list(expect["path"])
        elif etype == "fs_stat":
            state.fs_stat = box.fs_stat(expect["path"])
        elif etype == "fs_exists":
            state.fs_exists = box.fs_exists(expect["path"])
        elif etype == "fs_mkdir":
            box.fs_mkdir(expect["path"], recursive=expect.get("recursive", False))
            state.acked.add("mkdir")
        elif etype == "fs_remove":
            box.fs_remove(expect["path"])
        elif etype == "fs_remove_dir":
            box.fs_remove_dir(expect["path"], recursive=expect.get("recursive", False))
            state.acked.add("remove_dir")
        elif etype == "fs_copy":
            box.fs_copy(expect["from"], expect["to"])
            state.acked.add("copy")
        elif etype == "fs_rename":
            box.fs_rename(expect["from"], expect["to"])
            state.acked.add("rename")
        elif etype == "fs_copy_from_host":
            box.fs_copy_from_host(expect["host_path"], expect["guest_path"])
            state.acked.add("copy_from_host")
        elif etype == "fs_copy_to_host":
            box.fs_copy_to_host(expect["guest_path"], expect["host_path"])
            state.acked.add("copy_to_host")
        elif etype == "logs":
            if expect.get("follow"):
                box.logs_follow_with(
                    state.follow_lines.append,
                    tail=expect.get("tail"),
                    cursor=expect.get("cursor"),
                    source=expect.get("source"),
                )
            else:
                req: Dict[str, Any] = {"type": "logs", "name": name, "follow": False}
                for key in ("tail", "cursor", "source"):
                    if key in expect:
                        req[key] = expect[key]
                resp = client._call(req)
                if state.first_logs is None:
                    state.first_logs = resp
                else:
                    state.resume_logs = resp
        elif etype == "metrics":
            state.metrics = box.metrics()
        elif etype == "metrics_stream":
            box.metrics_stream(expect.get("interval_ms"), state.metrics_samples.append)
        elif etype == "volume_create":
            vol = expect["volume"]
            state.volume_created = box.volume_create(vol["name"], dir=vol.get("dir"))
        elif etype == "volume_list":
            state.volumes = box.volume_list()
        elif etype == "volume_get":
            state.volume_get = box.volume_get(expect["volume"])
        elif etype == "volume_remove":
            box.volume_remove(expect["volume"])
            state.acked.add("remove")
        elif etype == "mount_add":
            box.mount_add(expect["mount"])
            state.acked.add("mount_add")
        elif etype == "secret_set":
            box.secret_set(expect["key"], expect["value"])
            state.acked.add("set")
        elif etype == "secret_list":
            state.secrets = box.secret_list()
        elif etype == "secret_remove":
            box.secret_remove(expect["key"])
            state.acked.add("remove")
        elif etype == "snapshot_create":
            state.snapshot_created = box.snapshot_create(expect.get("dest"))
        elif etype == "snapshot_list":
            state.snapshots = client.snapshots()
        elif etype == "snapshot_restore":
            box.snapshot_restore(expect["snapshot"], expect["name"])
            state.acked.add("restore")
        elif etype == "snapshot_fork":
            box.snapshot_fork(expect["snapshot"], expect["name"])
            state.acked.add("fork")
        elif etype == "snapshot_remove":
            box.snapshot_remove(expect["snapshot"])
            state.acked.add("remove")
        else:
            raise RuntimeError(f"unhandled expect.type {etype!r}")
    except NanosbError as e:
        state.errors.append((e.code, e.message))


def _exec_stream_event(state: DriverState):
    def on_event(ev: Dict[str, Any]) -> None:
        if ev.get("event") == "stdout":
            state.stdout += ev.get("data", "")
        elif ev.get("event") == "stderr":
            state.stderr += ev.get("data", "")

    return on_event


def run_scenario(bin_path: str, scenario_file: str) -> tuple:
    """Run one scenario; returns (passed, message)."""
    with open(scenario_file) as fh:
        scenario = json.load(fh)
    assert_block = scenario.get("assert") or {}
    sid = scenario["id"]

    proc = subprocess.Popen(
        [bin_path, "serve", "--scenario", scenario_file],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        line = proc.stdout.readline().strip()
        if not line.startswith("READY "):
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
            stderr = proc.stderr.read().strip()
            return False, f"harness did not print READY: {stderr or '(no stderr)'}"
        sock = line.split(" ", 1)[1]

        client = SandboxClient.connect(sock)
        state = DriverState()
        write_conn: Optional[_Conn] = None
        try:
            for step in scenario["steps"]:
                expect = step.get("expect") or {}
                etype = expect.get("type")
                if assert_block.get("expects_timeout") and step.get("timeout_ms") is not None:
                    # Do NOT send the request; wait for the server's timeout error.
                    conn = _Conn(sock)
                    conn._sock.settimeout(10.0)
                    try:
                        resp = conn.recv()
                    finally:
                        conn.close()
                    state.timeout_code = resp.get("code")
                    break
                if etype == "fs_data":
                    write_conn.send(
                        {
                            "type": "fs_data",
                            "data_base64": expect["data_base64"],
                            "eof": expect["eof"],
                        }
                    )
                    write_conn.recv()
                    state.chunks_acked += 1
                    continue
                if etype == "fs_write_stream":
                    write_conn = _Conn(sock)
                    write_conn.send(
                        {"type": "fs_write_stream", "name": expect.get("name"), "path": expect["path"]}
                    )
                    ack = write_conn.recv()
                    if ack.get("type") == "error":
                        state.errors.append((ack.get("code"), ack.get("message")))
                    else:
                        state.acked.add("write_stream")
                    continue
                drive_step(client, state, expect)
        finally:
            if write_conn is not None:
                write_conn.close()

        if assert_block.get("expects_unsupported"):
            verb = assert_block.get("unsupported_verb")
            if client.supports(verb):
                state.unsupported_observed = False
            else:
                try:
                    if verb == "exec":
                        client.sandbox("box").exec("echo", [])
                    else:
                        raise RuntimeError(f"unsupported verb {verb!r} not wired")
                    state.unsupported_observed = False
                except NanosbError as e:
                    state.unsupported_observed = e.code == "unsupported"

        failures = evaluate_assert(assert_block, state)
        if failures:
            return False, "; ".join(failures)
        return True, "ok"
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()


_ACK_KEYS = {
    "create_acked": "create",
    "start_acked": "start",
    "stop_acked": "stop",
    "kill_acked": "kill",
    "wait_acked": "wait",
    "restart_acked": "restart",
    "remove_acked": "remove",
    "drain_acked": "drain",
    "touch_acked": "touch",
    "pause_acked": "pause",
    "resume_acked": "resume",
    "modify_acked": "modify",
    "write_acked": "write",
    "mkdir_acked": "mkdir",
    "remove_dir_acked": "remove_dir",
    "copy_acked": "copy",
    "rename_acked": "rename",
    "copy_from_host_acked": "copy_from_host",
    "copy_to_host_acked": "copy_to_host",
    "mount_add_acked": "mount_add",
    "set_acked": "set",
    "restore_acked": "restore",
    "fork_acked": "fork",
    "input_acked": "input",
    "resize_acked": "resize",
    "write_stream_acked": "write_stream",
}


def evaluate_assert(assert_block: Dict[str, Any], state: DriverState) -> List[str]:
    """Interpret the assert keys the driver understands (SPEC §7)."""
    failures: List[str] = []

    def check(cond: bool, msg: str) -> None:
        if not cond:
            failures.append(msg)

    if "exit_code" in assert_block:
        check(state.exit_code == assert_block["exit_code"], f"exit_code: expected {assert_block['exit_code']}, got {state.exit_code}")
    if "stdout_equals" in assert_block:
        check(state.stdout == assert_block["stdout_equals"], f"stdout_equals: expected {assert_block['stdout_equals']!r}, got {state.stdout!r}")
    if "stderr_equals" in assert_block:
        check(state.stderr == assert_block["stderr_equals"], f"stderr_equals: expected {assert_block['stderr_equals']!r}, got {state.stderr!r}")
    for key in ("stdout_contains", "stderr_contains"):
        if key in assert_block:
            haystack = state.stdout if key == "stdout_contains" else state.stderr
            for needle in assert_block[key]:
                check(needle in haystack, f"{key}: {needle!r} not in {haystack!r}")
    for key, name in _ACK_KEYS.items():
        if key in assert_block:
            check(name in state.acked, f"{key}: expected ack for {name!r}")
    if "wait_exit_code" in assert_block:
        got = state.wait_result[0] if state.wait_result else None
        check(got == assert_block["wait_exit_code"], f"wait_exit_code: expected {assert_block['wait_exit_code']}, got {got}")
    if "wait_state" in assert_block:
        got = state.wait_result[2] if state.wait_result else None
        check(got == assert_block["wait_state"], f"wait_state: expected {assert_block['wait_state']}, got {got}")
    if "get_name" in assert_block:
        check(state.get_result and state.get_result.get("name") == assert_block["get_name"], f"get_name: expected {assert_block['get_name']}, got {state.get_result and state.get_result.get('name')}")
    if "get_state" in assert_block:
        check(state.get_result and state.get_result.get("state") == assert_block["get_state"], f"get_state: expected {assert_block['get_state']}, got {state.get_result and state.get_result.get('state')}")
    if "get_image" in assert_block:
        check(state.get_result and state.get_result.get("image") == assert_block["get_image"], f"get_image: expected {assert_block['get_image']}, got {state.get_result and state.get_result.get('image')}")
    if "list_names" in assert_block:
        names = [s.get("name") for s in (state.list_result or [])]
        check(names == assert_block["list_names"], f"list_names: expected {assert_block['list_names']}, got {names}")
    if "list_states" in assert_block:
        states = [s.get("state") for s in (state.list_result or [])]
        check(states == assert_block["list_states"], f"list_states: expected {assert_block['list_states']}, got {states}")
    if "entries_contain" in assert_block:
        names = [e.get("name") for e in (state.fs_entries or [])]
        for n in assert_block["entries_contain"]:
            check(n in names, f"entries_contain: {n!r} not in {names}")
    if "stat_is_dir" in assert_block:
        check(state.fs_stat is not None and state.fs_stat.get("is_dir") == assert_block["stat_is_dir"], f"stat_is_dir: expected {assert_block['stat_is_dir']}, got {state.fs_stat}")
    if "exists" in assert_block:
        check(state.fs_exists == assert_block["exists"], f"exists: expected {assert_block['exists']}, got {state.fs_exists}")
    if "read_bytes_base64" in assert_block:
        got = _b64(state.fs_read_bytes or b"")
        check(got == assert_block["read_bytes_base64"], f"read_bytes_base64: expected {assert_block['read_bytes_base64']}, got {got}")
    if "content_equals" in assert_block:
        check(state.fs_content == assert_block["content_equals"], f"content_equals: expected {assert_block['content_equals']!r}, got {state.fs_content!r}")
    if "chunks_acked" in assert_block:
        check(state.chunks_acked == assert_block["chunks_acked"], f"chunks_acked: expected {assert_block['chunks_acked']}, got {state.chunks_acked}")
    for key, lines in (("lines_contain", state.first_logs), ("resume_lines_contain", state.resume_logs)):
        if key in assert_block:
            data = [l.get("data", "") for l in (lines or {}).get("lines", [])]
            for needle in assert_block[key]:
                check(any(needle in d for d in data), f"{key}: {needle!r} not in {data}")
    if "follow_lines_contain" in assert_block:
        data = [l.get("data", "") for l in state.follow_lines]
        for needle in assert_block["follow_lines_contain"]:
            check(any(needle in d for d in data), f"follow_lines_contain: {needle!r} not in {data}")
    if "next_cursor" in assert_block:
        got = state.first_logs.get("next_cursor") if state.first_logs else None
        check(got == assert_block["next_cursor"], f"next_cursor: expected {assert_block['next_cursor']}, got {got}")
    if "all_source" in assert_block:
        sources = [l.get("source") for l in (state.first_logs or {}).get("lines", [])]
        check(all(s == assert_block["all_source"] for s in sources), f"all_source: expected all {assert_block['all_source']}, got {sources}")
    if "cpu_percent" in assert_block:
        check(state.metrics and state.metrics.get("cpu_percent") == assert_block["cpu_percent"], f"cpu_percent: expected {assert_block['cpu_percent']}, got {state.metrics and state.metrics.get('cpu_percent')}")
    if "memory_used_bytes" in assert_block:
        check(state.metrics and state.metrics.get("memory_used_bytes") == assert_block["memory_used_bytes"], f"memory_used_bytes: expected {assert_block['memory_used_bytes']}, got {state.metrics and state.metrics.get('memory_used_bytes')}")
    if "has_fields" in assert_block:
        for f in assert_block["has_fields"]:
            check(state.metrics is not None and f in state.metrics, f"has_fields: {f!r} missing from {state.metrics}")
    if "stream_samples" in assert_block:
        check(len(state.metrics_samples) == assert_block["stream_samples"], f"stream_samples: expected {assert_block['stream_samples']}, got {len(state.metrics_samples)}")
    if "first_cpu_percent" in assert_block:
        got = state.metrics_samples[0].get("cpu_percent") if state.metrics_samples else None
        check(got == assert_block["first_cpu_percent"], f"first_cpu_percent: expected {assert_block['first_cpu_percent']}, got {got}")
    if "volume_created" in assert_block:
        check(state.volume_created and state.volume_created.get("name") == assert_block["volume_created"], f"volume_created: expected {assert_block['volume_created']}, got {state.volume_created}")
    if "volumes_contain" in assert_block:
        names = [v.get("name") for v in (state.volumes or [])]
        for n in assert_block["volumes_contain"]:
            check(n in names, f"volumes_contain: {n!r} not in {names}")
    if "volume_get_name" in assert_block:
        check(state.volume_get and state.volume_get.get("name") == assert_block["volume_get_name"], f"volume_get_name: expected {assert_block['volume_get_name']}, got {state.volume_get}")
    if "secrets_contain" in assert_block:
        keys = [s.get("key") for s in (state.secrets or [])]
        for k in assert_block["secrets_contain"]:
            check(k in keys, f"secrets_contain: {k!r} not in {keys}")
    if "secret_values_redacted" in assert_block:
        values = [s.get("value") for s in (state.secrets or [])]
        check(all(v in (None, "") for v in values), f"secret_values_redacted: values {values} not redacted")
    if "snapshot_created" in assert_block:
        check(state.snapshot_created and state.snapshot_created.get("id") == assert_block["snapshot_created"], f"snapshot_created: expected {assert_block['snapshot_created']}, got {state.snapshot_created}")
    if "snapshots_contain" in assert_block:
        ids = [s.get("id") for s in (state.snapshots or [])]
        for s in assert_block["snapshots_contain"]:
            check(s in ids, f"snapshots_contain: {s!r} not in {ids}")
    if "attach_events" in assert_block:
        got = [e.get("event") for e in state.attach_events]
        check(got == assert_block["attach_events"], f"attach_events: expected {assert_block['attach_events']}, got {got}")
    if "attach_exit_code" in assert_block:
        check(state.attach_exit_code == assert_block["attach_exit_code"], f"attach_exit_code: expected {assert_block['attach_exit_code']}, got {state.attach_exit_code}")
    if "error_code" in assert_block:
        got = state.errors[0][0] if state.errors else None
        check(got == assert_block["error_code"], f"error_code: expected {assert_block['error_code']}, got {got}")
    if "second_error_code" in assert_block:
        got = state.errors[1][0] if len(state.errors) > 1 else None
        check(got == assert_block["second_error_code"], f"second_error_code: expected {assert_block['second_error_code']}, got {got}")
    if "error_message_contains" in assert_block:
        msg = state.errors[0][1] if state.errors else ""
        check(assert_block["error_message_contains"] in msg, f"error_message_contains: {assert_block['error_message_contains']!r} not in {msg!r}")
    if "expects_timeout" in assert_block:
        check(state.timeout_code == assert_block.get("timeout_code", "timeout"), f"timeout_code: expected {assert_block.get('timeout_code', 'timeout')}, got {state.timeout_code}")
    if "expects_unsupported" in assert_block:
        check(state.unsupported_observed, "expected an unsupported error for the unadvertised verb")
    if "ping_ok" in assert_block:
        check(state.ping_ok, "ping_ok: ping did not succeed")
    if "exec_exit_code" in assert_block:
        check(state.exit_code == assert_block["exec_exit_code"], f"exec_exit_code: expected {assert_block['exec_exit_code']}, got {state.exit_code}")
    if "exec_stdout" in assert_block:
        check(state.stdout == assert_block["exec_stdout"], f"exec_stdout: expected {assert_block['exec_stdout']!r}, got {state.stdout!r}")
    return failures


def find_binary() -> str:
    env = os.environ.get("NANOSB_CONFORMANCE_BIN")
    if env:
        if not os.path.exists(env):
            sys.exit(f"error: NANOSB_CONFORMANCE_BIN={env} does not exist")
        return env
    default = os.path.join(_REPO_ROOT, "target", "debug", "nanosb-conformance")
    if not os.path.exists(default):
        sys.exit(
            f"error: nanosb-conformance binary not found at {default}\n"
            "build it with `cargo build -p nanosb-conformance` or set NANOSB_CONFORMANCE_BIN"
        )
    return default


def main() -> int:
    bin_path = find_binary()
    scenarios = sorted(
        os.path.join(_SCENARIOS_DIR, f)
        for f in os.listdir(_SCENARIOS_DIR)
        if f.endswith(".json")
    )
    if not scenarios:
        sys.exit(f"error: no scenarios found in {_SCENARIOS_DIR}")

    signal.signal(signal.SIGALRM, _alarm_handler)
    passed = 0
    failed = 0
    for scenario_file in scenarios:
        with open(scenario_file) as fh:
            sid = json.load(fh)["id"]
        signal.alarm(_SCENARIO_BUDGET_SECS)
        try:
            ok, message = run_scenario(bin_path, scenario_file)
        except _BudgetExceeded:
            ok, message = False, "scenario exceeded time budget"
        except Exception as exc:  # noqa: BLE001 - driver must report, not crash
            ok, message = False, f"driver error: {exc}"
        finally:
            signal.alarm(0)
        if ok:
            passed += 1
            print(f"PASS {sid}")
        else:
            failed += 1
            print(f"FAIL {sid}: {message}")

    print(f"\n{passed} passed, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())