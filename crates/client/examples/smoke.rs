//! Real end-to-end smoke test for the protocol helper.
//!
//! Start the daemon first:
//!   target/debug/nanosb-runtime serve --socket /tmp/nsb-e2e/helper.sock
//! Then run:
//!   cargo run -p nanosb-client --example smoke -- /tmp/nsb-e2e/helper.sock

use nanosb_client::{SandboxBuilder, SandboxClient};

fn main() {
    let socket = std::env::args().nth(1).expect("usage: smoke <helper.sock>");
    let client = SandboxClient::connect(&socket).expect("connect to helper");
    println!("capabilities: {:?}", client.capabilities());

    let name = format!("sdk-smoke-{}", std::process::id());
    let sandbox = client
        .create_with(
            SandboxBuilder::new("library/alpine:latest")
                .name(&name)
                .cpus(2)
                .memory_mb(1024),
        )
        .expect("create sandbox");
    println!("created: {} state={:?}", sandbox.name(), sandbox.state());

    let r = sandbox.exec("echo", &["hello-from-sdk"]).expect("exec");
    println!("exec: code={} stdout={:?}", r.exit_code, r.stdout.trim());

    sandbox
        .fs_write("/tmp/nanosc", b"written-by-sdk")
        .expect("fs_write");
    println!(
        "fs: read={:?} exists={}",
        sandbox.fs_read_to_string("/tmp/nanosc").expect("fs_read"),
        sandbox.fs_exists("/tmp/nanosc").expect("fs_exists"),
    );
    let listing = sandbox.fs_list("/tmp").expect("fs_list");
    println!("fs: /tmp entries={}", listing.len());

    let logs = sandbox.logs(Some(5)).expect("logs");
    println!(
        "logs: {} lines (last={:?})",
        logs.len(),
        logs.last().map(|l| l.data.clone())
    );

    let metrics = sandbox.metrics().expect("metrics");
    println!(
        "metrics: cpu={} mem={} disk={}",
        metrics.cpu_percent, metrics.memory_used_bytes, metrics.disk_used_bytes
    );

    let all = client.list(true).expect("list");
    println!(
        "list: {:?}",
        all.iter()
            .map(|s| (s.name.clone(), s.state))
            .collect::<Vec<_>>()
    );

    sandbox.stop(true).expect("stop");
    println!("stopped");

    let handle = client.sandbox(&name);
    handle.remove(true).expect("remove");
    println!("removed");
}
