# SandboxSpec — field inventory

The authoritative definition of the sandbox creation specification. This
document inventories every field of the protocol `SandboxSpec` and its nested
types, mirroring `crates/protocol/src/types.rs` 1:1. It is the canonical field
vocabulary that the cloud "Sandbox resource"
([nanosandboxai/cloud#41](https://github.com/nanosandboxai/cloud/issues/41))
projects onto.

- **Wire type**: `{"type":"create","spec":{...}}` — see
  [`verbs.rs`](../../crates/protocol/src/verbs.rs) (`CreateRequest`).
- **JSON encoding**: `snake_case` field names, serde defaults, and
  `skip_serializing_if` rules exactly as declared on the Rust struct.
- **Machine-readable form**: [`sandbox-spec.schema.json`](sandbox-spec.schema.json)
  (JSON Schema, draft 2020-12), guarded against drift by a Rust test in
  `crates/protocol`.
- **Cloud ↔ runtime naming**: [`sandbox-resource-naming.md`](sandbox-resource-naming.md).

## Conventions used in the tables

- **Required** — `yes` means the field has no serde default and no
  `skip_serializing_if`; it is always present on the wire. `no` means it is
  optional (an `Option`, or a field with a serde default).
- **Default** — the value applied when the key is absent. `—` means the field
  is required (no default). `skip` means the key is omitted from the JSON when
  it holds its default/`None`/empty value.
- **Units** — the physical unit of the value, where meaningful.

## `SandboxSpec`

The full creation spec. Only `image` is required; every other field is optional
or has a serde default, so a minimal create is `{"image":"alpine"}`.

| Field | Type | JSON key | Default | Required | Units | Notes |
|---|---|---|---|---|---|---|
| `name` | `Option<String>` | `name` | `None` (skip) | no | — | Optional sandbox handle name. |
| `image` | `String` | `image` | — | **yes** | — | OCI image reference. |
| `cpus` | `Option<u32>` | `cpus` | `None` (skip) | no | vCPU count | Cloud `vcpu`. |
| `memory_mb` | `Option<u32>` | `memory_mb` | `None` (skip) | no | MiB | Cloud `memory_mib`. |
| `env` | `HashMap<String,String>` | `env` | `{}` | no | — | Environment variables. |
| `mounts` | `Vec<MountSpec>` | `mounts` | `[]` | no | — | See [`MountSpec`](#mountspec). |
| `network` | `NetworkSpec` | `network` | enabled | no | — | See [`NetworkSpec`](#networkspec). |
| `workdir` | `Option<String>` | `workdir` | `None` (skip) | no | — | Working directory. Cloud `working_dir`. |
| `timeout_secs` | `Option<u32>` | `timeout_secs` | `None` (skip) | no | seconds | Cloud `ttl_seconds`. |
| `user` | `Option<String>` | `user` | `None` (skip) | no | — | `uid` / `uid:gid` / name. |
| `home` | `Option<String>` | `home` | `None` (skip) | no | — | Home directory override. |
| `command` | `Option<String>` | `command` | `None` (skip) | no | — | Entrypoint as a **single string**. Cloud `command[0]`. |
| `command_args` | `Vec<String>` | `command_args` | `[]` | no | — | Args appended to `command`. Cloud `command[1:]` + `args`. |
| `rlimits` | `Vec<Rlimit>` | `rlimits` | `[]` | no | — | See [`Rlimit`](#rlimit). |
| `labels` | `HashMap<String,String>` | `labels` | `{}` | no | — | Arbitrary labels. |
| `ephemeral` | `bool` | `ephemeral` | `false` | no | — | Remove automatically on stop. |
| `pull_policy` | `PullPolicy` | `pull_policy` | `always` | no | — | `always` \| `missing` \| `never`. |
| `scripts` | `HashMap<String,String>` | `scripts` | `{}` | no | — | Named scripts at `/.nanosb/scripts/<name>`. |
| `detached` | `bool` | `detached` | `true` | no | — | Helper owns the lifecycle. |
| `replace` | `bool` | `replace` | `false` | no | — | Replace an existing sandbox of the same name. |
| `hostname` | `Option<String>` | `hostname` | `None` (skip) | no | — | Guest hostname override. |
| `secret_env` | `BTreeMap<String,String>` | `secret_env` | `{}` (skip) | no | — | Env key → secret key. |
| `secrets` | `Vec<SecretEntry>` | `secrets` | `[]` (skip) | no | — | See [`SecretEntry`](#secreentry). |
| `rootfs` | `Option<RootfsSpec>` | `rootfs` | `None` (skip) | no | — | See [`RootfsSpec`](#rootfsspec). |
| `patches` | `Vec<RootfsPatch>` | `patches` | `[]` (skip) | no | — | See [`RootfsPatch`](#rootfspatch). |
| `registry` | `Vec<RegistrySpec>` | `registry` | `[]` (skip) | no | — | See [`RegistrySpec`](#registryspec). |
| `stat_virtualization` | `Option<StatVirtualization>` | `stat_virtualization` | `None` (skip) | no | — | `strict` \| `relaxed` \| `off`. |
| `security_profile` | `SecurityProfile` | `security_profile` | `default` | no | — | `default` \| `restricted`. |
| `security` | `Option<SecurityContext>` | `security` | `None` (skip) | no | — | See [`SecurityContext`](#securitycontext). |

## `MountSpec`

A mount in a create request.

| Field | Type | JSON key | Default | Required | Notes |
|---|---|---|---|---|---|
| `host_path` | `String` | `host_path` | — | **yes** | Host path (bind/virtiofs) or image-backed volume name. |
| `guest_path` | `String` | `guest_path` | — | **yes** | Mount point inside the guest. |
| `readonly` | `bool` | `readonly` | `false` | no | |
| `mount_type` | `MountType` | `mount_type` | `bind` | no | `bind` \| `virtiofs` \| `tmpfs`. |
| `file` | `bool` | `file` | `false` | no | Host path is a file, not a directory. |
| `tmpfs` | `bool` | `tmpfs` | `false` | no | tmpfs mount (`host_path` ignored). |
| `disk_image` | `Option<String>` | `disk_image` | `None` (skip) | no | `raw`/`qcow2`/`vmdk`; `host_path` is the image file. |
| `fstype` | `Option<String>` | `fstype` | `None` (skip) | no | Filesystem type (e.g. `ext4`). |
| `options` | `Vec<String>` | `options` | `[]` | no | e.g. `noexec`, `nosuid`, `nodev`. |
| `uid` | `Option<u32>` | `uid` | `None` (skip) | no | |
| `gid` | `Option<u32>` | `gid` | `None` (skip) | no | |

`MountType` — `bind` | `virtiofs` | `tmpfs` (serde `lowercase`).

## `NetworkSpec`

Networking config surface. Enforcement is a separate epic; the protocol only
carries the configuration.

| Field | Type | JSON key | Default | Required | Notes |
|---|---|---|---|---|---|
| `enabled` | `bool` | `enabled` | `true` | no | Egress on unless explicitly disabled. |
| `mode` | `Option<String>` | `mode` | `None` (skip) | no | |
| `scope` | `Option<String>` | `scope` | `None` (skip) | no | |
| `publish` | `Vec<PortPublish>` | `publish` | `[]` | no | See [`PortPublish`](#portpublish). |
| `dns` | `Vec<String>` | `dns` | `[]` | no | |
| `dns_zones` | `HashMap<String,String>` | `dns_zones` | `{}` | no | |
| `policy` | `Option<serde_json::Value>` | `policy` | `None` (skip) | no | Opaque policy passthrough. |

### `PortPublish`

| Field | Type | JSON key | Default | Required | Notes |
|---|---|---|---|---|---|
| `host_port` | `u16` | `host_port` | — | **yes** | |
| `container_port` | `u16` | `container_port` | — | **yes** | Cloud `ports[].container_port`. |
| `protocol` | `String` | `protocol` | `"tcp"` | no | Cloud `ports[].protocol`. |
| `bind_address` | `Option<String>` | `bind_address` | `None` (skip) | no | Loopback by default. |

## `Rlimit`

A POSIX resource limit applied to executions.

| Field | Type | JSON key | Default | Required | Notes |
|---|---|---|---|---|---|
| `resource` | `String` | `resource` | — | **yes** | e.g. `nofile`. |
| `soft` | `Option<u64>` | `soft` | `None` (skip) | no | |
| `hard` | `Option<u64>` | `hard` | `None` (skip) | no | |

## `RootfsSpec`

A non-OCI rootfs source. Only meaningful when `SandboxSpec.rootfs` is set.

| Field | Type | JSON key | Default | Required | Notes |
|---|---|---|---|---|---|
| `kind` | `RootfsKind` | `kind` | `oci` | **yes** | `oci` \| `local_dir` \| `disk_image`. |
| `path` | `Option<String>` | `path` | `None` (skip) | no | Directory (`local_dir`) or image file (`disk_image`). |
| `fstype` | `Option<String>` | `fstype` | `None` (skip) | no | e.g. `ext4`. |
| `format` | `Option<String>` | `format` | `None` (skip) | no | `raw` (default), `qcow2`, `vmdk`. |
| `read_only` | `bool` | `read_only` | `false` | no | Attach the rootfs block device read-only. |

`RootfsKind` — `oci` | `local_dir` | `disk_image` (serde `snake_case`).

## `RootfsPatch`

A pre-boot patch applied to the rootfs before the VM boots. Tagged union on
`op` (serde `tag = "op"`, `snake_case`).

| `op` | Fields | Notes |
|---|---|---|
| `mkdir` | `path` (string), `mode?` (u32) | Create a directory (parents as needed). |
| `text` | `path` (string), `content` (string), `mode?` (u32) | Write/replace a text file. |
| `copy_file` | `from` (string), `to` (string), `mode?` (u32) | Copy a single host file into the rootfs. |
| `copy_dir` | `from` (string), `to` (string) | Recursively copy a host directory. |
| `remove` | `path` (string), `recursive` (bool) | Remove a path from the rootfs. |

## `RegistrySpec`

Per-sandbox registry configuration.

| Field | Type | JSON key | Default | Required | Notes |
|---|---|---|---|---|---|
| `host` | `String` | `host` | — | **yes** | e.g. `ghcr.io` or `localhost:5050`. |
| `insecure` | `bool` | `insecure` | `false` | no | Allow insecure HTTP. |
| `skip_tls_verify` | `bool` | `skip_tls_verify` | `false` | no | |
| `username` | `Option<String>` | `username` | `None` (skip) | no | |
| `password` | `Option<String>` | `password` | `None` (skip) | no | |

## `SecurityProfile`

`default` | `restricted` (serde `snake_case`). `default` is the serde default.

## `SecurityContext`

In-guest OCI security context. This build enforces **no** in-guest
seccomp/capabilities/apparmor; any unenforced field is rejected at create (see
`SecurityContext::unenforced_field`). `run_as_user` is always allowed.

| Field | Type | JSON key | Default | Required | Notes |
|---|---|---|---|---|---|
| `capabilities` | `Vec<String>` | `capabilities` | `[]` | no | Non-empty → rejected. |
| `seccomp_profile` | `Option<String>` | `seccomp_profile` | `None` (skip) | no | Non-empty → rejected. |
| `apparmor_profile` | `Option<String>` | `apparmor_profile` | `None` (skip) | no | Non-empty → rejected. |
| `no_new_privileges` | `Option<bool>` | `no_new_privileges` | `None` (skip) | no | `true` → rejected. |
| `run_as_user` | `Option<String>` | `run_as_user` | `None` (skip) | no | Allowed; cloud `security_context.run_as_user`. |

## `SecretEntry`

| Field | Type | JSON key | Default | Required | Notes |
|---|---|---|---|---|---|
| `key` | `String` | `key` | — | **yes** | |
| `value` | `Option<String>` | `value` | `None` (skip) | no | Never returned in responses, never persisted. |

## Related enums

- **`PullPolicy`** — `always` | `missing` | `never` (serde `snake_case`).
- **`StatVirtualization`** — `strict` | `relaxed` | `off` (serde `snake_case`).