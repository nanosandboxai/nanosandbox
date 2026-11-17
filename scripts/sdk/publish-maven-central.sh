#!/usr/bin/env bash
#
# publish-maven-central.sh <version> [--dry-run]
#
# Signs the staged JVM Maven layout (dist/maven-repo, produced by
# `publish.sh java` + `publish.sh kotlin`) with GPG and uploads it as a bundle
# to the Sonatype Central Portal (Maven Central).
#
# Requires MAVEN_TOKEN (Central Portal user token, "user:pass") and, unless
# --dry-run, MAVEN_GPG_PRIVATE_KEY (and optionally MAVEN_GPG_PASSPHRASE).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
ver="${1:?usage: publish-maven-central.sh <version> [--dry-run]}"
dry=0
[ "${2:-}" = "--dry-run" ] && dry=1

repo="$root/dist/maven-repo"
[ -d "$repo" ] || { echo "error: $repo missing (run publish.sh java + kotlin first)" >&2; exit 1; }

note() { printf 'maven-central: %s\n' "$*"; }
artifacts() { find "$repo" -type f \( -name '*.jar' -o -name '*.pom' -o -name '*.module' \); }

if [ "$dry" = 0 ]; then
  : "${MAVEN_TOKEN:?MAVEN_TOKEN is required}"
  [ -n "${MAVEN_GPG_PRIVATE_KEY:-}" ] || { echo "error: MAVEN_GPG_PRIVATE_KEY is required (Maven Central mandates GPG signatures)" >&2; exit 1; }
  GNUPGHOME="$(mktemp -d)"
  export GNUPGHOME
  printf '%s' "$MAVEN_GPG_PRIVATE_KEY" | gpg --batch --quiet --import
  KEYID="$(gpg --list-secret-keys --with-colons | awk -F: '/^sec:/{print $5; exit}')"
  while IFS= read -r f; do
    if [ -n "${MAVEN_GPG_PASSPHRASE:-}" ]; then
      gpg --batch --yes --pinentry-mode loopback --passphrase "$MAVEN_GPG_PASSPHRASE" --local-user "$KEYID" --detach-sign --armor "$f"
    else
      gpg --batch --yes --pinentry-mode loopback --local-user "$KEYID" --detach-sign --armor "$f"
    fi
  done < <(artifacts)
fi

while IFS= read -r f; do
  openssl dgst -sha1 -r "$f" | awk '{print $1}' > "$f.sha1"
  openssl dgst -md5  -r "$f" | awk '{print $1}' > "$f.md5"
  if [ -f "$f.asc" ]; then
    openssl dgst -sha1 -r "$f.asc" | awk '{print $1}' > "$f.asc.sha1"
    openssl dgst -md5  -r "$f.asc" | awk '{print $1}' > "$f.asc.md5"
  fi
done < <(artifacts)

bundle="$root/dist/maven-central-bundle.zip"
rm -f "$bundle"
( cd "$repo" && zip -qr "$bundle" . )
note "bundle $bundle ($(du -h "$bundle" | cut -f1))"

if [ "$dry" = 1 ]; then
  note "dry-run: not uploading"
  exit 0
fi

note "uploading to Sonatype Central Portal"
# Portal auth = `Authorization: Bearer base64(username:password)`; accept either
# the raw "username:password" pair or an already base64-encoded token.
if printf '%s' "$MAVEN_TOKEN" | grep -q ':'; then
  MAVEN_TOKEN="$(printf '%s' "$MAVEN_TOKEN" | base64 | tr -d '\n')"
fi
auth=(-H "Authorization: Bearer $MAVEN_TOKEN")

deployment_id="$(curl -fS -X POST "https://central.sonatype.com/api/v1/publisher/upload" \
  "${auth[@]}" \
  -F "bundle=@$bundle" \
  -F "publishingType=AUTOMATIC" | tr -d '\r\n')"
[ -n "$deployment_id" ] || { echo "error: Portal returned no deployment id" >&2; exit 1; }
note "deployment $deployment_id submitted"

# Wait (best-effort) for the deployment to reach PUBLISHED; only a FAILED
# validation is fatal, so a slow/unparsable status never reds a good release.
for _ in $(seq 1 60); do
  status="$(curl -fsS "${auth[@]}" \
    "https://central.sonatype.com/api/v1/publisher/status?id=$deployment_id" 2>/dev/null || true)"
  state="$(printf '%s' "$status" | sed -nE 's/.*"deploymentState" *: *"([A-Z_]+)".*/\1/p')"
  [ -n "$state" ] || state="$(printf '%s' "$status" | sed -nE 's/.*"state" *: *"([A-Z_]+)".*/\1/p')"
  case "$state" in
    PUBLISHED) note "deployment $deployment_id PUBLISHED"; break ;;
    FAILED) echo "error: deployment $deployment_id FAILED: $status" >&2; exit 1 ;;
    "") note "deployment $deployment_id status unparsed: ${status:-<empty>}; waiting"; sleep 5 ;;
    *) note "deployment $deployment_id state=$state; waiting"; sleep 5 ;;
  esac
done
note "submitted (version $ver)"
