# nanosandbox — Java SDK

A **JDK-only** Java 21 protocol client for the nanosandbox helper
(`nanosb-protocol` v1.0.0). It speaks NDJSON over a Unix stream socket using
only the JDK (`java.net.UnixDomainSocketAddress`, `java.nio.channels.SocketChannel`,
`java.util.Base64`) — no third-party runtime dependencies. JUnit 5 is used for
tests only.

The client surface mirrors the Go SDK (`sdk/go/nanosandbox/`) and the driver
contract in `sdk/conformance/SPEC.md`.

## Requirements

- JDK 21+ (uses `UnixDomainSocketAddress`, `SocketChannel`, and the
  single-file source launcher).
- Maven 3.9+ for the test suite.

## Install

`ai.nanosandbox:nanosandbox-sdk` from Maven Central:

```xml
<dependency>
  <groupId>ai.nanosandbox</groupId>
  <artifactId>nanosandbox-sdk</artifactId>
  <version>0.3.0</version>
</dependency>
```

## Layout

```
sdk/java/
├── pom.xml                          # Java 21, JUnit 5 (test scope only)
├── README.md
├── src/main/java/nanosandbox/
│   ├── Json.java                    # minimal JSON value type + serializer
│   ├── JsonParser.java              # recursive-descent JSON parser
│   ├── NanosbError.java             # typed protocol error
│   ├── Connection.java              # Unix socket + NDJSON framing + hello handshake
│   ├── SandboxClient.java           # stateless client (dial-per-call)
│   ├── SandboxBuilder.java          # fluent create-spec builder
│   ├── Sandbox.java                 # lifecycle/exec/attach + verb surface
│   ├── SandboxFs.java               # fs/volumes/secrets/snapshots verbs
│   └── SandboxObs.java              # logs/metrics verbs
├── src/test/java/nanosandbox/
│   └── JsonTest.java                # unit tests + fake NDJSON helper
└── conformance/
    └── Run.java                     # conformance driver (single-file source launch)
```

## Quickstart (complete program)

Requires JDK 21+; the first run downloads the runtime bundle. Save as
`Demo.java` and run it with the SDK on the classpath:

```java
import nanosandbox.ManagedRuntime;
import nanosandbox.Sandbox;
import nanosandbox.SandboxBuilder;
import nanosandbox.SandboxClient;

public class Demo {
  public static void main(String[] args) throws Exception {
    try (ManagedRuntime rt = ManagedRuntime.start()) {
      SandboxClient client = SandboxClient.connect(rt.socket.toString());
      Sandbox sandbox = client.createWith(
          new SandboxBuilder("library/alpine:latest").withName("demo"));
      sandbox.start();

      System.out.print(sandbox.exec("echo", new String[]{"hello from a microVM"}).getStr("stdout"));

      sandbox.remove(true);
    }
  }
}
```

Expected output:

```text
hello from a microVM
```

## Usage

```java
import nanosandbox.*;

// Connect to the helper socket (performs the hello handshake).
SandboxClient client = SandboxClient.connect("/path/to/helper.sock");

// Capabilities advertised by the helper.
client.supports("exec"); // true | false

// Create a sandbox with the fluent builder.
SandboxBuilder builder = new SandboxBuilder("alpine:latest");
builder.withName("demo").withResources(2, 512).publishTcp(8080, 80).withLabel("team", "sdk");
Sandbox box = client.createWith(builder);

// Lifecycle.
box.start();
String state = box.state(); // "running"

// Buffered exec.
Json result = box.exec("echo", new String[]{"hi"});
// { exit_code: 0, stdout: "hi\n", stderr: "", duration_ms: 1 }

// Streamed exec: opening `ok`, then guest frames until `exit`/`error`.
int code = box.execStream(Json.object(), ev -> {
    if ("stdout".equals(ev.getStr("event"))) {
        System.out.print(ev.getStr("data"));
    }
});
// code === 0

// Filesystem.
box.fsWrite("/tmp/x", "hello".getBytes(), false);
byte[] data = box.fsRead("/tmp/x");
String text = box.fsReadToString("/etc/hostname");

// Logs (buffered and follow).
List<Json> lines = box.logsWith(10, 0, "stdout");
box.logsFollowWith(line -> System.out.print(line.getStr("data")), 10, 0, "stdout");

// Metrics.
Json m = box.metrics();
box.metricsStream(100, sample -> System.out.println(sample.get("cpu_percent").asNumber()));

// Volumes, secrets, snapshots.
Json vol = box.volumeCreate("data", "/host/volumes/data");
box.secretSet("API_KEY", "s3cr3t");
Json snap = box.snapshotCreate("");

// Attach: drive console input/resize and read guest events.
Sandbox.AttachSession session = box.attachSession();
session.sendInput("ls\n");
session.resize(120, 40);
Json ev;
while (!(ev = session.readEvents()).isNull()) {
    System.out.print(ev.getStr("data"));
}
session.close();

// Typed errors.
try {
    client.get("missing");
} catch (NanosbError e) {
    e.code();       // "not_found"
    e.isNotFound(); // true
}
```

Every call dials a fresh connection and re-negotiates the `hello` handshake
(stateless, mirroring the Python and Go SDKs). Streaming verbs (`exec_stream`,
`attach`, `logs` with `follow`, `metrics_stream`, `fs_write_stream`) hold one
connection for the duration of the stream: an opening `ok` ack, then frames
until the terminal frame (`guest exit/error` or `event done`).

## Managing a real sandbox (`ManagedRuntime`)

`ManagedRuntime.start()` locates and starts the real `nanosb-runtime` helper so
you can boot and manage microVMs without launching it yourself:

```java
try (ManagedRuntime rt = ManagedRuntime.start()) {
    SandboxClient client = SandboxClient.connect(rt.socket.toString());
    Sandbox box = client.createWith(new SandboxBuilder("library/alpine:latest").withName("demo"));
    System.out.println(box.exec("echo", new String[]{"hello from a microVM"}).getStr("stdout"));
}
```

The helper is found via `NANOSB_RUNTIME_BIN` → `$NANOSB_HOME/bin/nanosb-runtime`
→ `PATH` → the repo `target/debug/nanosb-runtime`.

If the helper is missing, `start()` auto-provisions it by running the
`ensure-runtime.sh` provisioner (resolved from `NANOSB_PROVISIONER`, then the
repo's `scripts/sdk/ensure-runtime.sh`, then an embedded copy shipped with the
SDK), which downloads + verifies + installs the runtime under `$NANOSB_HOME`,
pinned to the SDK's release version. Config env: `NANOSB_HOME`, `NANOSB_VERSION`
(required to download), `NANOSB_RUNTIME_URL`, `NANOSB_OFFLINE`,
`NANOSB_RUNTIME_SHA256`, and `NANOSB_PROVISIONER`.

## Testing

Unit tests exercise the client against an in-process fake NDJSON helper on a
temp Unix socket:

```bash
mvn -q -f sdk/java/pom.xml test
```

Conformance driver — runs all 40 scenarios in `sdk/conformance/scenarios/`
against the `nanosb-conformance` harness:

```bash
mvn -q -f sdk/java/pom.xml compile
java -cp sdk/java/target/classes sdk/java/conformance/Run.java
# NANOSB_CONFORMANCE_BIN=/path/to/nanosb-conformance \
#   java -cp sdk/java/target/classes sdk/java/conformance/Run.java
```

Run from the repository root (the driver resolves `sdk/conformance/scenarios`
and `target/debug/nanosb-conformance` relative to the working directory).
Expected output: `40 passed, 0 failed`, exit code 0.

Live microVM E2E (opt-in; needs the helper + libkrun/gvproxy + a cached image):

```bash
NANOSB_E2E=1 mvn -f sdk/java/pom.xml test -Dtest=E2ETest
```