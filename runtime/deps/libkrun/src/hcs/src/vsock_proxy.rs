// vsock_proxy.rs - AF_VSOCK proxy for HvSocket host↔guest communication.
//
// Runs inside the guest VM. Provides:
//
// INBOUND (host→guest):
//   vsock 50001 → TCP 127.0.0.1:8080 (agent-gateway HTTP)
//   vsock 50022 → TCP 127.0.0.1:22   (SSH)
//
// OUTBOUND (guest→internet via host):
//   UDP  127.0.0.1:53   → vsock 50053 → host DNS relay
//   TCP  127.0.0.1:1080 → vsock 50080 → host TCP connect relay
//
// The outbound proxies let the guest reach the internet without HCN NAT.
// Guest resolv.conf points to 127.0.0.1:53 for DNS.
// Guest iptables REDIRECT sends all outbound TCP to 127.0.0.1:1080.
//
// Build: CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
//        cargo build -p hcs --bin vsock_proxy --target x86_64-unknown-linux-musl --release

use std::os::raw::c_void;

const AF_VSOCK: i32 = 40;
const AF_INET: i32 = 2;
const SOCK_STREAM: i32 = 1;
const SOCK_DGRAM: i32 = 2;
const SOL_SOCKET: i32 = 1;
const SO_REUSEADDR: i32 = 2;
const SOL_IP: i32 = 0;
const SO_ORIGINAL_DST: i32 = 80;
const VMADDR_CID_ANY: u32 = u32::MAX; // -1U
const VMADDR_CID_HOST: u32 = 2;

#[repr(C)]
struct SockaddrVm {
    svm_family: u16,
    svm_reserved1: u16,
    svm_port: u32,
    svm_cid: u32,
    svm_flags: u8,
    svm_zero: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrIn {
    sin_family: u16,
    sin_port: u16, // network byte order
    sin_addr: u32, // network byte order
    sin_zero: [u8; 8],
}

extern "C" {
    fn socket(domain: i32, ty: i32, protocol: i32) -> i32;
    fn bind(sockfd: i32, addr: *const c_void, addrlen: u32) -> i32;
    fn listen(sockfd: i32, backlog: i32) -> i32;
    fn accept(sockfd: i32, addr: *mut c_void, addrlen: *mut u32) -> i32;
    fn connect(sockfd: i32, addr: *const c_void, addrlen: u32) -> i32;
    fn setsockopt(sockfd: i32, level: i32, optname: i32, optval: *const c_void, optlen: u32) -> i32;
    fn getsockopt(sockfd: i32, level: i32, optname: i32, optval: *mut c_void, optlen: *mut u32) -> i32;
    fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
    fn write(fd: i32, buf: *const u8, count: usize) -> isize;
    fn close(fd: i32) -> i32;
    fn fork() -> i32;
    fn _exit(status: i32) -> !;
    fn sendto(
        sockfd: i32, buf: *const u8, len: usize, flags: i32,
        dest_addr: *const c_void, addrlen: u32,
    ) -> isize;
    fn recvfrom(
        sockfd: i32, buf: *mut u8, len: usize, flags: i32,
        src_addr: *mut c_void, addrlen: *mut u32,
    ) -> isize;
    fn signal(signum: i32, handler: usize) -> usize;
}

const SIGCHLD: i32 = 17;
const SIG_IGN: usize = 1;

const IPPROTO_TCP: i32 = 6;
const TCP_NODELAY: i32 = 1;

/// Disable Nagle's algorithm on a TCP socket. Errors ignored — best-effort.
fn set_tcp_nodelay(fd: i32) {
    let one: i32 = 1;
    unsafe {
        setsockopt(fd, IPPROTO_TCP, TCP_NODELAY,
            &one as *const i32 as *const c_void, 4);
    }
}

fn write_all(fd: i32, buf: &[u8]) -> bool {
    let mut off = 0;
    while off < buf.len() {
        let n = unsafe { write(fd, buf.as_ptr().add(off), buf.len() - off) };
        if n <= 0 {
            return false;
        }
        off += n as usize;
    }
    true
}

fn read_exact(fd: i32, buf: &mut [u8]) -> bool {
    let mut off = 0;
    while off < buf.len() {
        let n = unsafe { read(fd, buf.as_mut_ptr().add(off), buf.len() - off) };
        if n <= 0 {
            return false;
        }
        off += n as usize;
    }
    true
}

fn write_str(s: &str) {
    let _ = write_all(2, s.as_bytes());
}

/// Proxy data between two fds until one side closes.
/// Uses a 64KB buffer to match the increased HvSocket ring buffer sizes
/// and reduce syscall overhead during heavy SSH traffic.
fn proxy_fds(a: i32, b: i32) {
    let mut buf = [0u8; 65536];
    loop {
        let n = unsafe { read(a, buf.as_mut_ptr(), buf.len()) };
        if n <= 0 {
            break;
        }
        if !write_all(b, &buf[..n as usize]) {
            break;
        }
    }
    unsafe {
        close(a);
        close(b);
    }
}

fn connect_tcp(tcp_port: u16) -> i32 {
    let sock = unsafe { socket(AF_INET, SOCK_STREAM, 0) };
    if sock < 0 {
        return -1;
    }

    let addr = SockaddrIn {
        sin_family: AF_INET as u16,
        sin_port: tcp_port.to_be(),
        sin_addr: u32::from_ne_bytes([127, 0, 0, 1]),
        sin_zero: [0; 8],
    };

    let ret = unsafe {
        connect(
            sock,
            &addr as *const SockaddrIn as *const c_void,
            std::mem::size_of::<SockaddrIn>() as u32,
        )
    };

    if ret < 0 {
        unsafe { close(sock) };
        return -1;
    }
    set_tcp_nodelay(sock);
    set_large_buffers(sock);
    sock
}

/// Increase socket buffer sizes to reduce backpressure deadlocks.
/// On HvSocket/vsock the kernel maps SO_SNDBUF/SO_RCVBUF to the
/// underlying VMBus ring buffer size (up to 256KB on Win10 v5+).
fn set_large_buffers(fd: i32) {
    const SO_SNDBUF: i32 = 7;  // Linux SOL_SOCKET SO_SNDBUF
    const SO_RCVBUF: i32 = 8;  // Linux SOL_SOCKET SO_RCVBUF
    let size: i32 = 262144; // 256KB
    unsafe {
        setsockopt(fd, SOL_SOCKET, SO_SNDBUF,
            &size as *const i32 as *const c_void, 4);
        setsockopt(fd, SOL_SOCKET, SO_RCVBUF,
            &size as *const i32 as *const c_void, 4);
    }
}

fn connect_vsock(port: u32) -> i32 {
    let sock = unsafe { socket(AF_VSOCK, SOCK_STREAM, 0) };
    if sock < 0 {
        return -1;
    }

    // Increase ring buffer sizes before connect.
    set_large_buffers(sock);

    let addr = SockaddrVm {
        svm_family: AF_VSOCK as u16,
        svm_reserved1: 0,
        svm_port: port,
        svm_cid: VMADDR_CID_HOST,
        svm_flags: 0,
        svm_zero: [0; 3],
    };

    let ret = unsafe {
        connect(
            sock,
            &addr as *const SockaddrVm as *const c_void,
            std::mem::size_of::<SockaddrVm>() as u32,
        )
    };

    if ret < 0 {
        unsafe { close(sock) };
        return -1;
    }
    sock
}

/// Run a vsock→TCP proxy loop (inbound): listen on `vsock_port`, forward to local `tcp_port`.
fn run_inbound_proxy(vsock_port: u32, tcp_port: u16) -> ! {
    let srv = unsafe { socket(AF_VSOCK, SOCK_STREAM, 0) };
    if srv < 0 {
        write_str("vsock_proxy: socket() failed\n");
        unsafe { _exit(1) };
    }

    let one: i32 = 1;
    unsafe {
        setsockopt(
            srv,
            SOL_SOCKET,
            SO_REUSEADDR,
            &one as *const i32 as *const c_void,
            4,
        );
    }
    // Pre-set large buffers on the listener — inherited by accepted sockets
    // on some kernels.
    set_large_buffers(srv);

    let addr = SockaddrVm {
        svm_family: AF_VSOCK as u16,
        svm_reserved1: 0,
        svm_port: vsock_port,
        svm_cid: VMADDR_CID_ANY,
        svm_flags: 0,
        svm_zero: [0; 3],
    };

    if unsafe {
        bind(
            srv,
            &addr as *const SockaddrVm as *const c_void,
            std::mem::size_of::<SockaddrVm>() as u32,
        )
    } < 0
    {
        write_str("vsock_proxy: bind() failed\n");
        unsafe { _exit(1) };
    }

    if unsafe { listen(srv, 8) } < 0 {
        write_str("vsock_proxy: listen() failed\n");
        unsafe { _exit(1) };
    }

    loop {
        let client = unsafe { accept(srv, std::ptr::null_mut(), std::ptr::null_mut()) };
        if client < 0 {
            continue;
        }

        // Increase ring buffer sizes on the accepted vsock connection.
        set_large_buffers(client);

        let backend = connect_tcp(tcp_port);
        if backend < 0 {
            write_str("vsock_proxy: TCP connect failed\n");
            unsafe { close(client) };
            continue;
        }

        let pid = unsafe { fork() };
        if pid < 0 {
            unsafe {
                close(client);
                close(backend);
            }
            continue;
        }

        if pid == 0 {
            proxy_fds(backend, client);
            unsafe { _exit(0) };
        } else {
            proxy_fds(client, backend);
        }
    }
}

/// DNS forwarder: binds UDP 127.0.0.1:53, forwards each query over vsock stream
/// to host port 50053 using length-prefix framing (2-byte big-endian length + data).
/// Host resolves and returns the response with the same framing.
fn run_dns_forwarder() -> ! {
    let udp = unsafe { socket(AF_INET, SOCK_DGRAM, 0) };
    if udp < 0 {
        write_str("vsock_proxy: DNS socket() failed\n");
        unsafe { _exit(1) };
    }

    let one: i32 = 1;
    unsafe {
        setsockopt(udp, SOL_SOCKET, SO_REUSEADDR, &one as *const i32 as *const c_void, 4);
    }

    let addr = SockaddrIn {
        sin_family: AF_INET as u16,
        sin_port: 53u16.to_be(),
        sin_addr: u32::from_ne_bytes([127, 0, 0, 1]),
        sin_zero: [0; 8],
    };

    if unsafe {
        bind(udp, &addr as *const SockaddrIn as *const c_void,
             std::mem::size_of::<SockaddrIn>() as u32)
    } < 0 {
        write_str("vsock_proxy: DNS bind(:53) failed\n");
        unsafe { _exit(1) };
    }

    write_str("vsock_proxy: DNS forwarder listening on 127.0.0.1:53\n");

    let mut buf = [0u8; 4096];
    loop {
        let mut client_addr: SockaddrIn = SockaddrIn {
            sin_family: 0, sin_port: 0, sin_addr: 0, sin_zero: [0; 8],
        };
        let mut addrlen = std::mem::size_of::<SockaddrIn>() as u32;

        let n = unsafe {
            recvfrom(
                udp, buf.as_mut_ptr(), buf.len(), 0,
                &mut client_addr as *mut SockaddrIn as *mut c_void,
                &mut addrlen,
            )
        };
        if n <= 0 {
            continue;
        }
        let query_len = n as usize;

        // Connect to host DNS relay via vsock
        let vs = connect_vsock(50053);
        if vs < 0 {
            continue;
        }

        // Send: 2-byte length prefix + DNS query
        let len_bytes = (query_len as u16).to_be_bytes();
        if !write_all(vs, &len_bytes) || !write_all(vs, &buf[..query_len]) {
            unsafe { close(vs) };
            continue;
        }

        // Read: 2-byte length prefix + DNS response
        let mut resp_len_buf = [0u8; 2];
        if !read_exact(vs, &mut resp_len_buf) {
            unsafe { close(vs) };
            continue;
        }
        let resp_len = u16::from_be_bytes(resp_len_buf) as usize;
        if resp_len > buf.len() {
            unsafe { close(vs) };
            continue;
        }
        if !read_exact(vs, &mut buf[..resp_len]) {
            unsafe { close(vs) };
            continue;
        }
        unsafe { close(vs) };

        // Send DNS response back to client
        unsafe {
            sendto(
                udp, buf.as_ptr(), resp_len, 0,
                &client_addr as *const SockaddrIn as *const c_void,
                std::mem::size_of::<SockaddrIn>() as u32,
            );
        }
    }
}

/// Dynamic inbound forwarder: listens on AF_VSOCK `vsock_port`, reads a 2-byte
/// big-endian destination TCP port from each connection header, then bridges
/// to 127.0.0.1:<port> inside the guest. Used by host to expose any guest TCP
/// port (e.g. OAuth callback listeners on localhost:1455) to host loopback.
fn run_dynamic_inbound_forwarder(vsock_port: u32) -> ! {
    let srv = unsafe { socket(AF_VSOCK, SOCK_STREAM, 0) };
    if srv < 0 {
        write_str("vsock_proxy: dyn-inbound socket() failed\n");
        unsafe { _exit(1) };
    }

    let one: i32 = 1;
    unsafe {
        setsockopt(srv, SOL_SOCKET, SO_REUSEADDR, &one as *const i32 as *const c_void, 4);
    }

    let addr = SockaddrVm {
        svm_family: AF_VSOCK as u16,
        svm_reserved1: 0,
        svm_port: vsock_port,
        svm_cid: VMADDR_CID_ANY,
        svm_flags: 0,
        svm_zero: [0; 3],
    };

    if unsafe {
        bind(srv, &addr as *const SockaddrVm as *const c_void,
             std::mem::size_of::<SockaddrVm>() as u32)
    } < 0 {
        write_str("vsock_proxy: dyn-inbound bind() failed\n");
        unsafe { _exit(1) };
    }

    if unsafe { listen(srv, 64) } < 0 {
        write_str("vsock_proxy: dyn-inbound listen() failed\n");
        unsafe { _exit(1) };
    }

    write_str("vsock_proxy: dynamic inbound forwarder listening on vsock:50090\n");

    // Auto-reap children.
    unsafe { signal(SIGCHLD, SIG_IGN); }

    loop {
        let client = unsafe { accept(srv, std::ptr::null_mut(), std::ptr::null_mut()) };
        if client < 0 {
            continue;
        }

        let pid = unsafe { fork() };
        if pid < 0 {
            unsafe { close(client) };
            continue;
        }
        if pid > 0 {
            unsafe { close(client) };
            continue;
        }

        // Child: read 2-byte BE port, connect TCP, bidir-proxy, exit.
        unsafe { close(srv) };
        let mut port_buf = [0u8; 2];
        if !read_exact(client, &mut port_buf) {
            unsafe { close(client); _exit(0) };
        }
        let dest_port = u16::from_be_bytes(port_buf);

        let backend = connect_tcp(dest_port);
        if backend < 0 {
            // Send 1-byte failure status so host can report cleanly.
            let _ = write_all(client, &[1u8]);
            unsafe { close(client); _exit(0) };
        }
        // Send 1-byte success status.
        if !write_all(client, &[0u8]) {
            unsafe { close(client); close(backend); _exit(0) };
        }

        // Bidir: subchild handles backend->client, this child handles client->backend.
        let pid2 = unsafe { fork() };
        if pid2 < 0 {
            unsafe { close(client); close(backend); _exit(1) };
        }
        if pid2 == 0 {
            proxy_fds(backend, client);
            unsafe { _exit(0) };
        } else {
            proxy_fds(client, backend);
            unsafe { _exit(0) };
        }
    }
}

/// TCP outbound proxy: binds TCP 127.0.0.1:1080, accepts redirected connections,
/// reads original destination via SO_ORIGINAL_DST (iptables REDIRECT),
/// opens vsock to host port 50080, sends 6-byte header (4-byte IP + 2-byte port),
/// then proxies bidirectionally.
fn run_tcp_outbound_proxy() -> ! {
    let srv = unsafe { socket(AF_INET, SOCK_STREAM, 0) };
    if srv < 0 {
        write_str("vsock_proxy: TCP outbound socket() failed\n");
        unsafe { _exit(1) };
    }

    let one: i32 = 1;
    unsafe {
        setsockopt(srv, SOL_SOCKET, SO_REUSEADDR, &one as *const i32 as *const c_void, 4);
    }

    let addr = SockaddrIn {
        sin_family: AF_INET as u16,
        sin_port: 1080u16.to_be(),
        sin_addr: u32::from_ne_bytes([127, 0, 0, 1]),
        sin_zero: [0; 8],
    };

    if unsafe {
        bind(srv, &addr as *const SockaddrIn as *const c_void,
             std::mem::size_of::<SockaddrIn>() as u32)
    } < 0 {
        write_str("vsock_proxy: TCP outbound bind(:1080) failed\n");
        unsafe { _exit(1) };
    }

    if unsafe { listen(srv, 64) } < 0 {
        write_str("vsock_proxy: TCP outbound listen() failed\n");
        unsafe { _exit(1) };
    }

    write_str("vsock_proxy: TCP outbound proxy listening on 127.0.0.1:1080\n");

    // Auto-reap children so parent never has to waitpid().
    unsafe { signal(SIGCHLD, SIG_IGN); }

    let mut conn_id: u64 = 0;
    loop {
        let client = unsafe { accept(srv, std::ptr::null_mut(), std::ptr::null_mut()) };
        if client < 0 {
            continue;
        }
        let cid = conn_id;
        conn_id += 1;

        // Fork immediately so the parent returns to accept() without blocking.
        // The child does ALL per-connection work (SO_ORIGINAL_DST, vsock connect,
        // header exchange, bidirectional proxy) and exits.
        let pid = unsafe { fork() };
        if pid < 0 {
            unsafe { close(client) };
            continue;
        }
        if pid > 0 {
            // Parent: child owns the client fd now. Loop back to accept() ASAP.
            unsafe { close(client) };
            continue;
        }

        // ---- child: handle this connection end-to-end ----
        unsafe { close(srv) }; // child doesn't need the listening socket
        set_tcp_nodelay(client); // small TLS records ship without Nagle delay
        let t_accept = std::time::Instant::now();
        let _ = write_all(2, format!("[vsock_proxy/{}] G0 accepted\n", cid).as_bytes());

        // Get original destination via SO_ORIGINAL_DST (set by iptables REDIRECT)
        let mut orig_dst = SockaddrIn {
            sin_family: 0, sin_port: 0, sin_addr: 0, sin_zero: [0; 8],
        };
        let mut optlen = std::mem::size_of::<SockaddrIn>() as u32;
        let ret = unsafe {
            getsockopt(
                client, SOL_IP, SO_ORIGINAL_DST,
                &mut orig_dst as *mut SockaddrIn as *mut c_void,
                &mut optlen,
            )
        };
        if ret < 0 || orig_dst.sin_addr == 0 {
            unsafe { close(client); _exit(0) };
        }
        let _ = write_all(2, format!("[vsock_proxy/{}] G1 SO_ORIGINAL_DST +{}us\n", cid, t_accept.elapsed().as_micros()).as_bytes());

        // Connect to host TCP relay via vsock
        let t_vs = std::time::Instant::now();
        let vs = connect_vsock(50080);
        let _ = write_all(2, format!("[vsock_proxy/{}] G2 connect_vsock(50080) +{}ms (total {}ms) fd={}\n", cid, t_vs.elapsed().as_millis(), t_accept.elapsed().as_millis(), vs).as_bytes());
        if vs < 0 {
            unsafe { close(client); _exit(0) };
        }

        // Send 6-byte header: 4-byte dest IP (network order) + 2-byte dest port (network order)
        let ip_bytes = orig_dst.sin_addr.to_ne_bytes();
        let port_bytes = orig_dst.sin_port.to_ne_bytes();
        let mut header = [0u8; 6];
        header[0..4].copy_from_slice(&ip_bytes);
        header[4..6].copy_from_slice(&port_bytes);
        let t_w = std::time::Instant::now();
        if !write_all(vs, &header) {
            unsafe { close(client); close(vs); _exit(0) };
        }
        let _ = write_all(2, format!("[vsock_proxy/{}] G3 write_header +{}ms (total {}ms)\n", cid, t_w.elapsed().as_millis(), t_accept.elapsed().as_millis()).as_bytes());

        // Read 1-byte status from host (0 = connected, 1 = failed)
        let mut status = [0u8; 1];
        let t_r = std::time::Instant::now();
        if !read_exact(vs, &mut status) || status[0] != 0 {
            unsafe { close(client); close(vs); _exit(0) };
        }
        let _ = write_all(2, format!("[vsock_proxy/{}] G4 read_status +{}ms (total {}ms)\n", cid, t_r.elapsed().as_millis(), t_accept.elapsed().as_millis()).as_bytes());

        // Bidirectional proxy: subchild handles vs->client, this child handles client->vs.
        let pid2 = unsafe { fork() };
        if pid2 < 0 {
            unsafe { close(client); close(vs); _exit(1) };
        }
        if pid2 == 0 {
            proxy_fds(vs, client);
            unsafe { _exit(0) };
        } else {
            proxy_fds(client, vs);
            unsafe { _exit(0) };
        }
    }
}

fn main() {
    write_str("vsock_proxy: starting (inbound: 50001->:8080, 50022->:22, 50090->dyn | outbound: dns:53, tcp:1080)\n");

    // Fork into 5 proxy processes:
    // 1. Gateway inbound       (vsock 50001 → TCP 8080)
    // 2. SSH inbound           (vsock 50022 → TCP 22)
    // 3. DNS forwarder         (UDP 127.0.0.1:53 → vsock 50053 → host)
    // 4. TCP outbound          (TCP 127.0.0.1:1080 → vsock 50080 → host)
    // 5. Dynamic TCP inbound   (vsock 50090 → TCP 127.0.0.1:<port-from-header>)

    if unsafe { fork() } == 0 {
        run_dns_forwarder();
    }
    if unsafe { fork() } == 0 {
        run_inbound_proxy(50022, 22);
    }
    if unsafe { fork() } == 0 {
        run_tcp_outbound_proxy();
    }
    if unsafe { fork() } == 0 {
        run_dynamic_inbound_forwarder(50090);
    }
    // Parent: gateway inbound
    run_inbound_proxy(50001, 8080);
}
