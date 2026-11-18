// Smoke test for the Kotlin SDK facade.
//
// Exercises the KtJson shim (round-trip) and the KotlinClient facade against a
// tiny scripted NDJSON helper on a temp Unix socket (hello handshake, exec,
// get-missing). Exits non-zero on any failed assertion.
//
// Usage (from the repository root, after building the jar):
//     java -cp sdk/kotlin/nanosandbox-kotlin.jar nanosandbox.kt.SmokeTestKt

package nanosandbox.kt

import nanosandbox.Json
import nanosandbox.NanosbError
import java.io.IOException
import java.net.StandardProtocolFamily
import java.net.UnixDomainSocketAddress
import java.nio.ByteBuffer
import java.nio.channels.ServerSocketChannel
import java.nio.channels.SocketChannel
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.Executors
import kotlin.time.Duration.Companion.seconds

private var failures = 0

private fun check(cond: Boolean, msg: String) {
    if (!cond) {
        failures++
        println("FAIL: $msg")
    }
}

private fun readLine(ch: SocketChannel): String {
    val sb = StringBuilder()
    val buf = ByteBuffer.allocate(1)
    while (true) {
        buf.clear()
        val n = ch.read(buf)
        if (n < 0) {
            throw IOException("eof")
        }
        val c = buf.get(0).toInt().toChar()
        if (c == '\n') {
            return sb.toString()
        }
        sb.append(c)
    }
}

private fun writeLine(ch: SocketChannel, line: String) {
    val buf = ByteBuffer.wrap((line + "\n").toByteArray(StandardCharsets.UTF_8))
    while (buf.hasRemaining()) {
        ch.write(buf)
    }
}

/** A tiny scripted NDJSON helper: hello, exec, get-missing. */
private fun serveFakeHelper(sock: Path) {
    try {
        val listener = ServerSocketChannel.open(StandardProtocolFamily.UNIX)
        try {
            listener.bind(UnixDomainSocketAddress.of(sock))
            while (true) {
                val ch = listener.accept()
                try {
                    readLine(ch) // hello handshake on every connection
                    writeLine(ch, "{\"type\":\"hello_ok\",\"protocol\":\"1.0.0\","
                            + "\"helper_version\":\"test\",\"capabilities\":[\"lifecycle\",\"exec\"]}")
                    while (true) {
                        val reqLine = try {
                            readLine(ch)
                        } catch (e: IOException) {
                            break
                        }
                        val req = KtJson.parse(reqLine)
                        val type = req.getStr("type")
                        if (type == "exec") {
                            writeLine(ch, "{\"type\":\"exec_result\",\"exit_code\":0,"
                                    + "\"stdout\":\"hi\\n\",\"stderr\":\"\",\"duration_ms\":1}")
                        } else if (type == "wait") {
                            writeLine(ch, "{\"type\":\"wait_result\",\"exit_code\":0,"
                                    + "\"exited_at_ms\":123,\"state\":\"stopped\"}")
                        } else if (type == "kill" || type == "restart") {
                            writeLine(ch, "{\"type\":\"ok\"}")
                        } else if (type == "get") {
                            writeLine(ch, "{\"type\":\"error\",\"code\":\"not_found\","
                                    + "\"message\":\"sandbox `missing` does not exist\"}")
                        } else {
                            throw IOException("unexpected request type $type")
                        }
                    }
                } finally {
                    ch.close()
                }
            }
        } finally {
            listener.close()
        }
    } catch (e: IOException) {
        // the test closes the listener by shutting down the executor
    }
}

fun main(args: Array<String>) {
    // 1. KtJson round-trip (the shim the facade relies on).
    val doc = KtJson.parse(
        "{\"name\":\"box\",\"state\":\"running\",\"pid\":4242,"
            + "\"cpu\":12.5,\"ok\":true,\"none\":null}")
    check(doc.getStr("name") == "box", "json name")
    check(doc.getStr("state") == "running", "json state")
    check(doc.getInt("pid") == 4242L, "json pid")
    check(doc.get("cpu").asNumber() == 12.5, "json cpu")
    check(doc.getBool("ok"), "json ok")
    check(doc.get("none").isNull(), "json null")
    check(!doc.has("missing"), "json missing key")
    val reparsed = KtJson.parse(doc.stringify())
    check(reparsed.getStr("name") == "box", "json round-trip name")
    check(reparsed.getInt("pid") == 4242L, "json round-trip pid")

    // 2. Facade against a fake NDJSON helper on a temp Unix socket.
    val sock = Files.createTempFile("nanosb-kotlin-test", ".sock")
    Files.delete(sock) // the listener binds the path itself
    val server = Executors.newSingleThreadExecutor()
    try {
        server.submit { serveFakeHelper(sock) }
        Thread.sleep(200L) // wait for the listener to be up

        val client = KotlinClient.connect(sock.toString())
        check(client.supports("lifecycle"), "supports lifecycle")
        check(client.supports("exec"), "supports exec")
        check(!client.supports("fs"), "does not support fs")

        val result = client.sandbox("box").exec("echo", arrayOf("hi"))
        check(result.getInt("exit_code") == 0L, "exec exit code")
        check(result.getStr("stdout") == "hi\n", "exec stdout")

        val box = client.sandbox("box")
        box.killWith(9)
        box.restart()
        val wr = box.wait(5.seconds)
        check(wr.exitCode == 0, "wait exit code")
        check(wr.exitedAtMs == 123L, "wait exitedAtMs")
        check(wr.state == "stopped", "wait state")
        check(box.waitUntilStopped(5.seconds) == "stopped", "waitUntilStopped state")

        var errCode = ""
        try {
            client.get("missing")
        } catch (e: NanosbError) {
            errCode = e.code()
        }
        check(errCode == "not_found", "get missing -> not_found")
    } finally {
        server.shutdownNow()
        Files.deleteIfExists(sock)
    }

    if (failures > 0) {
        println("$failures test(s) failed")
        System.exit(1)
    }
    println("all tests passed")
}