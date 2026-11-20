#!/usr/bin/env node
// conformance/run.mjs — conformance driver for the TypeScript SDK.
//
// Runs every scenario in `sdk/conformance/scenarios/` against the
// `nanosb-conformance` harness and evaluates the scenario `assert` blocks per
// `sdk/conformance/SPEC.md`. Exits non-zero if any scenario fails.
//
// Usage:
//     node sdk/typescript/conformance/run.mjs
//     NANOSB_CONFORMANCE_BIN=/path/to/nanosb-conformance node sdk/typescript/conformance/run.mjs
//
// allow: SIZE_OK — single-purpose driver mirroring the 568-line Python
// reference (sdk/python/conformance/run.py); the assert evaluator is a
// data-table of per-key checks, not reusable library code.
import { spawn } from "node:child_process";
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { SandboxClient, NanosbError, Conn } from "../nanosandbox/index.mjs";
import { makeLineReader } from "../nanosandbox/conn.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const SDK_ROOT = dirname(HERE); // sdk/typescript
const REPO_ROOT = dirname(dirname(SDK_ROOT)); // repo root
const SCENARIOS_DIR = join(REPO_ROOT, "sdk", "conformance", "scenarios");

// A generous per-scenario wall-clock budget so a wedged stream cannot hang CI.
const SCENARIO_BUDGET_MS = 60_000;
const READY_TIMEOUT_MS = 15_000;
const RECV_TIMEOUT_MS = 10_000;

function b64(data) {
  return Buffer.from(data).toString("base64");
}

function unb64(text) {
  return Buffer.from(text, "base64");
}

/** Race a promise against a timeout; rejects with a driver error on expiry. */
async function withTimeout(promise, ms) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`timed out after ${ms}ms`)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function waitExit(proc, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (proc.exitCode === null && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

async function drainStream(stream) {
  let out = "";
  try {
    while (true) {
      const chunk = await stream.read();
      if (chunk === null) break;
      out += chunk.toString();
    }
  } catch {
    // stream closed
  }
  return out;
}

/** Accumulated observations from driving a scenario's steps. */
class DriverState {
  constructor() {
    this.acked = new Set();
    this.errors = [];
    this.exitCode = null;
    this.stdout = "";
    this.stderr = "";
    this.getResult = null;
    this.listResult = null;
    this.fsEntries = null;
    this.fsStat = null;
    this.fsExists = null;
    this.fsReadBytes = null;
    this.fsContent = null;
    this.chunksAcked = 0;
    this.firstLogs = null;
    this.resumeLogs = null;
    this.followLines = [];
    this.metrics = null;
    this.metricsSamples = [];
    this.volumeCreated = null;
    this.volumes = null;
    this.volumeGet = null;
    this.secrets = null;
    this.snapshotCreated = null;
    this.snapshots = null;
    this.attachEvents = [];
    this.attachExitCode = null;
    this.pingOk = false;
    this.waitResult = null;
    this.timeoutCode = null;
    this.unsupportedObserved = false;
  }
}

/** The request fields for a verb, minus the `type`/`name` routing keys. */
function reqFromExpect(expect) {
  const out = {};
  for (const [key, value] of Object.entries(expect)) {
    if (key !== "type" && key !== "name") out[key] = value;
  }
  return out;
}

function execStreamEvent(state) {
  return (ev) => {
    if (ev.event === "stdout") state.stdout += ev.data ?? "";
    else if (ev.event === "stderr") state.stderr += ev.data ?? "";
  };
}

/** Invoke the SDK operation mapped from `expect.type` (SPEC §5). */
async function driveStep(client, state, expect) {
  const etype = expect.type;
  const name = expect.name;
  // snapshot_remove is not sandbox-scoped (no `name`); any handle works.
  const box = name !== undefined ? client.sandbox(name) : client.sandbox("");
  try {
    switch (etype) {
      case "create":
        await client.create(expect.spec);
        state.acked.add("create");
        break;
      case "get":
        state.getResult = await client.get(name);
        break;
      case "list":
        state.listResult = await client.list(expect.all ?? false);
        break;
      case "start":
        await box.start();
        state.acked.add("start");
        break;
      case "stop":
        await box.stop(expect.force ?? false);
        state.acked.add("stop");
        break;
      case "kill":
        if (expect.signal !== undefined) await box.killWith(expect.signal);
        else await box.kill();
        state.acked.add("kill");
        break;
      case "wait":
        state.waitResult = await box.wait(expect.timeout_secs);
        state.acked.add("wait");
        break;
      case "restart":
        await box.restart();
        state.acked.add("restart");
        break;
      case "remove":
        await box.remove(expect.force ?? false);
        state.acked.add("remove");
        break;
      case "drain":
        await box.drain();
        state.acked.add("drain");
        break;
      case "touch":
        await box.touch();
        state.acked.add("touch");
        break;
      case "pause":
        await box.pause();
        state.acked.add("pause");
        break;
      case "resume":
        await box.resume();
        state.acked.add("resume");
        break;
      case "modify":
        await box.modify(expect.spec);
        state.acked.add("modify");
        break;
      case "status":
        await box.state();
        break;
      case "ping":
        await client.ping();
        state.pingOk = true;
        break;
      case "exec": {
        const result = await box.execWith(reqFromExpect(expect));
        state.exitCode = result.exit_code;
        state.stdout = result.stdout ?? "";
        state.stderr = result.stderr ?? "";
        break;
      }
      case "exec_stream":
        state.exitCode = await box.execStream(reqFromExpect(expect), execStreamEvent(state));
        break;
      case "shell": {
        const result = await box.shell(expect.script);
        state.exitCode = result.exit_code;
        state.stdout = result.stdout ?? "";
        state.stderr = result.stderr ?? "";
        break;
      }
      case "attach":
        await box.attach((ev) => state.attachEvents.push(ev));
        state.attachExitCode = state.attachEvents.find((e) => e.event === "exit")?.code;
        break;
      case "attach_input":
        await client._call({ type: "attach_input", data: expect.data });
        state.acked.add("input");
        break;
      case "attach_resize":
        await client._call({ type: "attach_resize", cols: expect.cols, rows: expect.rows });
        state.acked.add("resize");
        break;
      case "fs_read":
        state.fsReadBytes = await box.fsRead(expect.path);
        break;
      case "fs_read_to_string":
        state.fsContent = await box.fsReadToString(expect.path);
        break;
      case "fs_write":
        await box.fsWrite(expect.path, unb64(expect.data_base64));
        state.acked.add("write");
        break;
      case "fs_list":
        state.fsEntries = await box.fsList(expect.path);
        break;
      case "fs_stat":
        state.fsStat = await box.fsStat(expect.path);
        break;
      case "fs_exists":
        state.fsExists = await box.fsExists(expect.path);
        break;
      case "fs_mkdir":
        await box.fsMkdir(expect.path, expect.recursive ?? false);
        state.acked.add("mkdir");
        break;
      case "fs_remove":
        await box.fsRemove(expect.path);
        break;
      case "fs_remove_dir":
        await box.fsRemoveDir(expect.path, expect.recursive ?? false);
        state.acked.add("remove_dir");
        break;
      case "fs_copy":
        await box.fsCopy(expect.from, expect.to);
        state.acked.add("copy");
        break;
      case "fs_rename":
        await box.fsRename(expect.from, expect.to);
        state.acked.add("rename");
        break;
      case "fs_copy_from_host":
        await box.fsCopyFromHost(expect.host_path, expect.guest_path);
        state.acked.add("copy_from_host");
        break;
      case "fs_copy_to_host":
        await box.fsCopyToHost(expect.guest_path, expect.host_path);
        state.acked.add("copy_to_host");
        break;
      case "logs":
        if (expect.follow) {
          await box.logsFollowWith(
            (line) => state.followLines.push(line),
            expect.tail,
            expect.cursor,
            expect.source,
          );
        } else {
          const req = { type: "logs", name, follow: false };
          for (const key of ["tail", "cursor", "source"]) {
            if (expect[key] !== undefined) req[key] = expect[key];
          }
          const resp = await client._call(req);
          if (state.firstLogs === null) state.firstLogs = resp;
          else state.resumeLogs = resp;
        }
        break;
      case "metrics":
        state.metrics = await box.metrics();
        break;
      case "metrics_stream":
        await box.metricsStream(expect.interval_ms, (sample) => state.metricsSamples.push(sample));
        break;
      case "volume_create": {
        const vol = expect.volume;
        state.volumeCreated = await box.volumeCreate(vol.name, vol.dir);
        break;
      }
      case "volume_list":
        state.volumes = await box.volumeList();
        break;
      case "volume_get":
        state.volumeGet = await box.volumeGet(expect.volume);
        break;
      case "volume_remove":
        await box.volumeRemove(expect.volume);
        state.acked.add("remove");
        break;
      case "mount_add":
        await box.mountAdd(expect.mount);
        state.acked.add("mount_add");
        break;
      case "secret_set":
        await box.secretSet(expect.key, expect.value);
        state.acked.add("set");
        break;
      case "secret_list":
        state.secrets = await box.secretList();
        break;
      case "secret_remove":
        await box.secretRemove(expect.key);
        state.acked.add("remove");
        break;
      case "snapshot_create":
        state.snapshotCreated = await box.snapshotCreate(expect.dest);
        break;
      case "snapshot_list":
        state.snapshots = await client.snapshots();
        break;
      case "snapshot_restore":
        await box.snapshotRestore(expect.snapshot, expect.name);
        state.acked.add("restore");
        break;
      case "snapshot_fork":
        await box.snapshotFork(expect.snapshot, expect.name);
        state.acked.add("fork");
        break;
      case "snapshot_remove":
        await box.snapshotRemove(expect.snapshot);
        state.acked.add("remove");
        break;
      default:
        throw new Error(`unhandled expect.type ${etype}`);
    }
  } catch (e) {
    if (e instanceof NanosbError) state.errors.push([e.code, e.message]);
    else throw e;
  }
}

/** Run one scenario; returns { ok, message }. */
async function runScenario(binPath, scenarioFile, onProc) {
  const scenario = JSON.parse(readFileSync(scenarioFile, "utf8"));
  const assertBlock = scenario.assert ?? {};
  const sid = scenario.id;

  const proc = spawn(binPath, ["serve", "--scenario", scenarioFile], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  onProc?.(proc);
  try {
    const readyLine = await withTimeout(makeLineReader(proc.stdout)(), READY_TIMEOUT_MS);
    if (!readyLine || !readyLine.startsWith("READY ")) {
      proc.kill();
      await waitExit(proc);
      const stderr = await drainStream(proc.stderr);
      return { ok: false, message: `harness did not print READY: ${stderr || "(no stderr)"}` };
    }
    const sock = readyLine.slice("READY ".length);

    const client = await SandboxClient.connect(sock);
    const state = new DriverState();
    let writeConn = null;
    try {
      for (const step of scenario.steps) {
        const expect = step.expect ?? {};
        const etype = expect.type;
        if (assertBlock.expects_timeout && step.timeout_ms !== undefined) {
          // Do NOT send the request; wait for the server's timeout error.
          const conn = new Conn(sock);
          try {
            await conn.open();
            const resp = await withTimeout(conn.recv(), RECV_TIMEOUT_MS);
            state.timeoutCode = resp.code;
          } finally {
            conn.close();
          }
          break;
        }
        if (etype === "fs_data") {
          writeConn.send({ type: "fs_data", data_base64: expect.data_base64, eof: expect.eof });
          await writeConn.recv();
          state.chunksAcked += 1;
          continue;
        }
        if (etype === "fs_write_stream") {
          writeConn = new Conn(sock);
          await writeConn.open();
          writeConn.send({ type: "fs_write_stream", name: expect.name, path: expect.path });
          const ack = await writeConn.recv();
          if (ack.type === "error") state.errors.push([ack.code, ack.message]);
          else state.acked.add("write_stream");
          continue;
        }
        await driveStep(client, state, expect);
      }
    } finally {
      if (writeConn) writeConn.close();
    }

    if (assertBlock.expects_unsupported) {
      const verb = assertBlock.unsupported_verb;
      if (client.supports(verb)) {
        state.unsupportedObserved = false;
      } else {
        try {
          if (verb === "exec") await client.sandbox("box").exec("echo", []);
          else throw new Error(`unsupported verb ${verb} not wired`);
          state.unsupportedObserved = false;
        } catch (e) {
          state.unsupportedObserved = e instanceof NanosbError && e.code === "unsupported";
        }
      }
    }

    const failures = evaluateAssert(assertBlock, state);
    if (failures.length) return { ok: false, message: failures.join("; ") };
    return { ok: true, message: "ok" };
  } finally {
    proc.kill();
  }
}

const ACK_KEYS = {
  create_acked: "create",
  start_acked: "start",
  stop_acked: "stop",
  kill_acked: "kill",
  wait_acked: "wait",
  restart_acked: "restart",
  remove_acked: "remove",
  drain_acked: "drain",
  touch_acked: "touch",
  pause_acked: "pause",
  resume_acked: "resume",
  modify_acked: "modify",
  write_acked: "write",
  mkdir_acked: "mkdir",
  remove_dir_acked: "remove_dir",
  copy_acked: "copy",
  rename_acked: "rename",
  copy_from_host_acked: "copy_from_host",
  copy_to_host_acked: "copy_to_host",
  mount_add_acked: "mount_add",
  set_acked: "set",
  restore_acked: "restore",
  fork_acked: "fork",
  input_acked: "input",
  resize_acked: "resize",
  write_stream_acked: "write_stream",
};

/** Interpret the assert keys the driver understands (SPEC §7). */
function evaluateAssert(assertBlock, state) {
  const failures = [];
  const check = (cond, msg) => {
    if (!cond) failures.push(msg);
  };

  if ("exit_code" in assertBlock) {
    check(
      state.exitCode === assertBlock.exit_code,
      `exit_code: expected ${assertBlock.exit_code}, got ${state.exitCode}`,
    );
  }
  if ("stdout_equals" in assertBlock) {
    check(
      state.stdout === assertBlock.stdout_equals,
      `stdout_equals: expected ${JSON.stringify(assertBlock.stdout_equals)}, got ${JSON.stringify(state.stdout)}`,
    );
  }
  if ("stderr_equals" in assertBlock) {
    check(
      state.stderr === assertBlock.stderr_equals,
      `stderr_equals: expected ${JSON.stringify(assertBlock.stderr_equals)}, got ${JSON.stringify(state.stderr)}`,
    );
  }
  for (const key of ["stdout_contains", "stderr_contains"]) {
    if (key in assertBlock) {
      const haystack = key === "stdout_contains" ? state.stdout : state.stderr;
      for (const needle of assertBlock[key]) {
        check(haystack.includes(needle), `${key}: ${JSON.stringify(needle)} not in ${JSON.stringify(haystack)}`);
      }
    }
  }
  for (const [key, name] of Object.entries(ACK_KEYS)) {
    if (key in assertBlock) {
      check(state.acked.has(name), `${key}: expected ack for ${name}`);
    }
  }
  if ("wait_exit_code" in assertBlock) {
    const got = state.waitResult ? state.waitResult.exitCode : null;
    check(
      got === assertBlock.wait_exit_code,
      `wait_exit_code: expected ${assertBlock.wait_exit_code}, got ${got}`,
    );
  }
  if ("wait_state" in assertBlock) {
    const got = state.waitResult ? state.waitResult.state : null;
    check(
      got === assertBlock.wait_state,
      `wait_state: expected ${assertBlock.wait_state}, got ${got}`,
    );
  }
  if ("get_name" in assertBlock) {
    check(
      state.getResult && state.getResult.name === assertBlock.get_name,
      `get_name: expected ${assertBlock.get_name}, got ${state.getResult && state.getResult.name}`,
    );
  }
  if ("get_state" in assertBlock) {
    check(
      state.getResult && state.getResult.state === assertBlock.get_state,
      `get_state: expected ${assertBlock.get_state}, got ${state.getResult && state.getResult.state}`,
    );
  }
  if ("get_image" in assertBlock) {
    check(
      state.getResult && state.getResult.image === assertBlock.get_image,
      `get_image: expected ${assertBlock.get_image}, got ${state.getResult && state.getResult.image}`,
    );
  }
  if ("list_names" in assertBlock) {
    const names = (state.listResult ?? []).map((s) => s.name);
    check(
      JSON.stringify(names) === JSON.stringify(assertBlock.list_names),
      `list_names: expected ${JSON.stringify(assertBlock.list_names)}, got ${JSON.stringify(names)}`,
    );
  }
  if ("list_states" in assertBlock) {
    const states = (state.listResult ?? []).map((s) => s.state);
    check(
      JSON.stringify(states) === JSON.stringify(assertBlock.list_states),
      `list_states: expected ${JSON.stringify(assertBlock.list_states)}, got ${JSON.stringify(states)}`,
    );
  }
  if ("entries_contain" in assertBlock) {
    const names = (state.fsEntries ?? []).map((e) => e.name);
    for (const n of assertBlock.entries_contain) {
      check(names.includes(n), `entries_contain: ${JSON.stringify(n)} not in ${JSON.stringify(names)}`);
    }
  }
  if ("stat_is_dir" in assertBlock) {
    check(
      state.fsStat !== null && state.fsStat.is_dir === assertBlock.stat_is_dir,
      `stat_is_dir: expected ${assertBlock.stat_is_dir}, got ${JSON.stringify(state.fsStat)}`,
    );
  }
  if ("exists" in assertBlock) {
    check(
      state.fsExists === assertBlock.exists,
      `exists: expected ${assertBlock.exists}, got ${state.fsExists}`,
    );
  }
  if ("read_bytes_base64" in assertBlock) {
    const got = b64(state.fsReadBytes ?? Buffer.alloc(0));
    check(
      got === assertBlock.read_bytes_base64,
      `read_bytes_base64: expected ${assertBlock.read_bytes_base64}, got ${got}`,
    );
  }
  if ("content_equals" in assertBlock) {
    check(
      state.fsContent === assertBlock.content_equals,
      `content_equals: expected ${JSON.stringify(assertBlock.content_equals)}, got ${JSON.stringify(state.fsContent)}`,
    );
  }
  if ("chunks_acked" in assertBlock) {
    check(
      state.chunksAcked === assertBlock.chunks_acked,
      `chunks_acked: expected ${assertBlock.chunks_acked}, got ${state.chunksAcked}`,
    );
  }
  for (const [key, lines] of [
    ["lines_contain", state.firstLogs],
    ["resume_lines_contain", state.resumeLogs],
  ]) {
    if (key in assertBlock) {
      const data = (lines?.lines ?? []).map((l) => l.data ?? "");
      for (const needle of assertBlock[key]) {
        check(data.some((d) => d.includes(needle)), `${key}: ${JSON.stringify(needle)} not in ${JSON.stringify(data)}`);
      }
    }
  }
  if ("follow_lines_contain" in assertBlock) {
    const data = state.followLines.map((l) => l.data ?? "");
    for (const needle of assertBlock.follow_lines_contain) {
      check(data.some((d) => d.includes(needle)), `follow_lines_contain: ${JSON.stringify(needle)} not in ${JSON.stringify(data)}`);
    }
  }
  if ("next_cursor" in assertBlock) {
    const got = state.firstLogs ? state.firstLogs.next_cursor : null;
    check(got === assertBlock.next_cursor, `next_cursor: expected ${assertBlock.next_cursor}, got ${got}`);
  }
  if ("all_source" in assertBlock) {
    const sources = (state.firstLogs?.lines ?? []).map((l) => l.source);
    check(
      sources.every((s) => s === assertBlock.all_source),
      `all_source: expected all ${assertBlock.all_source}, got ${JSON.stringify(sources)}`,
    );
  }
  if ("cpu_percent" in assertBlock) {
    check(
      state.metrics && state.metrics.cpu_percent === assertBlock.cpu_percent,
      `cpu_percent: expected ${assertBlock.cpu_percent}, got ${state.metrics && state.metrics.cpu_percent}`,
    );
  }
  if ("memory_used_bytes" in assertBlock) {
    check(
      state.metrics && state.metrics.memory_used_bytes === assertBlock.memory_used_bytes,
      `memory_used_bytes: expected ${assertBlock.memory_used_bytes}, got ${state.metrics && state.metrics.memory_used_bytes}`,
    );
  }
  if ("has_fields" in assertBlock) {
    for (const f of assertBlock.has_fields) {
      check(state.metrics !== null && f in state.metrics, `has_fields: ${JSON.stringify(f)} missing from ${JSON.stringify(state.metrics)}`);
    }
  }
  if ("stream_samples" in assertBlock) {
    check(
      state.metricsSamples.length === assertBlock.stream_samples,
      `stream_samples: expected ${assertBlock.stream_samples}, got ${state.metricsSamples.length}`,
    );
  }
  if ("first_cpu_percent" in assertBlock) {
    const got = state.metricsSamples.length ? state.metricsSamples[0].cpu_percent : null;
    check(got === assertBlock.first_cpu_percent, `first_cpu_percent: expected ${assertBlock.first_cpu_percent}, got ${got}`);
  }
  if ("volume_created" in assertBlock) {
    check(
      state.volumeCreated && state.volumeCreated.name === assertBlock.volume_created,
      `volume_created: expected ${assertBlock.volume_created}, got ${JSON.stringify(state.volumeCreated)}`,
    );
  }
  if ("volumes_contain" in assertBlock) {
    const names = (state.volumes ?? []).map((v) => v.name);
    for (const n of assertBlock.volumes_contain) {
      check(names.includes(n), `volumes_contain: ${JSON.stringify(n)} not in ${JSON.stringify(names)}`);
    }
  }
  if ("volume_get_name" in assertBlock) {
    check(
      state.volumeGet && state.volumeGet.name === assertBlock.volume_get_name,
      `volume_get_name: expected ${assertBlock.volume_get_name}, got ${JSON.stringify(state.volumeGet)}`,
    );
  }
  if ("secrets_contain" in assertBlock) {
    const keys = (state.secrets ?? []).map((s) => s.key);
    for (const k of assertBlock.secrets_contain) {
      check(keys.includes(k), `secrets_contain: ${JSON.stringify(k)} not in ${JSON.stringify(keys)}`);
    }
  }
  if ("secret_values_redacted" in assertBlock) {
    const values = (state.secrets ?? []).map((s) => s.value);
    check(
      values.every((v) => v === null || v === ""),
      `secret_values_redacted: values ${JSON.stringify(values)} not redacted`,
    );
  }
  if ("snapshot_created" in assertBlock) {
    check(
      state.snapshotCreated && state.snapshotCreated.id === assertBlock.snapshot_created,
      `snapshot_created: expected ${assertBlock.snapshot_created}, got ${JSON.stringify(state.snapshotCreated)}`,
    );
  }
  if ("snapshots_contain" in assertBlock) {
    const ids = (state.snapshots ?? []).map((s) => s.id);
    for (const s of assertBlock.snapshots_contain) {
      check(ids.includes(s), `snapshots_contain: ${JSON.stringify(s)} not in ${JSON.stringify(ids)}`);
    }
  }
  if ("attach_events" in assertBlock) {
    const got = state.attachEvents.map((e) => e.event);
    check(
      JSON.stringify(got) === JSON.stringify(assertBlock.attach_events),
      `attach_events: expected ${JSON.stringify(assertBlock.attach_events)}, got ${JSON.stringify(got)}`,
    );
  }
  if ("attach_exit_code" in assertBlock) {
    check(
      state.attachExitCode === assertBlock.attach_exit_code,
      `attach_exit_code: expected ${assertBlock.attach_exit_code}, got ${state.attachExitCode}`,
    );
  }
  if ("error_code" in assertBlock) {
    const got = state.errors.length ? state.errors[0][0] : null;
    check(got === assertBlock.error_code, `error_code: expected ${assertBlock.error_code}, got ${got}`);
  }
  if ("second_error_code" in assertBlock) {
    const got = state.errors.length > 1 ? state.errors[1][0] : null;
    check(got === assertBlock.second_error_code, `second_error_code: expected ${assertBlock.second_error_code}, got ${got}`);
  }
  if ("error_message_contains" in assertBlock) {
    const msg = state.errors.length ? state.errors[0][1] : "";
    check(
      msg.includes(assertBlock.error_message_contains),
      `error_message_contains: ${JSON.stringify(assertBlock.error_message_contains)} not in ${JSON.stringify(msg)}`,
    );
  }
  if ("expects_timeout" in assertBlock) {
    check(
      state.timeoutCode === (assertBlock.timeout_code ?? "timeout"),
      `timeout_code: expected ${assertBlock.timeout_code ?? "timeout"}, got ${state.timeoutCode}`,
    );
  }
  if ("expects_unsupported" in assertBlock) {
    check(state.unsupportedObserved, "expected an unsupported error for the unadvertised verb");
  }
  if ("ping_ok" in assertBlock) {
    check(state.pingOk, "ping_ok: ping did not succeed");
  }
  if ("exec_exit_code" in assertBlock) {
    check(
      state.exitCode === assertBlock.exec_exit_code,
      `exec_exit_code: expected ${assertBlock.exec_exit_code}, got ${state.exitCode}`,
    );
  }
  if ("exec_stdout" in assertBlock) {
    check(
      state.stdout === assertBlock.exec_stdout,
      `exec_stdout: expected ${JSON.stringify(assertBlock.exec_stdout)}, got ${JSON.stringify(state.stdout)}`,
    );
  }
  return failures;
}

function findBinary() {
  const env = process.env.NANOSB_CONFORMANCE_BIN;
  if (env) {
    if (!existsSync(env)) {
      console.error(`error: NANOSB_CONFORMANCE_BIN=${env} does not exist`);
      process.exit(1);
    }
    return env;
  }
  const def = join(REPO_ROOT, "target", "debug", "nanosb-conformance");
  if (!existsSync(def)) {
    console.error(
      `error: nanosb-conformance binary not found at ${def}\n` +
        "build it with `cargo build -p nanosb-conformance` or set NANOSB_CONFORMANCE_BIN",
    );
    process.exit(1);
  }
  return def;
}

async function main() {
  const binPath = findBinary();
  const scenarios = readdirSync(SCENARIOS_DIR)
    .filter((f) => f.endsWith(".json"))
    .map((f) => join(SCENARIOS_DIR, f))
    .sort();
  if (scenarios.length === 0) {
    console.error(`error: no scenarios found in ${SCENARIOS_DIR}`);
    process.exit(1);
  }

  let passed = 0;
  let failed = 0;
  for (const scenarioFile of scenarios) {
    const sid = JSON.parse(readFileSync(scenarioFile, "utf8")).id;
    let proc = null;
    let result;
    try {
      result = await Promise.race([
        runScenario(binPath, scenarioFile, (p) => {
          proc = p;
        }),
        new Promise((resolve) =>
          setTimeout(() => resolve({ ok: false, message: "scenario exceeded time budget" }), SCENARIO_BUDGET_MS),
        ),
      ]);
    } catch (e) {
      result = { ok: false, message: `driver error: ${e}` };
    }
    if (proc && proc.exitCode === null) proc.kill();
    if (result.ok) {
      passed += 1;
      console.log(`PASS ${sid}`);
    } else {
      failed += 1;
      console.log(`FAIL ${sid}: ${result.message}`);
    }
  }

  console.log(`\n${passed} passed, ${failed} failed`);
  return failed ? 1 : 0;
}

process.exit(await main());