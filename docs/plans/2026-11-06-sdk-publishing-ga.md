# SDK publishing — GA runbook

Status: pipeline implemented (`.github/workflows/sdk-release.yml`); the real
publish is gated only on registry credential provisioning. Branch:
`feat/sdk-publishing`.
Related: `docs/epics/epic-4-sdk-ci-cd-and-publishing.md`.

## 1. Secret provisioning (org admin)

`publish` (matrix) and `publish-jvm` authenticate every registry with a
repository Actions **token secret** (no OIDC / `id-token: write`):

| Registry | Secret | Job |
|---|---|---|
| crates.io | `CARGO_REGISTRY_TOKEN` — token auth requires a **verified email** on the owning account | `publish` (rust) |
| PyPI | `PYPI_API_TOKEN` (exported as `TWINE_PASSWORD`) | `publish` (python) |
| npm | `NPM_TOKEN` (exported as `NODE_AUTH_TOKEN`) | `publish` (typescript) |
| RubyGems | `RUBYGEMS_API_KEY` | `publish` (ruby) |
| NuGet | `NUGET_API_KEY` — must be scoped to allow the `Nanosandbox.Sdk` id | `publish` (dotnet) |
| Maven Central | `MAVEN_TOKEN` (Central Portal `username:password` user token) **and** `MAVEN_GPG_PRIVATE_KEY` (+ `MAVEN_GPG_PASSPHRASE`) | `publish-jvm` |
| GitHub | Environment `sdk-release` | all publish jobs |

Notes:

- **Maven Central cannot publish without a GPG key** — the Central Portal requires
  a `.asc` signature for every file. Add `MAVEN_GPG_PRIVATE_KEY` (ASCII-armored
  private key), set `MAVEN_GPG_PASSPHRASE` if the key has one, distribute the
  matching public key to a keyserver, and register its fingerprint for the
  `ai.nanosandbox` namespace.
- **crates.io token auth requires a verified email** on the token's account.
  Trusted Publishing (OIDC) bypasses that check but cannot create a new crate, so
  a *first* publish must be done with the token once the email is verified.
- Go has no registry: `publish.sh go` pushes the `sdk/go/v<version>` module tag.

## 2. Dress rehearsal (publishes nothing)

Remote:

```bash
gh workflow run sdk-release.yml -f version=0.2.0 -f dry_run=true
```

Local (all eight ecosystems):

```bash
for s in rust python typescript ruby dotnet java kotlin go; do
  scripts/sdk/publish.sh "$s" 0.2.0 --dry-run
done
```

Expected: `dist/` holds `nanosandbox_sdk-0.2.0-*.whl`, `nanosandbox-sdk-0.2.0.tgz`,
`nanosandbox-sdk-0.2.0.gem`, `Nanosandbox.Sdk.0.2.0.nupkg`, and
`dist/maven-repo/ai/nanosandbox/nanosandbox-{sdk,sdk-kotlin}/0.2.0/*`. No registry
receives a version.

## 3. GA

```bash
test "$(cat VERSION)" = 0.2.0
git tag sdk-v0.2.0 && git push origin sdk-v0.2.0
```

The workflow runs: `plan` (verify VERSION) → `publish` (matrix) → `publish-jvm`
(build the Maven layout → GPG-sign → upload to the Sonatype Central Portal) →
`smoke` (matrix) → GitHub Release.

Verify:

```bash
for s in rust python typescript ruby dotnet java kotlin go; do
  scripts/sdk/check-published.sh "$s" 0.2.0
done   # each prints "already published: <sdk> 0.2.0"
```

## 4. Recovery

- Registries reject re-publishing a version: to fix a bad release, bump `VERSION`
  and cut a new `sdk-v*` tag (**never re-tag**).
- Publish jobs are independent (`fail-fast: false`); re-dispatch re-runs them and
  `scripts/sdk/publish.sh` skips already-published versions.

## 5. Acceptance checklist

- [ ] `scripts/set-sdk-version.sh --check` → `version ok`
- [ ] dry run produces all artifacts, publishes nothing
- [ ] `check-published.sh` returns "already published" for all eight after GA
- [ ] `smoke` matrix green for all eight
- [ ] GitHub Release `sdk-v0.2.0` created
