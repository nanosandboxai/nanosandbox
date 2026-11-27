# nanosandbox — Ruby SDK

A **stdlib-only** Ruby protocol client for the nanosandbox helper
(`nanosb-protocol` v1.0.0). It speaks NDJSON over a Unix stream socket using
only the standard library (`socket`, `json`, `base64`).

The client surface mirrors the Python SDK (`sdk/python/nanosandbox/client.py`)
and the driver contract in `sdk/conformance/SPEC.md`.

## Requirements

- Ruby 2.6+ (uses stdlib-only code; tested on the system Ruby 2.6.10).

## Install

```bash
gem install nanosandbox-sdk
```

## Quickstart (complete program)

Requires Ruby 2.6+; the first run downloads the runtime bundle. Save as
`demo.rb` and run `ruby demo.rb`:

```ruby
require "nanosandbox"
require "nanosandbox/runtime"

Nanosandbox::ManagedRuntime.open do |rt|
  client = Nanosandbox::SandboxClient.connect(rt.socket)
  box = client.create(
    Nanosandbox::SandboxBuilder.new("library/alpine:latest").with_name("demo").with_resources(1, 512).build
  )
  box.start

  print box.exec("echo", ["hello from a microVM"])["stdout"]

  box.remove(true)
end
```

Expected output:

```text
hello from a microVM
```

## Usage

```ruby
$LOAD_PATH.unshift("sdk/ruby/lib")
require "nanosandbox"
include Nanosandbox

client = SandboxClient.connect("/path/to/helper.sock")
client.supports("exec")            # => true / false

spec = SandboxBuilder.new("alpine:latest")
                     .with_name("demo")
                     .with_resources(2, 512)
                     .publish_tcp(8080, 80)
                     .build
box = client.create(spec)

box.start
box.state                          # => "running"

res = box.exec("echo", ["hi"])
res["exit_code"]                   # => 0

code = box.exec_stream({ "command" => "echo", "args" => ["hi"] }) do |ev|
  $stdout.write(ev["data"]) if ev["event"] == "stdout"
end

box.fs_write("/tmp/x", "hello")
box.fs_read("/tmp/x")              # => "hello"
box.logs_with(10, 0, "stdout")
box.metrics
box.volume_create("data", "/host/volumes/data")
box.secret_set("API_KEY", "s3cr3t")

begin
  client.get("missing")
rescue NanosbError => e
  e.code                           # => "not_found"
end
```

Every call dials a fresh connection and re-negotiates the `hello` handshake
(stateless). Streaming verbs (`exec_stream`, `attach`, `logs` with `follow`,
`metrics_stream`, `fs_write_stream`) hold one connection for the stream: an
opening `ok` ack, then frames until the terminal frame (`guest exit/error` or
`event done`).

## Managing a real sandbox (`ManagedRuntime`)

`Nanosandbox::ManagedRuntime` locates and starts the real `nanosb-runtime` helper
so you can boot and manage microVMs without launching it yourself. If the helper
is not installed, `ManagedRuntime` auto-provisions it: it runs the shared
`ensure-runtime.sh` provisioner, which downloads the per-platform runtime bundle
(nanosb-runtime + libkrunfw + gvproxy + exec-agent) from the release, verifies
it, and installs it under `$NANOSB_HOME`, pinned to the SDK's release version.
So `gem install nanosandbox-sdk` is the only step needed — the first
`ManagedRuntime.open` downloads and installs the runtime automatically.

```ruby
require "nanosandbox"
require "nanosandbox/runtime"

Nanosandbox::ManagedRuntime.open do |rt|
  client = Nanosandbox::SandboxClient.connect(rt.socket)
  box = client.create("image" => "library/alpine:latest", "name" => "demo")
  puts box.exec("echo", ["hello from a microVM"])["stdout"]
end
```

The helper is found via `NANOSB_RUNTIME_BIN` → `$NANOSB_HOME/bin/nanosb-runtime`
→ `PATH` → the repo `target/debug/nanosb-runtime`. When missing, the provisioner
is located via `NANOSB_PROVISIONER` → the repo `scripts/sdk/ensure-runtime.sh` →
an embedded copy shipped in the gem. Provisioning is configured by env:

| Variable | Description |
|---|---|
| `NANOSB_HOME` | Install root (default `~/.nanosandbox`) |
| `NANOSB_VERSION` | Runtime version to install (defaults to the SDK's own version, e.g. `0.3.0`) |
| `NANOSB_RUNTIME_URL` | Release base URL (default the GitHub release) |
| `NANOSB_RUNTIME_SHA256` | Expected sha256 of the bundle (optional, verified) |
| `NANOSB_OFFLINE` | `1` = never download (fail if missing) |
| `NANOSB_PROVISIONER` | Explicit path to the provisioner script |

## Headless code agents

Run Claude, Codex, or another coding agent in the sandbox instead of Alpine:
see [Headless Code Agents](https://nanosandbox.ai/docs/sdk/agents) and the
shared example in [`../README.md`](../README.md).

## Testing

Unit tests (minitest, in-process fake helper):

```bash
ruby -I sdk/ruby/lib sdk/ruby/test/test_client.rb
```

Conformance driver — runs all 40 scenarios in `sdk/conformance/scenarios/`:

```bash
ruby -I sdk/ruby/lib sdk/ruby/conformance/run.rb
# NANOSB_CONFORMANCE_BIN=/path/to/nanosb-conformance ruby -I sdk/ruby/lib sdk/ruby/conformance/run.rb
```

Expected output: `40 passed, 0 failed`, exit code 0.

Live microVM E2E (opt-in; needs the helper + libkrun/gvproxy + a cached image):

```bash
NANOSB_E2E=1 ruby -I sdk/ruby/lib sdk/ruby/test/test_e2e.rb
```

## Layout

```
sdk/ruby/
├── lib/nanosandbox.rb        # Conn, NanosbError, builders, Sandbox, SandboxClient
├── test/test_client.rb       # minitest + fake NDJSON helper
├── conformance/run.rb        # conformance driver
└── README.md
```