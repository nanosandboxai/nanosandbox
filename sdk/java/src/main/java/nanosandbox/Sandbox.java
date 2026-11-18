package nanosandbox;

import java.io.IOException;
import java.time.Duration;
import java.util.List;
import java.util.function.Consumer;

/**
 * A handle bound to one sandbox: lifecycle, execution, filesystem, logs,
 * metrics, volumes, secrets, and snapshot verbs. Every non-streaming call
 * dials a fresh connection; streaming verbs ({@link #execStream},
 * {@link #attach}, {@link #logsFollowWith}, {@link #metricsStream},
 * {@link #fsWriteStream}) hold one connection for the duration of the stream:
 * an opening {@code ok} ack, then frames until the terminal frame
 * ({@code guest exit/error} or {@code event done}).
 *
 * <p>allow: SIZE_OK — the sandbox handle's verb surface is one cohesive unit
 * mirroring the Go reference's {@code sandbox.go} (484 LOC); the fs and
 * observability implementations live in {@link SandboxFs} and
 * {@link SandboxObs}.
 */
public final class Sandbox {

    private final SandboxClient client;
    private final String name;
    private final SandboxFs fs;
    private final SandboxObs obs;

    Sandbox(SandboxClient client, String name) {
        this.client = client;
        this.name = name;
        this.fs = new SandboxFs(client, name);
        this.obs = new SandboxObs(client, name);
    }

    /** Returns the sandbox name. */
    public String getName() {
        return name;
    }

    /** The terminal result of a {@link #wait(Duration)} call. */
    public record WaitResult(int exitCode, long exitedAtMs, String state) {}

    // ---- lifecycle -------------------------------------------------------

    /** Returns the sandbox lifecycle state. */
    public String state() throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("status"));
        req.set("name", Json.ofString(name));
        String st = client.call(req).getStr("state");
        return st == null ? "" : st;
    }

    /** Starts the sandbox. */
    public void start() throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("start"));
        req.set("name", Json.ofString(name));
        client.call(req);
    }

    /** Stops the sandbox. */
    public void stop(boolean force) throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("stop"));
        req.set("name", Json.ofString(name));
        req.set("force", Json.ofBool(force));
        client.call(req);
    }

    /** Kills the sandbox. */
    public void kill() throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("kill"));
        req.set("name", Json.ofString(name));
        client.call(req);
    }

    /** Kills the sandbox with a signal selector (SIGTERM=15, SIGKILL=9). */
    public void killWith(int signal) throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("kill"));
        req.set("name", Json.ofString(name));
        req.set("signal", Json.ofNumber(signal));
        client.call(req);
    }

    /** Re-boots the sandbox in place. */
    public void restart() throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("restart"));
        req.set("name", Json.ofString(name));
        client.call(req);
    }

    /**
     * Blocks until the sandbox reaches a terminal state, returning the exit
     * code, exit timestamp, and state. {@code timeout} bounds the wait; a null
     * value waits indefinitely.
     */
    public WaitResult wait(Duration timeout) throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("wait"));
        req.set("name", Json.ofString(name));
        if (timeout != null) {
            req.set("timeout_secs", Json.ofNumber(Math.max(1, timeout.toSeconds())));
        }
        Json resp = client.call(req);
        return new WaitResult(
                (int) resp.getInt("exit_code"),
                resp.getInt("exited_at_ms"),
                resp.getStr("state") == null ? "" : resp.getStr("state"));
    }

    /** Blocks until the sandbox stops, returning the terminal state. */
    public String waitUntilStopped(Duration timeout) throws IOException, NanosbError {
        return wait(timeout).state();
    }

    /** Removes the sandbox. */
    public void remove(boolean force) throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("remove"));
        req.set("name", Json.ofString(name));
        req.set("force", Json.ofBool(force));
        client.call(req);
    }

    /** Pauses the sandbox. */
    public void pause() throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("pause"));
        req.set("name", Json.ofString(name));
        client.call(req);
    }

    /** Resumes the sandbox. */
    public void resume() throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("resume"));
        req.set("name", Json.ofString(name));
        client.call(req);
    }

    /** Refreshes the sandbox's idle timer. */
    public void touch() throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("touch"));
        req.set("name", Json.ofString(name));
        client.call(req);
    }

    /** Drains the sandbox. */
    public void drain() throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("drain"));
        req.set("name", Json.ofString(name));
        client.call(req);
    }

    /** Applies a modification spec to the live sandbox. */
    public void modify(Json spec) throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("modify"));
        req.set("name", Json.ofString(name));
        req.set("spec", spec);
        client.call(req);
    }

    // ---- execution -------------------------------------------------------

    /** Runs a buffered command and returns the exec result. */
    public Json exec(String command, String[] args) throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("exec"));
        req.set("name", Json.ofString(name));
        req.set("command", Json.ofString(command));
        Json argsArr = Json.array();
        for (String a : args) {
            argsArr.add(Json.ofString(a));
        }
        req.set("args", argsArr);
        return client.call(req);
    }

    /** Runs a buffered command from a request object. */
    public Json execWith(Json request) throws IOException, NanosbError {
        request.set("type", Json.ofString("exec"));
        request.set("name", Json.ofString(name));
        if (!request.has("args")) {
            request.set("args", Json.array());
        }
        return client.call(request);
    }

    /** Runs a shell script and returns the buffered result. */
    public Json shell(String script) throws IOException, NanosbError {
        Json req = Json.object();
        req.set("type", Json.ofString("shell"));
        req.set("name", Json.ofString(name));
        req.set("script", Json.ofString(script));
        return client.call(req);
    }

    /**
     * Streams a command's output to {@code onEvent} until the exit event,
     * returning the exit code.
     */
    public int execStream(Json request, Consumer<Json> onEvent) throws IOException, NanosbError {
        request.set("type", Json.ofString("exec_stream"));
        request.set("name", Json.ofString(name));
        if (!request.has("args")) {
            request.set("args", Json.array());
        }
        try (Connection conn = new Connection(client.socket())) {
            conn.open();
            conn.send(request);
            Json ack = conn.recv();
            if ("error".equals(ack.getStr("type"))) {
                throw errorFrom(ack);
            }
            int code = -1;
            while (true) {
                Json resp = conn.recv();
                if (!"guest".equals(resp.getStr("type"))) {
                    continue;
                }
                String ev = resp.getStr("event");
                if ("exit".equals(ev)) {
                    code = (int) resp.getInt("code");
                    onEvent.accept(resp);
                    break;
                }
                if ("error".equals(ev)) {
                    onEvent.accept(resp);
                    String msg = resp.getStr("message");
                    throw new NanosbError("internal", msg == null ? "" : msg);
                }
                onEvent.accept(resp);
            }
            return code;
        }
    }

    /**
     * Attaches to the sandbox console, delivering guest events to
     * {@code onEvent} until the exit/error event.
     */
    public void attach(Consumer<Json> onEvent) throws IOException, NanosbError {
        try (Connection conn = new Connection(client.socket())) {
            conn.open();
            Json req = Json.object();
            req.set("type", Json.ofString("attach"));
            req.set("name", Json.ofString(name));
            conn.send(req);
            Json ack = conn.recv();
            if ("error".equals(ack.getStr("type"))) {
                throw errorFrom(ack);
            }
            while (true) {
                Json resp = conn.recv();
                if (!"guest".equals(resp.getStr("type"))) {
                    continue;
                }
                onEvent.accept(resp);
                String ev = resp.getStr("event");
                if ("exit".equals(ev) || "error".equals(ev)) {
                    break;
                }
            }
        }
    }

    /** Opens a live attach session for driving console input/resize. */
    public AttachSession attachSession() throws IOException, NanosbError {
        Connection conn = new Connection(client.socket());
        conn.open();
        Json req = Json.object();
        req.set("type", Json.ofString("attach"));
        req.set("name", Json.ofString(name));
        try {
            conn.send(req);
            Json ack = conn.recv();
            if ("error".equals(ack.getStr("type"))) {
                throw errorFrom(ack);
            }
        } catch (IOException | NanosbError e) {
            conn.close();
            throw e;
        }
        return new AttachSession(conn);
    }

    // ---- observability ---------------------------------------------------

    /** Returns a resource usage snapshot. */
    public Json metrics() throws IOException, NanosbError {
        return obs.metrics();
    }

    /** Streams metrics samples to {@code onEvent} until the done event. */
    public void metricsStream(int intervalMs, Consumer<Json> onEvent) throws IOException, NanosbError {
        obs.metricsStream(intervalMs, onEvent);
    }

    /** Returns buffered log lines. */
    public List<Json> logs(int tail) throws IOException, NanosbError {
        return obs.logs(tail);
    }

    /** Returns buffered log lines with tail/cursor/source filters. */
    public List<Json> logsWith(int tail, int cursor, String source) throws IOException, NanosbError {
        return obs.logsWith(tail, cursor, source);
    }

    /** Streams log lines to {@code onLine} until the done event. */
    public void logsFollow(Consumer<Json> onLine, int tail) throws IOException, NanosbError {
        obs.logsFollow(onLine, tail);
    }

    /** Streams log lines with tail/cursor/source filters. */
    public void logsFollowWith(Consumer<Json> onLine, int tail, int cursor, String source)
            throws IOException, NanosbError {
        obs.logsFollowWith(onLine, tail, cursor, source);
    }

    // ---- filesystem / storage / secrets / snapshots ----------------------

    /** Reads bytes from a guest file. */
    public byte[] fsRead(String path) throws IOException, NanosbError {
        return fs.fsRead(path);
    }

    /** Reads a guest file as UTF-8 text. */
    public String fsReadToString(String path) throws IOException, NanosbError {
        return fs.fsReadToString(path);
    }

    /** Writes bytes to a guest file. */
    public void fsWrite(String path, byte[] data, boolean append) throws IOException, NanosbError {
        fs.fsWrite(path, data, append);
    }

    /** Streams a file write in {@code fs_data} chunks. */
    public void fsWriteStream(String path, byte[] data, int chunkSize) throws IOException, NanosbError {
        fs.fsWriteStream(path, data, chunkSize);
    }

    /** Lists a guest directory. */
    public List<Json> fsList(String path) throws IOException, NanosbError {
        return fs.fsList(path);
    }

    /** Stats a guest path. */
    public Json fsStat(String path) throws IOException, NanosbError {
        return fs.fsStat(path);
    }

    /** Reports whether a guest path exists. */
    public boolean fsExists(String path) throws IOException, NanosbError {
        return fs.fsExists(path);
    }

    /** Creates a guest directory. */
    public void fsMkdir(String path, boolean recursive) throws IOException, NanosbError {
        fs.fsMkdir(path, recursive);
    }

    /** Removes a guest file. */
    public void fsRemove(String path) throws IOException, NanosbError {
        fs.fsRemove(path);
    }

    /** Removes a guest directory. */
    public void fsRemoveDir(String path, boolean recursive) throws IOException, NanosbError {
        fs.fsRemoveDir(path, recursive);
    }

    /** Copies a file inside the guest. */
    public void fsCopy(String from, String to) throws IOException, NanosbError {
        fs.fsCopy(from, to);
    }

    /** Renames a file inside the guest. */
    public void fsRename(String from, String to) throws IOException, NanosbError {
        fs.fsRename(from, to);
    }

    /** Copies a host file into the guest. */
    public void fsCopyFromHost(String hostPath, String guestPath) throws IOException, NanosbError {
        fs.fsCopyFromHost(hostPath, guestPath);
    }

    /** Copies a guest file to the host. */
    public void fsCopyToHost(String guestPath, String hostPath) throws IOException, NanosbError {
        fs.fsCopyToHost(guestPath, hostPath);
    }

    /** Creates a named volume. */
    public Json volumeCreate(String volumeName, String dir) throws IOException, NanosbError {
        return fs.volumeCreate(volumeName, dir);
    }

    /** Lists the sandbox's volumes. */
    public List<Json> volumeList() throws IOException, NanosbError {
        return fs.volumeList();
    }

    /** Fetches one volume's info. */
    public Json volumeGet(String volume) throws IOException, NanosbError {
        return fs.volumeGet(volume);
    }

    /** Removes a named volume. */
    public void volumeRemove(String volume) throws IOException, NanosbError {
        fs.volumeRemove(volume);
    }

    /** Mounts a volume into the running sandbox. */
    public void mountAdd(Json mount) throws IOException, NanosbError {
        fs.mountAdd(mount);
    }

    /** Sets a named secret. */
    public void secretSet(String key, String value) throws IOException, NanosbError {
        fs.secretSet(key, value);
    }

    /** Lists the sandbox's secrets (values redacted). */
    public List<Json> secretList() throws IOException, NanosbError {
        return fs.secretList();
    }

    /** Removes a named secret. */
    public void secretRemove(String key) throws IOException, NanosbError {
        fs.secretRemove(key);
    }

    /** Creates a snapshot of the sandbox. */
    public Json snapshotCreate(String dest) throws IOException, NanosbError {
        return fs.snapshotCreate(dest);
    }

    /** Restores a sandbox from a snapshot. */
    public void snapshotRestore(String snapshot, String snapshotName) throws IOException, NanosbError {
        fs.snapshotRestore(snapshot, snapshotName);
    }

    /** Forks a new sandbox from a snapshot. */
    public void snapshotFork(String snapshot, String snapshotName) throws IOException, NanosbError {
        fs.snapshotFork(snapshot, snapshotName);
    }

    /** Removes a snapshot. */
    public void snapshotRemove(String snapshot) throws IOException, NanosbError {
        fs.snapshotRemove(snapshot);
    }

    // ---- helpers ---------------------------------------------------------

    private static NanosbError errorFrom(Json v) {
        String code = v.getStr("code");
        String message = v.getStr("message");
        if (code == null || code.isEmpty()) {
            code = "internal";
        }
        return new NanosbError(code, message == null ? "" : message);
    }

    /**
     * A live attach session: drive console input/resize and read guest events
     * on the same connection. {@link #readEvents()} returns a null value once
     * the stream ends.
     */
    public static final class AttachSession implements AutoCloseable {

        private final Connection conn;

        AttachSession(Connection conn) {
            this.conn = conn;
        }

        /** Writes bytes to the guest console. */
        public void sendInput(String data) throws IOException, NanosbError {
            Json req = Json.object();
            req.set("type", Json.ofString("attach_input"));
            req.set("data", Json.ofString(data));
            conn.call(req);
        }

        /** Resizes the guest console. */
        public void resize(int cols, int rows) throws IOException, NanosbError {
            Json req = Json.object();
            req.set("type", Json.ofString("attach_resize"));
            req.set("cols", Json.ofNumber(cols));
            req.set("rows", Json.ofNumber(rows));
            conn.call(req);
        }

        /** Returns the next guest event, or a null value at stream end. */
        public Json readEvents() throws IOException, NanosbError {
            while (true) {
                Json resp = conn.recv();
                if (!"guest".equals(resp.getStr("type"))) {
                    continue;
                }
                String ev = resp.getStr("event");
                if ("exit".equals(ev) || "error".equals(ev)) {
                    return Json.ofNull();
                }
                return resp;
            }
        }

        /** Closes the attach session connection. */
        @Override
        public void close() {
            conn.close();
        }
    }
}