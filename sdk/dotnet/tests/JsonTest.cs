using System.Collections.Generic;
using System;
using System.Net.Sockets;
using System.Threading;

[TestClass]
public class JsonTest {
    [TestMethod]
    public void Test_Stringify_when_building_request() {
        JsonValue req = JsonValue.makeObject();
        req.put("type", JsonValue.of("ping"));
        req.put("n", JsonValue.of(42L));
        req.put("f", JsonValue.of(1.5));
        req.put("flag", JsonValue.of(true));
        req.put("nil", JsonValue.ofNull());
        JsonValue arr = JsonValue.makeArray();
        arr.add(JsonValue.of("a"));
        arr.add(JsonValue.of(2L));
        req.put("list", arr);
        Assert.AreEqual("{\"type\":\"ping\",\"n\":42,\"f\":1.5,\"flag\":true,\"nil\":null,\"list\":[\"a\",2]}", req.stringify());
    }

    [TestMethod]
    public void Test_Parse_when_reading_response() {
        JsonValue parsed = JsonValue.parse("{\"a\":1,\"b\":[true,null,\"x\"],\"c\":{\"d\":2.5}}");
        Assert.AreEqual(1L, parsed.intVal("a"));
        Assert.AreEqual(true, parsed.get("b").getAt(0).boolValue);
        Assert.AreEqual(true, parsed.get("b").getAt(1).isNull());
        Assert.AreEqual("x", parsed.get("b").getAt(2).stringValue);
        Assert.AreEqual(2.5, parsed.get("c").num("d"));
    }

    [TestMethod]
    public void Test_Stringify_when_escaping() {
        JsonValue s = JsonValue.of("line1\nline2 \"quoted\" \\ back");
        Assert.AreEqual("\"line1\\nline2 \\\"quoted\\\" \\\\ back\"", s.stringify());
    }

[TestMethod]
    public void Test_Wait_returns_WaitResult_and_sends_timeout() {
        using (FakeHelper h = new FakeHelper()) {
            h.pushScript("{\"type\":\"wait\",\"exit_code\":137,\"exited_at_ms\":1234,\"state\":\"stopped\"}");
            SandboxClient client = SandboxClient.connect(h.Path());
            WaitResult r = client.sandbox("box").wait(TimeSpan.FromSeconds(30));
            Assert.AreEqual(137, r.ExitCode);
            Assert.AreEqual(1234UL, r.ExitedAtMs);
            Assert.AreEqual("stopped", r.State);
            Assert.IsTrue(h.requests()[0].Contains("\"type\":\"wait\""));
            Assert.IsTrue(h.requests()[0].Contains("\"timeout_secs\":30"));
        }
    }

    [TestMethod]
    public void Test_Wait_omits_timeout_when_null() {
        using (FakeHelper h = new FakeHelper()) {
            h.pushScript("{\"type\":\"wait\",\"exit_code\":0,\"exited_at_ms\":5,\"state\":\"stopped\"}");
            SandboxClient client = SandboxClient.connect(h.Path());
            client.sandbox("box").wait(null);
            Assert.IsFalse(h.requests()[0].Contains("timeout_secs"));
        }
    }

    [TestMethod]
    public void Test_WaitResult_decoding() {
        JsonValue resp = JsonValue.parse("{\"type\":\"wait\",\"exit_code\":137,\"exited_at_ms\":1234,\"state\":\"stopped\"}");
        WaitResult r = new WaitResult((int) resp.intVal("exit_code"), (ulong) resp.intVal("exited_at_ms"), resp.str("state"));
        Assert.AreEqual(137, r.ExitCode);
        Assert.AreEqual(1234UL, r.ExitedAtMs);
        Assert.AreEqual("stopped", r.State);
    }

    [TestMethod]
    public void Test_KillWith_sends_signal() {
        using (FakeHelper h = new FakeHelper()) {
            h.pushScript("{\"type\":\"ok\",\"version\":\"1\"}");
            SandboxClient client = SandboxClient.connect(h.Path());
            client.sandbox("box").killWith(15);
            Assert.IsTrue(h.requests()[0].Contains("\"type\":\"kill\""));
            Assert.IsTrue(h.requests()[0].Contains("\"signal\":15"));
        }
    }

    [TestMethod]
    public void Test_Restart_sends_restart() {
        using (FakeHelper h = new FakeHelper()) {
            h.pushScript("{\"type\":\"ok\",\"version\":\"1\"}");
            SandboxClient client = SandboxClient.connect(h.Path());
            client.sandbox("box").restart();
            Assert.IsTrue(h.requests()[0].Contains("\"type\":\"restart\""));
        }
    }

    [TestMethod]
    public void Test_Wait_error_throws() {
        using (FakeHelper h = new FakeHelper()) {
            h.pushScript("{\"type\":\"error\",\"code\":\"not_found\",\"message\":\"no such sandbox\"}");
            SandboxClient client = SandboxClient.connect(h.Path());
            try {
                client.sandbox("box").wait(TimeSpan.FromSeconds(5));
                Assert.Fail("expected NanosbError");
            } catch (NanosbError e) {
                Assert.AreEqual("not_found", e.errorCode);
            }
        }
    }
}

/// A minimal fake helper: accepts one connection per call, answers the hello
/// handshake, and replies to each request with the next scripted frame.
class FakeHelper : IDisposable {
    private Socket listener;
    private String path;
    private Queue<String> responses = new Queue<String>();
    private List<String> requestList = new List<String>();
    private Thread thread;
    private volatile bool running = true;

    public FakeHelper() {
        path = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "nsb-" + Guid.NewGuid().ToString("N") + ".sock");
        listener = new Socket(AddressFamily.Unix, SocketType.Stream, ProtocolType.Unspecified);
        listener.Bind(new UnixDomainSocketEndPoint(path));
        listener.Listen(16);
        thread = new Thread(AcceptLoop);
        thread.IsBackground = true;
        thread.Start();
    }

    public String Path() { return path; }

    public void pushScript(String frame) {
        lock (responses) { responses.Enqueue(frame); }
    }

    public List<String> requests() {
        lock (requestList) { return new List<String>(requestList); }
    }

    private void AcceptLoop() {
        while (running) {
            if (!listener.Poll(100000, SelectMode.SelectRead)) {
                continue;
            }
            Socket conn;
            try { conn = listener.Accept(); } catch { break; }
            Thread t = new Thread(() => Handle(conn));
            t.IsBackground = true;
            t.Start();
        }
    }

    private void Handle(Socket conn) {
        try {
            ReadLine(conn); // hello
            Send(conn, "{\"type\":\"hello_ok\",\"protocol\":\"1.0.0\",\"capabilities\":[\"exec\",\"lifecycle\",\"wait\",\"restart\"]}");
            String line = ReadLine(conn);
            if (line == null) return;
            lock (requestList) { requestList.Add(line); }
            String resp;
            lock (responses) {
                if (responses.Count == 0) return;
                resp = responses.Dequeue();
            }
            Send(conn, resp);
        } catch { } finally {
            try { conn.Close(); } catch { }
        }
    }

    private static void Send(Socket conn, String line) {
        byte[] bytes = System.Text.Encoding.UTF8.GetBytes(line + "\n");
        conn.Send(bytes);
    }

    private static String ReadLine(Socket conn) {
        var buf = new byte[4096];
        var sb = new System.Text.StringBuilder();
        while (true) {
            int n = conn.Receive(buf);
            if (n <= 0) return null;
            for (int i = 0; i < n; i++) {
                if (buf[i] == (byte) '\n') return sb.ToString();
                sb.Append((char) buf[i]);
            }
        }
    }

    public void Dispose() {
        running = false;
        try { listener.Close(); } catch { }
        try { System.IO.File.Delete(path); } catch { }
    }
}