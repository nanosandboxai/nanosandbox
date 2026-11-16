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
curl -fS -X POST "https://central.sonatype.com/api/v1/publisher/upload" \
  --user "$MAVEN_TOKEN" \
  -F "bundle=@$bundle" \
  -F "publishingType=AUTOMATIC"
echo
note "submitted (version $ver)"
