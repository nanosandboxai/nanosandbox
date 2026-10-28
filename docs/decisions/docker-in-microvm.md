# Decision — Docker in the microVM (Epic 7)

Status: accepted — Tier 1 implemented (2026-10-19); Tier 2 kernel support implemented (2026-10-24)
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
  rebuild, and a deps bundle update. That is Epic 8 (#143); the kernel support
  and daemon-side bridge wiring are implemented (see §4). The rebuilt firmware
  has been installed into the local deps bundle (`~/.nanosandbox/libs/`).

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
  exposure. It was deferred to Epic 8 for that reason and is now implemented
  behind the opt-in `docker_bridge` capability.

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
- **`daemon_json` is validated against a Tier-1 allowlist.** Only keys that do
  not widen or subvert the Tier-1 isolation surface are accepted; keys such as
  `bridge`, `iptables`, `runtimes`, `default-runtime`, and `data-root` are
  rejected, as are unknown keys (so a typo fails loudly instead of being
  dropped). Validation runs host-side in `DockerConfig::validate()` and is
  enforced again in the guest before the daemon starts.
- **Secrets are never persisted into Docker state or preload images.** Secrets
  are injected into the guest environment and must not be written into the
  Docker storage directory, image layers, or container metadata. `preload_images`
  must not embed secret material, and registry mirror / insecure-registry
  entries must not carry `userinfo@`; both are rejected by `validate()`.

## 4. Current state (Tier 1 daemon + Tier 2 bridge networking)

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
environment. `rootless` is not supported in Tier 1: a `rootless: true` block is
rejected loudly at config resolution (and refused again in the guest) instead of
booting without a daemon. Tier 2 (bridge networking, netfilter, kernel rebuild)
is Epic 8 (#143); the kernel support is implemented and verified with Docker's
`check-config.sh` (all "Generally Necessary" options enabled).

Tier 1 has no default bridge. Containers that need outbound networking must use
`docker run --network host ...` (or `network_mode: host` in Compose); Docker's
default bridge requires the Tier-2 kernel. With a helper that advertises the
`docker_bridge` capability, set `docker.network: bridge` and Docker starts with
its default `docker0` bridge, iptables/NAT, and IPv4 forwarding (the exec-agent
enables the bridge sysctls at boot). `docker run hello-world` works once the
image is present because it needs no networking.

**Live evidence (2026-10-19, macOS arm64, next mode).** With
`claude-docker` from a local registry and a 4 GiB `data_size_bytes`, the sandbox
booted with `/dev/vda` (virtio-blk serial `disk0`) formatted ext4 and mounted at
`/var/lib/docker`; `docker info` reported `Storage Driver: overlay2`,
`Cgroup Version: 2`, `Docker Root Dir: /var/lib/docker`; and
`docker run --rm --network host hello-world` exited 0 with
"Hello from Docker!". The opt-in assertion is
`crates/client/tests/epic7_docker_live.rs`.

`dockerd` is started with `--group developer` when that group exists, and the
`claude-docker` image adds `developer` to the `docker` group, so the non-root
code agent can use the daemon. Socket access is still guest-root-equivalent
(§2): the agent is the intended privileged guest actor.

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
- Tier 2 changes the guest kernel: the netfilter/bridge/NAT stack is compiled
  in. The VM boundary still contains the added attack surface (§2).
- `daemon_json` is validated against a Tier-1 allowlist before a daemon start;
  unsafe, unknown, and credential-bearing entries fail loudly at resolution.
- The guest remains the trust boundary; container compromise is a guest
  compromise, contained by libkrun.

## 7. Follow-ups

- `daemon_json` validation: **done.** `DockerConfig::validate()` enforces a
  Tier-1 allowlist host-side, and the guest guard enforces the same rules before
  starting `dockerd`.
- Secrets-never-persisted: **enforced.** `preload_images` and registry mirror /
  insecure-registry entries that embed credentials are rejected.
- `rootless`: **decided — unsupported in Tier 1.** A `rootless: true` block is
  rejected explicitly rather than silently booting without a daemon. Revisiting
  rootless and its socket-permission model is Tier 2+ work.
- Tier 2 kernel rebuild and deps bundle (Epic 8, #143): **kernel rebuild done.**
  The base configs enable the netfilter/bridge/NAT stack; `check-config.sh`
  reports all "Generally Necessary" options enabled. The daemon-side bridge
  wiring is implemented in the exec-agent; the rebuilt firmware was installed
  into the local deps bundle (`~/.nanosandbox/libs/`). A live bridge-mode VM
  test remains opt-in.
