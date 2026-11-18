// sandbox.mjs — a handle bound to one sandbox: lifecycle, execution, and
// observability verbs. Filesystem / storage / secrets / snapshots verbs live
// in `sandbox-fs.mjs` (attached to `Sandbox.prototype`) to keep this file
// focused.
import { Conn, NanosbError } from "./conn.mjs";

/** A handle bound to one sandbox. */
export class Sandbox {
  constructor(client, name) {
    this._client = client;
    this._name = name;
  }

  get name() {
    return this._name;
  }

  // ── lifecycle ────────────────────────────────────────────────────────
  async state() {
    return (await this._client._call({ type: "status", name: this._name })).state;
  }

  async info() {
    return this._client.get(this._name);
  }

  async start() {
    await this._client._call({ type: "start", name: this._name });
  }

  async stop(force = false) {
    await this._client._call({ type: "stop", name: this._name, force });
  }

  async kill() {
    await this._client._call({ type: "kill", name: this._name });
  }

  async killWith(signal) {
    await this._client._call({ type: "kill", name: this._name, signal });
  }

  async restart() {
    await this._client._call({ type: "restart", name: this._name });
  }

  async wait(timeoutSecs = null) {
    const req = { type: "wait", name: this._name };
    if (timeoutSecs !== null && timeoutSecs !== undefined) req.timeout_secs = timeoutSecs;
    const resp = await this._client._call(req);
    return {
      exitCode: resp.exit_code,
      exitedAtMs: resp.exited_at_ms,
      state: resp.state,
    };
  }

  async remove(force = false) {
    await this._client._call({ type: "remove", name: this._name, force });
  }

  async pause() {
    await this._client._call({ type: "pause", name: this._name });
  }

  async resume() {
    await this._client._call({ type: "resume", name: this._name });
  }

  async touch() {
    await this._client._call({ type: "touch", name: this._name });
  }

  async drain() {
    await this._client._call({ type: "drain", name: this._name });
  }

  async modify(spec) {
    await this._client._call({ type: "modify", name: this._name, spec });
  }

  async waitUntilStopped(timeoutSecs = 30) {
    const { state } = await this.wait(timeoutSecs);
    return state;
  }

  // ── execution ────────────────────────────────────────────────────────
  async exec(command, args = []) {
    return this.execWith({ command, args });
  }

  async execWith(request) {
    const req = { ...request, type: "exec", name: this._name };
    req.args ??= [];
    return this._client._call(req);
  }

  async shell(script) {
    return this._client._call({ type: "shell", name: this._name, script });
  }

  async execStream(request, onEvent) {
    const req = { ...request, type: "exec_stream", name: this._name };
    req.args ??= [];
    const conn = new Conn(this._client._socket);
    try {
      await conn.open();
      conn.send(req);
      const ack = await conn.recv();
      if (ack.type === "error") {
        throw new NanosbError(ack.code ?? "internal", ack.message ?? "");
      }
      let code = -1;
      while (true) {
        const resp = await conn.recv();
        if (resp.type !== "guest") continue;
        const event = resp.event;
        if (event === "exit") {
          code = resp.code ?? -1;
          onEvent(resp);
          break;
        }
        if (event === "error") {
          onEvent(resp);
          throw new NanosbError("internal", resp.message ?? "");
        }
        onEvent(resp);
      }
      return code;
    } finally {
      conn.close();
    }
  }

  async shellStream(script, onEvent) {
    return this.execStream({ command: script, shell: true }, onEvent);
  }

  async attach(onEvent) {
    const conn = new Conn(this._client._socket);
    try {
      await conn.open();
      conn.send({ type: "attach", name: this._name });
      const ack = await conn.recv();
      if (ack.type === "error") {
        throw new NanosbError(ack.code ?? "internal", ack.message ?? "");
      }
      while (true) {
        const resp = await conn.recv();
        if (resp.type !== "guest") continue;
        onEvent(resp);
        if (resp.event === "exit" || resp.event === "error") break;
      }
    } finally {
      conn.close();
    }
  }

  async attachSession() {
    const conn = new Conn(this._client._socket);
    await conn.open();
    conn.send({ type: "attach", name: this._name });
    const ack = await conn.recv();
    if (ack.type === "error") {
      conn.close();
      throw new NanosbError(ack.code ?? "internal", ack.message ?? "");
    }
    return new AttachSession(conn);
  }

  // ── observability ────────────────────────────────────────────────────
  async metrics() {
    return this._client._call({ type: "metrics", name: this._name });
  }

  async metricsStream(intervalMs, onEvent) {
    const req = { type: "metrics_stream", name: this._name };
    if (intervalMs !== undefined && intervalMs !== null) req.interval_ms = intervalMs;
    const conn = new Conn(this._client._socket);
    try {
      await conn.open();
      conn.send(req);
      const ack = await conn.recv();
      if (ack.type === "error") {
        throw new NanosbError(ack.code ?? "internal", ack.message ?? "");
      }
      while (true) {
        const resp = await conn.recv();
        if (resp.type !== "event") continue;
        if (resp.event === "done") break;
        if (resp.event === "metrics") onEvent(resp);
      }
    } finally {
      conn.close();
    }
  }

  async logs(tail) {
    const req = { type: "logs", name: this._name, follow: false };
    if (tail !== undefined && tail !== null) req.tail = tail;
    return (await this._client._call(req)).lines;
  }

  async logsWith(tail, cursor, source) {
    const req = { type: "logs", name: this._name, follow: false };
    if (tail !== undefined && tail !== null) req.tail = tail;
    if (cursor !== undefined && cursor !== null) req.cursor = cursor;
    if (source !== undefined && source !== null) req.source = source;
    return (await this._client._call(req)).lines;
  }

  async logsFollow(onLine, tail) {
    return this.logsFollowWith(onLine, tail, undefined, undefined);
  }

  async logsFollowWith(onLine, tail, cursor, source) {
    const req = { type: "logs", name: this._name, follow: true };
    if (tail !== undefined && tail !== null) req.tail = tail;
    if (cursor !== undefined && cursor !== null) req.cursor = cursor;
    if (source !== undefined && source !== null) req.source = source;
    const conn = new Conn(this._client._socket);
    try {
      await conn.open();
      conn.send(req);
      const ack = await conn.recv();
      if (ack.type === "error") {
        throw new NanosbError(ack.code ?? "internal", ack.message ?? "");
      }
      while (true) {
        const resp = await conn.recv();
        if (resp.type !== "event") continue;
        if (resp.event === "done") break;
        if (resp.event === "log") onLine(resp);
      }
    } finally {
      conn.close();
    }
  }
}

/** A live attach session: drive console input/resize and read guest events
 * on the same connection. `readEvents` returns `null` once the stream ends. */
export class AttachSession {
  constructor(conn) {
    this._conn = conn;
  }

  async sendInput(data) {
    const text = Buffer.from(data).toString("utf8");
    await this._conn.call({ type: "attach_input", data: text });
  }

  async resize(cols, rows) {
    await this._conn.call({ type: "attach_resize", cols, rows });
  }

  async readEvents() {
    while (true) {
      const resp = await this._conn.recv();
      if (resp.type !== "guest") continue;
      if (resp.event === "exit" || resp.event === "error") return null;
      return resp;
    }
  }

  close() {
    this._conn.close();
  }
}