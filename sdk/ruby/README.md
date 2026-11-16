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
so you can boot and manage microVMs without launching it yourself:

```ruby
require "nanosandbox/runtime"

Nanosandbox::ManagedRuntime.open do |rt|
  client = Nanosandbox::SandboxClient.connect(rt.socket)
  box = client.create("image" => "library/alpine:latest", "name" => "demo")
  puts box.exec("echo", ["hello from a microVM"])["stdout"]
end
```

The helper is found via `NANOSB_RUNTIME_BIN` → `$NANOSB_HOME/bin/nanosb-runtime`
→ `PATH` → the repo `target/debug/nanosb-runtime`.

## Testing

Unit tests (minitest, in-process fake helper):

```bash
ruby -I sdk/ruby/lib sdk/ruby/test/test_client.rb
```

Conformance driver — runs all 35 scenarios in `sdk/conformance/scenarios/`:

```bash
ruby -I sdk/ruby/lib sdk/ruby/conformance/run.rb
# NANOSB_CONFORMANCE_BIN=/path/to/nanosb-conformance ruby -I sdk/ruby/lib sdk/ruby/conformance/run.rb
```

Expected output: `35 passed, 0 failed`, exit code 0.

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