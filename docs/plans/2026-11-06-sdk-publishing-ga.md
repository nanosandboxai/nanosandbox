# SDK publishing — GA runbook

Status: pipeline complete; the real publish is gated only on external
credential provisioning. Branch: `feat/sdk-publishing`.
Related: `docs/epics/epic-4-sdk-ci-cd-and-publishing.md`.

## 1. One-time provisioning (org admin)

| Registry | Provisioning | Job |
|---|---|---|
| crates.io | After a first manual `cargo publish` of `nanosb-protocol`/`nanosb-client`/`nanosandbox-sdk`, add a Trusted Publisher (repo `nanosandboxai/nanosandbox`, workflow `sdk-release.yml`) | `publish` (rust) |
| PyPI | Add a Trusted Publisher for project `nanosandbox-sdk` (workflow `sdk-release.yml`, environment `sdk-release`) | `publish` (python) |
| npm | Add a Trusted Publisher for `nanosandbox-sdk` | `publish` (typescript) |
| RubyGems | Add a Trusted Publisher for gem `nanosandbox` | `publish` (ruby) |
| NuGet | Add a Trusted Publisher for `Nanosandbox` and/or set the `NUGET_API_KEY` secret | `publish` (dotnet) |
| GitHub Pages | A `gh-pages` branch hosting the Maven repository at `https://nanosandboxai.github.io/maven/` | `publish-jvm` |
| GitHub | Protected environment `sdk-release` (required reviewers) | all publish jobs |

crates.io / PyPI / npm / RubyGems use OIDC (`id-token: write`); the only
long-lived secret is `NUGET_API_KEY`.

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
`nanosandbox-0.2.0.gem`, `Nanosandbox.0.2.0.nupkg`, and
`dist/maven-repo/ai/nanosandbox/nanosandbox-{java,kotlin}/0.2.0/*`. No registry
receives a version.

## 3. GA

```bash
test "$(cat VERSION)" = 0.2.0
git tag sdk-v0.2.0 && git push origin sdk-v0.2.0
```

The workflow runs: `plan` (verify VERSION) → `publish` (matrix) → `publish-jvm`
(Maven layout → GitHub Pages) → `smoke` (matrix) → GitHub Release.

Verify:

```bash
for s in rust python typescript ruby dotnet java kotlin go; do
  scripts/sdk/check-published.sh "$s" 0.2.0
done   # each prints "already published: <sdk> 0.2.0"
```

## 4. Recovery

- Registries reject re-publishing a version: to fix a bad release, bump `VERSION`
  and cut a new `sdk-v*` tag (never re-tag).
- Publish jobs are independent (`fail-fast: false`); re-dispatch re-runs them and
  `scripts/sdk/publish.sh` skips already-published versions.
- The JVM Pages deploy runs only when not dry-run and overwrites the layout
  idempotently.

## 5. Acceptance checklist

- [ ] `scripts/set-sdk-version.sh --check` → `version ok`
- [ ] dry run produces all artifacts, publishes nothing
- [ ] `check-published.sh` returns "already published" for all eight after GA
- [ ] `smoke` matrix green for all eight
- [ ] GitHub Release `sdk-v0.2.0` created
