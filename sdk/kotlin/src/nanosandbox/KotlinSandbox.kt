package nanosandbox.kt

import nanosandbox.Json
import nanosandbox.Sandbox
import java.util.function.Consumer
import kotlin.time.Duration

/**
 * Kotlin facade over the Java nanosandbox SDK's sandbox handle. A handle bound
 * to one sandbox: lifecycle, execution, filesystem, logs, metrics, volumes,
 * secrets, and snapshot verbs. Every non-streaming call dials a fresh
 * connection; streaming verbs hold one connection for the duration of the
 * stream (an opening `ok` ack, then frames until the terminal frame).
 *
 * Callbacks are Kotlin function types; the facade adapts them to the Java
 * SDK's {@link Consumer} parameters.
 */
class KotlinSandbox {

    private val delegate: Sandbox

    constructor(delegate: Sandbox) {
        this.delegate = delegate
    }

    /** Returns the sandbox name. */
    fun name(): String = delegate.getName()

    // ---- lifecycle -------------------------------------------------------

    /** Returns the sandbox lifecycle state. */
    fun state(): String = delegate.state()

    /** Starts the sandbox. */
    fun start() {
        delegate.start()
    }

    /** Stops the sandbox. */
    fun stop(force: Boolean) {
        delegate.stop(force)
    }

    /** Kills the sandbox. */
    fun kill() {
        delegate.kill()
    }

    /** Kills the sandbox with a signal selector (SIGTERM=15, SIGKILL=9). */
    fun killWith(signal: Int) {
        delegate.killWith(signal)
    }

    /** Re-boots the sandbox in place. */
    fun restart() {
        delegate.restart()
    }

    /**
     * Blocks until the sandbox reaches a terminal state, returning the exit
     * code, exit timestamp, and state. {@code timeout} bounds the wait; a null
     * value waits indefinitely.
     */
    fun wait(timeout: Duration?): WaitResult {
        val jt = timeout?.let { java.time.Duration.ofNanos(it.inWholeNanoseconds) }
        val r = delegate.wait(jt)
        return WaitResult(r.exitCode(), r.exitedAtMs(), r.state())
    }

    /** Blocks until the sandbox stops, returning the terminal state. */
    fun waitUntilStopped(timeout: Duration): String {
        val jt = java.time.Duration.ofNanos(timeout.inWholeNanoseconds)
        return delegate.waitUntilStopped(jt)
    }

    /** Removes the sandbox. */
    fun remove(force: Boolean) {
        delegate.remove(force)
    }

    /** Pauses the sandbox. */
    fun pause() {
        delegate.pause()
    }

    /** Resumes the sandbox. */
    fun resume() {
        delegate.resume()
    }

    /** Refreshes the sandbox's idle timer. */
    fun touch() {
        delegate.touch()
    }

    /** Drains the sandbox. */
    fun drain() {
        delegate.drain()
    }

    /** Applies a modification spec to the live sandbox. */
    fun modify(spec: Json) {
        delegate.modify(spec)
    }

    // ---- execution -------------------------------------------------------

    /** Runs a buffered command and returns the exec result. */
    fun exec(command: String?, args: Array<String>): Json = delegate.exec(command, args)

    /** Runs a buffered command from a request object. */
    fun execWith(request: Json): Json = delegate.execWith(request)

    /** Runs a shell script and returns the buffered result. */
    fun shell(script: String?): Json = delegate.shell(script)

    /** Streams a command's output to {@code onEvent} until the exit event. */
    fun execStream(request: Json, onEvent: (Json) -> Unit): Int =
        delegate.execStream(request, Consumer { ev -> onEvent(ev) })

    /** Attaches to the sandbox console, delivering guest events to {@code onEvent}. */
    fun attach(onEvent: (Json) -> Unit) {
        delegate.attach(Consumer { ev -> onEvent(ev) })
    }

    /** Opens a live attach session for driving console input/resize. */
    fun attachSession(): KotlinAttachSession = KotlinAttachSession(delegate.attachSession())

    // ---- observability ---------------------------------------------------

    /** Returns a resource usage snapshot. */
    fun metrics(): Json = delegate.metrics()

    /** Streams metrics samples to {@code onEvent} until the done event. */
    fun metricsStream(intervalMs: Int, onEvent: (Json) -> Unit) {
        delegate.metricsStream(intervalMs, Consumer { ev -> onEvent(ev) })
    }

    /** Streams log lines to {@code onLine} until the done event. */
    fun logs(tail: Int): List<Json> = delegate.logs(tail)

    fun logsFollowWith(onLine: (Json) -> Unit, tail: Int, cursor: Int, source: String?) {
        delegate.logsFollowWith(Consumer { line -> onLine(line) }, tail, cursor, source)
    }

    // ---- filesystem / storage / secrets / snapshots ----------------------

    /** Reads bytes from a guest file. */
    fun fsRead(path: String?): ByteArray = delegate.fsRead(path)

    /** Reads a guest file as UTF-8 text. */
    fun fsReadToString(path: String?): String = delegate.fsReadToString(path)

    /** Writes bytes to a guest file. */
    fun fsWrite(path: String?, data: ByteArray, append: Boolean) {
        delegate.fsWrite(path, data, append)
    }

    /** Lists a guest directory. */
    fun fsList(path: String?): List<Json> = delegate.fsList(path)

    /** Stats a guest path. */
    fun fsStat(path: String?): Json = delegate.fsStat(path)

    /** Reports whether a guest path exists. */
    fun fsExists(path: String?): Boolean = delegate.fsExists(path)

    /** Creates a guest directory. */
    fun fsMkdir(path: String?, recursive: Boolean) {
        delegate.fsMkdir(path, recursive)
    }

    /** Removes a guest file. */
    fun fsRemove(path: String?) {
        delegate.fsRemove(path)
    }

    /** Removes a guest directory. */
    fun fsRemoveDir(path: String?, recursive: Boolean) {
        delegate.fsRemoveDir(path, recursive)
    }

    /** Copies a file inside the guest. */
    fun fsCopy(from: String?, to: String?) {
        delegate.fsCopy(from, to)
    }

    /** Renames a file inside the guest. */
    fun fsRename(from: String?, to: String?) {
        delegate.fsRename(from, to)
    }

    /** Copies a host file into the guest. */
    fun fsCopyFromHost(hostPath: String?, guestPath: String?) {
        delegate.fsCopyFromHost(hostPath, guestPath)
    }

    /** Copies a guest file to the host. */
    fun fsCopyToHost(guestPath: String?, hostPath: String?) {
        delegate.fsCopyToHost(guestPath, hostPath)
    }

    /** Creates a named volume. */
    fun volumeCreate(volumeName: String?, dir: String?): Json = delegate.volumeCreate(volumeName, dir)

    /** Lists the sandbox's volumes. */
    fun volumeList(): List<Json> = delegate.volumeList()

    /** Fetches one volume's info. */
    fun volumeGet(volume: String?): Json = delegate.volumeGet(volume)

    /** Removes a named volume. */
    fun volumeRemove(volume: String?) {
        delegate.volumeRemove(volume)
    }

    /** Mounts a volume into the running sandbox. */
    fun mountAdd(mount: Json) {
        delegate.mountAdd(mount)
    }

    /** Sets a named secret. */
    fun secretSet(key: String?, value: String?) {
        delegate.secretSet(key, value)
    }

    /** Lists the sandbox's secrets (values redacted). */
    fun secretList(): List<Json> = delegate.secretList()

    /** Removes a named secret. */
    fun secretRemove(key: String?) {
        delegate.secretRemove(key)
    }

    /** Creates a snapshot of the sandbox. */
    fun snapshotCreate(dest: String?): Json = delegate.snapshotCreate(dest)

    /** Restores a sandbox from a snapshot. */
    fun snapshotRestore(snapshot: String?, snapshotName: String?) {
        delegate.snapshotRestore(snapshot, snapshotName)
    }

    /** Forks a new sandbox from a snapshot. */
    fun snapshotFork(snapshot: String?, snapshotName: String?) {
        delegate.snapshotFork(snapshot, snapshotName)
    }

    /** Removes a snapshot. */
    fun snapshotRemove(snapshot: String?) {
        delegate.snapshotRemove(snapshot)
    }
}

/**
 * The result of a blocking {@link KotlinSandbox#wait} call: the sandbox's exit
 * code, the exit timestamp (epoch millis), and the terminal state.
 */
data class WaitResult(val exitCode: Int, val exitedAtMs: Long, val state: String)

/**
 * A live attach session: drive console input/resize and read guest events on
 * the same connection. Facade over the Java SDK's {@link Sandbox.AttachSession}.
 */
class KotlinAttachSession(private val delegate: Sandbox.AttachSession) {

    /** Writes bytes to the guest console. */
    fun sendInput(data: String?) {
        delegate.sendInput(data)
    }

    /** Resizes the guest console. */
    fun resize(cols: Int, rows: Int) {
        delegate.resize(cols, rows)
    }

    /** Returns the next guest event, or a null value at stream end. */
    fun readEvents(): Json = delegate.readEvents()

    /** Closes the attach session connection. */
    fun close() {
        delegate.close()
    }
}
