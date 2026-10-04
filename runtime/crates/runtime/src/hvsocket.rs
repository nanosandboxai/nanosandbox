//! Windows AF_HYPERV socket wrapper for direct host↔guest communication.
//!
//! Hyper-V sockets (AF_HYPERV on Windows, AF_VSOCK on Linux) provide a direct
//! VMBus-based channel between host and guest, completely bypassing the network
//! stack. This eliminates the ~60s HCN NAT TCP convergence delay on Windows.
//!
//! The guest runs a vsock proxy (Python) on port 50001 that forwards to the
//! agent-gateway at localhost:8080. The host connects via AF_HYPERV using the
//! VM's HCS identity and a service GUID derived from the vsock port number.

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tracing::{debug, error, info, warn};

use devices::virtio::fs::socket_worker;

/// AF_HYPERV vsock port for FUSE rootfs server (host side).
pub const HVSOCK_FUSE_ROOTFS_PORT: u32 = 50000;

/// AF_HYPERV vsock port for optional FUSE blobs server (host side).
pub const HVSOCK_FUSE_BLOBS_PORT: u32 = 50002;

/// AF_HYPERV vsock base port for workspace FUSE servers (host side).
pub const HVSOCK_FUSE_WORKSPACE_BASE_PORT: u32 = 50010;

/// Maximum number of workspace FUSE shares exposed over HvSocket.
pub const HVSOCK_FUSE_WORKSPACE_MAX_SHARES: u32 = 16;

/// AF_HYPERV vsock proxy port for gateway HTTP (guest side).
pub const HVSOCK_GATEWAY_PORT: u32 = 50001;

/// AF_HYPERV vsock proxy port for SSH (guest side).
pub const HVSOCK_SSH_PORT: u32 = 50022;

/// AF_HYPERV vsock proxy port for DNS relay (guest side).
pub const HVSOCK_DNS_PORT: u32 = 50053;

/// AF_HYPERV vsock proxy port for TCP connect relay (guest side).
pub const HVSOCK_TCP_PROXY_PORT: u32 = 50080;

/// AF_HYPERV vsock proxy port for dynamic inbound forwarder (guest side).
/// Host sends a 2-byte BE port over the connection, guest dials 127.0.0.1:<port>
/// and bridges. Used to expose arbitrary guest TCP ports (e.g. OAuth callbacks)
/// to the host's loopback.
pub const HVSOCK_INBOUND_FWD_PORT: u32 = 50090;

/// Service GUID for vsock port 50001 (gateway).
/// Follows Microsoft's vsock-to-GUID template: {port_hex}-FACB-11E6-BD58-64006A7986D3
const HVSOCK_GATEWAY_GUID: [u8; 16] = [
    0x51, 0xC3, 0x00, 0x00, // Data1: 0x0000C351 (50001) in little-endian
    0xCB, 0xFA,             // Data2: 0xFACB in little-endian
    0xE6, 0x11,             // Data3: 0x11E6 in little-endian
    0xBD, 0x58, 0x64, 0x00, 0x6A, 0x79, 0x86, 0xD3, // Data4
];

/// Service GUID for vsock port 50022 (SSH).
const HVSOCK_SSH_GUID: [u8; 16] = [
    0x66, 0xC3, 0x00, 0x00, // Data1: 0x0000C366 (50022) in little-endian
    0xCB, 0xFA,             // Data2: 0xFACB in little-endian
    0xE6, 0x11,             // Data3: 0x11E6 in little-endian
    0xBD, 0x58, 0x64, 0x00, 0x6A, 0x79, 0x86, 0xD3, // Data4
];

const AF_HYPERV: i32 = 34;
const HV_PROTOCOL_RAW: i32 = 1;
const SOCK_STREAM: i32 = 1;

#[repr(C)]
struct SockaddrHv {
    family: u16,
    reserved: u16,
    vm_id: [u8; 16],
    service_id: [u8; 16],
}

extern "system" {
    fn socket(af: i32, socket_type: i32, protocol: i32) -> usize;
    fn connect(s: usize, name: *const SockaddrHv, namelen: i32) -> i32;
    fn send(s: usize, buf: *const u8, len: i32, flags: i32) -> i32;
    fn recv(s: usize, buf: *mut u8, len: i32, flags: i32) -> i32;
    fn closesocket(s: usize) -> i32;
    fn setsockopt(s: usize, level: i32, optname: i32, optval: *const u8, optlen: i32) -> i32;
}

const SOL_SOCKET: i32 = 0xFFFF;
const SO_RCVTIMEO: i32 = 0x1006;
const SO_SNDTIMEO: i32 = 0x1005;

const INVALID_SOCKET: usize = usize::MAX;

/// Parse an HCS VM identity string into a GUID byte array.
///
/// The VM identity is the sandbox UUID (e.g., "ae5189eb-1af2-49f7-a3c0-a447d5eef80b"),
/// which is a valid GUID that AF_HYPERV can use directly.
fn hcs_vm_id_to_guid(vm_id: &str) -> Option<[u8; 16]> {
    parse_uuid_to_guid_bytes(vm_id)
}

/// Parse a UUID string (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx) to GUID bytes.
fn parse_uuid_to_guid_bytes(uuid: &str) -> Option<[u8; 16]> {
    let hex: String = uuid.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return None;
    }

    let mut bytes = [0u8; 16];
    for i in 0..16 {
        bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }

    // Convert from UUID byte order to Windows GUID byte order:
    // UUID: big-endian throughout
    // GUID: Data1 (4 bytes LE), Data2 (2 bytes LE), Data3 (2 bytes LE), Data4 (8 bytes BE)
    let mut guid = [0u8; 16];
    // Data1: reverse bytes 0-3
    guid[0] = bytes[3];
    guid[1] = bytes[2];
    guid[2] = bytes[1];
    guid[3] = bytes[0];
    // Data2: reverse bytes 4-5
    guid[4] = bytes[5];
    guid[5] = bytes[4];
    // Data3: reverse bytes 6-7
    guid[6] = bytes[7];
    guid[7] = bytes[6];
    // Data4: bytes 8-15 unchanged
    guid[8..16].copy_from_slice(&bytes[8..16]);

    Some(guid)
}

/// A connected HvSocket stream that implements Read + Write.
pub struct HvSocketStream {
    sock: usize,
}

// SAFETY: The socket handle is owned by this struct and not shared.
unsafe impl Send for HvSocketStream {}

/// Map a vsock port number to its service GUID bytes.
fn service_guid_for_port(port: u32) -> [u8; 16] {
    match port {
        HVSOCK_GATEWAY_PORT => HVSOCK_GATEWAY_GUID,
        HVSOCK_SSH_PORT => HVSOCK_SSH_GUID,
        _ => {
            // Generic port-to-GUID: {port_le32}-FACB-11E6-BD58-64006A7986D3
            let p = port.to_le_bytes();
            [
                p[0], p[1], p[2], p[3],
                0xCB, 0xFA, 0xE6, 0x11,
                0xBD, 0x58, 0x64, 0x00, 0x6A, 0x79, 0x86, 0xD3,
            ]
        }
    }
}

impl HvSocketStream {
    /// Connect to a guest VM via AF_HYPERV on the gateway port (50001).
    ///
    /// `vm_id` is the HCS VM identity string (e.g., "libkrun-1234-abcdef-...").
    pub fn connect(vm_id: &str) -> io::Result<Self> {
        Self::connect_port(vm_id, HVSOCK_GATEWAY_PORT)
    }

    /// Connect to a guest VM via AF_HYPERV on a specific vsock port.
    pub fn connect_port(vm_id: &str, port: u32) -> io::Result<Self> {
        let vm_guid = hcs_vm_id_to_guid(vm_id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, format!("cannot parse VM ID '{}' as GUID", vm_id))
        })?;

        debug!("HvSocket: connecting to VM '{}' on vsock port {}", vm_id, port);

        let sock = unsafe { socket(AF_HYPERV, SOCK_STREAM, HV_PROTOCOL_RAW) };
        if sock == INVALID_SOCKET || sock == 0 {
            return Err(io::Error::last_os_error());
        }

        // Increase HvSocket VMBus ring buffer sizes from the default 24KB
        // to the maximum 256KB.  The ring buffer size is derived from
        // SO_SNDBUF/SO_RCVBUF on Windows 10 v5+ hosts.  Larger buffers
        // reduce the chance of a backpressure deadlock where both
        // directions fill their tiny ring buffers simultaneously.
        const HVSOCK_BUF_SIZE: u32 = 262144; // 256KB (max supported)
        const SO_SNDBUF: i32 = 0x1001;
        const SO_RCVBUF: i32 = 0x1002;
        unsafe {
            setsockopt(sock, SOL_SOCKET, SO_SNDBUF,
                &HVSOCK_BUF_SIZE as *const u32 as *const u8, 4);
            setsockopt(sock, SOL_SOCKET, SO_RCVBUF,
                &HVSOCK_BUF_SIZE as *const u32 as *const u8, 4);
        }

        // Set send/recv timeouts BEFORE connect() so the connect call itself
        // is bounded. Without this, connect() can hang forever if the guest
        // vsock listener isn't ready.
        let connect_timeout_ms: u32 = if port == HVSOCK_SSH_PORT { 10000 } else { 5000 };
        let io_timeout_ms: u32 = if port == HVSOCK_SSH_PORT { 30000 } else { 5000 };
        unsafe {
            setsockopt(sock, SOL_SOCKET, SO_SNDTIMEO,
                &connect_timeout_ms as *const u32 as *const u8, 4);
            setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO,
                &connect_timeout_ms as *const u32 as *const u8, 4);
        }

        let addr = SockaddrHv {
            family: AF_HYPERV as u16,
            reserved: 0,
            vm_id: vm_guid,
            service_id: service_guid_for_port(port),
        };

        let ret = unsafe {
            connect(sock, &addr, std::mem::size_of::<SockaddrHv>() as i32)
        };

        if ret != 0 {
            let err = io::Error::last_os_error();
            unsafe { closesocket(sock) };
            return Err(err);
        }

        // After connect succeeds, set the final I/O timeouts.
        unsafe {
            setsockopt(sock, SOL_SOCKET, SO_SNDTIMEO,
                &io_timeout_ms as *const u32 as *const u8, 4);
            setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO,
                &io_timeout_ms as *const u32 as *const u8, 4);
        }

        debug!("HvSocket: connected to VM '{}' port {}", vm_id, port);
        Ok(HvSocketStream { sock })
    }

    /// Try to connect with a timeout (poll-based).
    pub fn connect_with_retry(vm_id: &str, max_attempts: u32, delay_ms: u64) -> io::Result<Self> {
        Self::connect_port_with_retry(vm_id, HVSOCK_GATEWAY_PORT, max_attempts, delay_ms)
    }

    /// Try to connect to a specific port with retries.
    pub fn connect_port_with_retry(vm_id: &str, port: u32, max_attempts: u32, delay_ms: u64) -> io::Result<Self> {
        for attempt in 1..=max_attempts {
            match Self::connect_port(vm_id, port) {
                Ok(s) => return Ok(s),
                Err(e) => {
                    if attempt == max_attempts {
                        return Err(e);
                    }
                    debug!("HvSocket: connect attempt {}/{} failed: {}, retrying...", attempt, max_attempts, e);
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
            }
        }
        unreachable!()
    }
}

impl HvSocketStream {
    /// Set the receive timeout on this socket.
    ///
    /// `None` or `Duration::ZERO` means wait indefinitely (no timeout).
    /// Used by SSE streaming where the agent can take minutes between output.
    pub fn set_recv_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        let timeout_ms: u32 = match timeout {
            Some(d) if !d.is_zero() => d.as_millis() as u32,
            _ => 0, // 0 = infinite on Windows
        };
        let ret = unsafe {
            setsockopt(self.sock, SOL_SOCKET, SO_RCVTIMEO,
                &timeout_ms as *const u32 as *const u8, 4)
        };
        if ret != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Read for HvSocketStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let ret = unsafe { recv(self.sock, buf.as_mut_ptr(), buf.len() as i32, 0) };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ret as usize)
        }
    }
}

impl Write for HvSocketStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let ret = unsafe { send(self.sock, buf.as_ptr(), buf.len() as i32, 0) };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ret as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for HvSocketStream {
    fn drop(&mut self) {
        unsafe { closesocket(self.sock) };
    }
}

/// Register HvSocket service GUIDs in the Windows registry.
/// This is required for AF_HYPERV connections to work.
/// Registers both gateway (50001) and SSH (50022) ports.
/// Must be run as administrator (typically during installation).
pub fn ensure_hvsock_service_registered() {
    register_hvsock_port(HVSOCK_FUSE_ROOTFS_PORT, "nanosandbox-fuse-rootfs");
    register_hvsock_port(HVSOCK_GATEWAY_PORT, "nanosandbox-gateway");
    register_hvsock_port(HVSOCK_FUSE_BLOBS_PORT, "nanosandbox-fuse-blobs");
    for i in 0..HVSOCK_FUSE_WORKSPACE_MAX_SHARES {
        let port = HVSOCK_FUSE_WORKSPACE_BASE_PORT + i;
        register_hvsock_port(port, "nanosandbox-fuse-workspace");
    }
    register_hvsock_port(HVSOCK_SSH_PORT, "nanosandbox-ssh");
    register_hvsock_port(HVSOCK_DNS_PORT, "nanosandbox-dns");
    register_hvsock_port(HVSOCK_TCP_PROXY_PORT, "nanosandbox-tcp-proxy");
    register_hvsock_port(HVSOCK_INBOUND_FWD_PORT, "nanosandbox-inbound-fwd");
}

fn register_hvsock_port(port: u32, element_name: &str) {
    use std::process::Command;

    let guid = format!(
        "{:08X}-FACB-11E6-BD58-64006A7986D3",
        port
    );
    let reg_path = format!(
        r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices\{{{}}}",
        guid
    );

    // Check if already registered
    let check = Command::new("reg")
        .args(["query", &reg_path])
        .output();

    if let Ok(output) = check {
        if output.status.success() {
            debug!("HvSocket service GUID {} already registered", guid);
            return;
        }
    }

    // Register the service GUID
    let result = Command::new("reg")
        .args([
            "add",
            &reg_path,
            "/v", "ElementName",
            "/t", "REG_SZ",
            "/d", element_name,
            "/f",
        ])
        .output();

    match result {
        Ok(output) if output.status.success() => {
            debug!("HvSocket service GUID {} registered successfully", guid);
        }
        Ok(output) => {
            warn!(
                "Failed to register HvSocket service GUID {}: {}",
                guid,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(e) => {
            warn!("Failed to register HvSocket service GUID: {}", e);
        }
    }
}

/// Start a local TCP→HvSocket proxy for SSH.
///
/// Binds a random TCP port on 127.0.0.1, then spawns a background thread that
/// accepts connections and proxies each one through HvSocket to the guest's SSH
/// port (50022). Returns the local TCP port the SSH client should connect to.
pub fn start_ssh_proxy(vm_id: &str) -> io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let local_port = listener.local_addr()?.port();
    let vm_id = vm_id.to_string();

    debug!("SSH proxy: listening on 127.0.0.1:{} -> HvSocket port {}", local_port, HVSOCK_SSH_PORT);

    std::thread::spawn(move || {
        for tcp_stream in listener.incoming() {
            let tcp_stream = match tcp_stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let vm_id = vm_id.clone();

            // Each SSH connection gets its own proxy thread pair
            std::thread::spawn(move || {
                let hv_sock = match HvSocketStream::connect_port(&vm_id, HVSOCK_SSH_PORT) {
                    Ok(s) => {
                        let sock = s.sock;
                        std::mem::forget(s); // prevent Drop from closing
                        sock
                    },
                    Err(e) => {
                        warn!("SSH proxy: HvSocket connect failed: {}", e);
                        return;
                    }
                };

                // Duplicate the TCP stream for bidirectional proxy
                let tcp_read = match tcp_stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let tcp_write = tcp_stream;

                // Direction 1: TCP → HvSocket (background thread)
                let hv_sock_copy = hv_sock;
                let handle = std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    let mut tcp = tcp_read;
                    loop {
                        let n = match io::Read::read(&mut tcp, &mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        let ret = unsafe { send(hv_sock_copy, buf.as_ptr(), n as i32, 0) };
                        if ret <= 0 {
                            break;
                        }
                    }
                });

                // Direction 2: HvSocket → TCP (this thread)
                let mut tcp = tcp_write;
                let mut buf = [0u8; 8192];
                loop {
                    let ret = unsafe { recv(hv_sock, buf.as_mut_ptr(), buf.len() as i32, 0) };
                    if ret <= 0 {
                        break;
                    }
                    if io::Write::write_all(&mut tcp, &buf[..ret as usize]).is_err() {
                        break;
                    }
                }

                let _ = handle.join();
                unsafe { closesocket(hv_sock) };
            });
        }
    });

    Ok(local_port)
}

/// Start a host-side TCP listener on `127.0.0.1:host_port` that bridges each
/// connection to the guest's `127.0.0.1:guest_port` via AF_HYPERV vsock 50090.
///
/// Protocol: host opens vsock 50090 → sends 2-byte BE guest port → reads 1-byte
/// status (0=connected, 1=failed) → proxies bidirectionally.
///
/// Returns the actual host port bound (caller may pass 0 to get an ephemeral port).
pub fn start_inbound_port_forwarder(vm_id: &str, host_port: u16, guest_port: u16) -> io::Result<u16> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, host_port))?;
    let bound_port = listener.local_addr()?.port();
    let vm_id = vm_id.to_string();

    debug!(
        "Inbound port forwarder: 127.0.0.1:{} -> guest:{} (vsock {})",
        bound_port, guest_port, HVSOCK_INBOUND_FWD_PORT
    );

    std::thread::spawn(move || {
        for tcp_stream in listener.incoming() {
            let tcp_stream = match tcp_stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            // TCP_NODELAY on the host-side accepted socket — small OAuth callback
            // requests benefit from no Nagle delays.
            let _ = tcp_stream.set_nodelay(true);

            let vm_id = vm_id.clone();
            std::thread::spawn(move || {
                let hv_sock = match HvSocketStream::connect_port(&vm_id, HVSOCK_INBOUND_FWD_PORT) {
                    Ok(s) => {
                        let sock = s.sock;
                        std::mem::forget(s);
                        sock
                    }
                    Err(e) => {
                        warn!("inbound-fwd: vsock connect failed: {}", e);
                        return;
                    }
                };

                // Send 2-byte BE guest port.
                let port_bytes = guest_port.to_be_bytes();
                let sent = unsafe { send(hv_sock, port_bytes.as_ptr(), 2, 0) };
                if sent != 2 {
                    unsafe { closesocket(hv_sock) };
                    return;
                }

                // Read 1-byte status.
                let mut status = [0u8; 1];
                if hv_recv_exact(hv_sock, &mut status).is_err() || status[0] != 0 {
                    warn!("inbound-fwd: guest could not connect to 127.0.0.1:{}", guest_port);
                    unsafe { closesocket(hv_sock) };
                    return;
                }

                let tcp_read = match tcp_stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => {
                        unsafe { closesocket(hv_sock) };
                        return;
                    }
                };
                let tcp_write = tcp_stream;
                let hv_a = hv_sock;
                let hv_b = hv_sock;

                // Direction 1: TCP → HvSocket
                let handle = std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    let mut tcp = tcp_read;
                    loop {
                        let n = match io::Read::read(&mut tcp, &mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        let ret = unsafe { send(hv_a, buf.as_ptr(), n as i32, 0) };
                        if ret <= 0 {
                            break;
                        }
                    }
                });

                // Direction 2: HvSocket → TCP
                let mut tcp = tcp_write;
                let mut buf = [0u8; 8192];
                loop {
                    let ret = unsafe { recv(hv_b, buf.as_mut_ptr(), buf.len() as i32, 0) };
                    if ret <= 0 {
                        break;
                    }
                    if io::Write::write_all(&mut tcp, &buf[..ret as usize]).is_err() {
                        break;
                    }
                }

                let _ = handle.join();
                unsafe { closesocket(hv_sock) };
            });
        }
    });

    Ok(bound_port)
}

/// If `query` is a single-question DNS query for AAAA (qtype=28), return a
/// synthesized response with the same ID and question, RCODE=0, ANCOUNT=0
/// (i.e. "no IPv6 address exists, but this domain is fine"). This causes
/// resolvers to fall back to A queries immediately. Returns None for non-AAAA
/// queries so they can be forwarded normally.
fn synthesize_empty_aaaa_response(query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    if qdcount != 1 {
        return None;
    }
    // Skip QNAME starting at offset 12.
    let mut i = 12;
    while i < query.len() {
        let len = query[i] as usize;
        if len == 0 {
            i += 1;
            break;
        }
        // Compression pointer (top two bits set) — bail; let upstream handle it.
        if len & 0xC0 != 0 {
            return None;
        }
        i += 1 + len;
        if i > query.len() {
            return None;
        }
    }
    // Need 4 more bytes for QTYPE + QCLASS.
    if i + 4 > query.len() {
        return None;
    }
    let qtype = u16::from_be_bytes([query[i], query[i + 1]]);
    if qtype != 28 {
        return None; // not AAAA
    }
    // Build response: copy header, set flags, ancount=0, echo question.
    let mut resp = Vec::with_capacity(i + 4);
    resp.extend_from_slice(&query[..12]);
    // flags: QR=1, OPCODE=0, AA=0, TC=0, RD=copy, RA=1, Z=0, RCODE=0
    let rd = query[2] & 0x01;
    resp[2] = 0x80 | rd; // QR + RD-copy
    resp[3] = 0x80;       // RA=1, RCODE=0
    // qdcount=1, ancount=0, nscount=0, arcount=0
    resp[4] = 0; resp[5] = 1;
    resp[6] = 0; resp[7] = 0;
    resp[8] = 0; resp[9] = 0;
    resp[10] = 0; resp[11] = 0;
    // Question section.
    resp.extend_from_slice(&query[12..i + 4]);
    Some(resp)
}

/// Start a host-side DNS relay that accepts AF_HYPERV connections from the guest
/// on vsock port 50053 and forwards DNS queries to real DNS servers (8.8.8.8).
///
/// Protocol: guest sends [2-byte BE length][DNS query], host resolves via UDP
/// to 8.8.8.8:53, returns [2-byte BE length][DNS response].
pub fn start_dns_proxy(vm_id: &str) -> io::Result<()> {
    debug!("DNS proxy: starting for VM '{}'", vm_id);

    // Create the listener on the calling thread so errors propagate.
    let listener = hvsock_listener(vm_id, HVSOCK_DNS_PORT)?;
    debug!("DNS proxy: listener created on port {}", HVSOCK_DNS_PORT);

    std::thread::spawn(move || {
        loop {
            let client_sock = unsafe { accept(listener, std::ptr::null_mut(), std::ptr::null_mut()) };
            if client_sock == INVALID_SOCKET || client_sock == 0 {
                continue;
            }

            std::thread::spawn(move || {
                // Read 2-byte length prefix
                let mut len_buf = [0u8; 2];
                if hv_recv_exact(client_sock, &mut len_buf).is_err() {
                    unsafe { closesocket(client_sock) };
                    return;
                }
                let query_len = u16::from_be_bytes(len_buf) as usize;
                if query_len == 0 || query_len > 4096 {
                    unsafe { closesocket(client_sock) };
                    return;
                }

                // Read DNS query
                let mut query = vec![0u8; query_len];
                if hv_recv_exact(client_sock, &mut query).is_err() {
                    unsafe { closesocket(client_sock) };
                    return;
                }

                // Short-circuit AAAA queries (qtype=28): return empty NOERROR
                // response so apps immediately fall back to A. Without this,
                // upstream may return AAAA records that lead to long IPv6
                // connection-fail timeouts since we have no IPv6 NAT path.
                if let Some(synth) = synthesize_empty_aaaa_response(&query) {
                    let len_bytes = (synth.len() as u16).to_be_bytes();
                    let _ = hv_send_all(client_sock, &len_bytes);
                    let _ = hv_send_all(client_sock, &synth);
                    unsafe { closesocket(client_sock) };
                    return;
                }

                // Forward to 8.8.8.8:53 via UDP
                let udp = match std::net::UdpSocket::bind("0.0.0.0:0") {
                    Ok(s) => s,
                    Err(_) => {
                        unsafe { closesocket(client_sock) };
                        return;
                    }
                };
                let _ = udp.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                if udp.send_to(&query, "8.8.8.8:53").is_err() {
                    unsafe { closesocket(client_sock) };
                    return;
                }

                let mut resp_buf = [0u8; 4096];
                let resp_len = match udp.recv(&mut resp_buf) {
                    Ok(n) => n,
                    Err(_) => {
                        unsafe { closesocket(client_sock) };
                        return;
                    }
                };

                // Send response back: 2-byte length + data
                let resp_len_bytes = (resp_len as u16).to_be_bytes();
                let _ = hv_send_all(client_sock, &resp_len_bytes);
                let _ = hv_send_all(client_sock, &resp_buf[..resp_len]);
                unsafe { closesocket(client_sock) };
            });
        }
    });

    Ok(())
}

/// Start a host-side TCP connect relay that accepts AF_HYPERV connections from
/// the guest on vsock port 50080 and proxies them to real internet destinations.
///
/// Protocol: guest sends [4-byte dest IP][2-byte dest port] (network byte order),
/// host connects to that destination, sends 1-byte status (0=ok, 1=fail),
/// then proxies bidirectionally.
pub fn start_tcp_proxy(vm_id: &str) -> io::Result<()> {
    debug!("TCP proxy: starting for VM '{}'", vm_id);

    // Create the listener on the calling thread so errors propagate.
    let listener = hvsock_listener(vm_id, HVSOCK_TCP_PROXY_PORT)?;
    debug!("TCP proxy: listener created on port {}", HVSOCK_TCP_PROXY_PORT);

    static CONN_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    std::thread::spawn(move || {
        loop {
            let client_sock = unsafe { accept(listener, std::ptr::null_mut(), std::ptr::null_mut()) };
            if client_sock == INVALID_SOCKET || client_sock == 0 {
                continue;
            }
            let cid = CONN_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let t_accept = std::time::Instant::now();
            warn!("[tcp-proxy/{}] T0 accepted (active threads ~ kernel-level)", cid);

            std::thread::spawn(move || {
                // Read 6-byte header: 4-byte IP + 2-byte port (network byte order)
                let mut header = [0u8; 6];
                if hv_recv_exact(client_sock, &mut header).is_err() {
                    unsafe { closesocket(client_sock) };
                    return;
                }
                let t_header = t_accept.elapsed();

                let dest_ip = std::net::Ipv4Addr::new(header[0], header[1], header[2], header[3]);
                let dest_port = u16::from_be_bytes([header[4], header[5]]);

                warn!("[tcp-proxy/{}] T1 header_read +{}ms dest={}:{}", cid, t_header.as_millis(), dest_ip, dest_port);

                // Connect to real destination
                let dest_addr = std::net::SocketAddr::new(std::net::IpAddr::V4(dest_ip), dest_port);
                let t_connect_start = std::time::Instant::now();
                let tcp_stream = match std::net::TcpStream::connect_timeout(
                    &dest_addr,
                    std::time::Duration::from_secs(30),
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("[tcp-proxy/{}] connect to {}:{} failed after {}ms: {}", cid, dest_ip, dest_port, t_connect_start.elapsed().as_millis(), e);
                        let _ = hv_send_all(client_sock, &[1u8]); // status: failed
                        unsafe { closesocket(client_sock) };
                        return;
                    }
                };
                // Disable Nagle so small TLS records / HTTP headers ship immediately.
                let _ = tcp_stream.set_nodelay(true);
                let t_connect = t_connect_start.elapsed();
                warn!("[tcp-proxy/{}] T2 tcp_connect +{}ms (total {}ms)", cid, t_connect.as_millis(), t_accept.elapsed().as_millis());

                // Send success status
                if hv_send_all(client_sock, &[0u8]).is_err() {
                    unsafe { closesocket(client_sock) };
                    return;
                }
                warn!("[tcp-proxy/{}] T3 status_sent total={}ms", cid, t_accept.elapsed().as_millis());

                // Bidirectional proxy: HvSocket ↔ TCP
                let tcp_read = match tcp_stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => {
                        unsafe { closesocket(client_sock) };
                        return;
                    }
                };
                let tcp_write = tcp_stream;

                let hv_sock = client_sock;

                // Direction 1: HvSocket → TCP (background thread)
                let handle = std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    let mut tcp = tcp_write;
                    let mut first = true;
                    loop {
                        let ret = unsafe { recv(hv_sock, buf.as_mut_ptr(), buf.len() as i32, 0) };
                        if ret <= 0 {
                            break;
                        }
                        if first {
                            warn!("[tcp-proxy/{}] T4a hv->tcp first_chunk +{}ms ({} bytes)", cid, t_accept.elapsed().as_millis(), ret);
                            first = false;
                        }
                        if io::Write::write_all(&mut tcp, &buf[..ret as usize]).is_err() {
                            break;
                        }
                    }
                });

                // Direction 2: TCP → HvSocket (this thread)
                let mut buf = [0u8; 8192];
                let mut tcp = tcp_read;
                let mut first = true;
                loop {
                    let n = match io::Read::read(&mut tcp, &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if first {
                        warn!("[tcp-proxy/{}] T4b tcp->hv first_chunk +{}ms ({} bytes)", cid, t_accept.elapsed().as_millis(), n);
                        first = false;
                    }
                    let ret = unsafe { send(client_sock, buf.as_ptr(), n as i32, 0) };
                    if ret <= 0 {
                        break;
                    }
                }

                let _ = handle.join();
                warn!("[tcp-proxy/{}] T5 closed total={}ms", cid, t_accept.elapsed().as_millis());
                unsafe { closesocket(client_sock) };
            });
        }
    });

    Ok(())
}

/// Start a host-side FUSE rootfs server for the given VM.
///
/// The guest `fuse_mount` process connects to vsock port 50000. For each
/// accepted connection we run a FUSE protocol server backed by `root_dir`.
pub fn start_fuse_rootfs_server(vm_id: &str, root_dir: &Path) -> io::Result<()> {
    let root_dir = root_dir.to_string_lossy().to_string();
    info!(
        "FUSE rootfs: starting host server for VM '{}' on port {} (root={})",
        vm_id,
        HVSOCK_FUSE_ROOTFS_PORT,
        root_dir
    );

    let listener = hvsock_listener(vm_id, HVSOCK_FUSE_ROOTFS_PORT)?;
    debug!(
        "FUSE rootfs: listener created on port {} for VM '{}'",
        HVSOCK_FUSE_ROOTFS_PORT,
        vm_id
    );

    std::thread::spawn(move || {
        loop {
            let client_sock = unsafe { accept(listener, std::ptr::null_mut(), std::ptr::null_mut()) };
            if client_sock == INVALID_SOCKET || client_sock == 0 {
                warn!("FUSE rootfs: accept failed on port {}", HVSOCK_FUSE_ROOTFS_PORT);
                continue;
            }

            let root_dir_for_client = root_dir.clone();
            std::thread::spawn(move || {
                info!(
                    "FUSE rootfs: client connected on port {}",
                    HVSOCK_FUSE_ROOTFS_PORT
                );
                let stream = AcceptedHvSocket { sock: client_sock };
                let stop = Arc::new(AtomicBool::new(false));
                socket_worker::serve_fuse_on_stream(
                    "rootfs",
                    &root_dir_for_client,
                    stream,
                    &stop,
                );
                info!("FUSE rootfs: client disconnected");
            });
        }
    });

    Ok(())
}

/// Start a host-side blob streaming server for the given VM.
///
/// The guest connects to vsock port 50002 in extraction mode and sends a
/// manifest (newline-separated layer digests, terminated by an empty line).
/// For each digest the host sends `[8-byte LE file size][raw tar bytes]`.
/// This is vastly faster than FUSE-over-vsock because it eliminates per-chunk
/// round-trips and FUSE protocol overhead.
pub fn start_fuse_blobs_server(vm_id: &str, blobs_dir: &Path) -> io::Result<()> {
    let blobs_dir = blobs_dir.to_string_lossy().to_string();
    info!(
        "blob-stream: starting for VM '{}' on port {} (blobs={})",
        vm_id,
        HVSOCK_FUSE_BLOBS_PORT,
        blobs_dir
    );

    let listener = hvsock_listener(vm_id, HVSOCK_FUSE_BLOBS_PORT)?;

    std::thread::spawn(move || {
        loop {
            let client_sock = unsafe { accept(listener, std::ptr::null_mut(), std::ptr::null_mut()) };
            if client_sock == INVALID_SOCKET || client_sock == 0 {
                warn!("blob-stream: accept failed on port {}", HVSOCK_FUSE_BLOBS_PORT);
                continue;
            }

            let blobs_dir_for_client = blobs_dir.clone();
            std::thread::spawn(move || {
                info!("blob-stream: client connected on port {}", HVSOCK_FUSE_BLOBS_PORT);
                let mut stream = AcceptedHvSocket { sock: client_sock };

                // Read manifest: newline-separated digests, terminated by empty line.
                let mut manifest = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    match stream.read(&mut byte) {
                        Ok(1) => manifest.push(byte[0]),
                        _ => {
                            warn!("blob-stream: failed to read manifest");
                            return;
                        }
                    }
                    // Detect end: "\n\n"
                    if manifest.len() >= 2
                        && manifest[manifest.len() - 1] == b'\n'
                        && manifest[manifest.len() - 2] == b'\n'
                    {
                        break;
                    }
                }

                let manifest_str = String::from_utf8_lossy(&manifest);
                let digests: Vec<&str> = manifest_str
                    .lines()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .collect();

                info!("blob-stream: streaming {} layers", digests.len());

                for (i, digest) in digests.iter().enumerate() {
                    let tar_path = std::path::Path::new(&blobs_dir_for_client)
                        .join(format!("{}.tar", digest));

                    let file_len = match std::fs::metadata(&tar_path) {
                        Ok(m) => m.len(),
                        Err(e) => {
                            warn!("blob-stream: layer {}/{} missing {}: {}",
                                i + 1, digests.len(), tar_path.display(), e);
                            // Send size=0 to signal error to guest.
                            let _ = stream.write_all(&0u64.to_le_bytes());
                            return;
                        }
                    };

                    // Send 8-byte LE file size.
                    if stream.write_all(&file_len.to_le_bytes()).is_err() {
                        warn!("blob-stream: send size failed for layer {}", i + 1);
                        return;
                    }

                    // Stream the tar file contents in large chunks.
                    let mut file = match std::fs::File::open(&tar_path) {
                        Ok(f) => f,
                        Err(e) => {
                            warn!("blob-stream: open failed {}: {}", tar_path.display(), e);
                            return;
                        }
                    };

                    let mut buf = vec![0u8; 256 * 1024]; // 256 KB chunks
                    let mut sent: u64 = 0;
                    loop {
                        let n = match std::io::Read::read(&mut file, &mut buf) {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(e) => {
                                warn!("blob-stream: read file error: {}", e);
                                return;
                            }
                        };
                        if stream.write_all(&buf[..n]).is_err() {
                            warn!("blob-stream: send data failed at byte {}", sent);
                            return;
                        }
                        sent += n as u64;
                    }

                    if i < 3 || i == digests.len() - 1 {
                        info!(
                            "blob-stream: layer {}/{} streamed ({} bytes)",
                            i + 1,
                            digests.len(),
                            sent
                        );
                    }
                }

                info!("blob-stream: all layers streamed");
            });
        }
    });

    Ok(())
}

/// Start a host-side FUSE server for a workspace share on a custom port.
///
/// Used for additional mounts like /workspace. Each mount gets a dedicated
/// HvSocket/vsock port (typically 50010+N) and a dedicated FUSE server rooted
/// at the corresponding host path.
pub fn start_fuse_workspace_server(vm_id: &str, workspace_dir: &Path, port: u32) -> io::Result<()> {
    let workspace_dir = workspace_dir.to_string_lossy().to_string();
    let share_name = format!("workspace-{}", port);

    info!(
        "FUSE workspace: starting host server for VM '{}' on port {} (root={})",
        vm_id,
        port,
        workspace_dir
    );

    let listener = hvsock_listener(vm_id, port)?;
    debug!(
        "FUSE workspace: listener created on port {} for VM '{}'",
        port,
        vm_id
    );

    std::thread::spawn(move || {
        loop {
            let client_sock = unsafe { accept(listener, std::ptr::null_mut(), std::ptr::null_mut()) };
            if client_sock == INVALID_SOCKET || client_sock == 0 {
                warn!("FUSE workspace: accept failed on port {}", port);
                continue;
            }

            let workspace_dir_for_client = workspace_dir.clone();
            let share_name_for_client = share_name.clone();
            std::thread::spawn(move || {
                info!("FUSE workspace: client connected on port {}", port);
                let stream = AcceptedHvSocket { sock: client_sock };
                let stop = Arc::new(AtomicBool::new(false));
                socket_worker::serve_fuse_on_stream(
                    &share_name_for_client,
                    &workspace_dir_for_client,
                    stream,
                    &stop,
                );
                info!("FUSE workspace: client disconnected on port {}", port);
            });
        }
    });

    Ok(())
}

// --- Helper functions for host-side HvSocket listener ---

extern "system" {
    fn bind(s: usize, name: *const SockaddrHv, namelen: i32) -> i32;
    fn listen(s: usize, backlog: i32) -> i32;
    fn accept(s: usize, addr: *mut SockaddrHv, addrlen: *mut i32) -> usize;
}

/// Create an AF_HYPERV listener socket bound to a specific vsock port.
/// Uses the actual VM RuntimeId — wildcard GUIDs don't work for HCS utility VMs.
fn hvsock_listener(vm_id: &str, port: u32) -> io::Result<usize> {
    let vm_guid = hcs_vm_id_to_guid(vm_id).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput,
            format!("cannot parse VM ID '{}' as GUID for listener", vm_id))
    })?;

    let sock = unsafe { socket(AF_HYPERV, SOCK_STREAM, HV_PROTOCOL_RAW) };
    if sock == INVALID_SOCKET || sock == 0 {
        return Err(io::Error::last_os_error());
    }

    let addr = SockaddrHv {
        family: AF_HYPERV as u16,
        reserved: 0,
        vm_id: vm_guid,
        service_id: service_guid_for_port(port),
    };

    let ret = unsafe {
        bind(sock, &addr, std::mem::size_of::<SockaddrHv>() as i32)
    };
    if ret != 0 {
        let err = io::Error::last_os_error();
        unsafe { closesocket(sock) };
        return Err(err);
    }

    let ret = unsafe { listen(sock, 16) };
    if ret != 0 {
        let err = io::Error::last_os_error();
        unsafe { closesocket(sock) };
        return Err(err);
    }

    debug!("HvSocket listener: bound to port {} (GUID {})", port,
        format!("{:08X}-FACB-11E6-BD58-64006A7986D3", port));

    Ok(sock)
}

/// Wrapper for an accepted AF_HYPERV socket that implements Read/Write.
struct AcceptedHvSocket {
    sock: usize,
}

impl Read for AcceptedHvSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let ret = unsafe { recv(self.sock, buf.as_mut_ptr(), buf.len() as i32, 0) };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ret as usize)
        }
    }
}

impl Write for AcceptedHvSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let ret = unsafe { send(self.sock, buf.as_ptr(), buf.len() as i32, 0) };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ret as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for AcceptedHvSocket {
    fn drop(&mut self) {
        if unsafe { closesocket(self.sock) } != 0 {
            error!("FUSE rootfs: closesocket failed for accepted client socket");
        }
    }
}

/// Receive exactly `buf.len()` bytes from an HvSocket.
fn hv_recv_exact(sock: usize, buf: &mut [u8]) -> io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        let ret = unsafe { recv(sock, buf.as_mut_ptr().add(off), (buf.len() - off) as i32, 0) };
        if ret <= 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "recv failed"));
        }
        off += ret as usize;
    }
    Ok(())
}

/// Send all bytes to an HvSocket.
fn hv_send_all(sock: usize, buf: &[u8]) -> io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        let ret = unsafe { send(sock, buf.as_ptr().add(off), (buf.len() - off) as i32, 0) };
        if ret <= 0 {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "send failed"));
        }
        off += ret as usize;
    }
    Ok(())
}
