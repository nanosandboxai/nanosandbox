# Decision — Docker in the microVM (Epic 7)

Status: proposed (2026-10-19)
Related: Epic 7 (#132), Epic 8 (#143), `docs/decisions/volume-backings.md`,
`docs/decisions/security-context-contract.md`, `docs/protocol/protocol.md`.

## 1. Context

Epic 7 adds a Docker runtime inside the microVM so agents can build and run
containers without touching the host. The guest kernel is libkrunfw Linux
6.12.76. The runtime is delivered as a config surface plus a `Capability::Docker`
advertisement and a guest delivery channel (`NANOSB_DOCKER` JSON, present only
when Docker is configured, omitted otherwise).

Two tiers are planned:

- **Tier 1** runs `dockerd` with `--bridge=none --iptables=false
  --ip6tables=false`; containers use `--network host`. Outbound traffic goes
  through gvproxy virtio-net / TSI. No kernel change is required.
- **Tier 2** needs kernel modules (`BRIDGE`, `NETFILTER`, `NF_NAT`, `IP_NF_*`,
  `NETFILTER_XTABLES`, `NFT_*`, `NET_SCHED`, `NET_CLS_CGROUP`), a libkrunfw
  rebuild, and a deps bundle update. That is Epic 8 (#143) and is **not
  implemented**.

This document records the security and isolation posture for the feature and
the safe defaults that ship with it.

## 2. Threat model

The microVM (libkrun) is the trust boundary. Everything inside it is guest
state; everything outside it is host state. The model below lists the surfaces
and how each is contained.

- **Container reach (guest only).** Containers run inside the guest kernel.
  A container can only reach other guest processes, the guest filesystem, and
  whatever the guest network exposes. It has no direct path to the host.
- **Host boundary.** The microVM is the boundary. A container escape is a guest
  compromise, not a new host surface: the host already treats the guest as
  untrusted and isolates it via libkrun. Docker adds no host-facing attack
  surface beyond what the VM already presents.
- **dockerd as guest root.** `dockerd` runs as root inside the guest. Access to
  the Docker socket is guest-root-equivalent: anyone who can reach the socket
  can start privileged containers and read guest secrets. The socket is a guest
  resource, so this does not cross the boundary, but it does concentrate
  privilege inside the guest.
- **Container to virtiofs workspace.** The workspace is mounted into the guest
  via virtiofs. A container can read and write the workspace like any guest
  process. Workspace isolation keeps the host project behind virtiofs, so a
  container cannot reach host files outside the mounted tree.
- **Container to network.** Container traffic is subject to the same
  `NetworkConfig` / gvproxy / TSI rules as the rest of the guest. In Tier 1 the
  container shares the guest network namespace (`--network host`), so it is
  bounded by the guest's network policy, not by a per-container policy.
- **Privileged containers.** A privileged container runs with guest root and
  can mount devices, load kernel modules, and otherwise act as the guest root.
  Because the guest is already the boundary, this is contained, but it is still
  the highest-privilege guest actor and should be off by default.
- **Tier-2 netfilter attack surface.** Tier 2 enables bridge networking and
  netfilter inside the guest. That adds a large, kernel-level attack surface
  (iptables/nftables parsing, NAT, bridge handling) inside the guest. It is
  contained by the VM boundary, but it is a meaningful increase in guest kernel
  exposure and is deferred to Epic 8 for that reason.

## 3. Safe defaults

- **`docker.enabled` defaults to `false`.** Docker is opt-in. A sandbox does not
  run a daemon unless the user asks for it.
- **Host networking is the Tier 1 default.** `network: host` keeps containers
  on the guest network namespace, subject to the existing gvproxy / TSI policy,
  and avoids the netfilter surface entirely.
- **No privileged containers by default.** The default posture treats the guest
  as the boundary and does not grant extra guest privileges to containers unless
  explicitly configured.
- **Resource limits.** Docker inherits the existing VM ceilings (CPU, memory,
  PID, inode, IO). `data_size_bytes` (default 20 GiB) deliberately bounds the
  Docker storage footprint so a runaway image or layer cannot exhaust the guest
  disk. These are guidance, not a separate quota mechanism.
- **`daemon_json` is trusted, unvalidated passthrough in D5.** The value is
  forwarded to the daemon as-is. It is not schema-validated in this change.
  Treat it as configuration the user already controls, and validate it before
  Tier 1 ships a daemon start.
- **Secrets are never persisted into Docker state or preload images.** Secrets
  are injected into the guest environment and must not be written into the
  Docker storage directory, image layers, or container metadata. `preload_images`
  must not embed secret material.

## 4. Current state (Tier 1 daemon start; Tier 2 future)

D5 ships the config surface, `Capability::Docker`, and guest delivery; D3/D4
provision the data disk and start the Tier-1 daemon:

- `docker.enabled` (default `false`)
- `runtime` (`dockerd` | `podman` | `containerd`)
- `storage_driver` (`overlay2` | `vfs` | `fuse-overlayfs`)
- `data_size_bytes` (default 20 GiB)
- `network` (`host` | `none` | `bridge`)
- `daemon_json`
- `preload_images`
- `rootless`
- `socket` (default `/var/run/docker.sock`)

Guest delivery is `NANOSB_DOCKER` JSON, present only when Docker is configured
and omitted otherwise.

**Tier 1 is implemented.** Setting `docker.enabled: true` provisions a sparse
raw data disk (`disk_image: "raw"`, `fstype: "ext4"`) attached as virtio-blk
with block_id `diskN`, mounts it at `/var/lib/docker` (formatting it with
`mkfs.ext4` on first use), writes `/etc/docker/daemon.json`, and starts
`dockerd` in the guest at boot with `--bridge=none --iptables=false
--ip6tables=false`. `DOCKER_HOST` is exported to the PID-1/workload
environment. `rootless` is not yet supported in this build; enabling it logs a
diagnostic and does not start a daemon. Tier 2 (bridge networking, netfilter,
kernel rebuild) is Epic 8 (#143) and is not implemented.

Tier 1 has no default bridge. Containers that need outbound networking must use
`docker run --network host ...` (or `network_mode: host` in Compose); Docker's
default bridge requires the Tier-2 kernel. `docker run hello-world` works once
the image is present because it needs no networking.

**Live evidence (2026-10-19, macOS arm64, next mode).** With
`claude-docker` from a local registry and a 4 GiB `data_size_bytes`, the sandbox
booted with `/dev/vda` (virtio-blk serial `disk0`) formatted ext4 and mounted at
`/var/lib/docker`; `docker info` reported `Storage Driver: overlay2`,
`Cgroup Version: 2`, `Docker Root Dir: /var/lib/docker`; and
`docker run --rm --network host hello-world` exited 0 with
"Hello from Docker!". The opt-in assertion is
`crates/client/tests/epic7_docker_live.rs`.

## 5. Acceptance criteria mapping

- **#137 AC1** — The `docker` config round-trips protocol → runtime → guest.
  Covered by D5: the protocol `DockerConfig` maps into the runtime config and
  is delivered as `NANOSB_DOCKER`; the no-silent-drop test asserts every field.
- **#137 AC2** — The default (`None` / `enabled: false`) is a no-op with a
  byte-identical boot. Covered by D5: the field is omitted from serialized
  config and the guest env var is omitted when Docker is not configured.
- **#137 AC3** — A `docker` capability is advertised and can be gated by
  clients. Covered by D5: `Capability::Docker` is part of the protocol
  capability vocabulary.
- **#141 AC1** — A threat model for in-VM containers is documented. Covered by
  §2 of this document.
- **#141 AC2** — Defaults are safe and risky options are opt-in. Covered by §3.
- **#141 AC3** — No host escape beyond the existing microVM boundary is
  introduced. Covered by §2: the microVM remains the trust boundary.

## 6. Consequences

- Docker is opt-in and off by default, so existing sandboxes are unaffected.
- Tier 1 keeps the guest kernel unchanged; the netfilter surface is deferred to
  Epic 8.
- `daemon_json` passthrough is unvalidated in D5; it must be validated before a
  daemon start ships.
- The guest remains the trust boundary; container compromise is a guest
  compromise, contained by libkrun.

## 7. Follow-ups

- Validate `daemon_json` before Tier 1 ships a daemon start for real usage (D4
  starts the daemon with the passthrough as-is; it is not schema-validated).
- Enforce the secrets-never-persisted rule in the daemon start path (D4).
- Tier 2 kernel rebuild and deps bundle (Epic 8, #143).
- Decide whether `rootless` becomes a default and how it interacts with the
  socket permission model.