# nanosandbox — Kotlin SDK

A **Kotlin** protocol client for the nanosandbox helper (`nanosb-protocol`
v1.0.0). It is a Kotlin facade over the Java SDK (`sdk/java`): the Java SDK
speaks NDJSON over a Unix stream socket using only the JDK, and the Kotlin
facade (`nanosandbox.kt`) re-exposes its surface with Kotlin-idiomatic naming
and nullable parameters. The conformance driver mirrors
`sdk/java/conformance/Run.java` and the driver contract in
`sdk/conformance/SPEC.md`.

## Requirements

- JDK 21+ (for the runtime).
- Gradle — the committed `./gradlew` wrapper (Gradle 9.8.2) downloads it.
- Kotlin compiler 2.x (`kotlinc`) is only needed for the `build.sh` fallback.

## Install

`ai.nanosandbox:nanosandbox-sdk-kotlin` from Maven Central:

```kotlin
implementation("ai.nanosandbox:nanosandbox-sdk-kotlin:0.3.0")
```

## Layout

```
sdk/kotlin/
├── build.gradle.kts                  # Gradle build + maven-publish (canonical)
├── settings.gradle.kts
├── gradlew, gradle/wrapper/          # Gradle wrapper
├── build.sh                          # no-Gradle fallback: javac + kotlinc -> one jar
├── README.md
├── src/nanosandbox/
│   ├── KtJson.java                   # Java shim: Json factories under Kotlin-resolvable names
│   ├── KotlinClient.kt               # Kotlin facade: stateless client (dial-per-call)
│   └── KotlinSandbox.kt              # Kotlin facade: sandbox handle (lifecycle/exec/fs/obs)
├── src/test/nanosandbox/
│   └── SmokeTest.kt                  # smoke test: KtJson round-trip + facade vs a fake NDJSON helper
└── conformance/
    └── Run.kt                        # conformance driver (runs all 35 scenarios)
```

### Why `KtJson.java`?

Kotlin cannot call `Json.object()` directly: a Java static method named
`object` breaks the Kotlin compiler's companion-object synthesis for the whole
class (kotlinc 2.4.21), so every static factory on `Json` becomes unresolvable
from Kotlin. `KtJson` re-exposes the factories (`obj`, `arr`, `str`, `bool`,
`num`, `nul`, `parse`) under names Kotlin can resolve; the facade and driver
use those. `Json` is still used as a type throughout.

## Usage

```kotlin
import nanosandbox.kt.KotlinClient
import nanosandbox.kt.KotlinSandbox

// Connect to the helper socket (performs the hello handshake).
val client = KotlinClient.connect("/path/to/helper.sock")

// Capabilities advertised by the helper.
client.supports("exec") // true | false

// Create a sandbox from a spec object (build it with the Java SDK's Json).
val spec = KtJson.obj()
spec.set("image", KtJson.str("alpine:latest"))
spec.set("name", KtJson.str("demo"))
val box = client.create(spec)

// Lifecycle.
box.start()
val state = box.state() // "running"

// Buffered exec.
val result = box.exec("echo", arrayOf("hi"))
// { exit_code: 0, stdout: "hi\n", stderr: "", duration_ms: 1 }

// Streamed exec: opening `ok`, then guest frames until `exit`/`error`.
val code = box.execStream(KtJson.obj()) { ev ->
    if (ev.getStr("event") == "stdout") {
        print(ev.getStr("data"))
    }
}

// Filesystem.
box.fsWrite("/tmp/x", "hello".toByteArray(), false)
val data = box.fsRead("/tmp/x")
val text = box.fsReadToString("/etc/hostname")

// Logs (follow) and metrics (stream).
box.logsFollowWith({ line -> print(line.getStr("data")) }, 10, 0, "stdout")
box.metricsStream(100) { sample -> println(sample.get("cpu_percent").asNumber()) }

// Volumes, secrets, snapshots.
val vol = box.volumeCreate("data", "/host/volumes/data")
box.secretSet("API_KEY", "s3cr3t")
val snap = box.snapshotCreate("")

// Typed errors.
try {
    client.get("missing")
} catch (e: NanosbError) {
    e.code()       // "not_found"
    e.isNotFound() // true
}
```

Every call dials a fresh connection and re-negotiates the `hello` handshake
(stateless, mirroring the Java/Python/Go SDKs). Streaming verbs
(`exec_stream`, `attach`, `logs` with `follow`, `metrics_stream`,
`fs_write_stream`) hold one connection for the duration of the stream: an
opening `ok` ack, then frames until the terminal frame (`guest exit/error` or
`event done`).

## Managing a real sandbox (`ManagedRuntime`)

`nanosandbox.ManagedRuntime` (from the Java SDK) locates and starts the real
`nanosb-runtime` helper so you can boot and manage microVMs without launching it
yourself:

```kotlin
val rt = nanosandbox.ManagedRuntime.start()
try {
    val client = KotlinClient.connect(rt.socket.toString())
    val spec = KtJson.obj()
    spec.set("image", KtJson.str("library/alpine:latest"))
    spec.set("name", KtJson.str("demo"))
    val box = client.create(spec)
    println(box.exec("echo", arrayOf("hello from a microVM")).getStr("stdout"))
} finally {
    rt.close()
}
```

The helper is found via `NANOSB_RUNTIME_BIN` → `$NANOSB_HOME/bin/nanosb-runtime`
→ `PATH` → the repo `target/debug/nanosb-runtime`.

If the helper is missing, `ManagedRuntime` auto-provisions it: it runs the shared
`ensure-runtime.sh` provisioner (resolved from `NANOSB_PROVISIONER`, then the
repo `scripts/sdk/ensure-runtime.sh`, then an embedded copy shipped with the
Java SDK the Kotlin facade reuses), which downloads + verifies + installs the
runtime into `$NANOSB_HOME`, pinned to the SDK's release version. Config comes
from the environment: `NANOSB_HOME`, `NANOSB_VERSION` (required to download),
`NANOSB_RUNTIME_URL`, `NANOSB_RUNTIME_SHA256`, `NANOSB_OFFLINE=1` to never
download, and `NANOSB_PROVISIONER`.

## Building

The canonical build is Gradle (wrapper committed):

```bash
./gradlew -p sdk/kotlin jar                # -> sdk/kotlin/build/libs/nanosandbox-sdk-kotlin-0.3.0.jar
./gradlew -p sdk/kotlin publishToMavenLocal # installs ai.nanosandbox:nanosandbox-sdk-kotlin:0.3.0
```

The jar is self-contained: the Kotlin facade, the `KtJson` Java shim, and the
Java SDK classes (`sdk/java`) in one artifact. `bash sdk/kotlin/build.sh`
remains as a no-Gradle fallback that produces `sdk/kotlin/nanosandbox-sdk-kotlin.jar`.

## Testing

Smoke test — exercises the `KtJson` shim and the facade against an in-process
fake NDJSON helper on a temp Unix socket:

```bash
./gradlew -p sdk/kotlin smokeTest
# expected: all tests passed, exit code 0
```

Conformance driver — runs all 35 scenarios in `sdk/conformance/scenarios/`
against the `nanosb-conformance` harness:

```bash
cargo build -p nanosb-conformance     # the harness
./gradlew -p sdk/kotlin conformance   # runs the driver from the repository root
```

`conformance` runs from the repository root (the driver resolves
`sdk/conformance/scenarios` and `target/debug/nanosb-conformance`); override the
harness path with `NANOSB_CONFORMANCE_BIN`. Expected output: `35 passed, 0
failed`, exit code 0.

Live microVM E2E (opt-in; needs the helper + libkrun/gvproxy + a cached image):

```bash
NANOSB_E2E=1 java -cp sdk/kotlin/nanosandbox-sdk-kotlin.jar nanosandbox.kt.E2EKt
```