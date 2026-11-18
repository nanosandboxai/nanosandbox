// Sandbox.cs — the sandbox handle: lifecycle, exec, filesystem, observability,
// volumes, secrets and snapshots. Streaming verbs hold one connection for the
// stream: an opening `ok` ack, then frames until the terminal frame
// (`guest exit/error` or `event done`).

using System.Collections.Generic;
using System.Text;
using System;

/// A handle bound to one named sandbox.
public class Sandbox {
    private SandboxClient client;
    private String name;

    public Sandbox(SandboxClient client, String name) {
        this.client = client;
        this.name = name;
    }

    public String getName() {
        return name;
    }

    // Lifecycle ---------------------------------------------------------

    public void start() {
        client.call(verb("start"));
    }

    public void stop(bool force) {
        JsonValue req = verb("stop");
        req.put("force", JsonValue.of(force));
        client.call(req);
    }

    public void kill() {
        client.call(verb("kill"));
    }

    public void remove(bool force) {
        JsonValue req = verb("remove");
        req.put("force", JsonValue.of(force));
        client.call(req);
    }

    public void drain() {
        client.call(verb("drain"));
    }

    public void touch() {
        client.call(verb("touch"));
    }

    public void pause() {
        client.call(verb("pause"));
    }

    public void resume() {
        client.call(verb("resume"));
    }

    public void modify(JsonValue spec) {
        JsonValue req = verb("modify");
        req.put("spec", spec);
        client.call(req);
    }

    public JsonValue state() {
        return client.call(verb("status"));
    }

    public void killWith(int signal) {
        JsonValue req = verb("kill");
        req.put("signal", JsonValue.of((long) signal));
        client.call(req);
    }

    public void restart() {
        client.call(verb("restart"));
    }

    /// Block until the sandbox reaches a terminal state, returning the exit
    /// code, exit timestamp (ms) and state. `timeout` bounds the wait; null
    /// waits indefinitely. An `error` response throws `NanosbError`.
    public WaitResult wait(TimeSpan? timeout) {
        JsonValue req = verb("wait");
        if (timeout != null) {
            req.put("timeout_secs", JsonValue.of((long) timeout.Value.TotalSeconds));
        }
        JsonValue resp = client.call(req);
        return new WaitResult(
            (int) resp.intVal("exit_code"),
            (ulong) resp.intVal("exited_at_ms"),
            resp.str("state")
        );
    }

    /// Block until the sandbox stops, bounded by `timeout`. Delegates to `wait`.
    public void waitUntilStopped(TimeSpan timeout) {
        wait(timeout);
    }

    // Execution ---------------------------------------------------------

    public JsonValue exec(String command, List<String> args) {
        JsonValue req = JsonValue.makeObject();
        req.put("command", JsonValue.of(command));
        req.put("args", JsonValue.makeArray());
        if (args != null) {
            for (int i = 0; i < args.Count; i++) {
                req.get("args").add(JsonValue.of(args[i]));
            }
        }
        return execWith(req);
    }

    public JsonValue execWith(JsonValue request) {
        JsonValue req = cloneObject(request);
        req.put("type", JsonValue.of("exec"));
        req.put("name", JsonValue.of(name));
        if (!req.has("args")) {
            req.put("args", JsonValue.makeArray());
        }
        return client.call(req);
    }

    public JsonValue shell(String script) {
        JsonValue req = verb("shell");
        req.put("script", JsonValue.of(script));
        return client.call(req);
    }

    /// Streamed exec: opening `ok`, then guest frames until `exit`/`error`.
    /// Returns the exit code.
    public int execStream(JsonValue request, System.Action<JsonValue> onEvent) {
        JsonValue req = cloneObject(request);
        req.put("type", JsonValue.of("exec_stream"));
        req.put("name", JsonValue.of(name));
        if (!req.has("args")) {
            req.put("args", JsonValue.makeArray());
        }
        int code = -1;
        Conn conn = new Conn(client.getSocketPath());
        try {
            conn.send(req);
            JsonValue ack = conn.recv();
            if (ack.str("type").Equals("error")) {
                throw new NanosbError(ack.str("code"), ack.str("message"));
            }
            while (true) {
                JsonValue resp = conn.recv();
                if (!resp.str("type").Equals("guest")) {
                    continue;
                }
                String ev = resp.str("event");
                if (ev.Equals("exit")) {
                    code = (int) resp.intVal("code");
                    onEvent(resp);
                    break;
                }
                if (ev.Equals("error")) {
                    onEvent(resp);
                    throw new NanosbError("internal", resp.str("message"));
                }
                onEvent(resp);
            }
        } finally {
            conn.close();
        }
        return code;
    }

    /// Attach to the console; delivers guest events until `exit`/`error`.
    public void attach(System.Action<JsonValue> onEvent) {
        Conn conn = new Conn(client.getSocketPath());
        try {
            conn.send(verb("attach"));
            JsonValue ack = conn.recv();
            if (ack.str("type").Equals("error")) {
                throw new NanosbError(ack.str("code"), ack.str("message"));
            }
            while (true) {
                JsonValue resp = conn.recv();
                if (!resp.str("type").Equals("guest")) {
                    continue;
                }
                onEvent(resp);
                String ev = resp.str("event");
                if (ev.Equals("exit") || ev.Equals("error")) {
                    break;
                }
            }
        } finally {
            conn.close();
        }
    }

    /// A live attach session: drive console input/resize and read guest events.
    public AttachSession attachSession() {
        Conn conn = new Conn(client.getSocketPath());
        conn.send(verb("attach"));
        JsonValue ack = conn.recv();
        if (ack.str("type").Equals("error")) {
            conn.close();
            throw new NanosbError(ack.str("code"), ack.str("message"));
        }
        return new AttachSession(conn);
    }

    // Observability -----------------------------------------------------

    public JsonValue metrics() {
        return client.call(verb("metrics"));
    }

    public void metricsStream(int intervalMs, System.Action<JsonValue> onSample) {
        JsonValue req = verb("metrics_stream");
        req.put("interval_ms", JsonValue.of((long) intervalMs));
        Conn conn = new Conn(client.getSocketPath());
        try {
            conn.send(req);
            JsonValue ack = conn.recv();
            if (ack.str("type").Equals("error")) {
                throw new NanosbError(ack.str("code"), ack.str("message"));
            }
            while (true) {
                JsonValue resp = conn.recv();
                if (!resp.str("type").Equals("event")) {
                    continue;
                }
                String ev = resp.str("event");
                if (ev.Equals("done")) {
                    break;
                }
                if (ev.Equals("metrics")) {
                    onSample(resp);
                }
            }
        } finally {
            conn.close();
        }
    }

    public JsonValue logsWith(int tail, int cursor, String source) {
        JsonValue req = verb("logs");
        req.put("follow", JsonValue.of(false));
        if (tail >= 0) {
            req.put("tail", JsonValue.of((long) tail));
        }
        if (cursor >= 0) {
            req.put("cursor", JsonValue.of((long) cursor));
        }
        if (source != null) {
            req.put("source", JsonValue.of(source));
        }
        return client.call(req);
    }

    public void logsFollowWith(System.Action<JsonValue> onLine, int tail, int cursor, String source) {
        JsonValue req = verb("logs");
        req.put("follow", JsonValue.of(true));
        if (tail >= 0) {
            req.put("tail", JsonValue.of((long) tail));
        }
        if (cursor >= 0) {
            req.put("cursor", JsonValue.of((long) cursor));
        }
        if (source != null) {
            req.put("source", JsonValue.of(source));
        }
        Conn conn = new Conn(client.getSocketPath());
        try {
            conn.send(req);
            JsonValue ack = conn.recv();
            if (ack.str("type").Equals("error")) {
                throw new NanosbError(ack.str("code"), ack.str("message"));
            }
            while (true) {
                JsonValue resp = conn.recv();
                if (!resp.str("type").Equals("event")) {
                    continue;
                }
                String ev = resp.str("event");
                if (ev.Equals("done")) {
                    break;
                }
                if (ev.Equals("log")) {
                    onLine(resp);
                }
            }
        } finally {
            conn.close();
        }
    }

    // Filesystem --------------------------------------------------------

    public byte[] fsRead(String path) {
        JsonValue req = verb("fs_read");
        req.put("path", JsonValue.of(path));
        JsonValue resp = client.call(req);
        return unb64(resp.str("data_base64"));
    }

    public String fsReadToString(String path) {
        JsonValue req = verb("fs_read_to_string");
        req.put("path", JsonValue.of(path));
        JsonValue resp = client.call(req);
        return resp.str("content");
    }

    public void fsWrite(String path, byte[] data, bool append) {
        JsonValue req = verb("fs_write");
        req.put("path", JsonValue.of(path));
        req.put("data_base64", JsonValue.of(b64(data)));
        req.put("append", JsonValue.of(append));
        client.call(req);
    }

    public void fsWriteStream(String path, byte[] data, int chunkSize) {
        Conn conn = new Conn(client.getSocketPath());
        try {
            JsonValue req = verb("fs_write_stream");
            req.put("path", JsonValue.of(path));
            conn.send(req);
            JsonValue ack = conn.recv();
            if (ack.str("type").Equals("error")) {
                throw new NanosbError(ack.str("code"), ack.str("message"));
            }
            if (chunkSize < 1) {
                chunkSize = 1;
            }
            List<byte[]> chunks = new List<byte[]>();
            for (int i = 0; i < data.Length; i += chunkSize) {
                int len = Math.Min(chunkSize, data.Length - i);
                byte[] chunk = new byte[len];
                for (int j = 0; j < len; j++) {
                    chunk[j] = data[i + j];
                }
                chunks.Add(chunk);
            }
            if (chunks.Count == 0) {
                chunks.Add(new byte[0]);
            }
            for (int i = 0; i < chunks.Count; i++) {
                JsonValue body = JsonValue.makeObject();
                body.put("type", JsonValue.of("fs_data"));
                body.put("data_base64", JsonValue.of(b64(chunks[i])));
                body.put("eof", JsonValue.of(i == chunks.Count - 1));
                conn.call(body);
            }
        } finally {
            conn.close();
        }
    }

    public List<JsonValue> fsList(String path) {
        JsonValue req = verb("fs_list");
        req.put("path", JsonValue.of(path));
        JsonValue resp = client.call(req);
        return resp.get("entries").arrayValue;
    }

    public JsonValue fsStat(String path) {
        JsonValue req = verb("fs_stat");
        req.put("path", JsonValue.of(path));
        return client.call(req);
    }

    public bool fsExists(String path) {
        JsonValue req = verb("fs_exists");
        req.put("path", JsonValue.of(path));
        JsonValue resp = client.call(req);
        return resp.boolVal("exists");
    }

    public void fsMkdir(String path, bool recursive) {
        JsonValue req = verb("fs_mkdir");
        req.put("path", JsonValue.of(path));
        req.put("recursive", JsonValue.of(recursive));
        client.call(req);
    }

    public void fsRemove(String path) {
        JsonValue req = verb("fs_remove");
        req.put("path", JsonValue.of(path));
        client.call(req);
    }

    public void fsRemoveDir(String path, bool recursive) {
        JsonValue req = verb("fs_remove_dir");
        req.put("path", JsonValue.of(path));
        req.put("recursive", JsonValue.of(recursive));
        client.call(req);
    }

    public void fsCopy(String from, String to) {
        JsonValue req = verb("fs_copy");
        req.put("from", JsonValue.of(from));
        req.put("to", JsonValue.of(to));
        client.call(req);
    }

    public void fsRename(String from, String to) {
        JsonValue req = verb("fs_rename");
        req.put("from", JsonValue.of(from));
        req.put("to", JsonValue.of(to));
        client.call(req);
    }

    public void fsCopyFromHost(String hostPath, String guestPath) {
        JsonValue req = verb("fs_copy_from_host");
        req.put("host_path", JsonValue.of(hostPath));
        req.put("guest_path", JsonValue.of(guestPath));
        client.call(req);
    }

    public void fsCopyToHost(String guestPath, String hostPath) {
        JsonValue req = verb("fs_copy_to_host");
        req.put("host_path", JsonValue.of(hostPath));
        req.put("guest_path", JsonValue.of(guestPath));
        client.call(req);
    }

    // Volumes / secrets / snapshots -------------------------------------

    public JsonValue volumeCreate(String volumeName, String dir) {
        JsonValue req = verb("volume_create");
        JsonValue vol = JsonValue.makeObject();
        vol.put("name", JsonValue.of(volumeName));
        vol.put("dir", JsonValue.of(dir));
        req.put("volume", vol);
        return client.call(req);
    }

    public List<JsonValue> volumeList() {
        JsonValue resp = client.call(verb("volume_list"));
        return resp.get("volumes").arrayValue;
    }

    public JsonValue volumeGet(String volume) {
        JsonValue req = verb("volume_get");
        req.put("volume", JsonValue.of(volume));
        return client.call(req);
    }

    public void volumeRemove(String volume) {
        JsonValue req = verb("volume_remove");
        req.put("volume", JsonValue.of(volume));
        client.call(req);
    }

    public void mountAdd(JsonValue mount) {
        JsonValue req = verb("mount_add");
        req.put("mount", mount);
        client.call(req);
    }

    public void secretSet(String key, String value) {
        JsonValue req = verb("secret_set");
        req.put("key", JsonValue.of(key));
        req.put("value", JsonValue.of(value));
        client.call(req);
    }

    public List<JsonValue> secretList() {
        JsonValue resp = client.call(verb("secret_list"));
        return resp.get("secrets").arrayValue;
    }

    public void secretRemove(String key) {
        JsonValue req = verb("secret_remove");
        req.put("key", JsonValue.of(key));
        client.call(req);
    }

    public JsonValue snapshotCreate(String dest) {
        JsonValue req = verb("snapshot_create");
        if (dest != null) {
            req.put("dest", JsonValue.of(dest));
        }
        req.put("include_memory", JsonValue.of(false));
        return client.call(req);
    }

    public void snapshotRestore(String snapshot, String targetName) {
        JsonValue req = JsonValue.makeObject();
        req.put("type", JsonValue.of("snapshot_restore"));
        req.put("snapshot", JsonValue.of(snapshot));
        req.put("name", JsonValue.of(targetName));
        client.call(req);
    }

    public void snapshotFork(String snapshot, String targetName) {
        JsonValue req = JsonValue.makeObject();
        req.put("type", JsonValue.of("snapshot_fork"));
        req.put("snapshot", JsonValue.of(snapshot));
        req.put("name", JsonValue.of(targetName));
        client.call(req);
    }

    public void snapshotRemove(String snapshot) {
        JsonValue req = JsonValue.makeObject();
        req.put("type", JsonValue.of("snapshot_remove"));
        req.put("snapshot", JsonValue.of(snapshot));
        client.call(req);
    }

    // Helpers -----------------------------------------------------------

    private JsonValue verb(String type) {
        JsonValue req = JsonValue.makeObject();
        req.put("type", JsonValue.of(type));
        req.put("name", JsonValue.of(name));
        return req;
    }

    private static JsonValue cloneObject(JsonValue src) {
        JsonValue result = JsonValue.makeObject();
        foreach (String key in src.objectValue.Keys) {
            result.put(key, src.get(key));
        }
        return result;
    }

    private static String b64(byte[] data) {
        return Base64Codec.encode(data);
    }

    private static byte[] unb64(String text) {
        return Base64Codec.decode(text);
    }
}

/// The terminal result of a `wait`: exit code, exit timestamp (ms) and state.
public sealed record WaitResult(int ExitCode, ulong ExitedAtMs, String State);

/// A live attach session: drive console input/resize and read guest events on
/// the same connection. `readEvents` returns null once the stream ends.
public class AttachSession {
    private Conn conn;

    public AttachSession(Conn conn) {
        this.conn = conn;
    }

    public void sendInput(byte[] data) {
        String text = new String(conn.utf8Codec().GetChars(data));
        JsonValue req = JsonValue.makeObject();
        req.put("type", JsonValue.of("attach_input"));
        req.put("data", JsonValue.of(text));
        conn.call(req);
    }

    public void resize(int cols, int rows) {
        JsonValue req = JsonValue.makeObject();
        req.put("type", JsonValue.of("attach_resize"));
        req.put("cols", JsonValue.of((long) cols));
        req.put("rows", JsonValue.of((long) rows));
        conn.call(req);
    }

    /// Returns the next guest event, or null at the terminal frame.
    public JsonValue readEvents() {
        while (true) {
            JsonValue resp = conn.recv();
            if (!resp.str("type").Equals("guest")) {
                continue;
            }
            String ev = resp.str("event");
            if (ev.Equals("exit") || ev.Equals("error")) {
                return null;
            }
            return resp;
        }
    }

    public void close() {
        conn.close();
    }
}