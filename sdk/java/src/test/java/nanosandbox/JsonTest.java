package nanosandbox;

import org.junit.jupiter.api.Test;

import java.io.IOException;
import java.net.StandardProtocolFamily;
import java.net.UnixDomainSocketAddress;
import java.nio.ByteBuffer;
import java.nio.channels.ServerSocketChannel;
import java.nio.channels.SocketChannel;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** Unit tests for the JSON value type and the NDJSON connection. */
class JsonTest {

    @Test
    void roundTripsNestedDocument() {
        Json doc = Json.parse(
                "{\"name\":\"box\",\"state\":\"running\",\"pid\":4242,"
                        + "\"labels\":{\"team\":\"sdk\"},\"ports\":[8080,80],"
                        + "\"cpu\":12.5,\"ok\":true,\"none\":null}");
        assertEquals("box", doc.getStr("name"));
        assertEquals("running", doc.getStr("state"));
        assertEquals(4242, doc.getInt("pid"));
        assertEquals("sdk", doc.get("labels").getStr("team"));
        assertEquals(8080, doc.getArr("ports").get(0).asNumber());
        assertEquals(12.5, doc.get("cpu").asNumber());
        assertTrue(doc.getBool("ok"));
        assertTrue(doc.get("none").isNull());
        assertFalse(doc.has("missing"));

        Json reparsed = Json.parse(doc.stringify());
        assertEquals("box", reparsed.getStr("name"));
        assertEquals(4242, reparsed.getInt("pid"));
        assertEquals(12.5, reparsed.get("cpu").asNumber());
    }

    @Test
    void parsesEscapedStrings() {
        Json doc = Json.parse("{\"script\":\"echo one\\necho two\",\"q\":\"a\\\"b\\\\c\"}");
        assertEquals("echo one\necho two", doc.getStr("script"));
        assertEquals("a\"b\\c", doc.getStr("q"));
        // the serializer must re-escape the newline so the round trip holds
        assertEquals("echo one\necho two", Json.parse(doc.stringify()).getStr("script"));
    }

    @Test
    void serializesIntegersWithoutDecimalPoint() {
        Json req = Json.object();
        req.set("type", Json.ofString("exec"));
        req.set("cols", Json.ofNumber(120));
        req.set("ratio", Json.ofNumber(1.5));
        String out = req.stringify();
        assertTrue(out.contains("\"cols\":120"), out);
        assertTrue(out.contains("\"ratio\":1.5"), out);
    }

    @Test
    void typedErrorCarriesCodeAndMessage() {
        NanosbError err = new NanosbError("not_found", "sandbox `missing` does not exist");
        assertEquals("not_found", err.code());
        assertEquals("sandbox `missing` does not exist", err.message());
        assertTrue(err.isNotFound());
        assertTrue(err.getMessage().startsWith("not_found: "));
    }

    @Test
    void connectAndCallAgainstFakeHelper() throws Exception {
        Path sock = Files.createTempFile("nanosb-java-test", ".sock");
        Files.delete(sock); // the listener binds the path itself

        ExecutorService server = Executors.newSingleThreadExecutor();
        try {
            server.submit(() -> serveFakeHelper(sock));
            // wait for the listener to be up
            Thread.sleep(200);

            SandboxClient client = SandboxClient.connect(sock.toString());
            assertTrue(client.supports("lifecycle"));
            assertTrue(client.supports("exec"));
            assertFalse(client.supports("fs"));

            Json result = client.sandbox("box").exec("echo", new String[]{"hi"});
            assertEquals(0, result.getInt("exit_code"));
            assertEquals("hi\n", result.getStr("stdout"));

            NanosbError err = assertThrows(NanosbError.class, () -> client.get("missing"));
            assertEquals("not_found", err.code());
        } finally {
            server.shutdownNow();
            Files.deleteIfExists(sock);
        }
    }

    @Test
    void waitKillWithAndRestartFrames() throws Exception {
        Path sock = Files.createTempFile("nanosb-java-lifecycle", ".sock");
        Files.delete(sock);

        ExecutorService server = Executors.newSingleThreadExecutor();
        try {
            server.submit(() -> serveLifecycleHelper(sock));
            Thread.sleep(200);

            SandboxClient client = SandboxClient.connect(sock.toString());
            Sandbox box = client.sandbox("box");

            // wait frame shape + WaitResult decoding
            Sandbox.WaitResult wr = box.wait(java.time.Duration.ofSeconds(30));
            assertEquals(137, wr.exitCode());
            assertEquals(1234L, wr.exitedAtMs());
            assertEquals("stopped", wr.state());

            // waitUntilStopped delegates to wait
            assertEquals("stopped", box.waitUntilStopped(java.time.Duration.ofSeconds(30)));

            // killWith(15) frame contains "signal":15
            box.killWith(15);

            // restart frame contains "type":"restart"
            box.restart();

            // an error response throws
            NanosbError err = assertThrows(NanosbError.class, () -> box.wait(java.time.Duration.ofSeconds(1)));
            assertEquals("not_found", err.code());
        } finally {
            server.shutdownNow();
            Files.deleteIfExists(sock);
        }
    }

    /** A tiny scripted NDJSON helper: hello, exec, get-missing. */
    private static void serveFakeHelper(Path sock) {
        try (ServerSocketChannel listener = ServerSocketChannel.open(StandardProtocolFamily.UNIX)) {
            listener.bind(UnixDomainSocketAddress.of(sock));
            while (true) {
                try (SocketChannel ch = listener.accept()) {
                    // hello handshake on every connection
                    String hello = readLine(ch);
                    assertEquals("hello", Json.parse(hello).getStr("type"));
                    writeLine(ch, "{\"type\":\"hello_ok\",\"protocol\":\"1.0.0\","
                            + "\"helper_version\":\"test\",\"capabilities\":[\"lifecycle\",\"exec\"]}");

                    // serve requests until the client closes
                    while (true) {
                        String reqLine;
                        try {
                            reqLine = readLine(ch);
                        } catch (IOException eof) {
                            break;
                        }
                        Json req = Json.parse(reqLine);
                        String type = req.getStr("type");
                        if ("exec".equals(type)) {
                            assertEquals("box", req.getStr("name"));
                            assertEquals("echo", req.getStr("command"));
                            writeLine(ch, "{\"type\":\"exec_result\",\"exit_code\":0,"
                                    + "\"stdout\":\"hi\\n\",\"stderr\":\"\",\"duration_ms\":1}");
                        } else if ("get".equals(type)) {
                            writeLine(ch, "{\"type\":\"error\",\"code\":\"not_found\","
                                    + "\"message\":\"sandbox `missing` does not exist\"}");
                        } else {
                            throw new IOException("unexpected request type " + type);
                        }
                    }
                }
            }
        } catch (IOException e) {
            // the test closes the listener by shutting down the executor
        }
    }

    /** A scripted NDJSON helper for wait/killWith/restart frames. */
    private static void serveLifecycleHelper(Path sock) {
        try (ServerSocketChannel listener = ServerSocketChannel.open(StandardProtocolFamily.UNIX)) {
            listener.bind(UnixDomainSocketAddress.of(sock));
            while (true) {
                try (SocketChannel ch = listener.accept()) {
                    String hello = readLine(ch);
                    assertEquals("hello", Json.parse(hello).getStr("type"));
                    writeLine(ch, "{\"type\":\"hello_ok\",\"protocol\":\"1.0.0\","
                            + "\"helper_version\":\"test\",\"capabilities\":[\"lifecycle\",\"wait\"]}");

                    while (true) {
                        String reqLine;
                        try {
                            reqLine = readLine(ch);
                        } catch (IOException eof) {
                            break;
                        }
                        Json req = Json.parse(reqLine);
                        String type = req.getStr("type");
                        if ("wait".equals(type)) {
                            assertEquals("box", req.getStr("name"));
                            if (req.getInt("timeout_secs") == 30) {
                                writeLine(ch, "{\"type\":\"wait\",\"exit_code\":137,"
                                        + "\"exited_at_ms\":1234,\"state\":\"stopped\"}");
                            } else {
                                writeLine(ch, "{\"type\":\"error\",\"code\":\"not_found\","
                                        + "\"message\":\"sandbox `box` does not exist\"}");
                            }
                        } else if ("kill".equals(type)) {
                            assertEquals("box", req.getStr("name"));
                            assertEquals(15, req.getInt("signal"));
                            writeLine(ch, "{\"type\":\"ok\",\"version\":\"1\"}");
                        } else if ("restart".equals(type)) {
                            assertEquals("box", req.getStr("name"));
                            writeLine(ch, "{\"type\":\"ok\",\"version\":\"1\"}");
                        } else if ("get".equals(type)) {
                            writeLine(ch, "{\"type\":\"error\",\"code\":\"not_found\","
                                    + "\"message\":\"sandbox `missing` does not exist\"}");
                        } else {
                            throw new IOException("unexpected request type " + type);
                        }
                    }
                }
            }
        } catch (IOException e) {
            // the test closes the listener by shutting down the executor
        }
    }

    private static String readLine(SocketChannel ch) throws IOException {
        StringBuilder sb = new StringBuilder();
        ByteBuffer buf = ByteBuffer.allocate(1);
        while (true) {
            buf.clear();
            int n = ch.read(buf);
            if (n < 0) {
                throw new IOException("eof");
            }
            char c = (char) buf.get(0);
            if (c == '\n') {
                return sb.toString();
            }
            sb.append(c);
        }
    }

    private static void writeLine(SocketChannel ch, String line) throws IOException {
        ByteBuffer buf = ByteBuffer.wrap((line + "\n").getBytes(StandardCharsets.UTF_8));
        while (buf.hasRemaining()) {
            ch.write(buf);
        }
    }
}