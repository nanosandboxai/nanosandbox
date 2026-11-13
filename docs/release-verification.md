# Release Verification Runbook (Epic 6 · R10)

How to verify that a real `v*` release cut produces a working, signed install on a
clean machine. This is an **acceptance gate**: run it after tagging, attach the
transcript to the release issue.

> Status: the procedure below is documented and scriptable, but an actual tag cut
> and notarized release can only be produced by a maintainer with the Apple
> Developer ID certificate + notarization secrets configured in CI (see R3). Until
> then this runbook is unexecuted.

## 0. Preconditions

- A tag `vX.Y.Z` pushed to `main` (triggers `.github/workflows/release.yml`).
- Apple secrets present for a signed+notarized macOS build:
  `MACOS_CERT_P12`, `MACOS_CERT_PASSWORD`, `APPLE_ID`, `APPLE_TEAM_ID`,
  `APPLE_APP_PASSWORD`. Without them the macOS binary is ad-hoc signed only.

## 1. Verify the release contains every asset

```bash
gh release view vX.Y.Z --json assets --jq '.assets[].name' | sort
```

Expect:

- `nanosb` (macOS arm64, signed + notarized)
- `nanosb-linux-amd64`
- `deps-darwin-arm64.tar.gz`, `deps-linux-amd64.tar.gz`, `deps-linux-arm64.tar.gz`
- `exec-agent-aarch64`, `exec-agent-x86_64`
- `install.sh`, `uninstall.sh`, `install-deps.sh`, `uninstall-deps.sh`
- `agent_defaults.yaml`, `LICENSE`
- `checksums.sha256`, plus `nanosandbox-sdk-<lang>.tar.gz` bundles
- `checksums-*.sha256` per platform

## 2. Fresh install on a clean macOS arm64 machine

```bash
# Use a throwaway HOME to prove there are no manual steps.
export NANOSANDBOX_HOME="$HOME/.nanosandbox"
curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/download/vX.Y.Z/install.sh | bash
```

Expect the installer to lay out:

```
~/.nanosandbox/bin/nanosb
~/.nanosandbox/bin/gvproxy
~/.nanosandbox/libs/libkrunfw.5.dylib
~/.nanosandbox/guest-bin/exec-agent
~/.nanosandbox/LICENSE
```

Assert:

```bash
test -x ~/.nanosandbox/bin/nanosb
test -x ~/.nanosandbox/guest-bin/exec-agent
```

## 3. `nanosb doctor` → 4 checks passed

```bash
nanosb doctor
```

Expect (macOS arm64):

```
  [✓] Architecture: Apple Silicon (aarch64)
  [✓] libkrunfw Kernel Firmware: found (libkrunfw.5.dylib)
  [✓] Hypervisor.framework: available
  [✓] gvproxy: available (full outbound networking)

4 checks passed, 0 errors, 0 warnings
```

## 4. Boot and run a sandbox end-to-end

```bash
nanosb pull ghcr.io/nanosandboxai/nanosandbox/claude:latest
nanosb run ghcr.io/nanosandboxai/nanosandbox/claude:latest -- echo "hello from the VM"
```

Expect a running microVM that executes the command and a clean teardown.

## 5. Gatekeeper (ties to R3)

```bash
spctl -a -vv "$(command -v nanosb)"
codesign --verify --verbose=2 "$(command -v nanosb)"
```

Expect: `accepted`, and a Developer ID signature (not `adhoc`) when the Apple
secrets were configured.

## 6. Record evidence

Attach the full transcript of steps 1–5 to the release verification issue, and
mark this runbook's status as executed with the tag and date.

## Failure handling

- Missing `exec-agent`/`deps-*` asset → the corresponding `release.yml` job failed
  or `publish.needs` was not satisfied; check the workflow run.
- `nanosb doctor` < 4 checks → the deps bundle did not install; inspect the
  `install-deps` step output.
- Gatekeeper rejection → signing/notarization secrets missing or the cert is not
  a Developer ID; re-run with secrets present.
