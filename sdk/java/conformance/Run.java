// Conformance driver for the Java SDK.
//
// Runs every scenario in `sdk/conformance/scenarios/` against the
// `nanosb-conformance` harness and evaluates the scenario `assert` blocks per
// `sdk/conformance/SPEC.md`. Exits non-zero if any scenario fails.
//
// Usage (from the repository root, after `mvn -q -f sdk/java/pom.xml compile`):
//     java -cp sdk/java/target/classes sdk/java/conformance/Run.java
//     NANOSB_CONFORMANCE_BIN=/path/to/nanosb-conformance \
//         java -cp sdk/java/target/classes sdk/java/conformance/Run.java
//
// allow: SIZE_OK — single-purpose driver mirroring the Go reference
// (sdk/go/conformance/run.go); the assert evaluator is a data-table of
// per-key checks, not reusable library code.

import nanosandbox.Connection;
import nanosandbox.Json;
import nanosandbox.NanosbError;
import nanosandbox.Sandbox;
import nanosandbox.SandboxClient;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.DirectoryStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.time.Duration;
import java.util.ArrayList;
import java.util.Base64;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;

public class Run {

    private static final long READY_TIMEOUT_MS = 15000;
    private static final long RECV_TIMEOUT_MS = 10000;

    private static final class ErrorPair {
        final String code;
        final String message;

        ErrorPair(String code, String message) {
            this.code = code;
            this.message = message;
        }
    }

    private static final class DriverState {
        final Set<String> acked = new HashSet<>();
        final List<ErrorPair> errors = new ArrayList<>();
        int exitCode;
        String stdout = "";
        String stderr = "";
        Json getResult = Json.ofNull();
        List<Json> listResult = new ArrayList<>();
        List<Json> fsEntries = new ArrayList<>();
        Json fsStat = Json.ofNull();
        boolean fsExists;
        byte[] fsReadBytes = new byte[0];
        String fsContent = "";
        int chunksAcked;
        Json firstLogs = Json.ofNull();
        Json resumeLogs = Json.ofNull();
        final List<Json> followLines = new ArrayList<>();
        Json metrics = Json.ofNull();
        final List<Json> metricsSamples = new ArrayList<>();
        Json volumeCreated = Json.ofNull();
        List<Json> volumes = new ArrayList<>();
        Json volumeGet = Json.ofNull();
        List<Json> secrets = new ArrayList<>();
        Json snapshotCreated = Json.ofNull();
        List<Json> snapshots = new ArrayList<>();
        final List<Json> attachEvents = new ArrayList<>();
        int attachExitCode = -1;
        boolean pingOk;
        String timeoutCode = "";
        boolean unsupportedObserved;
        Sandbox.WaitResult waitResult;
    }

    private static boolean hasAck(DriverState state, String name) {
        return state.acked.contains(name);
    }

    private static void addAck(DriverState state, String name) {
        state.acked.add(name);
    }

    /** Returns the request fields for a verb, minus the type/name routing keys. */
    private static Json reqFromExpect(Json expect) {
        Json out = Json.object();
        for (var pair : expect.asObject().entrySet()) {
            if (!"type".equals(pair.getKey()) && !"name".equals(pair.getKey())) {
                out.set(pair.getKey(), pair.getValue());
            }
        }
        return out;
    }

    /** Returns an event handler that accumulates stdout/stderr. */
    private static java.util.function.Consumer<Json> execStreamEvent(DriverState state) {
        return ev -> {
            String event = ev.getStr("event");
            String data = ev.getStr("data");
            if ("stdout".equals(event)) {
                state.stdout += data == null ? "" : data;
            } else if ("stderr".equals(event)) {
                state.stderr += data == null ? "" : data;
            }
        };
    }

    /** Records a NanosbError observation without aborting. */
    private static void recordError(DriverState state, Exception err) {
        if (err instanceof NanosbError ne) {
            state.errors.add(new ErrorPair(ne.code(), ne.message()));
        }
    }

    /** Invokes the SDK operation mapped from {@code expect.type} (SPEC §5). */
    private static void driveStep(SandboxClient client, DriverState state, Json expect)
            throws IOException, NanosbError {
        String etype = expect.getStr("type");
        String name = expect.getStr("name");
        Sandbox box = client.sandbox(name == null ? "" : name);
        switch (etype) {
            case "create": {
                client.create(expect.get("spec"));
                addAck(state, "create");
                break;
            }
            case "get": {
                state.getResult = client.get(name);
                break;
            }
            case "list": {
                state.listResult = client.list(expect.getBool("all"));
                break;
            }
            case "start": {
                box.start();
                addAck(state, "start");
                break;
            }
            case "stop": {
                box.stop(expect.getBool("force"));
                addAck(state, "stop");
                break;
            }
            case "kill": {
                if (expect.has("signal")) {
                    box.killWith((int) expect.getInt("signal"));
                } else {
                    box.kill();
                }
                addAck(state, "kill");
                break;
            }
            case "wait": {
                Duration timeout = expect.has("timeout_secs")
                        ? Duration.ofSeconds(expect.getInt("timeout_secs"))
                        : null;
                state.waitResult = box.wait(timeout);
                addAck(state, "wait");
                break;
            }
            case "restart": {
                box.restart();
                addAck(state, "restart");
                break;
            }
            case "remove": {
                box.remove(expect.getBool("force"));
                addAck(state, "remove");
                break;
            }
            case "drain": {
                box.drain();
                addAck(state, "drain");
                break;
            }
            case "touch": {
                box.touch();
                addAck(state, "touch");
                break;
            }
            case "pause": {
                box.pause();
                addAck(state, "pause");
                break;
            }
            case "resume": {
                box.resume();
                addAck(state, "resume");
                break;
            }
            case "modify": {
                box.modify(expect.get("spec"));
                addAck(state, "modify");
                break;
            }
            case "status": {
                box.state();
                break;
            }
            case "ping": {
                client.ping();
                state.pingOk = true;
                break;
            }
            case "exec": {
                Json result = box.execWith(reqFromExpect(expect));
                state.exitCode = (int) result.getInt("exit_code");
                state.stdout = result.getStr("stdout") == null ? "" : result.getStr("stdout");
                state.stderr = result.getStr("stderr") == null ? "" : result.getStr("stderr");
                break;
            }
            case "exec_stream": {
                state.exitCode = box.execStream(reqFromExpect(expect), execStreamEvent(state));
                break;
            }
            case "shell": {
                Json result = box.shell(expect.getStr("script"));
                state.exitCode = (int) result.getInt("exit_code");
                state.stdout = result.getStr("stdout") == null ? "" : result.getStr("stdout");
                state.stderr = result.getStr("stderr") == null ? "" : result.getStr("stderr");
                break;
            }
            case "attach": {
                box.attach(state.attachEvents::add);
                state.attachExitCode = -1;
                for (Json ev : state.attachEvents) {
                    if ("exit".equals(ev.getStr("event"))) {
                        state.attachExitCode = (int) ev.getInt("code");
                    }
                }
                break;
            }
            case "attach_input": {
                Json req = Json.object();
                req.set("type", Json.ofString("attach_input"));
                req.set("data", Json.ofString(expect.getStr("data")));
                client.call(req);
                addAck(state, "input");
                break;
            }
            case "attach_resize": {
                Json req = Json.object();
                req.set("type", Json.ofString("attach_resize"));
                req.set("cols", Json.ofNumber(expect.getInt("cols")));
                req.set("rows", Json.ofNumber(expect.getInt("rows")));
                client.call(req);
                addAck(state, "resize");
                break;
            }
            case "fs_read": {
                state.fsReadBytes = box.fsRead(expect.getStr("path"));
                break;
            }
            case "fs_read_to_string": {
                state.fsContent = box.fsReadToString(expect.getStr("path"));
                break;
            }
            case "fs_write": {
                String b64 = expect.getStr("data_base64");
                box.fsWrite(expect.getStr("path"), Base64.getDecoder().decode(b64), false);
                addAck(state, "write");
                break;
            }
            case "fs_list": {
                state.fsEntries = box.fsList(expect.getStr("path"));
                break;
            }
            case "fs_stat": {
                state.fsStat = box.fsStat(expect.getStr("path"));
                break;
            }
            case "fs_exists": {
                state.fsExists = box.fsExists(expect.getStr("path"));
                break;
            }
            case "fs_mkdir": {
                box.fsMkdir(expect.getStr("path"), expect.getBool("recursive"));
                addAck(state, "mkdir");
                break;
            }
            case "fs_remove": {
                box.fsRemove(expect.getStr("path"));
                break;
            }
            case "fs_remove_dir": {
                box.fsRemoveDir(expect.getStr("path"), expect.getBool("recursive"));
                addAck(state, "remove_dir");
                break;
            }
            case "fs_copy": {
                box.fsCopy(expect.getStr("from"), expect.getStr("to"));
                addAck(state, "copy");
                break;
            }
            case "fs_rename": {
                box.fsRename(expect.getStr("from"), expect.getStr("to"));
                addAck(state, "rename");
                break;
            }
            case "fs_copy_from_host": {
                box.fsCopyFromHost(expect.getStr("host_path"), expect.getStr("guest_path"));
                addAck(state, "copy_from_host");
                break;
            }
            case "fs_copy_to_host": {
                box.fsCopyToHost(expect.getStr("guest_path"), expect.getStr("host_path"));
                addAck(state, "copy_to_host");
                break;
            }
            case "logs": {
                if (expect.getBool("follow")) {
                    box.logsFollowWith(state.followLines::add,
                            (int) expect.getInt("tail"), (int) expect.getInt("cursor"),
                            expect.getStr("source"));
                } else {
                    Json req = Json.object();
                    req.set("type", Json.ofString("logs"));
                    req.set("name", Json.ofString(name));
                    req.set("follow", Json.ofBool(false));
                    if (expect.has("tail")) {
                        req.set("tail", Json.ofNumber(expect.getInt("tail")));
                    }
                    if (expect.has("cursor")) {
                        req.set("cursor", Json.ofNumber(expect.getInt("cursor")));
                    }
                    if (expect.has("source")) {
                        req.set("source", Json.ofString(expect.getStr("source")));
                    }
                    Json resp = client.call(req);
                    if (state.firstLogs.isNull()) {
                        state.firstLogs = resp;
                    } else {
                        state.resumeLogs = resp;
                    }
                }
                break;
            }
            case "metrics": {
                state.metrics = box.metrics();
                break;
            }
            case "metrics_stream": {
                box.metricsStream((int) expect.getInt("interval_ms"), state.metricsSamples::add);
                break;
            }
            case "volume_create": {
                Json vol = expect.get("volume");
                state.volumeCreated = box.volumeCreate(vol.getStr("name"), vol.getStr("dir"));
                break;
            }
            case "volume_list": {
                state.volumes = box.volumeList();
                break;
            }
            case "volume_get": {
                state.volumeGet = box.volumeGet(expect.getStr("volume"));
                break;
            }
            case "volume_remove": {
                box.volumeRemove(expect.getStr("volume"));
                addAck(state, "remove");
                break;
            }
            case "mount_add": {
                box.mountAdd(expect.get("mount"));
                addAck(state, "mount_add");
                break;
            }
            case "secret_set": {
                box.secretSet(expect.getStr("key"), expect.getStr("value"));
                addAck(state, "set");
                break;
            }
            case "secret_list": {
                state.secrets = box.secretList();
                break;
            }
            case "secret_remove": {
                box.secretRemove(expect.getStr("key"));
                addAck(state, "remove");
                break;
            }
            case "snapshot_create": {
                state.snapshotCreated = box.snapshotCreate(expect.getStr("dest"));
                break;
            }
            case "snapshot_list": {
                state.snapshots = client.snapshots();
                break;
            }
            case "snapshot_restore": {
                box.snapshotRestore(expect.getStr("snapshot"), expect.getStr("name"));
                addAck(state, "restore");
                break;
            }
            case "snapshot_fork": {
                box.snapshotFork(expect.getStr("snapshot"), expect.getStr("name"));
                addAck(state, "fork");
                break;
            }
            case "snapshot_remove": {
                box.snapshotRemove(expect.getStr("snapshot"));
                addAck(state, "remove");
                break;
            }
            default:
                throw new NanosbError("internal", "unhandled expect.type " + etype);
        }
    }

    private static final String[] ACK_KEYS = {
            "create_acked", "start_acked", "stop_acked", "kill_acked", "wait_acked",
            "restart_acked", "remove_acked",
            "drain_acked", "touch_acked", "pause_acked", "resume_acked", "modify_acked",
            "write_acked", "mkdir_acked", "remove_dir_acked", "copy_acked", "rename_acked",
            "copy_from_host_acked", "copy_to_host_acked", "mount_add_acked", "set_acked",
            "restore_acked", "fork_acked", "input_acked", "resize_acked", "write_stream_acked",
    };

    private static final String[] ACK_NAMES = {
            "create", "start", "stop", "kill", "wait",
            "restart", "remove",
            "drain", "touch", "pause", "resume", "modify",
            "write", "mkdir", "remove_dir", "copy", "rename",
            "copy_from_host", "copy_to_host", "mount_add", "set",
            "restore", "fork", "input", "resize", "write_stream",
    };

    /** Interprets the assert keys the driver understands (SPEC §7). */
    private static List<String> evaluateAssert(Json assertBlock, DriverState state) {
        List<String> failures = new ArrayList<>();
        java.util.function.BiConsumer<Boolean, String> check = (cond, msg) -> {
            if (!cond) {
                failures.add(msg);
            }
        };

        if (assertBlock.has("exit_code")) {
            long want = assertBlock.getInt("exit_code");
            check.accept(state.exitCode == want,
                    "exit_code: expected " + want + ", got " + state.exitCode);
        }
        if (assertBlock.has("stdout_equals")) {
            String want = assertBlock.getStr("stdout_equals");
            check.accept(want.equals(state.stdout),
                    "stdout_equals: expected " + want + ", got " + state.stdout);
        }
        if (assertBlock.has("stderr_equals")) {
            String want = assertBlock.getStr("stderr_equals");
            check.accept(want.equals(state.stderr),
                    "stderr_equals: expected " + want + ", got " + state.stderr);
        }
        for (String key : new String[]{"stdout_contains", "stderr_contains"}) {
            if (assertBlock.has(key)) {
                String haystack = "stdout_contains".equals(key) ? state.stdout : state.stderr;
                for (Json nv : assertBlock.getArr(key)) {
                    String needle = nv.asString();
                    check.accept(haystack.contains(needle),
                            key + ": " + needle + " not in " + haystack);
                }
            }
        }
        for (int i = 0; i < ACK_KEYS.length; i++) {
            if (assertBlock.has(ACK_KEYS[i])) {
                check.accept(hasAck(state, ACK_NAMES[i]),
                        ACK_KEYS[i] + ": expected ack for " + ACK_NAMES[i]);
            }
        }
        if (assertBlock.has("wait_exit_code")) {
            long want = assertBlock.getInt("wait_exit_code");
            long got = state.waitResult == null ? -1 : state.waitResult.exitCode();
            check.accept(got == want, "wait_exit_code: expected " + want + ", got " + got);
        }
        if (assertBlock.has("wait_state")) {
            String want = assertBlock.getStr("wait_state");
            String got = state.waitResult == null ? "" : state.waitResult.state();
            check.accept(want.equals(got), "wait_state: expected " + want + ", got " + got);
        }
        if (assertBlock.has("get_name")) {
            String want = assertBlock.getStr("get_name");
            String got = state.getResult.getStr("name");
            check.accept(want.equals(got), "get_name: expected " + want + ", got " + got);
        }
        if (assertBlock.has("get_state")) {
            String want = assertBlock.getStr("get_state");
            String got = state.getResult.getStr("state");
            check.accept(want.equals(got), "get_state: expected " + want + ", got " + got);
        }
        if (assertBlock.has("get_image")) {
            String want = assertBlock.getStr("get_image");
            String got = state.getResult.getStr("image");
            check.accept(want.equals(got), "get_image: expected " + want + ", got " + got);
        }
        if (assertBlock.has("list_names")) {
            List<String> names = new ArrayList<>();
            for (Json s : state.listResult) {
                names.add(s.getStr("name"));
            }
            check.accept(String.join(",", names).equals(joinStrings(assertBlock.getArr("list_names"))),
                    "list_names: expected " + joinStrings(assertBlock.getArr("list_names"))
                            + ", got " + String.join(",", names));
        }
        if (assertBlock.has("list_states")) {
            List<String> states = new ArrayList<>();
            for (Json s : state.listResult) {
                states.add(s.getStr("state"));
            }
            check.accept(String.join(",", states).equals(joinStrings(assertBlock.getArr("list_states"))),
                    "list_states: expected " + joinStrings(assertBlock.getArr("list_states"))
                            + ", got " + String.join(",", states));
        }
        if (assertBlock.has("entries_contain")) {
            List<String> names = new ArrayList<>();
            for (Json e : state.fsEntries) {
                names.add(e.getStr("name"));
            }
            for (Json nv : assertBlock.getArr("entries_contain")) {
                String needle = nv.asString();
                check.accept(names.contains(needle),
                        "entries_contain: " + needle + " not in " + String.join(",", names));
            }
        }
        if (assertBlock.has("stat_is_dir")) {
            boolean want = assertBlock.getBool("stat_is_dir");
            boolean got = state.fsStat.getBool("is_dir");
            check.accept(got == want, "stat_is_dir: expected " + want + ", got " + got);
        }
        if (assertBlock.has("exists")) {
            boolean want = assertBlock.getBool("exists");
            check.accept(state.fsExists == want, "exists: expected " + want + ", got " + state.fsExists);
        }
        if (assertBlock.has("read_bytes_base64")) {
            String want = assertBlock.getStr("read_bytes_base64");
            String got = Base64.getEncoder().encodeToString(state.fsReadBytes);
            check.accept(got.equals(want), "read_bytes_base64: expected " + want + ", got " + got);
        }
        if (assertBlock.has("content_equals")) {
            String want = assertBlock.getStr("content_equals");
            check.accept(state.fsContent.equals(want),
                    "content_equals: expected " + want + ", got " + state.fsContent);
        }
        if (assertBlock.has("chunks_acked")) {
            long want = assertBlock.getInt("chunks_acked");
            check.accept(state.chunksAcked == want,
                    "chunks_acked: expected " + want + ", got " + state.chunksAcked);
        }
        for (String pair : new String[]{"lines_contain", "resume_lines_contain"}) {
            if (assertBlock.has(pair)) {
                Json lines = "lines_contain".equals(pair) ? state.firstLogs : state.resumeLogs;
                List<String> data = new ArrayList<>();
                for (Json l : lines.getArr("lines")) {
                    data.add(l.getStr("data"));
                }
                for (Json nv : assertBlock.getArr(pair)) {
                    String needle = nv.asString();
                    check.accept(anyContains(data, needle),
                            pair + ": " + needle + " not in " + String.join("|", data));
                }
            }
        }
        if (assertBlock.has("follow_lines_contain")) {
            List<String> data = new ArrayList<>();
            for (Json l : state.followLines) {
                data.add(l.getStr("data"));
            }
            for (Json nv : assertBlock.getArr("follow_lines_contain")) {
                String needle = nv.asString();
                check.accept(anyContains(data, needle),
                        "follow_lines_contain: " + needle + " not in " + String.join("|", data));
            }
        }
        if (assertBlock.has("next_cursor")) {
            long want = assertBlock.getInt("next_cursor");
            long got = state.firstLogs.getInt("next_cursor");
            check.accept(got == want, "next_cursor: expected " + want + ", got " + got);
        }
        if (assertBlock.has("all_source")) {
            String want = assertBlock.getStr("all_source");
            boolean allMatch = true;
            for (Json l : state.firstLogs.getArr("lines")) {
                if (!want.equals(l.getStr("source"))) {
                    allMatch = false;
                }
            }
            check.accept(allMatch, "all_source: expected all " + want);
        }
        if (assertBlock.has("cpu_percent")) {
            double want = assertBlock.get("cpu_percent").asNumber();
            double got = state.metrics.get("cpu_percent").asNumber();
            check.accept(got == want, "cpu_percent: expected " + want + ", got " + got);
        }
        if (assertBlock.has("memory_used_bytes")) {
            long want = assertBlock.getInt("memory_used_bytes");
            long got = state.metrics.getInt("memory_used_bytes");
            check.accept(got == want, "memory_used_bytes: expected " + want + ", got " + got);
        }
        if (assertBlock.has("has_fields")) {
            for (Json fv : assertBlock.getArr("has_fields")) {
                String f = fv.asString();
                check.accept(state.metrics.has(f), "has_fields: " + f + " missing");
            }
        }
        if (assertBlock.has("stream_samples")) {
            long want = assertBlock.getInt("stream_samples");
            check.accept(state.metricsSamples.size() == want,
                    "stream_samples: expected " + want + ", got " + state.metricsSamples.size());
        }
        if (assertBlock.has("first_cpu_percent")) {
            double want = assertBlock.get("first_cpu_percent").asNumber();
            double got = -1.0;
            if (!state.metricsSamples.isEmpty()) {
                got = state.metricsSamples.get(0).get("cpu_percent").asNumber();
            }
            check.accept(got == want, "first_cpu_percent: expected " + want + ", got " + got);
        }
        if (assertBlock.has("volume_created")) {
            String want = assertBlock.getStr("volume_created");
            String got = state.volumeCreated.getStr("name");
            check.accept(want.equals(got), "volume_created: expected " + want + ", got " + got);
        }
        if (assertBlock.has("volumes_contain")) {
            List<String> names = new ArrayList<>();
            for (Json v : state.volumes) {
                names.add(v.getStr("name"));
            }
            for (Json nv : assertBlock.getArr("volumes_contain")) {
                String needle = nv.asString();
                check.accept(names.contains(needle),
                        "volumes_contain: " + needle + " not in " + String.join(",", names));
            }
        }
        if (assertBlock.has("volume_get_name")) {
            String want = assertBlock.getStr("volume_get_name");
            String got = state.volumeGet.getStr("name");
            check.accept(want.equals(got), "volume_get_name: expected " + want + ", got " + got);
        }
        if (assertBlock.has("secrets_contain")) {
            List<String> keys = new ArrayList<>();
            for (Json s : state.secrets) {
                keys.add(s.getStr("key"));
            }
            for (Json nv : assertBlock.getArr("secrets_contain")) {
                String needle = nv.asString();
                check.accept(keys.contains(needle),
                        "secrets_contain: " + needle + " not in " + String.join(",", keys));
            }
        }
        if (assertBlock.has("secret_values_redacted")) {
            boolean redacted = true;
            for (Json s : state.secrets) {
                String val = s.getStr("value");
                if (val != null && !val.isEmpty()) {
                    redacted = false;
                }
            }
            check.accept(redacted, "secret_values_redacted: values not redacted");
        }
        if (assertBlock.has("snapshot_created")) {
            String want = assertBlock.getStr("snapshot_created");
            String got = state.snapshotCreated.getStr("id");
            check.accept(want.equals(got), "snapshot_created: expected " + want + ", got " + got);
        }
        if (assertBlock.has("snapshots_contain")) {
            List<String> ids = new ArrayList<>();
            for (Json s : state.snapshots) {
                ids.add(s.getStr("id"));
            }
            for (Json nv : assertBlock.getArr("snapshots_contain")) {
                String needle = nv.asString();
                check.accept(ids.contains(needle),
                        "snapshots_contain: " + needle + " not in " + String.join(",", ids));
            }
        }
        if (assertBlock.has("attach_events")) {
            List<String> got = new ArrayList<>();
            for (Json ev : state.attachEvents) {
                got.add(ev.getStr("event"));
            }
            check.accept(String.join(",", got).equals(joinStrings(assertBlock.getArr("attach_events"))),
                    "attach_events: expected " + joinStrings(assertBlock.getArr("attach_events"))
                            + ", got " + String.join(",", got));
        }
        if (assertBlock.has("attach_exit_code")) {
            long want = assertBlock.getInt("attach_exit_code");
            check.accept(state.attachExitCode == want,
                    "attach_exit_code: expected " + want + ", got " + state.attachExitCode);
        }
        if (assertBlock.has("error_code")) {
            String want = assertBlock.getStr("error_code");
            String got = state.errors.isEmpty() ? "" : state.errors.get(0).code;
            check.accept(want.equals(got), "error_code: expected " + want + ", got " + got);
        }
        if (assertBlock.has("second_error_code")) {
            String want = assertBlock.getStr("second_error_code");
            String got = state.errors.size() > 1 ? state.errors.get(1).code : "";
            check.accept(want.equals(got), "second_error_code: expected " + want + ", got " + got);
        }
        if (assertBlock.has("error_message_contains")) {
            String want = assertBlock.getStr("error_message_contains");
            String msg = state.errors.isEmpty() ? "" : state.errors.get(0).message;
            check.accept(msg.contains(want), "error_message_contains: " + want + " not in " + msg);
        }
        if (assertBlock.has("expects_timeout")) {
            String want = assertBlock.getStr("timeout_code");
            if (want == null || want.isEmpty()) {
                want = "timeout";
            }
            check.accept(want.equals(state.timeoutCode),
                    "timeout_code: expected " + want + ", got " + state.timeoutCode);
        }
        if (assertBlock.has("expects_unsupported")) {
            check.accept(state.unsupportedObserved,
                    "expected an unsupported error for the unadvertised verb");
        }
        if (assertBlock.has("ping_ok")) {
            check.accept(state.pingOk, "ping_ok: ping did not succeed");
        }
        if (assertBlock.has("exec_exit_code")) {
            long want = assertBlock.getInt("exec_exit_code");
            check.accept(state.exitCode == want,
                    "exec_exit_code: expected " + want + ", got " + state.exitCode);
        }
        if (assertBlock.has("exec_stdout")) {
            String want = assertBlock.getStr("exec_stdout");
            check.accept(state.stdout.equals(want),
                    "exec_stdout: expected " + want + ", got " + state.stdout);
        }
        return failures;
    }

    private static String joinStrings(List<Json> vals) {
        List<String> out = new ArrayList<>();
        for (Json v : vals) {
            out.add(v.asString());
        }
        return String.join(",", out);
    }

    private static boolean anyContains(List<String> haystack, String needle) {
        for (String s : haystack) {
            if (s != null && s.contains(needle)) {
                return true;
            }
        }
        return false;
    }

    private static final class ScenarioResult {
        final boolean ok;
        final String message;

        ScenarioResult(boolean ok, String message) {
            this.ok = ok;
            this.message = message;
        }
    }

    /** Runs one scenario; returns (passed, message). */
    private static ScenarioResult runScenario(String binPath, String scenarioFile) {
        Json scenario;
        try {
            scenario = Json.parse(Files.readString(Paths.get(scenarioFile), StandardCharsets.UTF_8));
        } catch (Exception e) {
            return new ScenarioResult(false, "cannot read scenario: " + e.getMessage());
        }
        Json assertBlock = scenario.get("assert");

        Process proc = null;
        String sock = "";
        try {
            ProcessBuilder pb = new ProcessBuilder(binPath, "serve", "--scenario", scenarioFile);
            proc = pb.start();
            final Process p = proc;

            BlockingQueue<String> outLines = new LinkedBlockingQueue<>();
            Thread outThread = new Thread(() -> {
                try (BufferedReader br = new BufferedReader(
                        new InputStreamReader(p.getInputStream(), StandardCharsets.UTF_8))) {
                    String line;
                    while ((line = br.readLine()) != null) {
                        outLines.add(line);
                    }
                } catch (IOException ignored) {
                    // process died
                }
            });
            outThread.setDaemon(true);
            outThread.start();

            StringBuilder stderr = new StringBuilder();
            Thread errThread = new Thread(() -> {
                try (BufferedReader br = new BufferedReader(
                        new InputStreamReader(p.getErrorStream(), StandardCharsets.UTF_8))) {
                    String line;
                    while ((line = br.readLine()) != null) {
                        stderr.append(line).append('\n');
                    }
                } catch (IOException ignored) {
                    // process died
                }
            });
            errThread.setDaemon(true);
            errThread.start();

            String line = outLines.poll(READY_TIMEOUT_MS, TimeUnit.MILLISECONDS);
            if (line == null) {
                return new ScenarioResult(false,
                        "harness did not print READY (stderr: " + stderr + ")");
            }
            if (!line.startsWith("READY ")) {
                return new ScenarioResult(false, "harness did not print READY: " + line);
            }
            sock = line.substring("READY ".length());

            SandboxClient client = SandboxClient.connect(sock);
            DriverState state = new DriverState();
            Connection writeConn = null;
            Exception derr = null;
            try {
                for (Json step : scenario.getArr("steps")) {
                    Json expect = step.get("expect");
                    boolean hasTimeout = step.has("timeout_ms");
                    boolean expectsTimeout = assertBlock.getBool("expects_timeout");
                    if (expectsTimeout && hasTimeout) {
                        // Do NOT send the request; wait for the server's timeout error.
                        Connection conn = new Connection(sock);
                        conn.open();
                        ExecutorService ex = Executors.newSingleThreadExecutor();
                        try {
                            Future<Json> f = ex.submit(conn::recv);
                            Json resp = f.get(RECV_TIMEOUT_MS, TimeUnit.MILLISECONDS);
                            state.timeoutCode = resp.getStr("code");
                        } catch (Exception e) {
                            derr = e;
                        } finally {
                            conn.close();
                            ex.shutdownNow();
                        }
                        break;
                    }
                    String etype = expect.getStr("type");
                    if ("fs_data".equals(etype)) {
                        if (writeConn == null) {
                            derr = new IOException("fs_data step without an open write stream");
                            break;
                        }
                        Json body = Json.object();
                        body.set("type", Json.ofString("fs_data"));
                        body.set("data_base64", Json.ofString(expect.getStr("data_base64")));
                        body.set("eof", Json.ofBool(expect.getBool("eof")));
                        writeConn.call(body);
                        state.chunksAcked++;
                        continue;
                    }
                    if ("fs_write_stream".equals(etype)) {
                        writeConn = new Connection(sock);
                        writeConn.open();
                        Json req = Json.object();
                        req.set("type", Json.ofString("fs_write_stream"));
                        req.set("name", Json.ofString(strOrEmpty(expect, "name")));
                        req.set("path", Json.ofString(strOrEmpty(expect, "path")));
                        writeConn.send(req);
                        Json ack = writeConn.recv();
                        if ("error".equals(ack.getStr("type"))) {
                            state.errors.add(new ErrorPair(ack.getStr("code"), ack.getStr("message")));
                        } else {
                            addAck(state, "write_stream");
                        }
                        continue;
                    }
                    try {
                        driveStep(client, state, expect);
                    } catch (Exception e) {
                        recordError(state, e);
                    }
                }
            } finally {
                if (writeConn != null) {
                    writeConn.close();
                }
            }

            if (derr != null) {
                recordError(state, derr);
            }

            if (assertBlock.getBool("expects_unsupported")) {
                String verb = assertBlock.getStr("unsupported_verb");
                if (client.supports(verb)) {
                    state.unsupportedObserved = false;
                } else {
                    try {
                        client.sandbox("box").exec("echo", new String[]{});
                        state.unsupportedObserved = false;
                    } catch (NanosbError e) {
                        state.unsupportedObserved = "unsupported".equals(e.code());
                    }
                }
            }

            List<String> failures = evaluateAssert(assertBlock, state);
            if (!failures.isEmpty()) {
                return new ScenarioResult(false, String.join("; ", failures));
            }
            return new ScenarioResult(true, "ok");
        } catch (Exception e) {
            return new ScenarioResult(false, "driver error: " + e.getMessage());
        } finally {
            if (proc != null) {
                proc.destroy();
            }
            if (!sock.isEmpty()) {
                try {
                    Files.deleteIfExists(Paths.get(sock));
                } catch (IOException ignored) {
                    // best-effort cleanup
                }
            }
        }
    }

    private static String strOrEmpty(Json v, String key) {
        String s = v.getStr(key);
        return s == null ? "" : s;
    }

    /** Locates the nanosb-conformance harness binary. */
    private static String findBinary() {
        String env = System.getenv("NANOSB_CONFORMANCE_BIN");
        if (env != null && !env.isEmpty()) {
            return env;
        }
        return "target/debug/nanosb-conformance";
    }

    public static void main(String[] args) throws Exception {
        String binPath = findBinary();
        String scenariosDir = "sdk/conformance/scenarios";
        List<String> files = new ArrayList<>();
        try (DirectoryStream<Path> ds = Files.newDirectoryStream(Paths.get(scenariosDir), "*.json")) {
            for (Path p : ds) {
                files.add(p.toString());
            }
        } catch (IOException e) {
            System.err.println("error: cannot read scenarios dir " + scenariosDir + ": " + e.getMessage());
            System.exit(1);
        }
        if (files.isEmpty()) {
            System.err.println("error: no scenarios found in " + scenariosDir);
            System.exit(1);
        }
        Collections.sort(files);

        int passed = 0;
        int failed = 0;
        for (String scenarioFile : files) {
            String sid;
            try {
                Json scenario = Json.parse(Files.readString(Paths.get(scenarioFile), StandardCharsets.UTF_8));
                sid = scenario.getStr("id");
            } catch (Exception e) {
                failed++;
                System.out.println("FAIL " + scenarioFile + ": " + e.getMessage());
                continue;
            }
            ScenarioResult r = runScenario(binPath, scenarioFile);
            if (r.ok) {
                passed++;
                System.out.println("PASS " + sid);
            } else {
                failed++;
                System.out.println("FAIL " + sid + ": " + r.message);
            }
        }

        System.out.println();
        System.out.println(passed + " passed, " + failed + " failed");
        if (failed > 0) {
            System.exit(1);
        }
    }
}