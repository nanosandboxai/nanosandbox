# Epic 3 — Runtime Networking & Isolation

Status: draft
Date: 2026-10-07
**Depends on: Epic 1** (`docs/epics/epic-1-multi-language-sdk.md`) — the helper
(`nanosb-runtime`) + protocol + networking config passthrough.

## 1. Context (verified 2026-10-07)

**What we have today**
- `gvproxy` provides a host-side virtio-net stack (DHCP/DNS/port-forward); TSI is
  the fallback. Per-sandbox `port_mappings` (TCP) are passed to the VM boot.
- **Guest-side firewalling is impossible on our kernel**: the libkrunfw kernel
  has `CONFIG_NETFILTER` and `CONFIG_BRIDGE` **unset** (verified). So any policy
  enforcement **must be host-side**, in the gvproxy path.

**microsandbox's model (the bar)**
- All sandbox traffic flows through a **host-controlled** stack; every packet is
  checked against policy before it leaves.
- Default: reach public internet; block private / loopback / link-local / cloud
  metadata. Only published ports accept inbound.
- Policy: two direction defaults + ordered first-match rules; groups
  `public`/`private`/`host`; targets IP / CIDR / domain / domain-suffix /
  port-range; `none`/`all` terminal choices.
- DNS interception (gateway forwarder), rebind protection, nameserver pinning,
  per-query timeout; deny → `NXDOMAIN`.
- TLS/SNI observation + plaintext `Host` inspection; **strict hostname mode**
  (fail-closed when authority can't be inspected); TLS interception (MITM).
- Secrets substitution + `onSecretViolation`.
- Rate limits: `maxConnections` (TCP/UDP), multi-tenant defaults, LRU eviction,
  60s session expiry.
- Interface overrides (ipv4/ipv6 pools/addresses, mac, mtu); `trustHostCAs`;
  host access (`host.*.internal`); NAT64 prefixes (`/96`, default `64:ff9b::/96`).
- Egress denial responses (`403` for HTTP; close for TLS/HTTP2).
- Isolation floor: `single-tenant` (default) vs `multi-tenant` (platform policy
  ∧ sandbox policy; DNS rebinding forced on; custom DNS + interface overrides
  removed; host CA import + published ports disabled).

## 2. Goal

Implement the **host-side networking enforcement engine** so a sandbox's egress
and ingress are governed by an explicit, auditable policy — matching
microsandbox's surface — driven by the config that Epic 1 passes through the
protocol.

Non-goals: replacing gvproxy as the L2/L3 datapath; guest-side firewalling (the
kernel lacks netfilter).

## 3. Architecture (ADR-E3-1)

```
 guest ──virtio-net──► gvproxy ──► [ EGRESS POLICY ENGINE (host) ] ──► internet
                                  │  ├── DNS intercept (UDP/TCP 53)
                                  │  ├── TCP connect policy (IP/CIDR/port)
                                  │  ├── HTTP Host / TLS SNI inspection
                                  │  ├── secret substitution / violation
                                  │  └── rate limits / conn caps
  host ◄──published ports───────────────────────────────────────────────┘
```

- Enforcement lives **in the helper**, in front of gvproxy's forwarder (a
  policy-aware forwarder/proxy), because the guest cannot filter.
- **DNS** is intercepted by an in-helper resolver (configurable upstreams,
  rebind protection, pinning, timeout); denies resolve to `NXDOMAIN`.
- **Hostname policy** uses what's observable: DNS query names, TLS SNI, and (for
  plaintext HTTP) the `Host` header. Strict mode fails closed when authority is
  not observable.
- The engine is a **separate crate/module** (`nanosb-egress`) so it can be
  developed and tested independently of the helper.

## 4. Workstreams

### WS1 — Policy model & schema
- Config types: direction defaults (ingress/egress), ordered first-match rules,
  terminal `none`/`all`, composable `public`/`private`/`host` profiles.
- Canonical rule ordering (groups then explicit); validation with typed errors.
- AC: policy parses, validates, canonicalizes; a policy matrix test passes.

### WS2 — Host-side egress filter
- Decide connections by IP/CIDR/port against the policy; default public-only,
  block private/loopback/link-local/metadata.
- Enforce in the gvproxy forward path.
- Denial responses: close by default; `403` for plaintext/intercepted HTTP.
- AC: allowlisted destination connects; others are closed; metadata blocked.

### WS3 — DNS interception
- In-helper DNS forwarder: nameserver pinning, per-query timeout, rebind
  protection (multi-tenant forced on).
- Deny → `NXDOMAIN` before any connection.
- AC: allowed name resolves; denied name returns `NXDOMAIN`; rebind attempt
  blocked.

### WS4 — Hostname / TLS inspection
- Observe SNI (TLS) and `Host` (plaintext HTTP); enforce hostname rules.
- Strict hostname mode (default): fail closed when authority isn't observable;
  `network.strict=false` opts out.
- Optional **TLS interception** (MITM with generated/host CAs) for intercepted
  ports; bypass list.
- AC: hostname allow works for plaintext + intercepted HTTPS; non-intercepted
  HTTPS is denied under a hostname-only allow.

### WS5 — Secrets substitution & violations
- Substitute configured secrets into matching requests; `onSecretViolation`
  action. Values never persisted (Epic 1 invariant).
- AC: a request carrying a placeholder is substituted; a violation triggers the
  configured action.

### WS6 — Rate limits & connection caps
- `maxTcpConnections` / `maxUdpConnections`; single- vs multi-tenant defaults;
  LRU eviction; 60s session expiry; memory guidance (64 KiB/tracked socket).
- AC: cap enforced; oldest session evicted; sessions expire.

### WS7 — Interface, ports, host access, NAT64
- Interface overrides (pools/addresses/mac/mtu); UDP publishing; explicit bind
  addresses; `host.*.internal`; NAT64 `/96` prefixes.
- AC: UDP port publishes; bind address honored; host alias resolves; NAT64
  destination matched.

### WS8 — Isolation floor
- `single-tenant` (default) vs `multi-tenant` (platform ∧ sandbox policy; DNS
  rebind forced; custom DNS + interface overrides removed; host CA import +
  published ports disabled).
- AC: multi-tenant cannot broaden the floor; overrides rejected.

### WS9 — SDK + protocol wiring
- `NetworkBuilder` in every language (from Epic 1's config passthrough);
  policy introspection (`config()` returns the effective policy).
- AC: each SDK can express policy, publish ports, and read back the config.

### WS10 — Tests
- Policy matrix; egress allow/deny (IP/CIDR/port); DNS allow/deny/rebind;
  hostname strict mode; rate-limit eviction; NAT64 match; denial responses.

## 5. Acceptance criteria

- AC1 Deny-by-default blocks private/loopback/link-local/metadata by default.
- AC2 Profile-based policy (`public`/`private`/`host`) composes correctly.
- AC3 Allowlist rules (IP/CIDR/domain/domain-suffix/port-range) enforced,
  first-match wins.
- AC4 DNS deny → `NXDOMAIN`; rebind protection blocks rebinding.
- AC5 Strict hostname mode fails closed when authority is unobservable.
- AC6 `maxTcpConnections`/`maxUdpConnections` enforced with LRU eviction.
- AC7 Published UDP + explicit bind address work.
- AC8 `trustHostCAs`, host access, NAT64 implemented and tested.
- AC9 Multi-tenant floor cannot be broadened by sandbox policy.
- AC10 The engine is host-side only (no guest kernel dependency).

## 6. Milestones

- **N0** Policy model + schema + matrix tests (WS1).
- **N1** Egress filter (IP/CIDR/port) + denial responses (WS2).
- **N2** DNS interception + rebind (WS3).
- **N3** Hostname/TLS inspection + strict mode (WS4).
- **N4** Rate limits + interface/ports/host access/NAT64 (WS6, WS7).
- **N5** Secrets substitution + violations (WS5).
- **N6** Isolation floor + SDK wiring + full test suite (WS8, WS9, WS10).

## 7. Risks

- R1 **Host-side enforcement** (guest can't filter) means every path must route
  through the helper's forwarder — validate gvproxy can be fronted without
  breaking TSI/port forwarding.
- R2 **TLS interception** is heavy (MITM, CA management, non-HTTP/HTTP2 edges) →
  time-box; ship strict hostname mode + SNI-only first, MITM later.
- R3 **Strict-mode false denies** (hostname rules on non-intercepted HTTPS) →
  default strict but document the opt-out clearly.
- R4 **Performance** of an in-path proxy → benchmark; keep the fast path (IP/CIDR)
  cheap and only escalate to L7 inspection when a policy needs it.
- R5 **Multi-tenant semantics** are subtle → implement the floor conservatively
  (restrict, never broaden) and test overrides are rejected.

## 8. Open questions

1. Build the egress engine as a **separate `nanosb-egress` crate** (recommended)
   or extend gvproxy directly?
2. TLS interception: ship in N3 or defer to a follow-up after SNI-only?
3. Do we need `multi-tenant` mode for v1 (single-user today), or defer it?
4. Rate-limit defaults: mirror microsandbox's (1024 multi-tenant) or pick our own?
