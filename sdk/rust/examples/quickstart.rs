use nanosandbox_sdk::{ManagedRuntime, Result, SandboxBuilder, SandboxClient};

fn main() -> Result<()> {
    let rt = ManagedRuntime::start().unwrap();
    let client = SandboxClient::connect(rt.socket())?;
    let sandbox = client.create_with(
        SandboxBuilder::new("library/alpine:latest")
            .name("demo-rust")
            .cpus(1)
            .memory_mb(512),
    )?;

    sandbox.start()?;
    let out = sandbox.exec("echo", &["hello from rust"])?;
    print!("{}", out.stdout);

    sandbox.remove(true)?;
    drop(rt);
    Ok(())
}
