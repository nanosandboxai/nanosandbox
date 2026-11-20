// Driver.cs — conformance driver state, step driving and assert evaluation.
// Mirrors the Go driver (sdk/go/conformance/run.go) per SPEC §4–§7.

using System.Collections.Generic;
using System.Text;
using System;

class ErrorPair {
    public String errorCode;
    public String message;

    public ErrorPair(String code, String message) {
        this.errorCode = code;
        this.message = message;
    }
}

class DriverState {
    public List<String> acked;
    public List<ErrorPair> errors;
    public int exitCode;
    public String stdout;
    public String stderr;
    public JsonValue getResult;
    public List<JsonValue> listResult;
    public List<JsonValue> fsEntries;
    public JsonValue fsStat;
    public bool fsExists;
    public byte[] fsReadBytes;
    public String fsContent;
    public int chunksAcked;
    public JsonValue firstLogs;
    public JsonValue resumeLogs;
    public List<JsonValue> followLines;
    public JsonValue metrics;
    public List<JsonValue> metricsSamples;
    public JsonValue volumeCreated;
    public List<JsonValue> volumes;
    public JsonValue volumeGet;
    public List<JsonValue> secrets;
    public JsonValue snapshotCreated;
    public List<JsonValue> snapshots;
    public List<JsonValue> attachEvents;
    public int attachExitCode;
    public bool pingOk;
    public WaitResult waitResult;
    public String timeoutCode;
    public bool unsupportedObserved;

    public DriverState() {
        this.acked = new List<String>();
        this.errors = new List<ErrorPair>();
        this.exitCode = -1;
        this.stdout = "";
        this.stderr = "";
        this.fsExists = false;
        this.fsReadBytes = new byte[0];
        this.fsContent = "";
        this.chunksAcked = 0;
        this.followLines = new List<JsonValue>();
        this.metricsSamples = new List<JsonValue>();
        this.volumes = new List<JsonValue>();
        this.secrets = new List<JsonValue>();
        this.snapshots = new List<JsonValue>();
        this.attachEvents = new List<JsonValue>();
        this.attachExitCode = -1;
        this.pingOk = false;
        this.timeoutCode = "";
        this.unsupportedObserved = false;
    }
}

class Driver {
    public static bool hasAck(DriverState state, String name) {
        for (int i = 0; i < state.acked.Count; i++) {
            if (state.acked[i].Equals(name)) {
                return true;
            }
        }
        return false;
    }

    public static void addAck(DriverState state, String name) {
        state.acked.Add(name);
    }

    public static JsonValue reqFromExpect(JsonValue expect) {
        JsonValue result = JsonValue.makeObject();
        foreach (String key in expect.objectValue.Keys) {
            if (!key.Equals("type") && !key.Equals("name")) {
                result.put(key, expect.get(key));
            }
        }
        return result;
    }

    public static void recordError(DriverState state, NanosbError e) {
        state.errors.Add(new ErrorPair(e.errorCode, e.message));
    }

    public static void driveStep(SandboxClient client, DriverState state, JsonValue expect) {
        String etype = expect.str("type");
        String name = expect.str("name");
        Sandbox box = client.sandbox(name);
        if (etype.Equals("create")) {
            client.create(expect.get("spec"));
            addAck(state, "create");
        } else if (etype.Equals("get")) {
            state.getResult = client.get(name);
        } else if (etype.Equals("list")) {
            state.listResult = client.list(expect.boolVal("all"));
        } else if (etype.Equals("start")) {
            box.start();
            addAck(state, "start");
        } else if (etype.Equals("stop")) {
            box.stop(expect.boolVal("force"));
            addAck(state, "stop");
        } else if (etype.Equals("kill")) {
            if (expect.has("signal")) {
                box.killWith((int) expect.intVal("signal"));
            } else {
                box.kill();
            }
            addAck(state, "kill");
        } else if (etype.Equals("wait")) {
            TimeSpan? timeout = expect.has("timeout_secs")
                ? TimeSpan.FromSeconds(expect.intVal("timeout_secs"))
                : (TimeSpan?) null;
            state.waitResult = box.wait(timeout);
            addAck(state, "wait");
        } else if (etype.Equals("restart")) {
            box.restart();
            addAck(state, "restart");
        } else if (etype.Equals("remove")) {
            box.remove(expect.boolVal("force"));
            addAck(state, "remove");
        } else if (etype.Equals("drain")) {
            box.drain();
            addAck(state, "drain");
        } else if (etype.Equals("touch")) {
            box.touch();
            addAck(state, "touch");
        } else if (etype.Equals("pause")) {
            box.pause();
            addAck(state, "pause");
        } else if (etype.Equals("resume")) {
            box.resume();
            addAck(state, "resume");
        } else if (etype.Equals("modify")) {
            box.modify(expect.get("spec"));
            addAck(state, "modify");
        } else if (etype.Equals("status")) {
            box.state();
        } else if (etype.Equals("ping")) {
            client.ping();
            state.pingOk = true;
        } else if (etype.Equals("exec")) {
            JsonValue result = box.execWith(reqFromExpect(expect));
            state.exitCode = (int) result.intVal("exit_code");
            state.stdout = result.str("stdout");
            state.stderr = result.str("stderr");
        } else if (etype.Equals("exec_stream")) {
            state.exitCode = box.execStream(reqFromExpect(expect), (JsonValue ev) => {
                String evType = ev.str("event");
                if (evType.Equals("stdout")) {
                    state.stdout += ev.str("data");
                } else if (evType.Equals("stderr")) {
                    state.stderr += ev.str("data");
                }
            });
        } else if (etype.Equals("shell")) {
            JsonValue result = box.shell(expect.str("script"));
            state.exitCode = (int) result.intVal("exit_code");
            state.stdout = result.str("stdout");
            state.stderr = result.str("stderr");
        } else if (etype.Equals("attach")) {
            box.attach((JsonValue ev) => {
                state.attachEvents.Add(ev);
            });
            state.attachExitCode = -1;
            for (int i = 0; i < state.attachEvents.Count; i++) {
                if (state.attachEvents[i].str("event").Equals("exit")) {
                    state.attachExitCode = (int) state.attachEvents[i].intVal("code");
                }
            }
        } else if (etype.Equals("attach_input")) {
            JsonValue req = JsonValue.makeObject();
            req.put("type", JsonValue.of("attach_input"));
            req.put("data", JsonValue.of(expect.str("data")));
            client.call(req);
            addAck(state, "input");
        } else if (etype.Equals("attach_resize")) {
            JsonValue req = JsonValue.makeObject();
            req.put("type", JsonValue.of("attach_resize"));
            req.put("cols", JsonValue.of(expect.intVal("cols")));
            req.put("rows", JsonValue.of(expect.intVal("rows")));
            client.call(req);
            addAck(state, "resize");
        } else if (etype.Equals("fs_read")) {
            state.fsReadBytes = box.fsRead(expect.str("path"));
        } else if (etype.Equals("fs_read_to_string")) {
            state.fsContent = box.fsReadToString(expect.str("path"));
        } else if (etype.Equals("fs_write")) {
            box.fsWrite(expect.str("path"), Base64Codec.decode(expect.str("data_base64")), false);
            addAck(state, "write");
        } else if (etype.Equals("fs_list")) {
            state.fsEntries = box.fsList(expect.str("path"));
        } else if (etype.Equals("fs_stat")) {
            state.fsStat = box.fsStat(expect.str("path"));
        } else if (etype.Equals("fs_exists")) {
            state.fsExists = box.fsExists(expect.str("path"));
        } else if (etype.Equals("fs_mkdir")) {
            box.fsMkdir(expect.str("path"), expect.boolVal("recursive"));
            addAck(state, "mkdir");
        } else if (etype.Equals("fs_remove")) {
            box.fsRemove(expect.str("path"));
        } else if (etype.Equals("fs_remove_dir")) {
            box.fsRemoveDir(expect.str("path"), expect.boolVal("recursive"));
            addAck(state, "remove_dir");
        } else if (etype.Equals("fs_copy")) {
            box.fsCopy(expect.str("from"), expect.str("to"));
            addAck(state, "copy");
        } else if (etype.Equals("fs_rename")) {
            box.fsRename(expect.str("from"), expect.str("to"));
            addAck(state, "rename");
        } else if (etype.Equals("fs_copy_from_host")) {
            box.fsCopyFromHost(expect.str("host_path"), expect.str("guest_path"));
            addAck(state, "copy_from_host");
        } else if (etype.Equals("fs_copy_to_host")) {
            box.fsCopyToHost(expect.str("guest_path"), expect.str("host_path"));
            addAck(state, "copy_to_host");
        } else if (etype.Equals("logs")) {
            driveLogs(client, state, expect, name);
        } else if (etype.Equals("metrics")) {
            state.metrics = box.metrics();
        } else if (etype.Equals("metrics_stream")) {
            box.metricsStream((int) expect.intVal("interval_ms"), (JsonValue sample) => {
                state.metricsSamples.Add(sample);
            });
        } else if (etype.Equals("volume_create")) {
            JsonValue vol = expect.get("volume");
            state.volumeCreated = box.volumeCreate(vol.str("name"), vol.str("dir"));
        } else if (etype.Equals("volume_list")) {
            state.volumes = box.volumeList();
        } else if (etype.Equals("volume_get")) {
            state.volumeGet = box.volumeGet(expect.str("volume"));
        } else if (etype.Equals("volume_remove")) {
            box.volumeRemove(expect.str("volume"));
            addAck(state, "remove");
        } else if (etype.Equals("mount_add")) {
            box.mountAdd(expect.get("mount"));
            addAck(state, "mount_add");
        } else if (etype.Equals("secret_set")) {
            box.secretSet(expect.str("key"), expect.str("value"));
            addAck(state, "set");
        } else if (etype.Equals("secret_list")) {
            state.secrets = box.secretList();
        } else if (etype.Equals("secret_remove")) {
            box.secretRemove(expect.str("key"));
            addAck(state, "remove");
        } else if (etype.Equals("snapshot_create")) {
            state.snapshotCreated = box.snapshotCreate(expect.str("dest"));
        } else if (etype.Equals("snapshot_list")) {
            state.snapshots = client.snapshots();
        } else if (etype.Equals("snapshot_restore")) {
            box.snapshotRestore(expect.str("snapshot"), expect.str("name"));
            addAck(state, "restore");
        } else if (etype.Equals("snapshot_fork")) {
            box.snapshotFork(expect.str("snapshot"), expect.str("name"));
            addAck(state, "fork");
        } else if (etype.Equals("snapshot_remove")) {
            box.snapshotRemove(expect.str("snapshot"));
            addAck(state, "remove");
        } else {
            throw new NanosbError("internal", "unhandled expect.type " + etype);
        }
    }

    private static void driveLogs(SandboxClient client, DriverState state, JsonValue expect, String name) {
        if (expect.boolVal("follow")) {
            int tail = expect.has("tail") ? (int) expect.intVal("tail") : -1;
            int cursor = expect.has("cursor") ? (int) expect.intVal("cursor") : -1;
            String source = expect.has("source") ? expect.str("source") : null;
            Sandbox box = client.sandbox(name);
            box.logsFollowWith((JsonValue line) => {
                state.followLines.Add(line);
            }, tail, cursor, source);
        } else {
            JsonValue req = JsonValue.makeObject();
            req.put("type", JsonValue.of("logs"));
            req.put("name", JsonValue.of(name));
            req.put("follow", JsonValue.of(false));
            if (expect.has("tail")) {
                req.put("tail", JsonValue.of(expect.intVal("tail")));
            }
            if (expect.has("cursor")) {
                req.put("cursor", JsonValue.of(expect.intVal("cursor")));
            }
            if (expect.has("source")) {
                req.put("source", JsonValue.of(expect.str("source")));
            }
            JsonValue resp = client.call(req);
            if (state.firstLogs == null) {
                state.firstLogs = resp;
            } else {
                state.resumeLogs = resp;
            }
        }
    }

    public static List<String> evaluateAssert(JsonValue assertBlock, DriverState state) {
        List<String> failures = new List<String>();
        if (assertBlock.has("exit_code")) {
            long want = assertBlock.intVal("exit_code");
            if (state.exitCode != (int) want) {
                failures.Add(String.Format("exit_code: expected %d, got %d", want, state.exitCode));
            }
        }
        if (assertBlock.has("stdout_equals")) {
            String want = assertBlock.str("stdout_equals");
            if (!state.stdout.Equals(want)) {
                failures.Add(String.Format("stdout_equals: expected %s, got %s", want, state.stdout));
            }
        }
        if (assertBlock.has("stderr_equals")) {
            String want = assertBlock.str("stderr_equals");
            if (!state.stderr.Equals(want)) {
                failures.Add(String.Format("stderr_equals: expected %s, got %s", want, state.stderr));
            }
        }
        checkContains(failures, assertBlock, state.stdout, "stdout_contains");
        checkContains(failures, assertBlock, state.stderr, "stderr_contains");
        checkAcks(failures, assertBlock, state);
        if (assertBlock.has("get_name")) {
            String want = assertBlock.str("get_name");
            String got = state.getResult == null ? "" : state.getResult.str("name");
            if (!got.Equals(want)) {
                failures.Add(String.Format("get_name: expected %s, got %s", want, got));
            }
        }
        if (assertBlock.has("get_state")) {
            String want = assertBlock.str("get_state");
            String got = state.getResult == null ? "" : state.getResult.str("state");
            if (!got.Equals(want)) {
                failures.Add(String.Format("get_state: expected %s, got %s", want, got));
            }
        }
        if (assertBlock.has("get_image")) {
            String want = assertBlock.str("get_image");
            String got = state.getResult == null ? "" : state.getResult.str("image");
            if (!got.Equals(want)) {
                failures.Add(String.Format("get_image: expected %s, got %s", want, got));
            }
        }
        if (assertBlock.has("list_names")) {
            List<JsonValue> want = assertBlock.arr("list_names");
            List<String> names = new List<String>();
            for (int i = 0; i < state.listResult.Count; i++) {
                names.Add(state.listResult[i].str("name"));
            }
            if (!join(names).Equals(joinValues(want))) {
                failures.Add(String.Format("list_names: expected %s, got %s", joinValues(want), join(names)));
            }
        }
        if (assertBlock.has("list_states")) {
            List<JsonValue> want = assertBlock.arr("list_states");
            List<String> states = new List<String>();
            for (int i = 0; i < state.listResult.Count; i++) {
                states.Add(state.listResult[i].str("state"));
            }
            if (!join(states).Equals(joinValues(want))) {
                failures.Add(String.Format("list_states: expected %s, got %s", joinValues(want), join(states)));
            }
        }
        if (assertBlock.has("entries_contain")) {
            List<JsonValue> want = assertBlock.arr("entries_contain");
            List<String> names = new List<String>();
            for (int i = 0; i < state.fsEntries.Count; i++) {
                names.Add(state.fsEntries[i].str("name"));
            }
            for (int i = 0; i < want.Count; i++) {
                String needle = want[i].stringValue;
                if (!contains(names, needle)) {
                    failures.Add(String.Format("entries_contain: %s not in %s", needle, join(names)));
                }
            }
        }
        if (assertBlock.has("stat_is_dir")) {
            bool want = assertBlock.boolVal("stat_is_dir");
            bool got = state.fsStat != null && state.fsStat.boolVal("is_dir");
            if (got != want) {
                failures.Add(String.Format("stat_is_dir: expected %b, got %b", want, got));
            }
        }
        if (assertBlock.has("exists")) {
            bool want = assertBlock.boolVal("exists");
            if (state.fsExists != want) {
                failures.Add(String.Format("exists: expected %b, got %b", want, state.fsExists));
            }
        }
        if (assertBlock.has("read_bytes_base64")) {
            String want = assertBlock.str("read_bytes_base64");
            String got = Base64Codec.encode(state.fsReadBytes);
            if (!got.Equals(want)) {
                failures.Add(String.Format("read_bytes_base64: expected %s, got %s", want, got));
            }
        }
        if (assertBlock.has("content_equals")) {
            String want = assertBlock.str("content_equals");
            if (!state.fsContent.Equals(want)) {
                failures.Add(String.Format("content_equals: expected %s, got %s", want, state.fsContent));
            }
        }
        if (assertBlock.has("chunks_acked")) {
            long want = assertBlock.intVal("chunks_acked");
            if (state.chunksAcked != (int) want) {
                failures.Add(String.Format("chunks_acked: expected %d, got %d", want, state.chunksAcked));
            }
        }
        checkLogLines(failures, assertBlock, state.firstLogs, "lines_contain");
        checkLogLines(failures, assertBlock, state.resumeLogs, "resume_lines_contain");
        if (assertBlock.has("follow_lines_contain")) {
            List<JsonValue> want = assertBlock.arr("follow_lines_contain");
            List<String> data = new List<String>();
            for (int i = 0; i < state.followLines.Count; i++) {
                data.Add(state.followLines[i].str("data"));
            }
            for (int i = 0; i < want.Count; i++) {
                String needle = want[i].stringValue;
                if (!anyContains(data, needle)) {
                    failures.Add(String.Format("follow_lines_contain: %s not in %s", needle, join(data)));
                }
            }
        }
        if (assertBlock.has("next_cursor")) {
            long want = assertBlock.intVal("next_cursor");
            long got = state.firstLogs == null ? -1 : state.firstLogs.intVal("next_cursor");
            if (got != want) {
                failures.Add(String.Format("next_cursor: expected %d, got %d", want, got));
            }
        }
        if (assertBlock.has("all_source")) {
            String want = assertBlock.str("all_source");
            if (state.firstLogs != null && state.firstLogs.has("lines")) {
                List<JsonValue> lines = state.firstLogs.arr("lines");
                for (int i = 0; i < lines.Count; i++) {
                    if (!lines[i].str("source").Equals(want)) {
                        failures.Add(String.Format("all_source: expected all %s", want));
                        break;
                    }
                }
            }
        }
        if (assertBlock.has("cpu_percent")) {
            double want = assertBlock.num("cpu_percent");
            double got = state.metrics == null ? -1 : state.metrics.num("cpu_percent");
            if (got != want) {
                failures.Add(String.Format("cpu_percent: expected %f, got %f", want, got));
            }
        }
        if (assertBlock.has("memory_used_bytes")) {
            long want = assertBlock.intVal("memory_used_bytes");
            long got = state.metrics == null ? -1 : state.metrics.intVal("memory_used_bytes");
            if (got != want) {
                failures.Add(String.Format("memory_used_bytes: expected %d, got %d", want, got));
            }
        }
        if (assertBlock.has("has_fields")) {
            List<JsonValue> fields = assertBlock.arr("has_fields");
            for (int i = 0; i < fields.Count; i++) {
                String f = fields[i].stringValue;
                if (state.metrics == null || !state.metrics.has(f)) {
                    failures.Add(String.Format("has_fields: %s missing", f));
                }
            }
        }
        if (assertBlock.has("stream_samples")) {
            long want = assertBlock.intVal("stream_samples");
            if (state.metricsSamples.Count != (int) want) {
                failures.Add(String.Format("stream_samples: expected %d, got %d", want, state.metricsSamples.Count));
            }
        }
        if (assertBlock.has("first_cpu_percent")) {
            double want = assertBlock.num("first_cpu_percent");
            double got = -1.0;
            if (state.metricsSamples.Count > 0) {
                got = state.metricsSamples[0].num("cpu_percent");
            }
            if (got != want) {
                failures.Add(String.Format("first_cpu_percent: expected %f, got %f", want, got));
            }
        }
        if (assertBlock.has("volume_created")) {
            String want = assertBlock.str("volume_created");
            String got = state.volumeCreated == null ? "" : state.volumeCreated.str("name");
            if (!got.Equals(want)) {
                failures.Add(String.Format("volume_created: expected %s, got %s", want, got));
            }
        }
        if (assertBlock.has("volumes_contain")) {
            List<JsonValue> want = assertBlock.arr("volumes_contain");
            List<String> names = new List<String>();
            for (int i = 0; i < state.volumes.Count; i++) {
                names.Add(state.volumes[i].str("name"));
            }
            for (int i = 0; i < want.Count; i++) {
                String needle = want[i].stringValue;
                if (!contains(names, needle)) {
                    failures.Add(String.Format("volumes_contain: %s not in %s", needle, join(names)));
                }
            }
        }
        if (assertBlock.has("volume_get_name")) {
            String want = assertBlock.str("volume_get_name");
            String got = state.volumeGet == null ? "" : state.volumeGet.str("name");
            if (!got.Equals(want)) {
                failures.Add(String.Format("volume_get_name: expected %s, got %s", want, got));
            }
        }
        if (assertBlock.has("secrets_contain")) {
            List<JsonValue> want = assertBlock.arr("secrets_contain");
            List<String> keys = new List<String>();
            for (int i = 0; i < state.secrets.Count; i++) {
                keys.Add(state.secrets[i].str("key"));
            }
            for (int i = 0; i < want.Count; i++) {
                String needle = want[i].stringValue;
                if (!contains(keys, needle)) {
                    failures.Add(String.Format("secrets_contain: %s not in %s", needle, join(keys)));
                }
            }
        }
        if (assertBlock.has("secret_values_redacted")) {
            for (int i = 0; i < state.secrets.Count; i++) {
                JsonValue s = state.secrets[i];
                if (s.has("value") && !s.get("value").isNull() && s.str("value").Length > 0) {
                    failures.Add("secret_values_redacted: values not redacted");
                    break;
                }
            }
        }
        if (assertBlock.has("snapshot_created")) {
            String want = assertBlock.str("snapshot_created");
            String got = state.snapshotCreated == null ? "" : state.snapshotCreated.str("id");
            if (!got.Equals(want)) {
                failures.Add(String.Format("snapshot_created: expected %s, got %s", want, got));
            }
        }
        if (assertBlock.has("snapshots_contain")) {
            List<JsonValue> want = assertBlock.arr("snapshots_contain");
            List<String> ids = new List<String>();
            for (int i = 0; i < state.snapshots.Count; i++) {
                ids.Add(state.snapshots[i].str("id"));
            }
            for (int i = 0; i < want.Count; i++) {
                String needle = want[i].stringValue;
                if (!contains(ids, needle)) {
                    failures.Add(String.Format("snapshots_contain: %s not in %s", needle, join(ids)));
                }
            }
        }
        if (assertBlock.has("attach_events")) {
            List<JsonValue> want = assertBlock.arr("attach_events");
            List<String> got = new List<String>();
            for (int i = 0; i < state.attachEvents.Count; i++) {
                got.Add(state.attachEvents[i].str("event"));
            }
            if (!join(got).Equals(joinValues(want))) {
                failures.Add(String.Format("attach_events: expected %s, got %s", joinValues(want), join(got)));
            }
        }
        if (assertBlock.has("attach_exit_code")) {
            long want = assertBlock.intVal("attach_exit_code");
            if (state.attachExitCode != (int) want) {
                failures.Add(String.Format("attach_exit_code: expected %d, got %d", want, state.attachExitCode));
            }
        }
        if (assertBlock.has("error_code")) {
            String want = assertBlock.str("error_code");
            String got = state.errors.Count > 0 ? state.errors[0].errorCode : "";
            if (!got.Equals(want)) {
                failures.Add(String.Format("error_code: expected %s, got %s", want, got));
            }
        }
        if (assertBlock.has("second_error_code")) {
            String want = assertBlock.str("second_error_code");
            String got = state.errors.Count > 1 ? state.errors[1].errorCode : "";
            if (!got.Equals(want)) {
                failures.Add(String.Format("second_error_code: expected %s, got %s", want, got));
            }
        }
        if (assertBlock.has("error_message_contains")) {
            String want = assertBlock.str("error_message_contains");
            String msg = state.errors.Count > 0 ? state.errors[0].message : "";
            if (!msg.Contains(want)) {
                failures.Add(String.Format("error_message_contains: %s not in %s", want, msg));
            }
        }
        if (assertBlock.has("expects_timeout")) {
            String want = assertBlock.has("timeout_code") ? assertBlock.str("timeout_code") : "timeout";
            if (!state.timeoutCode.Equals(want)) {
                failures.Add(String.Format("timeout_code: expected %s, got %s", want, state.timeoutCode));
            }
        }
        if (assertBlock.has("expects_unsupported")) {
            if (!state.unsupportedObserved) {
                failures.Add("expected an unsupported error for the unadvertised verb");
            }
        }
        if (assertBlock.has("ping_ok")) {
            if (!state.pingOk) {
                failures.Add("ping_ok: ping did not succeed");
            }
        }
        if (assertBlock.has("exec_exit_code")) {
            long want = assertBlock.intVal("exec_exit_code");
            if (state.exitCode != (int) want) {
                failures.Add(String.Format("exec_exit_code: expected %d, got %d", want, state.exitCode));
            }
        }
        if (assertBlock.has("exec_stdout")) {
            String want = assertBlock.str("exec_stdout");
            if (!state.stdout.Equals(want)) {
                failures.Add(String.Format("exec_stdout: expected %s, got %s", want, state.stdout));
            }
        }
        if (assertBlock.has("wait_exit_code")) {
            long want = assertBlock.intVal("wait_exit_code");
            int got = state.waitResult == null ? -1 : state.waitResult.ExitCode;
            if (got != (int) want) {
                failures.Add(String.Format("wait_exit_code: expected %d, got %d", want, got));
            }
        }
        if (assertBlock.has("wait_state")) {
            String want = assertBlock.str("wait_state");
            String got = state.waitResult == null ? "" : state.waitResult.State;
            if (!got.Equals(want)) {
                failures.Add(String.Format("wait_state: expected %s, got %s", want, got));
            }
        }
        return failures;
    }

    private static void checkContains(List<String> failures, JsonValue assertBlock, String haystack, String key) {
        if (!assertBlock.has(key)) {
            return;
        }
        List<JsonValue> needles = assertBlock.arr(key);
        for (int i = 0; i < needles.Count; i++) {
            String needle = needles[i].stringValue;
            if (!haystack.Contains(needle)) {
                failures.Add(String.Format("%s: %s not in %s", key, needle, haystack));
            }
        }
    }

    private static void checkAcks(List<String> failures, JsonValue assertBlock, DriverState state) {
        String[] keys = {
            "create_acked", "start_acked", "stop_acked", "kill_acked", "wait_acked",
            "restart_acked", "remove_acked",
            "drain_acked", "touch_acked", "pause_acked", "resume_acked", "modify_acked",
            "write_acked", "mkdir_acked", "remove_dir_acked", "copy_acked", "rename_acked",
            "copy_from_host_acked", "copy_to_host_acked", "mount_add_acked", "set_acked",
            "restore_acked", "fork_acked", "input_acked", "resize_acked", "write_stream_acked"
        };
        String[] names = {
            "create", "start", "stop", "kill", "wait",
            "restart", "remove",
            "drain", "touch", "pause", "resume", "modify",
            "write", "mkdir", "remove_dir", "copy", "rename",
            "copy_from_host", "copy_to_host", "mount_add", "set",
            "restore", "fork", "input", "resize", "write_stream"
        };
        for (int i = 0; i < keys.Length; i++) {
            if (assertBlock.has(keys[i]) && !hasAck(state, names[i])) {
                failures.Add(String.Format("%s: expected ack for %s", keys[i], names[i]));
            }
        }
    }

    private static void checkLogLines(List<String> failures, JsonValue assertBlock, JsonValue logs, String key) {
        if (!assertBlock.has(key) || logs == null) {
            return;
        }
        List<JsonValue> want = assertBlock.arr(key);
        List<String> data = new List<String>();
        if (logs.has("lines")) {
            List<JsonValue> lines = logs.arr("lines");
            for (int i = 0; i < lines.Count; i++) {
                data.Add(lines[i].str("data"));
            }
        }
        for (int i = 0; i < want.Count; i++) {
            String needle = want[i].stringValue;
            if (!anyContains(data, needle)) {
                failures.Add(String.Format("%s: %s not in %s", key, needle, join(data)));
            }
        }
    }

    private static String join(List<String> values) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < values.Count; i++) {
            if (i > 0) sb.Append(',');
            sb.Append(values[i]);
        }
        return sb.ToString();
    }

    private static String joinValues(List<JsonValue> values) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < values.Count; i++) {
            if (i > 0) sb.Append(',');
            sb.Append(values[i].stringValue);
        }
        return sb.ToString();
    }

    private static bool contains(List<String> haystack, String needle) {
        for (int i = 0; i < haystack.Count; i++) {
            if (haystack[i].Equals(needle)) {
                return true;
            }
        }
        return false;
    }

    private static bool anyContains(List<String> haystack, String needle) {
        for (int i = 0; i < haystack.Count; i++) {
            if (haystack[i].Contains(needle)) {
                return true;
            }
        }
        return false;
    }
}