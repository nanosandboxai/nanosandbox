#!/usr/bin/env bash
#
# live-e2e.sh <sdk> <version>
#
# End-to-end test of a PUBLISHED SDK: installs it from its registry into a
# scratch project, starts the nanosandbox runtime (via the SDK's ManagedRuntime,
# or by spawning nanosb-runtime for Rust), boots a real microVM, runs a command
# through the SDK, and asserts the output. Prints LIVE_E2E_OK on success.
#
# Requires a working nanosandbox runtime (nanosb-runtime helper + libkrunfw +
# gvproxy) and NANOSB_RUNTIME_BIN pointing at nanosb-runtime.
set -euo pipefail

sdk="${1:?usage: live-e2e.sh <sdk> <version>}"
ver="${2:?usage: live-e2e.sh <sdk> <version>}"
IMAGE="${NANOSB_TEST_IMAGE:-library/alpine:latest}"
RT_BIN="${NANOSB_RUNTIME_BIN:-$HOME/.nanosandbox/bin/nanosb-runtime}"
export NANOSB_HOME="${NANOSB_HOME:-$HOME/.nanosandbox}"
export NANOSB_RUNTIME_BIN="$RT_BIN"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
say() { printf 'live-e2e[%s] %s\n' "$sdk" "$*"; }
[ -x "$RT_BIN" ] || { echo "error: NANOSB_RUNTIME_BIN not executable: $RT_BIN" >&2; exit 1; }

case "$sdk" in
  rust)
    (cd "$work" && cargo new --quiet app && cd app)
    (cd "$work/app" && cargo add --quiet "nanosandbox-sdk@$ver")
    cat > "$work/app/src/main.rs" <<'RS'
use nanosandbox_sdk::{SandboxBuilder, SandboxClient};
use std::process::Command;
use std::time::{Duration, Instant};

fn main() {
    let bin = std::env::var("NANOSB_RUNTIME_BIN").expect("NANOSB_RUNTIME_BIN");
    let image = std::env::var("NANOSB_TEST_IMAGE").unwrap_or_else(|_| "library/alpine:latest".into());
    let dir = std::env::temp_dir().join(format!("nsb-rust-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("helper.sock");
    let mut child = Command::new(bin).arg("serve").arg("--socket").arg(&socket).spawn().expect("spawn helper");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "helper socket never appeared");
        std::thread::sleep(Duration::from_millis(50));
    }
    let client = SandboxClient::connect(&socket).expect("connect");
    assert!(!client.capabilities().is_empty(), "no capabilities");
    let name = format!("e2e-{}", std::process::id());
    let sandbox = client.create_with(SandboxBuilder::new(&image).name(&name).cpus(2).memory_mb(1024)).expect("create");
    assert_eq!(format!("{:?}", sandbox.state().unwrap()), "Running");
    let out = sandbox.exec("echo", &["hello-from-sdk"]).expect("exec");
    assert_eq!(out.exit_code, 0, "exit {}", out.exit_code);
    assert_eq!(out.stdout.trim(), "hello-from-sdk", "stdout {:?}", out.stdout);
    let _ = child.kill();
    println!("LIVE_E2E_OK");
}
RS
    (cd "$work/app" && cargo run --quiet)
    ;;

  python)
    python3 -m pip install -q --target "$work/site" "nanosandbox-sdk==$ver"
    cat > "$work/app.py" <<'PY'
import os
from nanosandbox import SandboxClient
from nanosandbox.runtime import ManagedRuntime

name = f"e2e-{os.getpid()}"
image = os.environ.get("NANOSB_TEST_IMAGE", "library/alpine:latest")
with ManagedRuntime() as rt:
    client = SandboxClient.connect(str(rt.socket))
    assert client.capabilities(), "no capabilities"
    box = client.create({"image": image, "name": name, "cpus": 2, "memory_mb": 1024, "detached": True})
    assert box.state() == "running", box.state()
    out = box.exec("echo", ["hello-from-sdk"])
    assert out["exit_code"] == 0 and out["stdout"].strip() == "hello-from-sdk", out
    box.stop(True)
    client.sandbox(name).remove(True)
print("LIVE_E2E_OK")
PY
    PYTHONPATH="$work/site" python3 "$work/app.py"
    ;;

  typescript)
    mkdir -p "$work/app"; (cd "$work/app" && npm init -y >/dev/null)
    (cd "$work/app" && npm install --silent "nanosandbox-sdk@$ver")
    cat > "$work/app/main.mjs" <<'JS'
import { SandboxClient, ManagedRuntime } from "nanosandbox-sdk";

const image = process.env.NANOSB_TEST_IMAGE || "library/alpine:latest";
const name = `e2e-${process.pid}`;
const rt = await ManagedRuntime.start();
try {
  const client = await SandboxClient.connect(rt.socket);
  const box = await client.create({ image, name, cpus: 2, memory_mb: 1024, detached: true });
  if ((await box.state()) !== "running") throw new Error("not running");
  const out = await box.exec("echo", ["hello-from-sdk"]);
  if (out.exit_code !== 0 || out.stdout.trim() !== "hello-from-sdk") throw new Error(JSON.stringify(out));
  await box.stop(true);
  await client.sandbox(name).remove(true);
} finally {
  await rt.close();
}
console.log("LIVE_E2E_OK");
JS
    (cd "$work/app" && node main.mjs)
    ;;

  go)
    mkdir -p "$work/app"; (cd "$work/app" && go mod init example.com/e2e >/dev/null)
    (cd "$work/app" && go get "github.com/nanosandboxai/nanosandbox/sdk/go@v$ver")
    cat > "$work/app/main.go" <<'GO'
package main

import (
	"fmt"
	"os"
	"time"

	"github.com/nanosandboxai/nanosandbox/sdk/go/nanosandbox"
)

func main() {
	rt, err := nanosandbox.StartRuntime(15 * time.Second)
	if err != nil { panic(err) }
	defer rt.Close()
	client, err := nanosandbox.Connect(rt.Socket)
	if err != nil { panic(err) }
	if len(client.Capabilities()) == 0 { panic("no capabilities") }
	name := fmt.Sprintf("e2e-%d", os.Getpid())
	spec := nanosandbox.NewSandboxBuilder("library/alpine:latest").WithName(name).WithResources(2, 1024).Build()
	box, err := client.Create(spec)
	if err != nil { panic(err) }
	if st, _ := box.State(); st != "running" { panic("state " + st) }
	out, err := box.Exec("echo", []string{"hello-from-sdk"})
	if err != nil { panic(err) }
	code, _ := out.Num("exit_code")
	stdout, _ := out.Str("stdout")
	if int(code) != 0 || stdout != "hello-from-sdk\n" { panic(fmt.Sprintf("out %v %q", code, stdout)) }
	fmt.Println("LIVE_E2E_OK")
}
GO
    (cd "$work/app" && go run .)
    ;;

  ruby)
    export GEM_HOME="$work/gems" GEM_PATH="$work/gems"
    gem install nanosandbox-sdk -v "$ver" --no-document
    cat > "$work/app.rb" <<'RB'
require "nanosandbox"
require "nanosandbox/runtime"

image = ENV["NANOSB_TEST_IMAGE"] || "library/alpine:latest"
name = "e2e-#{Process.pid}"
Nanosandbox::ManagedRuntime.open do |rt|
  client = Nanosandbox::SandboxClient.connect(rt.socket)
  box = client.create("image" => image, "name" => name, "cpus" => 2, "memory_mb" => 1024, "detached" => true)
  raise "state #{box.state}" unless box.state == "running"
  out = box.exec("echo", ["hello-from-sdk"])
  raise out.inspect unless out["exit_code"] == 0 && out["stdout"].strip == "hello-from-sdk"
end
puts "LIVE_E2E_OK"
RB
    ruby "$work/app.rb"
    ;;

  java)
    mkdir -p "$work/app/src/main/java/e2e"
    cat > "$work/app/pom.xml" <<POM
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>e2e</groupId><artifactId>e2e</artifactId><version>1.0.0</version>
  <properties><maven.compiler.release>21</maven.compiler.release></properties>
  <dependencies>
    <dependency><groupId>ai.nanosandbox</groupId><artifactId>nanosandbox-sdk</artifactId><version>$ver</version></dependency>
  </dependencies>
</project>
POM
    cat > "$work/app/src/main/java/e2e/Main.java" <<'JAVA'
package e2e;
import nanosandbox.*;
public class Main {
  public static void main(String[] args) throws Exception {
    try (ManagedRuntime rt = ManagedRuntime.start()) {
      SandboxClient client = SandboxClient.connect(rt.socket.toString());
      if (client.capabilities().isEmpty()) throw new RuntimeException("no capabilities");
      Sandbox box = client.createWith(new SandboxBuilder("library/alpine:latest").withName("e2e-java-" + ProcessHandle.current().pid()).withResources(2, 1024));
      if (!"running".equals(box.state())) throw new RuntimeException("state " + box.state());
      Json out = box.exec("echo", new String[]{"hello-from-sdk"});
      if (out.getInt("exit_code") != 0 || !"hello-from-sdk".equals(out.getStr("stdout").trim())) throw new RuntimeException(out.toString());
    }
    System.out.println("LIVE_E2E_OK");
  }
}
JAVA
    (cd "$work/app" && mvn -q -B -DskipTests compile && mvn -q -B exec:java -Dexec.mainClass=e2e.Main)
    ;;

  dotnet)
    (cd "$work" && dotnet new console -n app -o app >/dev/null)
    (cd "$work/app" && dotnet add package Nanosandbox.Sdk --version "$ver" >/dev/null)
    cat > "$work/app/Program.cs" <<'CS'
using System;
using System.Collections.Generic;

var image = Environment.GetEnvironmentVariable("NANOSB_TEST_IMAGE") ?? "library/alpine:latest";
var name = "e2e-" + Environment.ProcessId;
using (var rt = ManagedRuntime.start())
{
    var client = SandboxClient.connect(rt.Socket);
    if (client.capabilities().Count == 0) throw new Exception("no capabilities");
    var box = client.create(new SandboxBuilder(image).withName(name).withResources(2, 1024).build());
    if (box.state().str("state") != "running") throw new Exception("state");
    var outv = box.exec("echo", new List<String> { "hello-from-sdk" });
    if (outv.intVal("exit_code") != 0 || outv.str("stdout").Trim() != "hello-from-sdk") throw new Exception(outv.ToString());
}
Console.WriteLine("LIVE_E2E_OK");
CS
    (cd "$work/app" && dotnet run -c Release)
    ;;

  kotlin)
    mkdir -p "$work/app/src/main/kotlin"
    cat > "$work/app/settings.gradle.kts" <<'GK'
rootProject.name = "e2e"
GK
    cat > "$work/app/build.gradle.kts" <<GK
plugins { kotlin("jvm") version "2.2.20"; application }
kotlin { jvmToolchain(21) }
repositories { mavenCentral() }
dependencies { implementation("ai.nanosandbox:nanosandbox-sdk-kotlin:$ver") }
application { mainClass.set("MainKt") }
GK
    cat > "$work/app/src/main/kotlin/Main.kt" <<'KT'
import nanosandbox.ManagedRuntime
import nanosandbox.SandboxBuilder
import nanosandbox.kt.KotlinClient

fun main() {
    val image = System.getenv("NANOSB_TEST_IMAGE") ?: "library/alpine:latest"
    val rt = ManagedRuntime.start()
    try {
        val client = KotlinClient.connect(rt.socket.toString())
        check(client.capabilities().isNotEmpty()) { "no capabilities" }
        val box = client.create(SandboxBuilder(image).withName("e2e-kotlin-${ProcessHandle.current().pid()}").withResources(2, 1024).build())
        check(box.state() == "running") { "state ${box.state()}" }
        val out = box.exec("echo", arrayOf("hello-from-sdk"))
        check(out.getInt("exit_code") == 0L && out.getStr("stdout").trim() == "hello-from-sdk") { out.toString() }
    } finally { rt.close() }
    println("LIVE_E2E_OK")
}
KT
    (cd "$work/app" && "${GRADLE:-gradle}" --no-daemon -q run)
    ;;

  *)
    echo "unknown sdk: $sdk" >&2; exit 2 ;;
esac

say "ok"
