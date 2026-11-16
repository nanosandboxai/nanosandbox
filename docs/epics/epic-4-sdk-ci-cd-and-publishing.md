# Epic 4 — SDK CI/CD, versioning & public package publishing

Status: in-progress (2026-11-06)
Date: 2026-10-11
Related: `docs/epics/epic-1-multi-language-sdk.md` (the SDKs), `docs/conformance-report.md`, `docs/runtime-setup.md`, `.github/workflows/{ci,release}.yml`

## 1. Context

Epic 1 shipped **eight** protocol clients — Rust, Python, TypeScript/Node, Go,
Ruby, Java, C#/.NET, Kotlin — all validated by one language-agnostic conformance
harness (`crates/conformance` + `sdk/conformance/`). Today:

- `.github/workflows/ci.yml` checks **only the Rust workspace** (CLI + runtime).
- `.github/workflows/release.yml` publishes a **GitHub release** (CLI binaries +
  runtime deps) on `v*` tags. It does **not** build or publish any language SDK.
- No SDK is publishable as-is:
  - `sdk/rust/Cargo.toml` (`nanosandbox` 0.1.0) has no crates.io metadata.
  - `sdk/python/pyproject.toml` (`nanosandbox` 0.1.0) lacks URLs/license/readme.
  - `sdk/typescript/package.json` is `"private": true` with no `files`/`repository`.
  - `sdk/java/pom.xml` (`nanosandbox-java` 1.0.0) has no distributionManagement,
    GPG or sources/javadoc jars.
  - `sdk/dotnet/dotnet.csproj` has no version/package metadata.
  - `sdk/ruby` has no `.gemspec`.
  - `sdk/go` has no release-tag scheme.
  - `sdk/kotlin` produces a jar but no Maven coordinates.
- **Versions are inconsistent** (`0.1.0` vs `1.0.0`) with no single source of truth.

There is no automated way to (a) prove every SDK builds + passes conformance in
CI, or (b) cut a release that publishes to public package managers.

## 2. Goal

Add a **CI/CD pipeline that builds, tests, and conformance-checks every SDK on
every PR**, and a **gated release pipeline that publishes each SDK to its public
package manager** (or its nearest equivalent for Go), with a single versioned
release, provenance/signing where the ecosystem supports it, and per-ecosystem
install docs.

## 3. Non-goals

- Changing the protocol, the conformance harness, or any SDK's client behaviour.
- Building the runtime/CLI release (already covered by `release.yml`).
- Language set expansion beyond the eight shipped in Epic 1.

## 4. Design

### 4.1 One conformance harness, shared across languages

Every driver spawns `nanosb-conformance` and passes `NANOSB_CONFORMANCE_BIN`.
CI therefore builds the harness **once** (job `build-harness`, uploaded as an
artifact) and each language job downloads it. This keeps the matrix cheap and
the contract identical to local runs.

### 4.2 Version: single source of truth

`VERSION` now exists at the repo root (`0.2.0`, added 2026-11-06). A sync script
`scripts/set-sdk-version.sh <version>` rewrites every manifest (not yet written):

| Ecosystem | File(s) | Field |
|---|---|---|
| Rust | `sdk/rust/Cargo.toml` | `package.version` |
| Python | `sdk/python/pyproject.toml` | `[project].version` |
| TypeScript | `sdk/typescript/package.json` | `version` |
| Java | `sdk/java/pom.xml` | `<version>` |
| Kotlin | `sdk/kotlin/build.gradle.kts` (new) | `version` |
| C# | `sdk/dotnet/nanosandbox/Nanosandbox.csproj` (new) | `<Version>` |
| Ruby | `sdk/ruby/nanosandbox.gemspec` (new) | `spec.version` |
| Go | git tag `sdk/go/vX.Y.Z` | (VCS) |

A CI job `version-drift` runs `scripts/set-sdk-version.sh --check` and fails if
any manifest disagrees with `VERSION`.

### 4.3 CI matrix (PR gate)

New workflow `.github/workflows/sdk-ci.yml`, `on: [pull_request, push main]`,
with a job per language (fail-fast off so all report):

| Job | Setup | Build / test | Conformance |
|---|---|---|---|
| `rust` | `dtolnay/rust-toolchain` | `cargo test -p nanosandbox` (and workspace) | `cargo test -p nanosb-conformance` |
| `python` | `actions/setup-python` | `python -m build`; `python -m unittest discover -s sdk/python/tests` | `python sdk/python/conformance/run.py` |
| `typescript` | `actions/setup-node` | `npm test` | `node sdk/typescript/conformance/run.mjs` |
| `go` | `actions/setup-go` | `go test ./sdk/go/...` (via `go.work`) | `go run ./sdk/go/conformance/run.go` |
| `ruby` | `ruby/setup-ruby` | `ruby sdk/ruby/test/test_client.rb` | `ruby -I sdk/ruby/lib sdk/ruby/conformance/run.rb` |
| `java` | `actions/setup-java` + maven cache | `mvn -f sdk/java/pom.xml test` | compile + `java … Run` |
| `kotlin` | `actions/setup-java`; `kotlinc` (sdkman/brew) or Gradle wrapper | `bash sdk/kotlin/build.sh` | `java … RunKt` |
| `dotnet` | `actions/setup-dotnet` | `dotnet test sdk/dotnet` | `dotnet run --project sdk/dotnet/conformance` |

All jobs download the `build-harness` artifact and export
`NANOSB_CONFORMANCE_BIN`. Every job must report **`35 passed, 0 failed`**.

### 4.4 Release pipeline (tag → publish)

New workflow `.github/workflows/sdk-release.yml`, `on: push tags ['sdk-v*']`
(separate tag namespace from the product `v*` release, or reuse `v*` and add
SDK publishing jobs to `release.yml`). Steps:

1. `version-drift` check (tag version == `VERSION`).
2. Re-run the full conformance matrix (publish only green).
3. **Publish** each SDK, gated by a protected GitHub **environment**
   (`sdk-release`, required reviewers):
   - **Rust** → crates.io: `cargo publish` (token `CARGO_REGISTRY_TOKEN`).
   - **Python** → PyPI: `python -m build` + `pypa/gh-action-pypi-publish`
     (**trusted publishing / OIDC**, no token).
   - **TypeScript** → npm: `npm publish --provenance --access public`
     (**OIDC provenance**; remove `"private": true`).
   - **Java** → GitHub-hosted Maven repository (GitHub Pages;
     `ai.nanosandbox:nanosandbox-sdk`): build the Maven layout with `mvn deploy`
     to a local dir and publish it to a `gh-pages` branch. No registration, no
     GPG, no namespace verification.
   - **Kotlin** → same GitHub-hosted Maven repository (artifact
     `ai.nanosandbox:nanosandbox-sdk-kotlin`).
   - **C#/.NET** → NuGet: `dotnet pack -c Release` + `dotnet nuget push`
     (`NUGET_API_KEY`; optional `dotnet nuget sign`).
   - **Ruby** → RubyGems: `gem build sdk/ruby/nanosandbox.gemspec` +
     `gem push` (`RUBYGEMS_API_KEY`; optional `gem sign`).
   - **Go** → VCS: create the `sdk/go/vX.Y.Z` tag so the Go module proxy serves
     `github.com/nanosandboxai/nanosandbox/sdk/go`; verify with
     `GOPROXY=… go list -m …@vX.Y.Z`.
4. Dry-run mode: `workflow_dispatch` input `dry_run: true` runs packaging +
   `--dry-run`/`--no-publish` equivalents and uploads the artifacts, publishing
   nothing.
5. Attach provenance: **Sigstore/cosign** keyless signatures for tarballs/jars
   where the ecosystem lacks native signing; generate an SBOM (syft) per SDK.
6. Emit release notes / changelog from conventional commits.

### 4.5 Publish metadata to add (per ecosystem)

- **Rust**: `description`, `license`, `repository`, `readme`, `keywords`,
  `categories`, `include`.
- **Python**: `readme`, `license`, `authors`, `[project.urls]`, `classifiers`.
- **TypeScript**: drop `private`, add `repository`, `files`, `license`,
  `publishConfig.access=public`.
- **Java/Kotlin**: `licenses`, `developers`, `scm` (the GitHub-hosted Maven
  repository needs no `distributionManagement` and no GPG).
- **.NET**: `PackageId`, `Version`, `Authors`, `Description`, `PackageLicenseExpression`,
  `RepositoryUrl`, `PackageReadmeFile`, `IncludeSymbols`.
- **Ruby**: new `nanosandbox.gemspec` (`name`, `version`, `summary`,
  `license`, `homepage`, `required_ruby_version`, `files`).
- **Go**: ensure the module path is stable + tag scheme documented.

### 4.6 Docker/registry images

Out of scope for the first cut; if the SDKs ever ship container images they join
the existing `registry-*.yml` flows.

## 5. Workstreams

- **WS1 — CI matrix** (4.3): `sdk-ci.yml`, shared harness artifact, per-language
  jobs, all-green conformance.
- **WS2 — Version source of truth** (4.2): `VERSION`, `set-sdk-version.sh`,
  `--check`, drift job.
- **WS3 — Publish metadata** (4.5): manifests for all eight ecosystems.
- **WS4 — Release pipeline** (4.4): `sdk-release.yml`, environment gate,
  per-ecosystem publish, dry-run.
- **WS5 — Signing & provenance** (4.4): PyPI trusted publishing, npm provenance,
  Maven GPG, NuGet/RubyGems signing, cosign + SBOM.
- **WS6 — Secrets & policy**: OIDC first; document each token, its scope, and
  the environment that holds it; least privilege; no secret in logs.
- **WS7 — Docs**: per-ecosystem install snippets (`pip install nanosandbox`,
  `npm i nanosandbox`, `cargo add nanosandbox`, `go get …/sdk/go`,
  `gem install nanosandbox-sdk`, `dotnet add package nanosandbox-sdk`, Maven/Kotlin
  coordinates), checked into each SDK README + `docs/install/`.

## 6. Acceptance criteria

- **AC1** Every PR runs `sdk-ci.yml`; all eight language jobs build, run unit
  tests, and report **`35 passed, 0 failed`** conformance.
- **AC2** A single root `VERSION` is the only place the version is set; one
  script updates all manifests; a CI job fails on drift.
- **AC3** Each SDK produces a valid, install-ready artifact in a dry run:
  `cargo package`, `python -m build`, `npm pack`, `mvn -DperformRelease=true package`
  (signed in release), `dotnet pack`, `gem build`, Kotlin jar + POM.
- **AC4** Pushing a release tag publishes every SDK to its package manager, gated
  by a protected environment, and is re-runnable (idempotent) — a second run with
  the same version is a no-op or clean failure, never a corrupt publish.
- **AC5** PyPI uses trusted publishing (OIDC); npm publishes with `--provenance`;
  Maven Central is GPG-signed; artifacts carry cosign signatures + an SBOM.
- **AC6** A documented `dry_run` path publishes nothing but uploads all artifacts
  for inspection.
- **AC7** Each SDK README documents the exact install command; a prerelease
  published to each ecosystem installs cleanly in a scratch environment.
- **AC8** No long-lived secret is required for PyPI/npm (OIDC); every remaining
  token is documented with its scope and stored in a GitHub environment.

## 7. Milestones

- **M0** `VERSION` + `set-sdk-version.sh` + `sdk-ci.yml` matrix (WS1, WS2) — the
  PR gate and the version contract.
- **M1** Publish metadata for all eight ecosystems + dry-run packaging (WS3, WS4
  dry-run) — `AC3`, `AC6`.
- **M2** Gated release pipeline publishing to all ecosystems (WS4) — `AC4`.
- **M3** Signing & provenance (WS5, WS6) — `AC5`, `AC8`.
- **M4** Install docs + first public `1.0.0` GA (WS7) — `AC7`.

## 8. Risks

- **R1 Registry namespace (RESOLVED)** — `nanosandbox` is taken on npm, PyPI and
  crates.io by an unrelated project, so the SDKs publish as `nanosandbox-sdk`
  (Rust/Python/npm); Ruby keeps `nanosandbox`, .NET keeps `Nanosandbox`.
- **R2 Maven Central groupId (DROPPED)** — Maven Central is out of scope; the JVM
  SDKs publish to a GitHub-hosted Maven repository (GitHub Pages) that needs no
  namespace verification and no GPG key.
- **R3 Toolchain availability in CI** — Kotlin needs `kotlinc` (no official
  GitHub action); use a Gradle wrapper or `sdkman`. Go submodule tagging is easy
  to get wrong.
- **R4 Version drift** — mitigated by AC2's drift job.
- **R5 Secret handling** — prefer OIDC; a leaked registry token can publish
  malicious releases. Environment gates + least privilege + no secret echo.
- **R6 Idempotency** — some registries reject re-publishing a version; the
  pipeline must detect "already published" and skip, not fail the whole release.

## 9. Decisions (locked 2026-11-06)

- **D1 Tag namespace:** `sdk-v*` (does not match the product `v*` triggers).
- **D2 Names:** Rust/Python/npm/Ruby/.NET → `nanosandbox-sdk`; Go module path
  unchanged; Java → `ai.nanosandbox:nanosandbox-sdk`, Kotlin →
  `ai.nanosandbox:nanosandbox-sdk-kotlin`.
- **D3 JVM target:** GitHub-hosted Maven repository (GitHub Pages) — no Maven
  Central, no GPG, no Sonatype account, no fallback.
- **D4 Kotlin build:** introduce a Gradle `maven-publish` build in `sdk/kotlin`.
- **D5 Mirroring:** none in v1 (the registry / GitHub Pages repo is the
  distribution).
- **Version:** `0.2.0` (current workspace version); Go is tagged only.
- **Q3 (Go):** module-proxy-only (git tags).
- **Q4 (release shape):** independent SDK releases via `sdk-v*` tags.
