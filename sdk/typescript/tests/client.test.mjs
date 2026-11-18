// client.test.mjs — unit tests for the TypeScript SDK using an in-process
// fake NDJSON helper on a temp Unix socket (mirrors sdk/python/tests/test_client.py).
import { test, after } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const _tmpDirs = [];
after(() => {
  for (const d of _tmpDirs) rmSync(d, { recursive: true, force: true });
});
import { createServer } from "node:net";
import {
  SandboxClient,
  NanosbError,
  SandboxBuilder,
  VolumeBuilder,
  PROTOCOL_VERSION,
} from "../nanosandbox/index.mjs";
import { makeLineReader } from "../nanosandbox/conn.mjs";

/** A scripted NDJSON helper on a temp Unix socket. */
class FakeHelper {
  constructor() {
    this._dir = mkdtempSync(join(tmpdir(), "nanosb-ts-"));
    _tmpDirs.push(this._dir);
    this.path = join(this._dir, "helper.sock");
    this._queue = [];
    this._requests = [];
    this._server = createServer({ type: "unix", path: this.path }, (conn) => this._handle(conn));
    this._ready = new Promise((resolve, reject) => {
      this._server.once("listening", resolve);
      this._server.once("error", reject);
      this._server.listen({ type: "unix", path: this.path });
    });
  }

  async start() {
    await this._ready;
  }

  push(obj) {
    this._queue.push(JSON.stringify(obj));
  }

  _pop() {
    return this._queue.length ? this._queue.shift() : null;
  }

  requests() {
    return this._requests;
  }

  static _isStreaming(req) {
    const type = req.type;
    if (type === "exec_stream" || type === "attach" || type === "metrics_stream") return true;
    if (type === "logs" && req.follow) return true;
    return false;
  }

  static _isTerminal(line) {
    let obj;
    try {
      obj = JSON.parse(line);
    } catch {
      return false;
    }
    if (obj.type === "guest") return obj.event === "exit" || obj.event === "error";
    if (obj.type === "event") return obj.event === "done";
    return false;
  }

  async _handle(conn) {
    const readLine = makeLineReader(conn);
    try {
      while (true) {
        const line = await readLine();
        if (line === null) break;
        const req = JSON.parse(line);
        this._requests.push(req);
        if (req.type === "hello") {
          conn.write(
            JSON.stringify({
              type: "hello_ok",
              protocol: PROTOCOL_VERSION,
              helper_version: "test",
              capabilities: ["lifecycle", "exec", "fs", "logs", "metrics"],
            }) + "\n",
          );
          continue;
        }
        const streaming = FakeHelper._isStreaming(req);
        let scripted = this._pop();
        if (scripted === null) {
          scripted = JSON.stringify({ type: "error", code: "internal", message: "no script" });
        }
        conn.write(scripted + "\n");
        if (streaming && !FakeHelper._isTerminal(scripted)) {
          // Flush the rest of the scripted stream without waiting for another
          // request line (the client only sends one for streams).
          while (true) {
            const next = this._pop();
            if (next === null) break;
            conn.write(next + "\n");
            if (FakeHelper._isTerminal(next)) break;
          }
        }
      }
    } catch {
      // connection closed
    }
    conn.destroySoon();
  }

  close() {
    this._server.close();
  }
}

async function withHelper(fn) {
  const helper = new FakeHelper();
  await helper.start();
  try {
    await fn(helper);
  } finally {
    helper.close();
  }
}

test("handshake and capabilities", async () => {
  await withHelper(async (helper) => {
    const client = await SandboxClient.connect(helper.path);
    assert.ok(client.capabilities().includes("lifecycle"));
    assert.ok(client.supports("exec"));
  });
});

test("ping", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "pong", version: "9" });
    const client = await SandboxClient.connect(helper.path);
    assert.equal(await client.ping(), "9");
  });
});

test("list", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "list", sandboxes: [{ name: "a", state: "running", image: "alpine" }] });
    const client = await SandboxClient.connect(helper.path);
    assert.equal((await client.list())[0].name, "a");
  });
});

test("exec", async () => {
  await withHelper(async (helper) => {
    helper.push({
      type: "exec_result",
      exit_code: 0,
      stdout: "hi\n",
      stderr: "",
      duration_ms: 1,
    });
    const client = await SandboxClient.connect(helper.path);
    const out = await client.sandbox("a").exec("echo", ["hi"]);
    assert.equal(out.stdout, "hi\n");
  });
});

test("error is typed", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "error", code: "not_found", message: "nope" });
    const client = await SandboxClient.connect(helper.path);
    try {
      await client.get("missing");
      assert.fail("expected NanosbError");
    } catch (e) {
      assert.ok(e instanceof NanosbError);
      assert.ok(e.isNotFound());
    }
  });
});

test("fs write then read", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" });
    helper.push({ type: "fs_data", data_base64: "aGVsbG8=", eof: true });
    const client = await SandboxClient.connect(helper.path);
    const box = client.sandbox("a");
    await box.fsWrite("/tmp/x", Buffer.from("hello"));
    assert.equal((await box.fsRead("/tmp/x")).toString(), "hello");
  });
});

test("exec stream", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" });
    helper.push({ type: "guest", event: "stdout", data: "a" });
    helper.push({ type: "guest", event: "exit", code: 0 });
    const client = await SandboxClient.connect(helper.path);
    const events = [];
    const code = await client.sandbox("a").execStream({ command: "x" }, (ev) => events.push(ev));
    assert.equal(code, 0);
    assert.equal(events[0].data, "a");
  });
});

test("builder", () => {
  const spec = new SandboxBuilder("alpine")
    .withName("demo")
    .withResources(2, 512)
    .publishTcp(8080, 80)
    .withLabel("team", "sdk")
    .build();
  assert.equal(spec.name, "demo");
  assert.equal(spec.network.publish[0].protocol, "tcp");
  assert.equal(spec.labels.team, "sdk");
});

test("builder mount and secret methods", () => {
  const spec = new SandboxBuilder("alpine")
    .detached(false)
    .hostname("box")
    .mountFile("/host/f", "/guest/f")
    .mountTmpfs("/tmp")
    .mountOptions(["noexec", "nosuid"])
    .mountOwner(1000, 1000)
    .secretEnv("API_KEY", "api-key")
    .build();
  assert.equal(spec.detached, false);
  assert.equal(spec.hostname, "box");
  assert.equal(spec.mounts[0].file, true);
  assert.equal(spec.mounts[1].tmpfs, true);
  assert.equal(spec.mounts[1].mount_type, "tmpfs");
  assert.deepEqual(spec.mounts[1].options, ["noexec", "nosuid"]);
  assert.equal(spec.mounts[1].uid, 1000);
  assert.equal(spec.mounts[1].gid, 1000);
  assert.equal(spec.secret_env.API_KEY, "api-key");
});

test("volume builder", () => {
  const vol = new VolumeBuilder("data").withDir("/tmp/data").volumeQuota(1024).build();
  assert.equal(vol.name, "data");
  assert.equal(vol.dir, "/tmp/data");
  assert.equal(vol.quota_bytes, 1024);
});

test("modify", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" });
    const client = await SandboxClient.connect(helper.path);
    await client.sandbox("a").modify({ idle_timeout_secs: 300, labels: { team: "sdk" } });
  });
});

test("wait until stopped", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "wait", exit_code: 0, exited_at_ms: 1234, state: "stopped" });
    const client = await SandboxClient.connect(helper.path);
    const state = await client.sandbox("a").waitUntilStopped(5);
    assert.equal(state, "stopped");
  });
});

test("wait returns exit triple", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "wait", exit_code: 137, exited_at_ms: 1234, state: "stopped" });
    const client = await SandboxClient.connect(helper.path);
    const res = await client.sandbox("a").wait(30);
    assert.equal(res.exitCode, 137);
    assert.equal(res.exitedAtMs, 1234);
    assert.equal(res.state, "stopped");
  });
});

test("wait omits timeout_secs when null", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "wait", exit_code: 0, exited_at_ms: 0, state: "stopped" });
    const client = await SandboxClient.connect(helper.path);
    await client.sandbox("a").wait();
    const req = helper.requests().find((r) => r.type === "wait");
    assert.equal(req.name, "a");
    assert.equal(req.timeout_secs, undefined);
  });
});

test("killWith sends signal 15", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" });
    const client = await SandboxClient.connect(helper.path);
    await client.sandbox("a").killWith(15);
    const req = helper.requests().find((r) => r.type === "kill");
    assert.equal(req.signal, 15);
  });
});

test("restart sends restart frame", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" });
    const client = await SandboxClient.connect(helper.path);
    await client.sandbox("a").restart();
    assert.ok(helper.requests().some((r) => r.type === "restart"));
  });
});

test("wait error frame rejects", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "error", code: "internal", message: "boom" });
    const client = await SandboxClient.connect(helper.path);
    try {
      await client.sandbox("a").wait(5);
      assert.fail("expected NanosbError");
    } catch (e) {
      assert.ok(e instanceof NanosbError);
      assert.equal(e.message, "boom");
    }
  });
});

test("shell stream", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" });
    helper.push({ type: "guest", event: "stdout", data: "hi" });
    helper.push({ type: "guest", event: "exit", code: 0 });
    const client = await SandboxClient.connect(helper.path);
    const out = [];
    const code = await client.sandbox("a").shellStream("echo hi", (ev) => out.push(ev));
    assert.equal(code, 0);
    assert.equal(out[0].data, "hi");
  });
});

test("attach session drives input resize and reads events", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" }); // attach ack
    helper.push({ type: "ok", version: "1" }); // send_input ack
    helper.push({ type: "ok", version: "1" }); // resize ack
    helper.push({ type: "guest", event: "stdout", data: "hi" });
    helper.push({ type: "guest", event: "exit", code: 0 });
    const client = await SandboxClient.connect(helper.path);
    const session = await client.sandbox("a").attachSession();
    try {
      await session.sendInput(Buffer.from("x"));
      await session.resize(80, 24);
      const out = [];
      while (true) {
        const ev = await session.readEvents();
        if (ev === null) break;
        out.push(ev.data);
      }
      assert.deepEqual(out, ["hi"]);
    } finally {
      session.close();
    }
  });
});

test("metrics stream", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" });
    helper.push({
      type: "event",
      event: "metrics",
      cpu_percent: 1.5,
      memory_used_bytes: 10,
      memory_total_bytes: 100,
      disk_used_bytes: 0,
      disk_total_bytes: 0,
      net_rx_bytes: 0,
      net_tx_bytes: 0,
      timestamp_ms: 0,
    });
    helper.push({ type: "event", event: "done" });
    const client = await SandboxClient.connect(helper.path);
    const samples = [];
    await client.sandbox("a").metricsStream(100, (ev) => samples.push(ev));
    assert.equal(samples.length, 1);
    assert.equal(samples[0].cpu_percent, 1.5);
  });
});

test("logs with cursor and source", async () => {
  await withHelper(async (helper) => {
    helper.push({
      type: "logs",
      lines: [{ cursor: 5, timestamp_ms: 0, source: "stdout", data: "x" }],
      next_cursor: 6,
    });
    const client = await SandboxClient.connect(helper.path);
    const lines = await client.sandbox("a").logsWith(10, 5, "stdout");
    assert.equal(lines.length, 1);
    assert.equal(lines[0].cursor, 5);
  });
});

test("logs follow with cursor and source", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" });
    helper.push({
      type: "event",
      event: "log",
      cursor: 5,
      timestamp_ms: 0,
      source: "stdout",
      data: "x",
    });
    helper.push({ type: "event", event: "done" });
    const client = await SandboxClient.connect(helper.path);
    const out = [];
    await client.sandbox("a").logsFollowWith((ev) => out.push(ev), 10, 5, "stdout");
    assert.deepEqual(out.map((line) => line.data), ["x"]);
  });
});

test("fs read range", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "fs_data", data_base64: "aGVsbG8=", eof: true });
    const client = await SandboxClient.connect(helper.path);
    assert.equal((await client.sandbox("a").fsReadRange("/tmp/x", 0, 5)).toString(), "hello");
  });
});

test("fs read stream chunks", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "fs_data", data_base64: "aGVsbG8=", eof: true });
    helper.push({ type: "fs_data", data_base64: "", eof: true });
    const client = await SandboxClient.connect(helper.path);
    const out = [];
    await client.sandbox("a").fsReadStream("/tmp/x", 3, (chunk) => out.push(chunk));
    assert.equal(Buffer.concat(out).toString(), "hello");
  });
});

test("fs write stream", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" }); // opening ack
    helper.push({ type: "ok", version: "1" }); // chunk 1 ack
    helper.push({ type: "ok", version: "1" }); // chunk 2 ack
    const client = await SandboxClient.connect(helper.path);
    await client.sandbox("a").fsWriteStream("/tmp/big.bin", Buffer.from("chunk1chunk2"), 6);
  });
});

test("volume get remove and fs", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "volume", name: "data", dir: "/tmp/vol-data", used_bytes: 0 });
    helper.push({ type: "volume", name: "data", dir: "/tmp/vol-data", used_bytes: 0 });
    helper.push({ type: "ok", version: "1" });
    const client = await SandboxClient.connect(helper.path);
    const box = client.sandbox("a");
    assert.equal((await box.volumeGet("data")).name, "data");
    assert.equal(await box.volumeFs("data"), "/tmp/vol-data");
    await box.volumeRemove("data");
  });
});

test("mount add", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" });
    const client = await SandboxClient.connect(helper.path);
    await client.sandbox("a").mountAdd({
      host_path: "data",
      guest_path: "/mnt/data",
      mount_type: "bind",
    });
  });
});

test("secret remove", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" });
    const client = await SandboxClient.connect(helper.path);
    await client.sandbox("a").secretRemove("API_KEY");
  });
});

test("snapshot restore fork remove", async () => {
  await withHelper(async (helper) => {
    helper.push({ type: "ok", version: "1" });
    helper.push({ type: "ok", version: "1" });
    helper.push({ type: "ok", version: "1" });
    const client = await SandboxClient.connect(helper.path);
    const box = client.sandbox("a");
    await box.snapshotRestore("snap-1", "box");
    await box.snapshotFork("snap-1", "forked");
    await box.snapshotRemove("snap-1");
  });
});

test("list page returns next cursor", async () => {
  await withHelper(async (helper) => {
    helper.push({
      type: "list",
      sandboxes: [{ name: "a", state: "running", image: "alpine" }],
      next_cursor: "abc",
    });
    const client = await SandboxClient.connect(helper.path);
    const page = await client.listPage(10, "prev", "team=sdk");
    assert.equal(page.sandboxes.length, 1);
    assert.equal(page.next_cursor, "abc");
  });
});

test("list with label", async () => {
  await withHelper(async (helper) => {
    helper.push({
      type: "list",
      sandboxes: [{ name: "a", state: "running", image: "alpine" }],
    });
    const client = await SandboxClient.connect(helper.path);
    assert.equal((await client.listWithLabel("team=sdk"))[0].name, "a");
  });
});

test("snapshots", async () => {
  await withHelper(async (helper) => {
    helper.push({
      type: "snapshots",
      snapshots: [{ id: "snap-1", sandbox: "box", created_at_ms: 1000 }],
    });
    const client = await SandboxClient.connect(helper.path);
    assert.equal((await client.snapshots())[0].id, "snap-1");
  });
});