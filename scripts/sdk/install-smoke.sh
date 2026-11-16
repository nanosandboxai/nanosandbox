#!/usr/bin/env bash
#
# install-smoke.sh <sdk> <version> [--artifact <path>]
#
# Proves a consumer can install the SDK from the real package manager (default)
# or from a local built artifact, in a scratch environment, and then use it.
# No VM / helper is required.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
sdk="${1:?usage: install-smoke.sh <sdk> <version> [--artifact <path>]}"
ver="${2:?usage: install-smoke.sh <sdk> <version> [--artifact <path>]}"
shift 2

artifact=""
while [ $# -gt 0 ]; do
  case "$1" in
    --artifact) artifact="${2:?--artifact needs a path}"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
mode=registry; [ -n "$artifact" ] && mode=artifact

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
say() { printf 'install-smoke[%s] %s\n' "$sdk" "$*"; }
GRADLE_BIN="${GRADLE:-gradle}"

case "$sdk" in
  rust)
    (cd "$work" && cargo new --quiet smoke && cd smoke)
    if [ "$mode" = artifact ]; then
      (cd "$work/smoke" && cargo add --quiet nanosandbox-sdk --path "$root/sdk/rust")
    else
      (cd "$work/smoke" && cargo add --quiet "nanosandbox-sdk@$ver")
    fi
    printf 'fn main() { println!("{:?}", std::any::type_name::<nanosandbox_sdk::SandboxClient>()); }\n' > "$work/smoke/src/main.rs"
    (cd "$work/smoke" && cargo run --quiet)
    ;;

  python)
    python3 -m venv "$work/venv"
    if [ "$mode" = artifact ]; then
      "$work/venv/bin/pip" -q install "$artifact"
    else
      "$work/venv/bin/pip" -q install "nanosandbox-sdk==$ver"
    fi
    "$work/venv/bin/python" -c 'import nanosandbox; assert nanosandbox.SandboxClient; print("ok")'
    ;;

  typescript|npm)
    mkdir -p "$work/app"; cd "$work/app"; npm init -y >/dev/null
    if [ "$mode" = artifact ]; then
      npm install --silent "$artifact"
    else
      npm install --silent "nanosandbox-sdk@$ver"
    fi
    node -e 'import("nanosandbox-sdk").then(m => { if (!m.SandboxClient) throw new Error("no SandboxClient"); console.log("ok"); })'
    ;;

  go)
    mkdir -p "$work/app"; cd "$work/app"; go mod init example.com/smoke >/dev/null
    go get "github.com/nanosandboxai/nanosandbox/sdk/go@v$ver"
    cat > main.go <<'EOF'
package main

import (
	"fmt"

	"github.com/nanosandboxai/nanosandbox/sdk/go/nanosandbox"
)

func main() {
	var _ *nanosandbox.SandboxClient
	fmt.Println("ok")
}
EOF
    go build ./...
    ;;

  ruby)
    export GEM_HOME="$work/gems" GEM_PATH="$work/gems"
    if [ "$mode" = artifact ]; then
      gem install --local "$artifact" --no-document
    else
      gem install nanosandbox-sdk -v "$ver" --no-document
    fi
    ruby -e 'require "nanosandbox"; abort "no client" unless defined?(Nanosandbox::SandboxClient); puts "ok"'
    ;;

  java)
    mkdir -p "$work/app/src/main/java/smoke"
    cat > "$work/app/pom.xml" <<EOF
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>smoke</groupId>
  <artifactId>smoke</artifactId>
  <version>1.0.0</version>
  <properties><maven.compiler.release>21</maven.compiler.release></properties>
  <repositories>
    <repository>
      <id>nanosandbox-pages</id>
      <url>https://nanosandboxai.github.io/maven/</url>
    </repository>
  </repositories>
  <dependencies>
    <dependency>
      <groupId>ai.nanosandbox</groupId>
      <artifactId>nanosandbox-sdk</artifactId>
      <version>$ver</version>
    </dependency>
  </dependencies>
</project>
EOF
    cat > "$work/app/src/main/java/smoke/Main.java" <<'EOF'
package smoke;

public class Main {
    public static void main(String[] args) {
        System.out.println(nanosandbox.SandboxClient.class.getName());
    }
}
EOF
    (cd "$work/app" && mvn -q -B -DskipTests compile)
    ;;

  kotlin)
    mkdir -p "$work/app/src/main/kotlin"
    cat > "$work/app/settings.gradle.kts" <<'EOF'
rootProject.name = "smoke"
EOF
    cat > "$work/app/build.gradle.kts" <<EOF
plugins {
    kotlin("jvm") version "2.2.20"
    application
}
repositories {
    mavenCentral()
    maven { url = uri("https://nanosandboxai.github.io/maven/") }
}
dependencies {
    implementation("ai.nanosandbox:nanosandbox-sdk-kotlin:$ver")
}
application {
    mainClass.set("MainKt")
}
EOF
    cat > "$work/app/src/main/kotlin/Main.kt" <<'EOF'
fun main() {
    println(nanosandbox.kt.KotlinClient::class.java.name)
}
EOF
    (cd "$work/app" && "$GRADLE_BIN" --no-daemon -q build)
    ;;

  dotnet)
    (cd "$work" && dotnet new console -n app -o app >/dev/null)
    if [ "$mode" = artifact ]; then
      dotnet nuget add source "$(cd "$(dirname "$artifact")" && pwd)" -n local >/dev/null
      (cd "$work/app" && dotnet add package nanosandbox-sdk --version "$ver" && dotnet build -c Release)
    else
      (cd "$work/app" && dotnet add package nanosandbox-sdk --version "$ver" && dotnet build -c Release)
    fi
    ;;

  *)
    echo "unknown sdk: $sdk" >&2
    exit 2
    ;;
esac

say "ok ($mode)"
