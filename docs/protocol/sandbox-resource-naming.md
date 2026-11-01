# Sandbox resource — cloud ↔ runtime naming reconciliation

The cloud "Sandbox resource" ([nanosandboxai/cloud#41](https://github.com/nanosandboxai/cloud/issues/41))
is the canonical, backend-neutral spec. Its canonical names are the public API
contract and are **kept as-is**; the nanosb runtime uses a different vocabulary.
This document is the authoritative bridge between the two, sourced from the
naming map in the cloud design doc
[`docs/design/sandbox-resource.md` §4](../../../cloud/docs/design/sandbox-resource.md).

The runtime side is the protocol `SandboxSpec` (see
[`sandbox-spec.md`](sandbox-spec.md) for the full field inventory).

## Naming map (cloud → nanosb runtime)

| Cloud (canonical) | nanosb runtime (`SandboxSpec`) | Notes |
|---|---|---|
| `image` | `image` | Same name, same meaning. |
| `command` | `command` (string) + `command_args` | Runtime `command` is a **single string**. `command[0]` → `command`; `command[1:]` + `args` → `command_args`. |
| `args` | `command_args` | Appended after `command[1:]`. |
| `working_dir` | `workdir` | |
| `user` | `user` | |
| `env` | `env` | |
| `vcpu` | `cpus` | |
| `memory_mib` | `memory_mb` | |
| `ttl_seconds` | `timeout_secs` | |
| `ports[].container_port` | `network.publish[].container_port` | |
| `ports[].protocol` | `network.publish[].protocol` | Default `tcp`. |
| `volumes` + `volume_mounts` | `mounts` (`MountSpec`) | Cloud `mount_path` → `guest_path`. |
| `security_context.run_as_user` | `security.run_as_user` | The **only** honored security field. |

## Field-by-field reconciliation

| Cloud field | Runtime field | Direction | Notes |
|---|---|---|---|
| `vcpu` | `cpus` | cloud → runtime | Integer vCPU count. |
| `memory_mib` | `memory_mb` | cloud → runtime | Integer MiB. |
| `ttl_seconds` | `timeout_secs` | cloud → runtime | Integer seconds. |
| `command` (string[]) | `command` (string) + `command_args` (string[]) | cloud → runtime | Split: `command[0]` → `command`; `command[1:]` → `command_args`. |
| `args` (string[]) | `command_args` (string[]) | cloud → runtime | Appended after `command[1:]`. |
| `working_dir` | `workdir` | cloud → runtime | |
| `ports[].container_port` | `network.publish[].container_port` | cloud → runtime | |
| `ports[].protocol` | `network.publish[].protocol` | cloud → runtime | Default `tcp`. |
| `volumes` + `volume_mounts` | `mounts` | cloud → runtime | `mount_path` → `guest_path`. |
| `security_context.run_as_user` | `security.run_as_user` | cloud → runtime | Only honored security field. |

## Fail-closed on the nanosb path

The runtime `SandboxSpec` cannot represent every cloud field. The nanosb
projection **fails closed** (a clear error) rather than silently dropping a
field. These cloud fields are not representable in the runtime spec:

- `env_from`
- `service_account_name`
- `image_pull_policy`
- `scheduling`
- non-`run_as_user` security fields (`capabilities`, `seccomp_profile`,
  `apparmor_profile`, `no_new_privileges == true`)

Conversely, the runtime carries fields that are **not** part of the cloud v1
resource and are set by the node image / runtime config, not the tenant:
`security_profile`, `rootfs`, `registry`, `rlimits`, `patches`,
`stat_virtualization`, `scripts`, `secret_env`, `secrets`, `detached`,
`replace`, `hostname`, `ephemeral`, `home`, `labels`.

## Units and defaults

| Cloud field | Runtime field | Unit | Cloud default | Runtime default |
|---|---|---|---|---|
| `vcpu` | `cpus` | vCPU | required (no default) | `None` (engine default) |
| `memory_mib` | `memory_mb` | MiB | required (no default) | `None` (engine default) |
| `ttl_seconds` | `timeout_secs` | seconds | `3600` | `None` (no timeout) |
| `command` | `command` + `command_args` | — | `null` | `None` + `[]` |
| `working_dir` | `workdir` | — | `""` | `None` |
| `ports[].protocol` | `network.publish[].protocol` | — | `tcp` | `"tcp"` |

## Source of truth

The naming map above is the runtime-side projection of the cloud design doc
[`docs/design/sandbox-resource.md` §4](../../../cloud/docs/design/sandbox-resource.md)
and §8 (nanosb projection). If the cloud schema changes, update that doc first,
then reconcile this table and the field inventory in
[`sandbox-spec.md`](sandbox-spec.md).