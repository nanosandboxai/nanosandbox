//! `ManagedRuntime` — locate, provision, and spawn the `nanosb-runtime` helper.
//!
//! Lets a Rust SDK user manage *real* sandboxes without starting the helper by
//! hand. The helper (`nanosb-runtime serve`) owns the microVM lifecycle; SDK
//! calls are protocol messages over its Unix socket.
//!
//! If the helper is not installed, `ManagedRuntime` provisions it: it runs the
//! shared `ensure-runtime.sh` provisioner (located via `NANOSB_PROVISIONER` →
//! the repo `scripts/sdk/ensure-runtime.sh` → an embedded copy shipped with this
//! crate), which downloads the per-platform runtime bundle (nanosb-runtime +
//! libkrunfw + gvproxy + exec-agent), verifies it, and installs it under
//! `$NANOSB_HOME`.
//!
//! ```no_run
//! use nanosandbox_sdk::{ManagedRuntime, SandboxClient};
//!
//! let rt = ManagedRuntime::start().unwrap();
//! let client = SandboxClient::connect(rt.socket()).unwrap();
//! let sandbox = client
//!     .create_with(nanosandbox_sdk::SandboxBuilder::new("alpine:latest").name("demo"))
//!     .unwrap();
//! let out = sandbox.exec("echo", &["hi"]).unwrap();
//! assert_eq!(out.stdout, "hi\n");
//! drop(rt);
//! ```
//!
//! Config comes from the environment: `NANOSB_RUNTIME_BIN`, `NANOSB_HOME`,
//! `NANOSB_PROVISIONER`, `NANOSB_VERSION` (required to download),
//! `NANOSB_RUNTIME_URL`, `NANOSB_RUNTIME_SHA256`, and `NANOSB_OFFLINE=1` to
//! never download. When `NANOSB_VERSION` is not set, the SDK pins it to its own
//! version (`CARGO_PKG_VERSION`), so a `0.2.0` crate provisions runtime v0.2.0.

use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The default helper binary name resolved via `PATH`.
const HELPER_NAME: &str = "nanosb-runtime";

/// How long `start` waits for the helper socket before giving up.
const DEFAULT_WAIT: Duration = Duration::from_secs(15);

/// How many ancestors to walk up looking for the repo `target` / `scripts` dirs.
const MAX_UP: usize = 8;

/// Per-crate unique suffix for temp dirs (PID + counter).
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The embedded provisioner, shipped with the SDK so auto-provisioning works
/// from an installed package with no repo present. Kept in sync with
/// `scripts/sdk/ensure-runtime.sh` (see `sdk/rust/src/ensure_runtime.sh`).
const EMBEDDED_PROVISIONER: &str = include_str!("ensure_runtime.sh");

/// Errors produced while locating, provisioning, or spawning the helper.
#[derive(Debug)]
pub enum RuntimeError {
    /// No helper found and provisioning did not produce one.
    NotFound(String),
    /// The provisioner script failed (non-zero exit, missing `bash`, …).
    Provision(String),
    /// A subprocess could not be spawned.
    Spawn(io::Error),
    /// A filesystem operation failed.
    Io(io::Error),
    /// The helper exited before its socket appeared.
    Exited(String),
    /// The helper socket did not appear in time.
    Timeout(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::NotFound(m) => write!(f, "{m}"),
            RuntimeError::Provision(m) => write!(f, "provision failed: {m}"),
            RuntimeError::Spawn(e) => write!(f, "spawn failed: {e}"),
            RuntimeError::Io(e) => write!(f, "io error: {e}"),
            RuntimeError::Exited(m) => write!(f, "{m}"),
            RuntimeError::Timeout(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<io::Error> for RuntimeError {
    fn from(e: io::Error) -> Self {
        RuntimeError::Io(e)
    }
}

/// Result alias for [`ManagedRuntime`] operations.
pub type Result<T> = std::result::Result<T, RuntimeError>;

/// A running `nanosb-runtime` helper on a private socket.
///
/// Dropping the value kills the helper and removes its temporary directory.
pub struct ManagedRuntime {
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
}

impl ManagedRuntime {
    /// Locate (provisioning if needed) and spawn the helper, waiting up to the
    /// default timeout for its socket.
    pub fn start() -> Result<Self> {
        Self::start_with(DEFAULT_WAIT)
    }

    /// Like [`ManagedRuntime::start`] but with an explicit socket wait timeout.
    pub fn start_with(wait: Duration) -> Result<Self> {
        let binary = ensure_runtime().map_err(|err| {
            RuntimeError::NotFound(format!(
                "nanosb-runtime not found and could not be provisioned; set \
                 NANOSB_RUNTIME_BIN, or NANOSB_VERSION to allow auto-download: {err}"
            ))
        })?;

        let dir = make_temp_dir("nanosb-e2e-")?;
        let socket = dir.join("helper.sock");
        let mut cmd = Command::new(&binary);
        cmd.arg("serve").arg("--socket").arg(&socket);
        if matches!(env::var_os("NANOSB_HOME"), Some(h) if h.is_empty()) {
            cmd.env_remove("NANOSB_HOME");
        }
        let mut child = cmd.spawn().map_err(|e| {
            let _ = fs::remove_dir_all(&dir);
            RuntimeError::Spawn(e)
        })?;

        let deadline = Instant::now() + wait;
        loop {
            if socket.exists() {
                return Ok(Self { child, dir, socket });
            }
            if let Some(status) = child.try_wait().map_err(RuntimeError::from)? {
                let _ = fs::remove_dir_all(&dir);
                return Err(RuntimeError::Exited(format!(
                    "nanosb-runtime exited with {status} before opening its socket"
                )));
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_dir_all(&dir);
                return Err(RuntimeError::Timeout(format!(
                    "timed out waiting for the helper socket {}",
                    socket.display()
                )));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The Unix socket the helper serves on. Pass to
    /// [`SandboxClient::connect`](nanosb_client::SandboxClient::connect).
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for ManagedRuntime {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Locate the `nanosb-runtime` helper, in order of preference:
/// `NANOSB_RUNTIME_BIN` → `$NANOSB_HOME/bin/nanosb-runtime` → `PATH` → the repo
/// `target/debug/nanosb-runtime`.
pub fn find_runtime_bin() -> Option<PathBuf> {
    if let Some(path) = env::var_os("NANOSB_RUNTIME_BIN") {
        let cand = PathBuf::from(path);
        if cand.is_file() {
            return Some(cand);
        }
    }

    let home = match env::var_os("NANOSB_HOME") {
        Some(h) if !h.is_empty() => Some(PathBuf::from(h)),
        _ => env::var_os("HOME").map(|h| PathBuf::from(h).join(".nanosandbox")),
    };
    if let Some(home) = home {
        let cand = home.join("bin").join(HELPER_NAME);
        if cand.is_file() {
            return Some(cand);
        }
    }

    if let Some(paths) = env::var_os("PATH") {
        if let Some(cand) = which_in(&paths, HELPER_NAME) {
            return Some(cand);
        }
    }

    find_up(
        env!("CARGO_MANIFEST_DIR"),
        &["target", "debug", HELPER_NAME],
    )
    .or_else(|| current_dir_up(&["target", "debug", HELPER_NAME]))
}

/// Locate the `ensure-runtime.sh` provisioner, in order of preference:
/// `NANOSB_PROVISIONER` → the repo `scripts/sdk/ensure-runtime.sh`. Returns
/// `None` so the caller can fall back to the embedded copy.
pub fn find_provisioner() -> Option<PathBuf> {
    if let Some(path) = env::var_os("NANOSB_PROVISIONER") {
        let cand = PathBuf::from(path);
        if cand.is_file() {
            return Some(cand);
        }
    }
    find_up(
        env!("CARGO_MANIFEST_DIR"),
        &["scripts", "sdk", "ensure-runtime.sh"],
    )
    .or_else(|| current_dir_up(&["scripts", "sdk", "ensure-runtime.sh"]))
}

/// Return the helper path, provisioning it (download + verify) if missing.
pub fn ensure_runtime() -> Result<PathBuf> {
    if let Some(bin) = find_runtime_bin() {
        return Ok(bin);
    }
    run_provisioner()?;
    find_runtime_bin()
        .ok_or_else(|| RuntimeError::NotFound("nanosb-runtime not found after provisioning".into()))
}

/// Run the shared provisioner: a located script, else the embedded copy written
/// to a temp file and run with `bash`.
fn run_provisioner() -> Result<()> {
    let script = match find_provisioner() {
        Some(path) => path,
        None => {
            let dir = make_temp_dir("nanosandbox-ensure-")?;
            let path = dir.join("ensure-runtime.sh");
            fs::write(&path, EMBEDDED_PROVISIONER)?;
            let result = run_bash(&path);
            let _ = fs::remove_dir_all(&dir);
            return result;
        }
    };
    run_bash(&script)
}

fn run_bash(script: &Path) -> Result<()> {
    let mut cmd = Command::new("bash");
    cmd.arg(script);
    // Pin the runtime version to the SDK's own version unless the user set
    // NANOSB_VERSION, so a 0.2.0 crate provisions the matching runtime v0.2.0.
    if env::var_os("NANOSB_VERSION").map_or(true, |v| v.is_empty()) {
        cmd.env("NANOSB_VERSION", env!("CARGO_PKG_VERSION"));
    }
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(RuntimeError::Provision(format!(
            "ensure-runtime.sh exited with {status}"
        )))
    }
}

/// Search `PATH` (colon-separated) for an executable named `name`.
fn which_in(paths: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    env::split_paths(paths).find_map(|dir| {
        let cand = dir.join(name);
        if is_executable(&cand) {
            Some(cand)
        } else {
            None
        }
    })
}

fn is_executable(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// Walk up from `start` looking for `rel`; return the first existing path.
fn find_up(start: impl AsRef<Path>, rel: &[&str]) -> Option<PathBuf> {
    let mut dir = start.as_ref();
    for _ in 0..MAX_UP {
        let cand = rel
            .iter()
            .fold(dir.to_path_buf(), |acc, part| acc.join(part));
        if cand.is_file() {
            return Some(cand);
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    None
}

/// Like [`find_up`] but starting from the current working directory.
fn current_dir_up(rel: &[&str]) -> Option<PathBuf> {
    match env::current_dir() {
        Ok(cwd) => find_up(cwd, rel),
        Err(_) => None,
    }
}

/// Create a unique temp directory under the system temp dir.
fn make_temp_dir(prefix: &str) -> io::Result<PathBuf> {
    let pid = std::process::id();
    for _ in 0..100 {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = env::temp_dir().join(format!("{prefix}{pid}-{n}"));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique temp directory",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_in_finds_executable_on_path() {
        let dir = temp_root();
        let bin = dir.join("nanosb-runtime");
        write_executable(&bin);
        let paths = env::join_paths([&dir]).unwrap();
        assert_eq!(which_in(&paths, HELPER_NAME), Some(bin));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn which_in_skips_missing_and_non_executable() {
        let dir = temp_root();
        let plain = dir.join("nanosb-runtime");
        fs::write(&plain, "#!/bin/sh\n").unwrap();
        let paths = env::join_paths([&dir]).unwrap();
        assert_eq!(which_in(&paths, HELPER_NAME), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_up_walks_to_ancestor() {
        let root = temp_root();
        let target = root.join("target").join("debug").join("nanosb-runtime");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        write_executable(&target);

        let deep = root.join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            find_up(&deep, &["target", "debug", HELPER_NAME]),
            Some(target)
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn find_up_stops_at_fs_root() {
        assert_eq!(find_up("/", &["definitely-not-here"]), None);
    }

    #[test]
    fn make_temp_dir_is_unique() {
        let a = make_temp_dir("nanosb-test-").unwrap();
        let b = make_temp_dir("nanosb-test-").unwrap();
        assert_ne!(a, b);
        fs::remove_dir_all(a).unwrap();
        fs::remove_dir_all(b).unwrap();
    }

    fn temp_root() -> PathBuf {
        make_temp_dir("nanosb-test-root-").unwrap()
    }

    fn write_executable(path: &Path) {
        fs::write(path, "#!/bin/sh\n").unwrap();
        let mut perm = fs::metadata(path).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(path, perm).unwrap();
    }
}
