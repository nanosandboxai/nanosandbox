//! gvproxy process manager for libkrun VM networking
//!
//! Manages the lifecycle of `gvproxy` sidecar processes that provide
//! user-mode networking for libkrun VMs via virtio-net.
//!
//! gvproxy (from `containers/gvisor-tap-vsock`) creates a virtual network
//! stack with DHCP, DNS, and routing. The VM communicates with gvproxy
//! through a Unix datagram socket connected via `krun_add_net_unixgram`.
//!
//! Default network configuration (gvproxy defaults):
//! - Subnet: 192.168.127.0/24
//! - Gateway: 192.168.127.1 (also serves DNS)
//! - Guest IP: 192.168.127.2 (DHCP static lease)
//! - Guest MAC: 5a:94:ef:e4:0c:ee

use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Guest IP address assigned by gvproxy DHCP.
pub const GUEST_IP: &str = "192.168.127.2";

/// A running gvproxy instance associated with a sandbox.
pub struct GvproxyInstance {
    /// The gvproxy child process.
    child: Child,
    /// Path to the vfkit unixgram socket used by libkrun.
    socket_path: PathBuf,
    /// Path to the gvproxy HTTP control socket (for port forwarding API).
    control_socket_path: PathBuf,
}

impl GvproxyInstance {
    /// Get the socket path for use with `krun_add_net_unixgram`.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Expose a guest port on a host port via gvproxy's HTTP control API.
    ///
    /// This dynamically adds a port forward: host `127.0.0.1:{host_port}` ->
    /// guest `192.168.127.2:{guest_port}`.
    ///
    /// Must be called after gvproxy has started and the control socket is ready.
    pub fn expose_port(&self, host_port: u16, guest_port: u16) -> Result<(), String> {
        let body = format!(
            r#"{{"local":"127.0.0.1:{}","remote":"{}:{}","protocol":"tcp"}}"#,
            host_port, GUEST_IP, guest_port
        );

        info!(
            "Exposing port via gvproxy: host:{} -> guest:{}",
            host_port, guest_port
        );

        // Connect to the Unix control socket and POST the expose request
        self.http_post_unix(
            &self.control_socket_path,
            "/services/forwarder/expose",
            &body,
        )
    }

    /// Remove a previously exposed port forward.
    #[allow(dead_code)]
    pub fn unexpose_port(&self, host_port: u16, guest_port: u16) -> Result<(), String> {
        let body = format!(
            r#"{{"local":"127.0.0.1:{}","remote":"{}:{}","protocol":"tcp"}}"#,
            host_port, GUEST_IP, guest_port
        );

        self.http_post_unix(
            &self.control_socket_path,
            "/services/forwarder/unexpose",
            &body,
        )
    }

    /// Send an HTTP POST to a Unix domain socket.
    fn http_post_unix(&self, socket_path: &Path, path: &str, body: &str) -> Result<(), String> {
        use std::os::unix::net::UnixStream;

        let mut stream =
            UnixStream::connect(socket_path).map_err(|e| {
                format!(
                    "Failed to connect to gvproxy control socket {}: {}",
                    socket_path.display(),
                    e
                )
            })?;

        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;

        let request = format!(
            "POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            path,
            body.len(),
            body
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("Failed to write to gvproxy control socket: {}", e))?;

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|e| format!("Failed to read gvproxy response: {}", e))?;

        let status_code = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);

        if (200..300).contains(&status_code) {
            Ok(())
        } else {
            // Read response body for error details
            let mut resp_body = String::new();
            // Skip headers
            loop {
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                if line.trim().is_empty() {
                    break;
                }
            }
            let _ = reader.read_to_string(&mut resp_body);
            Err(format!(
                "gvproxy expose returned HTTP {}: {}",
                status_code,
                resp_body.trim()
            ))
        }
    }

    /// Stop the gvproxy process and clean up socket files.
    pub fn stop(&mut self) {
        debug!("Stopping gvproxy (pid: {})", self.child.id());

        // Send SIGTERM first for graceful shutdown
        if let Err(e) = self.child.kill() {
            debug!("Failed to kill gvproxy: {} (may have already exited)", e);
        }

        // Wait for the process to exit
        match self.child.wait() {
            Ok(status) => debug!("gvproxy exited with status: {}", status),
            Err(e) => debug!("Failed to wait for gvproxy: {}", e),
        }

        // Clean up socket files
        for path in [&self.socket_path, &self.control_socket_path] {
            if path.exists() {
                if let Err(e) = std::fs::remove_file(path) {
                    debug!("Failed to remove gvproxy socket {}: {}", path.display(), e);
                }
            }
        }
    }
}

impl Drop for GvproxyInstance {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Manages gvproxy process lifecycle.
pub struct GvproxyManager;

impl GvproxyManager {
    /// Check if gvproxy is available on this system.
    pub fn is_available() -> bool {
        Self::find_binary().is_some()
    }

    /// Find the gvproxy binary in common locations.
    pub fn find_binary() -> Option<PathBuf> {
        // Check PATH first
        if let Ok(output) = Command::new("which")
            .arg("gvproxy")
            .output()
        {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(PathBuf::from(path));
                }
            }
        }

        // Check well-known locations
        let known_paths = [
            "/usr/local/bin/gvproxy",
            "/opt/homebrew/bin/gvproxy",
            "/usr/bin/gvproxy",
            "/usr/libexec/podman/gvproxy",
        ];

        for path in &known_paths {
            let p = Path::new(path);
            if p.exists() {
                return Some(p.to_path_buf());
            }
        }

        // Check HOME/.local/bin
        if let Ok(home) = std::env::var("HOME") {
            let local_path = PathBuf::from(home).join(".local/bin/gvproxy");
            if local_path.exists() {
                return Some(local_path);
            }
        }

        None
    }

    /// Find a free TCP port by binding to port 0 and reading the OS-assigned port.
    fn find_free_port() -> Option<u16> {
        std::net::TcpListener::bind("127.0.0.1:0")
            .ok()
            .and_then(|listener| listener.local_addr().ok())
            .map(|addr| addr.port())
    }

    /// Start a gvproxy instance for the given sandbox.
    ///
    /// Creates a unique Unix datagram socket at `/tmp/nanosb-{sandbox_id}-vfkit.sock`
    /// and a control socket at `/tmp/nanosb-{sandbox_id}-control.sock` for the
    /// HTTP API (port forwarding, etc.).
    pub fn start(sandbox_id: &str) -> Result<GvproxyInstance, String> {
        let binary = Self::find_binary()
            .ok_or_else(|| "gvproxy binary not found".to_string())?;

        let socket_path = PathBuf::from(format!("/tmp/nanosb-{}-vfkit.sock", sandbox_id));
        let control_socket_path =
            PathBuf::from(format!("/tmp/nanosb-{}-control.sock", sandbox_id));

        // Clean up any stale sockets from a previous run
        for path in [&socket_path, &control_socket_path] {
            if path.exists() {
                let _ = std::fs::remove_file(path);
            }
        }

        let socket_uri = format!("unixgram://{}", socket_path.display());
        let control_uri = format!("unix://{}", control_socket_path.display());

        info!(
            "Starting gvproxy: {} --listen-vfkit {} -listen {}",
            binary.display(),
            socket_uri,
            control_uri
        );

        // Each gvproxy instance binds an SSH forwarding listener on 127.0.0.1.
        // The default port is 2222, which means only one instance can run at a
        // time. We find a free port dynamically so multiple instances coexist.
        let ssh_port = Self::find_free_port().unwrap_or(2222);

        let child = Command::new(&binary)
            .arg("--listen-vfkit")
            .arg(&socket_uri)
            .arg("-listen")
            .arg(&control_uri)
            .arg("-ssh-port")
            .arg(ssh_port.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn gvproxy: {}", e))?;

        info!("gvproxy started (pid: {})", child.id());

        // Wait for the socket to appear (gvproxy needs a moment to create it)
        let start = Instant::now();
        let timeout = Duration::from_secs(5);
        let poll_interval = Duration::from_millis(50);

        while start.elapsed() < timeout {
            if socket_path.exists() {
                info!("gvproxy socket ready: {}", socket_path.display());
                return Ok(GvproxyInstance {
                    child,
                    socket_path,
                    control_socket_path,
                });
            }
            std::thread::sleep(poll_interval);
        }

        // Timeout -- try to get stderr for diagnostics
        let mut instance = GvproxyInstance {
            child,
            socket_path: socket_path.clone(),
            control_socket_path,
        };

        // Check if the process is still running
        match instance.child.try_wait() {
            Ok(Some(status)) => {
                // Process exited early -- read stderr
                let stderr = if let Some(ref mut stderr) = instance.child.stderr {
                    let mut buf = String::new();
                    use std::io::Read;
                    let _ = stderr.read_to_string(&mut buf);
                    buf
                } else {
                    String::new()
                };
                Err(format!(
                    "gvproxy exited early with status {} (stderr: {})",
                    status,
                    stderr.trim()
                ))
            }
            Ok(None) => {
                // Still running but socket not created
                warn!("gvproxy socket not ready after {:?}, proceeding anyway", timeout);
                Ok(instance)
            }
            Err(e) => {
                Err(format!("Failed to check gvproxy status: {}", e))
            }
        }
    }
}
