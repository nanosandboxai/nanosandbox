# Protocol Documentation

The wire contract between the `nanosb-runtime` helper and every language SDK,
published for Epic 1 (Multi-language Sandbox SDK). The authoritative
implementation is the frozen `crates/protocol` crate; these documents mirror it.

## Documents

- **[`protocol.md`](protocol.md)** — the protocol specification: the
  helper-based substrate (no C FFI), the Unix-socket NDJSON transport, the
  hello/version/capability handshake, every request verb and response variant,
  the shared message types, streaming semantics, error codes, and the guest
  filesystem frames.
- **[`conformance.md`](conformance.md)** — the conformance harness: the scenario
  schema, the per-SDK driver contract, and the `expect.type` → SDK-operation
  mapping. The authoritative contract is
  [`sdk/conformance/SPEC.md`](../../sdk/conformance/SPEC.md).
- **[`sandbox-spec.md`](sandbox-spec.md)** — the `SandboxSpec` field inventory:
  every field of the creation spec and its nested types (name, type, JSON key,
  default, required, units). The canonical field vocabulary the cloud "Sandbox
  resource" projects onto.
- **[`sandbox-spec.schema.json`](sandbox-spec.schema.json)** — the JSON Schema
  (draft 2020-12) for the `create` wire type, guarded against drift by a Rust
  test in `crates/protocol`.
- **[`sandbox-resource-naming.md`](sandbox-resource-naming.md)** — the cloud ↔
  runtime naming reconciliation map (`vcpu`→`cpus`, `memory_mib`→`memory_mb`,
  `ttl_seconds`→`timeout_secs`, `command[]`→`command`+`command_args`,
  `working_dir`→`workdir`).

## Related

- Epic: [`docs/epics/epic-1-multi-language-sdk.md`](../epics/epic-1-multi-language-sdk.md)
- Conformance corpus: [`sdk/conformance/`](../../sdk/conformance/)
- Protocol crate: [`crates/protocol/`](../../crates/protocol/)