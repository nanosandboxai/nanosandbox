// Conformance driver for the Kotlin SDK.
//
// Runs every scenario in `sdk/conformance/scenarios/` against the
// `nanosb-conformance` harness and evaluates the scenario `assert` blocks per
// `sdk/conformance/SPEC.md`. Exits non-zero if any scenario fails.
//
// Usage (from the repository root, after compiling the SDK + driver):
//     java -cp sdk/kotlin/nanosandbox-kotlin.jar RunKt
//     NANOSB_CONFORMANCE_BIN=/path/to/nanosb-conformance \
//         java -cp sdk/kotlin/nanosandbox-kotlin.jar RunKt
//
// allow: SIZE_OK — single-purpose driver mirroring the Java reference
// (sdk/java/conformance/Run.java); the assert evaluator is a data-table of
// per-key checks, not reusable library code.

package nanosandbox.kt

import nanosandbox.Connection
import nanosandbox.Json
import nanosandbox.NanosbError
import java.io.BufferedReader
import java.io.File
import java.io.IOException
import java.io.InputStreamReader
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import java.util.ArrayList
import java.util.Base64
import java.util.HashSet
import java.util.concurrent.Executors
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import kotlin.time.DurationUnit
import kotlin.time.toDuration

private const val READY_TIMEOUT_MS = 15000L
private const val RECV_TIMEOUT_MS = 10000L

private class ErrorPair(val code: String, val message: String)

private class DriverState {
    val acked = HashSet<String>()
    val errors = ArrayList<ErrorPair>()
    var exitCode = 0
    var stdout = ""
    var stderr = ""
    var getResult = KtJson.nul()
    var listResult: List<Json> = ArrayList()
    var fsEntries: List<Json> = ArrayList()
    var fsStat = KtJson.nul()
    var fsExists = false
    var fsReadBytes = ByteArray(0)
    var fsContent = ""
    var chunksAcked = 0
    var firstLogs = KtJson.nul()
    var resumeLogs = KtJson.nul()
    val followLines = ArrayList<Json>()
    var metrics = KtJson.nul()
    val metricsSamples = ArrayList<Json>()
    var volumeCreated = KtJson.nul()
    var volumes: List<Json> = ArrayList()
    var volumeGet = KtJson.nul()
    var secrets: List<Json> = ArrayList()
    var snapshotCreated = KtJson.nul()
    var snapshots: List<Json> = ArrayList()
    val attachEvents = ArrayList<Json>()
    var attachExitCode = -1
    var pingOk = false
    var waitResult: WaitResult? = null
    var timeoutCode = ""
    var unsupportedObserved = false
}

private fun hasAck(state: DriverState, name: String): Boolean = state.acked.contains(name)

private fun addAck(state: DriverState, name: String) {
    state.acked.add(name)
}

/** Returns the request fields for a verb, minus the type/name routing keys. */
private fun reqFromExpect(expect: Json): Json {
    val out = KtJson.obj()
    for ((key, value) in expect.asObject()) {
        if (key != "type" && key != "name") {
            out.set(key, value)
        }
    }
    return out
}

/** Returns an event handler that accumulates stdout/stderr. */
private fun execStreamEvent(state: DriverState): (Json) -> Unit {
    return { ev ->
        val event = ev.getStr("event")
        val data = ev.getStr("data")
        if (event == "stdout") {
            state.stdout += data ?: ""
        } else if (event == "stderr") {
            state.stderr += data ?: ""
        }
    }
}

/** Records a NanosbError observation without aborting. */
private fun recordError(state: DriverState, err: Exception) {
    if (err is NanosbError) {
        state.errors.add(ErrorPair(err.code(), err.message()))
    }
}

/** Invokes the SDK operation mapped from {@code expect.type} (SPEC §5). */
private fun driveStep(client: KotlinClient, state: DriverState, expect: Json) {
    val etype = expect.getStr("type")
    val name = expect.getStr("name")
    val box = client.sandbox(name ?: "")
    when (etype) {
        "create" -> {
            client.create(expect.get("spec"))
            addAck(state, "create")
        }
        "get" -> {
            state.getResult = client.get(name)
        }
        "list" -> {
            state.listResult = client.list(expect.getBool("all"))
        }
        "start" -> {
            box.start()
            addAck(state, "start")
        }
        "stop" -> {
            box.stop(expect.getBool("force"))
            addAck(state, "stop")
        }
        "kill" -> {
            if (expect.has("signal")) {
                box.killWith(expect.getInt("signal").toInt())
            } else {
                box.kill()
            }
            addAck(state, "kill")
        }
        "wait" -> {
            val timeout = if (expect.has("timeout_secs")) {
                expect.getInt("timeout_secs").toDouble().toDuration(DurationUnit.SECONDS)
            } else {
                null
            }
            state.waitResult = box.wait(timeout)
            addAck(state, "wait")
        }
        "restart" -> {
            box.restart()
            addAck(state, "restart")
        }
        "remove" -> {
            box.remove(expect.getBool("force"))
            addAck(state, "remove")
        }
        "drain" -> {
            box.drain()
            addAck(state, "drain")
        }
        "touch" -> {
            box.touch()
            addAck(state, "touch")
        }
        "pause" -> {
            box.pause()
            addAck(state, "pause")
        }
        "resume" -> {
            box.resume()
            addAck(state, "resume")
        }
        "modify" -> {
            box.modify(expect.get("spec"))
            addAck(state, "modify")
        }
        "status" -> {
            box.state()
        }
        "ping" -> {
            client.ping()
            state.pingOk = true
        }
        "exec" -> {
            val result = box.execWith(reqFromExpect(expect))
            state.exitCode = result.getInt("exit_code").toInt()
            state.stdout = result.getStr("stdout") ?: ""
            state.stderr = result.getStr("stderr") ?: ""
        }
        "exec_stream" -> {
            state.exitCode = box.execStream(reqFromExpect(expect), execStreamEvent(state))
        }
        "shell" -> {
            val result = box.shell(expect.getStr("script"))
            state.exitCode = result.getInt("exit_code").toInt()
            state.stdout = result.getStr("stdout") ?: ""
            state.stderr = result.getStr("stderr") ?: ""
        }
        "attach" -> {
            box.attach { ev -> state.attachEvents.add(ev) }
            state.attachExitCode = -1
            for (ev in state.attachEvents) {
                if (ev.getStr("event") == "exit") {
                    state.attachExitCode = ev.getInt("code").toInt()
                }
            }
        }
        "attach_input" -> {
            val req = KtJson.obj()
            req.set("type", KtJson.str("attach_input"))
            req.set("data", KtJson.str(expect.getStr("data")))
            client.call(req)
            addAck(state, "input")
        }
        "attach_resize" -> {
            val req = KtJson.obj()
            req.set("type", KtJson.str("attach_resize"))
            req.set("cols", KtJson.num(expect.getInt("cols").toDouble()))
            req.set("rows", KtJson.num(expect.getInt("rows").toDouble()))
            client.call(req)
            addAck(state, "resize")
        }
        "fs_read" -> {
            state.fsReadBytes = box.fsRead(expect.getStr("path"))
        }
        "fs_read_to_string" -> {
            state.fsContent = box.fsReadToString(expect.getStr("path"))
        }
        "fs_write" -> {
            val b64 = expect.getStr("data_base64")
            box.fsWrite(expect.getStr("path"), Base64.getDecoder().decode(b64), false)
            addAck(state, "write")
        }
        "fs_list" -> {
            state.fsEntries = box.fsList(expect.getStr("path"))
        }
        "fs_stat" -> {
            state.fsStat = box.fsStat(expect.getStr("path"))
        }
        "fs_exists" -> {
            state.fsExists = box.fsExists(expect.getStr("path"))
        }
        "fs_mkdir" -> {
            box.fsMkdir(expect.getStr("path"), expect.getBool("recursive"))
            addAck(state, "mkdir")
        }
        "fs_remove" -> {
            box.fsRemove(expect.getStr("path"))
        }
        "fs_remove_dir" -> {
            box.fsRemoveDir(expect.getStr("path"), expect.getBool("recursive"))
            addAck(state, "remove_dir")
        }
        "fs_copy" -> {
            box.fsCopy(expect.getStr("from"), expect.getStr("to"))
            addAck(state, "copy")
        }
        "fs_rename" -> {
            box.fsRename(expect.getStr("from"), expect.getStr("to"))
            addAck(state, "rename")
        }
        "fs_copy_from_host" -> {
            box.fsCopyFromHost(expect.getStr("host_path"), expect.getStr("guest_path"))
            addAck(state, "copy_from_host")
        }
        "fs_copy_to_host" -> {
            box.fsCopyToHost(expect.getStr("guest_path"), expect.getStr("host_path"))
            addAck(state, "copy_to_host")
        }
        "logs" -> {
            if (expect.getBool("follow")) {
                box.logsFollowWith(
                    { line -> state.followLines.add(line) },
                    expect.getInt("tail").toInt(),
                    expect.getInt("cursor").toInt(),
                    expect.getStr("source"))
            } else {
                val req = KtJson.obj()
                req.set("type", KtJson.str("logs"))
                req.set("name", KtJson.str(name))
                req.set("follow", KtJson.bool(false))
                if (expect.has("tail")) {
                    req.set("tail", KtJson.num(expect.getInt("tail").toDouble()))
                }
                if (expect.has("cursor")) {
                    req.set("cursor", KtJson.num(expect.getInt("cursor").toDouble()))
                }
                if (expect.has("source")) {
                    req.set("source", KtJson.str(expect.getStr("source")))
                }
                val resp = client.call(req)
                if (state.firstLogs.isNull()) {
                    state.firstLogs = resp
                } else {
                    state.resumeLogs = resp
                }
            }
        }
        "metrics" -> {
            state.metrics = box.metrics()
        }
        "metrics_stream" -> {
            box.metricsStream(expect.getInt("interval_ms").toInt()) { sample ->
                state.metricsSamples.add(sample)
            }
        }
        "volume_create" -> {
            val vol = expect.get("volume")
            state.volumeCreated = box.volumeCreate(vol.getStr("name"), vol.getStr("dir"))
        }
        "volume_list" -> {
            state.volumes = box.volumeList()
        }
        "volume_get" -> {
            state.volumeGet = box.volumeGet(expect.getStr("volume"))
        }
        "volume_remove" -> {
            box.volumeRemove(expect.getStr("volume"))
            addAck(state, "remove")
        }
        "mount_add" -> {
            box.mountAdd(expect.get("mount"))
            addAck(state, "mount_add")
        }
        "secret_set" -> {
            box.secretSet(expect.getStr("key"), expect.getStr("value"))
            addAck(state, "set")
        }
        "secret_list" -> {
            state.secrets = box.secretList()
        }
        "secret_remove" -> {
            box.secretRemove(expect.getStr("key"))
            addAck(state, "remove")
        }
        "snapshot_create" -> {
            state.snapshotCreated = box.snapshotCreate(expect.getStr("dest"))
        }
        "snapshot_list" -> {
            state.snapshots = client.snapshots()
        }
        "snapshot_restore" -> {
            box.snapshotRestore(expect.getStr("snapshot"), expect.getStr("name"))
            addAck(state, "restore")
        }
        "snapshot_fork" -> {
            box.snapshotFork(expect.getStr("snapshot"), expect.getStr("name"))
            addAck(state, "fork")
        }
        "snapshot_remove" -> {
            box.snapshotRemove(expect.getStr("snapshot"))
            addAck(state, "remove")
        }
        else -> throw NanosbError("internal", "unhandled expect.type $etype")
    }
}

private val ACK_KEYS = arrayOf(
    "create_acked", "start_acked", "stop_acked", "kill_acked", "wait_acked",
    "restart_acked", "remove_acked",
    "drain_acked", "touch_acked", "pause_acked", "resume_acked", "modify_acked",
    "write_acked", "mkdir_acked", "remove_dir_acked", "copy_acked", "rename_acked",
    "copy_from_host_acked", "copy_to_host_acked", "mount_add_acked", "set_acked",
    "restore_acked", "fork_acked", "input_acked", "resize_acked", "write_stream_acked")

private val ACK_NAMES = arrayOf(
    "create", "start", "stop", "kill", "wait",
    "restart", "remove",
    "drain", "touch", "pause", "resume", "modify",
    "write", "mkdir", "remove_dir", "copy", "rename",
    "copy_from_host", "copy_to_host", "mount_add", "set",
    "restore", "fork", "input", "resize", "write_stream")

private fun joinStrings(vals: List<Json>): String =
    vals.map { it.asString() ?: "" }.joinToString(",")

private fun anyContains(haystack: List<String>, needle: String): Boolean {
    for (s in haystack) {
        if (s.contains(needle)) {
            return true
        }
    }
    return false
}

/** Interprets the assert keys the driver understands (SPEC §7). */
private fun evaluateAssert(assertBlock: Json, state: DriverState): List<String> {
    val failures = ArrayList<String>()
    fun check(cond: Boolean, msg: String) {
        if (!cond) {
            failures.add(msg)
        }
    }

    if (assertBlock.has("exit_code")) {
        val want = assertBlock.getInt("exit_code")
        check(state.exitCode.toLong() == want, "exit_code: expected $want, got ${state.exitCode}")
    }
    if (assertBlock.has("stdout_equals")) {
        val want = assertBlock.getStr("stdout_equals")
        check(state.stdout == want, "stdout_equals: expected $want, got ${state.stdout}")
    }
    if (assertBlock.has("stderr_equals")) {
        val want = assertBlock.getStr("stderr_equals")
        check(state.stderr == want, "stderr_equals: expected $want, got ${state.stderr}")
    }
    for (key in arrayOf("stdout_contains", "stderr_contains")) {
        if (assertBlock.has(key)) {
            val haystack = if (key == "stdout_contains") state.stdout else state.stderr
            for (nv in assertBlock.getArr(key)) {
                val needle = nv.asString()
                check(needle != null && haystack.contains(needle),
                    "$key: $needle not in $haystack")
            }
        }
    }
    for (i in 0 until ACK_KEYS.size) {
        if (assertBlock.has(ACK_KEYS[i])) {
            check(hasAck(state, ACK_NAMES[i]),
                "${ACK_KEYS[i]}: expected ack for ${ACK_NAMES[i]}")
        }
    }
    if (assertBlock.has("wait_exit_code")) {
        val want = assertBlock.getInt("wait_exit_code")
        val got = state.waitResult?.exitCode?.toLong()
        check(got == want, "wait_exit_code: expected $want, got $got")
    }
    if (assertBlock.has("wait_state")) {
        val want = assertBlock.getStr("wait_state")
        val got = state.waitResult?.state
        check(got == want, "wait_state: expected $want, got $got")
    }
    if (assertBlock.has("get_name")) {
        val want = assertBlock.getStr("get_name")
        val got = state.getResult.getStr("name")
        check(got == want, "get_name: expected $want, got $got")
    }
    if (assertBlock.has("get_state")) {
        val want = assertBlock.getStr("get_state")
        val got = state.getResult.getStr("state")
        check(got == want, "get_state: expected $want, got $got")
    }
    if (assertBlock.has("get_image")) {
        val want = assertBlock.getStr("get_image")
        val got = state.getResult.getStr("image")
        check(got == want, "get_image: expected $want, got $got")
    }
    if (assertBlock.has("list_names")) {
        val names = state.listResult.map { it.getStr("name") ?: "" }
        check(names.joinToString(",") == joinStrings(assertBlock.getArr("list_names")),
            "list_names: expected ${joinStrings(assertBlock.getArr("list_names"))}, got ${names.joinToString(",")}")
    }
    if (assertBlock.has("list_states")) {
        val states = state.listResult.map { it.getStr("state") ?: "" }
        check(states.joinToString(",") == joinStrings(assertBlock.getArr("list_states")),
            "list_states: expected ${joinStrings(assertBlock.getArr("list_states"))}, got ${states.joinToString(",")}")
    }
    if (assertBlock.has("entries_contain")) {
        val names = state.fsEntries.map { it.getStr("name") ?: "" }
        for (nv in assertBlock.getArr("entries_contain")) {
            val needle = nv.asString()
            check(needle != null && names.contains(needle),
                "entries_contain: $needle not in ${names.joinToString(",")}")
        }
    }
    if (assertBlock.has("stat_is_dir")) {
        val want = assertBlock.getBool("stat_is_dir")
        val got = state.fsStat.getBool("is_dir")
        check(got == want, "stat_is_dir: expected $want, got $got")
    }
    if (assertBlock.has("exists")) {
        val want = assertBlock.getBool("exists")
        check(state.fsExists == want, "exists: expected $want, got ${state.fsExists}")
    }
    if (assertBlock.has("read_bytes_base64")) {
        val want = assertBlock.getStr("read_bytes_base64")
        val got = Base64.getEncoder().encodeToString(state.fsReadBytes)
        check(got == want, "read_bytes_base64: expected $want, got $got")
    }
    if (assertBlock.has("content_equals")) {
        val want = assertBlock.getStr("content_equals")
        check(state.fsContent == want, "content_equals: expected $want, got ${state.fsContent}")
    }
    if (assertBlock.has("chunks_acked")) {
        val want = assertBlock.getInt("chunks_acked")
        check(state.chunksAcked.toLong() == want,
            "chunks_acked: expected $want, got ${state.chunksAcked}")
    }
    for (pair in arrayOf("lines_contain", "resume_lines_contain")) {
        if (assertBlock.has(pair)) {
            val lines = if (pair == "lines_contain") state.firstLogs else state.resumeLogs
            val data = lines.getArr("lines").map { it.getStr("data") ?: "" }
            for (nv in assertBlock.getArr(pair)) {
                val needle = nv.asString()
                check(needle != null && anyContains(data, needle),
                    "$pair: $needle not in ${data.joinToString("|")}")
            }
        }
    }
    if (assertBlock.has("follow_lines_contain")) {
        val data = state.followLines.map { it.getStr("data") ?: "" }
        for (nv in assertBlock.getArr("follow_lines_contain")) {
            val needle = nv.asString()
            check(needle != null && anyContains(data, needle),
                "follow_lines_contain: $needle not in ${data.joinToString("|")}")
        }
    }
    if (assertBlock.has("next_cursor")) {
        val want = assertBlock.getInt("next_cursor")
        val got = state.firstLogs.getInt("next_cursor")
        check(got == want, "next_cursor: expected $want, got $got")
    }
    if (assertBlock.has("all_source")) {
        val want = assertBlock.getStr("all_source")
        var allMatch = true
        for (l in state.firstLogs.getArr("lines")) {
            if (l.getStr("source") != want) {
                allMatch = false
            }
        }
        check(allMatch, "all_source: expected all $want")
    }
    if (assertBlock.has("cpu_percent")) {
        val want = assertBlock.get("cpu_percent").asNumber()
        val got = state.metrics.get("cpu_percent").asNumber()
        check(got == want, "cpu_percent: expected $want, got $got")
    }
    if (assertBlock.has("memory_used_bytes")) {
        val want = assertBlock.getInt("memory_used_bytes")
        val got = state.metrics.getInt("memory_used_bytes")
        check(got == want, "memory_used_bytes: expected $want, got $got")
    }
    if (assertBlock.has("has_fields")) {
        for (fv in assertBlock.getArr("has_fields")) {
            val f = fv.asString()
            check(f != null && state.metrics.has(f), "has_fields: $f missing")
        }
    }
    if (assertBlock.has("stream_samples")) {
        val want = assertBlock.getInt("stream_samples")
        check(state.metricsSamples.size.toLong() == want,
            "stream_samples: expected $want, got ${state.metricsSamples.size}")
    }
    if (assertBlock.has("first_cpu_percent")) {
        val want = assertBlock.get("first_cpu_percent").asNumber()
        var got = -1.0
        if (!state.metricsSamples.isEmpty()) {
            got = state.metricsSamples[0].get("cpu_percent").asNumber()
        }
        check(got == want, "first_cpu_percent: expected $want, got $got")
    }
    if (assertBlock.has("volume_created")) {
        val want = assertBlock.getStr("volume_created")
        val got = state.volumeCreated.getStr("name")
        check(got == want, "volume_created: expected $want, got $got")
    }
    if (assertBlock.has("volumes_contain")) {
        val names = state.volumes.map { it.getStr("name") ?: "" }
        for (nv in assertBlock.getArr("volumes_contain")) {
            val needle = nv.asString()
            check(needle != null && names.contains(needle),
                "volumes_contain: $needle not in ${names.joinToString(",")}")
        }
    }
    if (assertBlock.has("volume_get_name")) {
        val want = assertBlock.getStr("volume_get_name")
        val got = state.volumeGet.getStr("name")
        check(got == want, "volume_get_name: expected $want, got $got")
    }
    if (assertBlock.has("secrets_contain")) {
        val keys = state.secrets.map { it.getStr("key") ?: "" }
        for (nv in assertBlock.getArr("secrets_contain")) {
            val needle = nv.asString()
            check(needle != null && keys.contains(needle),
                "secrets_contain: $needle not in ${keys.joinToString(",")}")
        }
    }
    if (assertBlock.has("secret_values_redacted")) {
        var redacted = true
        for (s in state.secrets) {
            val v = s.getStr("value")
            if (v != null && !v.isEmpty()) {
                redacted = false
            }
        }
        check(redacted, "secret_values_redacted: values not redacted")
    }
    if (assertBlock.has("snapshot_created")) {
        val want = assertBlock.getStr("snapshot_created")
        val got = state.snapshotCreated.getStr("id")
        check(got == want, "snapshot_created: expected $want, got $got")
    }
    if (assertBlock.has("snapshots_contain")) {
        val ids = state.snapshots.map { it.getStr("id") ?: "" }
        for (nv in assertBlock.getArr("snapshots_contain")) {
            val needle = nv.asString()
            check(needle != null && ids.contains(needle),
                "snapshots_contain: $needle not in ${ids.joinToString(",")}")
        }
    }
    if (assertBlock.has("attach_events")) {
        val got = state.attachEvents.map { it.getStr("event") ?: "" }
        check(got.joinToString(",") == joinStrings(assertBlock.getArr("attach_events")),
            "attach_events: expected ${joinStrings(assertBlock.getArr("attach_events"))}, got ${got.joinToString(",")}")
    }
    if (assertBlock.has("attach_exit_code")) {
        val want = assertBlock.getInt("attach_exit_code")
        check(state.attachExitCode.toLong() == want,
            "attach_exit_code: expected $want, got ${state.attachExitCode}")
    }
    if (assertBlock.has("error_code")) {
        val want = assertBlock.getStr("error_code")
        val got = if (state.errors.isEmpty()) "" else state.errors[0].code
        check(got == want, "error_code: expected $want, got $got")
    }
    if (assertBlock.has("second_error_code")) {
        val want = assertBlock.getStr("second_error_code")
        val got = if (state.errors.size > 1) state.errors[1].code else ""
        check(got == want, "second_error_code: expected $want, got $got")
    }
    if (assertBlock.has("error_message_contains")) {
        val want = assertBlock.getStr("error_message_contains")
        val msg = if (state.errors.isEmpty()) "" else state.errors[0].message
        check(want != null && msg.contains(want), "error_message_contains: $want not in $msg")
    }
    if (assertBlock.has("expects_timeout")) {
        var want = assertBlock.getStr("timeout_code")
        if (want == null || want.isEmpty()) {
            want = "timeout"
        }
        check(want == state.timeoutCode, "timeout_code: expected $want, got ${state.timeoutCode}")
    }
    if (assertBlock.has("expects_unsupported")) {
        check(state.unsupportedObserved,
            "expected an unsupported error for the unadvertised verb")
    }
    if (assertBlock.has("ping_ok")) {
        check(state.pingOk, "ping_ok: ping did not succeed")
    }
    if (assertBlock.has("exec_exit_code")) {
        val want = assertBlock.getInt("exec_exit_code")
        check(state.exitCode.toLong() == want,
            "exec_exit_code: expected $want, got ${state.exitCode}")
    }
    if (assertBlock.has("exec_stdout")) {
        val want = assertBlock.getStr("exec_stdout")
        check(state.stdout == want, "exec_stdout: expected $want, got ${state.stdout}")
    }
    return failures
}

private class ScenarioResult(val ok: Boolean, val message: String)

private fun strOrEmpty(v: Json, key: String): String = v.getStr(key) ?: ""

/** Runs one scenario; returns (passed, message). */
private fun runScenario(binPath: String, scenarioFile: String): ScenarioResult {
    val scenario = try {
        KtJson.parse(File(scenarioFile).readText())
    } catch (e: Exception) {
        return ScenarioResult(false, "cannot read scenario: ${e.message}")
    }
    val assertBlock = scenario.get("assert")

    var proc: Process? = null
    var sock = ""
    try {
        val pb = ProcessBuilder(binPath, "serve", "--scenario", scenarioFile)
        val p = pb.start()
        proc = p

        val outLines = LinkedBlockingQueue<String>()
        val outThread = Thread {
            try {
                val br = BufferedReader(InputStreamReader(p.getInputStream(), StandardCharsets.UTF_8))
                while (true) {
                    val line = br.readLine()
                    if (line == null) {
                        break
                    }
                    outLines.add(line)
                }
            } catch (e: IOException) {
                // process died
            }
        }
        outThread.isDaemon = true
        outThread.start()

        val stderr = StringBuilder()
        val errThread = Thread {
            try {
                val br = BufferedReader(InputStreamReader(p.getErrorStream(), StandardCharsets.UTF_8))
                while (true) {
                    val line = br.readLine()
                    if (line == null) {
                        break
                    }
                    stderr.append(line).append('\n')
                }
            } catch (e: IOException) {
                // process died
            }
        }
        errThread.isDaemon = true
        errThread.start()

        val line = outLines.poll(READY_TIMEOUT_MS, TimeUnit.MILLISECONDS)
        if (line == null) {
            return ScenarioResult(false, "harness did not print READY (stderr: $stderr)")
        }
        if (!line.startsWith("READY ")) {
            return ScenarioResult(false, "harness did not print READY: $line")
        }
        sock = line.substring("READY ".length)

        val client = KotlinClient.connect(sock)
        val state = DriverState()
        var writeConn: Connection? = null
        var derr: Exception? = null
        try {
            for (step in scenario.getArr("steps")) {
                val expect = step.get("expect")
                val hasTimeout = step.has("timeout_ms")
                val expectsTimeout = assertBlock.getBool("expects_timeout")
                if (expectsTimeout && hasTimeout) {
                    // Do NOT send the request; wait for the server's timeout error.
                    val conn = Connection(sock)
                    conn.open()
                    val ex = Executors.newSingleThreadExecutor()
                    try {
                        val f = ex.submit<Json> { conn.recv() }
                        val resp = f.get(RECV_TIMEOUT_MS, TimeUnit.MILLISECONDS)
                        state.timeoutCode = resp.getStr("code") ?: ""
                    } catch (e: Exception) {
                        derr = e
                    } finally {
                        conn.close()
                        ex.shutdownNow()
                    }
                    break
                }
                val etype = expect.getStr("type")
                if (etype == "fs_data") {
                    if (writeConn == null) {
                        derr = IOException("fs_data step without an open write stream")
                        break
                    }
                    val body = KtJson.obj()
                    body.set("type", KtJson.str("fs_data"))
                    body.set("data_base64", KtJson.str(expect.getStr("data_base64")))
                    body.set("eof", KtJson.bool(expect.getBool("eof")))
                    writeConn.call(body)
                    state.chunksAcked++
                    continue
                }
                if (etype == "fs_write_stream") {
                    writeConn = Connection(sock)
                    writeConn.open()
                    val req = KtJson.obj()
                    req.set("type", KtJson.str("fs_write_stream"))
                    req.set("name", KtJson.str(strOrEmpty(expect, "name")))
                    req.set("path", KtJson.str(strOrEmpty(expect, "path")))
                    writeConn.send(req)
                    val ack = writeConn.recv()
                    if (ack.getStr("type") == "error") {
                        state.errors.add(ErrorPair(ack.getStr("code") ?: "", ack.getStr("message") ?: ""))
                    } else {
                        addAck(state, "write_stream")
                    }
                    continue
                }
                try {
                    driveStep(client, state, expect)
                } catch (e: Exception) {
                    recordError(state, e)
                }
            }
        } finally {
            if (writeConn != null) {
                writeConn.close()
            }
        }

        if (derr != null) {
            recordError(state, derr)
        }

        if (assertBlock.getBool("expects_unsupported")) {
            val verb = assertBlock.getStr("unsupported_verb")
            if (client.supports(verb)) {
                state.unsupportedObserved = false
            } else {
                try {
                    client.sandbox("box").exec("echo", arrayOf<String>())
                    state.unsupportedObserved = false
                } catch (e: NanosbError) {
                    state.unsupportedObserved = (e.code() == "unsupported")
                }
            }
        }

        val failures = evaluateAssert(assertBlock, state)
        if (!failures.isEmpty()) {
            return ScenarioResult(false, failures.joinToString("; "))
        }
        return ScenarioResult(true, "ok")
    } catch (e: Exception) {
        return ScenarioResult(false, "driver error: ${e.message}")
    } finally {
        if (proc != null) {
            proc.destroy()
        }
        if (!sock.isEmpty()) {
            try {
                Files.deleteIfExists(Path.of(sock))
            } catch (e: IOException) {
                // best-effort cleanup
            }
        }
    }
}

/** Locates the nanosb-conformance harness binary. */
private fun findBinary(): String {
    val env = System.getenv("NANOSB_CONFORMANCE_BIN")
    if (env != null && !env.isEmpty()) {
        return env
    }
    return "target/debug/nanosb-conformance"
}

fun main(args: Array<String>) {
    val binPath = findBinary()
    val scenariosDir = "sdk/conformance/scenarios"
    val files = ArrayList<String>()
    val entries = File(scenariosDir).listFiles() ?: arrayOf<File>()
    for (f in entries) {
        if (f.name.endsWith(".json")) {
            files.add(f.path)
        }
    }
    if (files.isEmpty()) {
        System.err.println("error: no scenarios found in $scenariosDir")
        System.exit(1)
    }
    files.sort()

    var passed = 0
    var failed = 0
    for (scenarioFile in files) {
        var sid: String? = null
        try {
            sid = KtJson.parse(File(scenarioFile).readText()).getStr("id")
        } catch (e: Exception) {
            failed++
            println("FAIL $scenarioFile: ${e.message}")
            continue
        }
        val r = runScenario(binPath, scenarioFile)
        if (r.ok) {
            passed++
            println("PASS $sid")
        } else {
            failed++
            println("FAIL $sid: ${r.message}")
        }
    }

    println()
    println("$passed passed, $failed failed")
    if (failed > 0) {
        System.exit(1)
    }
}