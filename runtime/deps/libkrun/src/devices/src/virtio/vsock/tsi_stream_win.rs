// Windows implementation of TsiStreamProxy using Winsock2 via windows-sys.
//
// This file provides the same TsiStreamProxy interface as the Unix version
// (tsi_stream.rs), but uses Winsock2 APIs for socket operations.

use std::collections::HashMap;
use std::io;
use std::num::Wrapping;
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Networking::WinSock::{
    accept, bind, closesocket, connect, getpeername, listen, recv, send, setsockopt, shutdown,
    socket, AF_INET, AF_INET6, FIONBIO, INVALID_SOCKET, IPPROTO_TCP, SD_BOTH, SD_RECEIVE,
    SD_SEND, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6, SOCKET, SOCKET_ERROR, SOCK_STREAM,
    SOL_SOCKET, SO_REUSEADDR, WSAECONNREFUSED, WSAEINPROGRESS, WSAEWOULDBLOCK, WSAGetLastError,
};
use windows_sys::Win32::Networking::WinSock::ioctlsocket;

use super::super::Queue as VirtQueue;
use super::defs;
use super::defs::uapi;
use super::muxer::{push_packet, MuxerRx};
use super::muxer_rxq::MuxerRxQ;
use super::packet::{
    TsiAcceptReq, TsiConnectReq, TsiGetnameRsp, TsiListenReq, TsiSendtoAddr, VsockPacket,
};
use super::proxy::{
    NewProxyType, PollableFd, Proxy, ProxyAddressFamily, ProxyError, ProxyOwnedFd, ProxyRemoval,
    ProxyStatus, ProxyUpdate, RecvPkt,
};
use utils::epoll::EventSet;

use vm_memory::GuestMemoryMmap;

use std::net::{Ipv4Addr, SocketAddrV4};

// Linux errno values for the guest protocol.
const LINUX_EINVAL: i32 = 22;
const LINUX_ECONNREFUSED: i32 = 111;
const LINUX_EWOULDBLOCK: i32 = 11;
// Linux O_NONBLOCK value used in the accept flags from the guest.
const LINUX_O_NONBLOCK: u32 = 2048;

/// A Winsock2 socket wrapper that closes on Drop.
struct WinSocket {
    sock: SOCKET,
}

impl WinSocket {
    fn new(sock: SOCKET) -> Self {
        WinSocket { sock }
    }

    fn raw(&self) -> SOCKET {
        self.sock
    }

    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        let mut mode: u32 = if nonblocking { 1 } else { 0 };
        let result = unsafe { ioctlsocket(self.sock, FIONBIO, &mut mode as *mut u32) };
        if result == SOCKET_ERROR {
            Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
        } else {
            Ok(())
        }
    }

    fn as_pollable_fd(&self) -> PollableFd {
        self.sock as PollableFd
    }
}

impl Drop for WinSocket {
    fn drop(&mut self) {
        if self.sock != INVALID_SOCKET {
            unsafe {
                closesocket(self.sock);
            }
        }
    }
}

// SAFETY: SOCKET is just a usize handle, safe to send across threads.
unsafe impl Send for WinSocket {}

/// Convert a ProxyAddressFamily to a Winsock address family constant.
fn af_to_winsock(family: ProxyAddressFamily) -> i32 {
    match family {
        ProxyAddressFamily::Inet => AF_INET as i32,
        ProxyAddressFamily::Inet6 => AF_INET6 as i32,
        ProxyAddressFamily::Unix => {
            // AF_UNIX on Windows is 1
            1
        }
    }
}

/// Build a SOCKADDR + length from the raw bytes stored in our SockaddrStorage compat type.
/// The guest sends Linux-format sockaddr, which for AF_INET and AF_INET6 is wire-compatible
/// with Windows SOCKADDR_IN / SOCKADDR_IN6 (both use network byte order for port/addr).
/// We just need to fix the sa_family field (Linux uses u16 le; Windows uses u16 le too for
/// SOCKADDR, so it's compatible for AF_INET=2 and AF_INET6=23 vs Linux AF_INET6=10).
fn sockaddr_from_storage(
    storage: &super::packet::SockaddrStorage,
) -> Option<(Vec<u8>, i32)> {
    let len = storage.len() as usize;
    if len < 2 {
        return None;
    }
    let src_ptr = storage.as_ptr();
    let mut buf = vec![0u8; len];
    unsafe {
        std::ptr::copy_nonoverlapping(src_ptr, buf.as_mut_ptr(), len);
    }

    // Fix up address family: Linux AF_INET6 = 10, Windows AF_INET6 = 23.
    // Linux AF_INET = 2 matches Windows AF_INET = 2.
    let linux_family = u16::from_le_bytes([buf[0], buf[1]]);
    let win_family: u16 = match linux_family {
        2 => AF_INET as u16,   // AF_INET
        10 => AF_INET6 as u16, // AF_INET6
        1 => 1,                // AF_UNIX
        other => other,
    };
    buf[0..2].copy_from_slice(&win_family.to_le_bytes());

    Some((buf, len as i32))
}

/// Create a SockaddrStorage-like response from a Windows SOCKADDR buffer.
/// Converts Windows AF values back to Linux AF values for the guest.
fn storage_from_sockaddr(buf: &[u8], len: usize) -> super::packet::SockaddrStorage {
    let mut out = vec![0u8; len];
    out[..len].copy_from_slice(&buf[..len]);

    // Fix up address family: Windows AF_INET6 = 23 -> Linux AF_INET6 = 10
    if len >= 2 {
        let win_family = u16::from_le_bytes([out[0], out[1]]);
        let linux_family: u16 = match win_family {
            x if x == AF_INET as u16 => 2,
            x if x == AF_INET6 as u16 => 10,
            1 => 1,
            other => other,
        };
        out[0..2].copy_from_slice(&linux_family.to_le_bytes());
    }

    super::packet::SockaddrStorage::from_raw_bytes(&out, len as u32)
        .unwrap_or_default()
}

pub struct TsiStreamProxy {
    id: u64,
    cid: u64,
    parent_id: u64,
    family: ProxyAddressFamily,
    local_port: u32,
    peer_port: u32,
    control_port: u32,
    sock: WinSocket,
    pub status: ProxyStatus,
    mem: GuestMemoryMmap,
    queue: Arc<Mutex<VirtQueue>>,
    rxq: Arc<Mutex<MuxerRxQ>>,
    rx_cnt: Wrapping<u32>,
    tx_cnt: Wrapping<u32>,
    last_tx_cnt_sent: Wrapping<u32>,
    peer_buf_alloc: u32,
    peer_fwd_cnt: Wrapping<u32>,
    push_cnt: Wrapping<u32>,
    pending_accepts: u64,
}

impl TsiStreamProxy {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        cid: u64,
        family: u16,
        local_port: u32,
        peer_port: u32,
        control_port: u32,
        mem: GuestMemoryMmap,
        queue: Arc<Mutex<VirtQueue>>,
        rxq: Arc<Mutex<MuxerRxQ>>,
    ) -> Result<Self, ProxyError> {
        let family = match family {
            defs::LINUX_AF_INET => ProxyAddressFamily::Inet,
            defs::LINUX_AF_INET6 => ProxyAddressFamily::Inet6,
            _ => return Err(ProxyError::InvalidFamily),
        };

        let af = af_to_winsock(family);
        let raw_sock = unsafe { socket(af, SOCK_STREAM as i32, IPPROTO_TCP as i32) };
        if raw_sock == INVALID_SOCKET {
            return Err(ProxyError::CreatingSocket(io::Error::from_raw_os_error(
                unsafe { WSAGetLastError() },
            )));
        }

        let sock = WinSocket::new(raw_sock);

        // Set non-blocking mode.
        if let Err(e) = sock.set_nonblocking(true) {
            warn!("error switching to non-blocking: id={id}, err={e}");
        }

        // Set SO_REUSEADDR.
        let optval: i32 = 1;
        let ret = unsafe {
            setsockopt(
                sock.raw(),
                SOL_SOCKET,
                SO_REUSEADDR,
                &optval as *const i32 as *const u8,
                std::mem::size_of::<i32>() as i32,
            )
        };
        if ret == SOCKET_ERROR {
            return Err(ProxyError::SettingReuseAddr(io::Error::from_raw_os_error(
                unsafe { WSAGetLastError() },
            )));
        }

        Ok(TsiStreamProxy {
            id,
            cid,
            parent_id: 0,
            family,
            local_port,
            peer_port,
            control_port,
            sock,
            status: ProxyStatus::Idle,
            mem,
            queue,
            rxq,
            rx_cnt: Wrapping(0),
            tx_cnt: Wrapping(0),
            last_tx_cnt_sent: Wrapping(0),
            peer_buf_alloc: 0,
            peer_fwd_cnt: Wrapping(0),
            push_cnt: Wrapping(0),
            pending_accepts: 0,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_reverse(
        id: u64,
        cid: u64,
        parent_id: u64,
        family: ProxyAddressFamily,
        local_port: u32,
        peer_port: u32,
        owned_fd: ProxyOwnedFd,
        mem: GuestMemoryMmap,
        queue: Arc<Mutex<VirtQueue>>,
        rxq: Arc<Mutex<MuxerRxQ>>,
    ) -> Self {
        debug!("new_reverse: id={id} local_port={local_port} peer_port={peer_port}");
        // Extract the raw socket handle from ProxyOwnedFd.
        let raw_handle = owned_fd.into_raw_handle();
        let sock = WinSocket::new(raw_handle as SOCKET);

        TsiStreamProxy {
            id,
            cid,
            parent_id,
            family,
            local_port,
            peer_port,
            control_port: 0,
            sock,
            status: ProxyStatus::ReverseInit,
            mem,
            queue,
            rxq,
            rx_cnt: Wrapping(0),
            tx_cnt: Wrapping(0),
            last_tx_cnt_sent: Wrapping(0),
            peer_buf_alloc: 0,
            peer_fwd_cnt: Wrapping(0),
            push_cnt: Wrapping(0),
            pending_accepts: 0,
        }
    }

    fn init_data_pkt(&self, pkt: &mut VsockPacket) {
        debug!(
            "init_data_pkt: id={}, local_port={}, peer_port={}",
            self.id, self.local_port, self.peer_port
        );
        pkt.set_op(uapi::VSOCK_OP_RW)
            .set_src_cid(uapi::VSOCK_HOST_CID)
            .set_dst_cid(self.cid)
            .set_src_port(self.local_port)
            .set_dst_port(self.peer_port)
            .set_type(uapi::VSOCK_TYPE_STREAM)
            .set_buf_alloc(defs::CONN_TX_BUF_SIZE as u32)
            .set_fwd_cnt(self.tx_cnt.0);
    }

    fn try_listen(
        &mut self,
        req: &TsiListenReq,
        host_port_map: &Option<HashMap<u16, u16>>,
    ) -> i32 {
        use super::packet::SockaddrStorage;

        if self.status == ProxyStatus::Listening || self.status == ProxyStatus::WaitingOnAccept {
            return 0;
        }

        let addr: SockaddrStorage = if let Some(port_map) = host_port_map {
            if let Some(sin) = req.addr.as_sockaddr_in() {
                debug!("sockaddr is ipv4");
                if let Some(port) = port_map.get(&sin.port()) {
                    SocketAddrV4::new(sin.ip(), *port).into()
                } else {
                    req.addr
                }
            } else if let Some(sin6) = req.addr.as_sockaddr_in6() {
                debug!("sockaddr is ipv6");
                if let Some(port) = port_map.get(&sin6.port()) {
                    use std::net::SocketAddrV6;
                    SocketAddrV6::new(sin6.ip(), *port, sin6.flowinfo(), sin6.flowinfo()).into()
                } else {
                    req.addr
                }
            } else {
                return -LINUX_EINVAL;
            }
        } else {
            req.addr
        };

        // Convert to Winsock sockaddr and bind.
        let (sa_buf, sa_len) = match sockaddr_from_storage(&addr) {
            Some(v) => v,
            None => return -LINUX_EINVAL,
        };

        let ret =
            unsafe { bind(self.sock.raw(), sa_buf.as_ptr() as *const SOCKADDR, sa_len) };
        if ret == SOCKET_ERROR {
            let err = unsafe { WSAGetLastError() };
            warn!("tcp bind: id={} err={}", self.id, err);
            return -LINUX_EINVAL;
        }

        debug!("tcp bind: id={}", self.id);

        // Clamp backlog to a reasonable value. Windows SOMAXCONN is 0x7fffffff.
        let clamped_backlog = req.backlog.clamp(0, 128);
        let ret = unsafe { listen(self.sock.raw(), clamped_backlog) };
        if ret == SOCKET_ERROR {
            let err = unsafe { WSAGetLastError() };
            warn!("proxy: id={} listen err={}", self.id, err);
            return -LINUX_EINVAL;
        }

        debug!("proxy: id={}", self.id);
        0
    }

    fn peer_avail_credit(&self) -> usize {
        (Wrapping(self.peer_buf_alloc) - (self.rx_cnt - self.peer_fwd_cnt)).0 as usize
    }

    fn recv_to_pkt(&self, pkt: &mut VsockPacket) -> RecvPkt {
        if let Some(buf) = pkt.buf_mut() {
            let peer_credit = self.peer_avail_credit();
            let max_len = std::cmp::min(buf.len(), peer_credit);

            debug!(
                "recv_to_pkt: peer_avail_credit={}, buf.len={}, max_len={}",
                self.peer_avail_credit(),
                buf.len(),
                max_len,
            );

            if max_len == 0 {
                return RecvPkt::WaitForCredit;
            }

            let cnt = unsafe {
                recv(
                    self.sock.raw(),
                    buf.as_mut_ptr(),
                    max_len as i32,
                    0,
                )
            };

            if cnt > 0 {
                debug!("recv cnt={cnt}");
                debug!("recv rx_cnt={}", self.rx_cnt);
                RecvPkt::Read(cnt as usize)
            } else if cnt == 0 {
                RecvPkt::Close
            } else {
                let err = unsafe { WSAGetLastError() };
                if err == WSAEWOULDBLOCK {
                    // No data available right now, not an error.
                    RecvPkt::Error
                } else {
                    debug!("recv_pkt: recv error: {err}");
                    RecvPkt::Error
                }
            }
        } else {
            debug!("recv_pkt: pkt without buf");
            RecvPkt::Error
        }
    }

    fn recv_pkt(&mut self) -> (bool, bool) {
        let mut have_used = false;
        let mut wait_credit = false;
        let mut queue = self.queue.lock().unwrap();

        while let Some(head) = queue.pop(&self.mem) {
            let len = match VsockPacket::from_rx_virtq_head(&head) {
                Ok(mut pkt) => match self.recv_to_pkt(&mut pkt) {
                    RecvPkt::WaitForCredit => {
                        wait_credit = true;
                        0
                    }
                    RecvPkt::Read(cnt) => {
                        self.rx_cnt += Wrapping(cnt as u32);
                        self.init_data_pkt(&mut pkt);
                        pkt.set_len(cnt as u32);
                        pkt.hdr().len() + cnt
                    }
                    RecvPkt::Close => {
                        self.status = ProxyStatus::Closed;
                        0
                    }
                    RecvPkt::Error => 0,
                },
                Err(e) => {
                    debug!("recv_pkt: RX queue error: {e:?}");
                    0
                }
            };

            if len == 0 {
                queue.undo_pop();
                break;
            } else {
                have_used = true;
                self.push_cnt += Wrapping(len as u32);
                debug!(
                    "recv_pkt: pushing packet with {} bytes, push_cnt={}",
                    len, self.push_cnt
                );
                if let Err(e) = queue.add_used(&self.mem, head.index, len as u32) {
                    error!("failed to add used elements to the queue: {e:?}");
                }
            }
        }

        debug!("recv_pkt: have_used={have_used}");
        (have_used, wait_credit)
    }

    fn push_connect_rsp(&self, result: i32) {
        debug!(
            "push_connect_rsp: id: {}, control_port: {}, result: {}",
            self.id, self.control_port, result
        );

        let rx = MuxerRx::ConnResponse {
            local_port: 1025,
            peer_port: self.control_port,
            result,
        };
        push_packet(self.cid, rx, &self.rxq, &self.queue, &self.mem);
    }

    fn push_reset(&self) {
        debug!(
            "push_reset: id: {}, peer_port: {}, local_port: {}",
            self.id, self.peer_port, self.local_port
        );

        let rx = MuxerRx::Reset {
            local_port: self.local_port,
            peer_port: self.peer_port,
        };
        push_packet(self.cid, rx, &self.rxq, &self.queue, &self.mem);
    }

    fn switch_to_connected(&mut self) {
        self.status = ProxyStatus::Connected;
        // Switch to blocking mode for data transfer.
        if let Err(e) = self.sock.set_nonblocking(false) {
            warn!("error switching to blocking: id={}, err={}", self.id, e);
        }
    }

    fn get_addr_len_for_family(&self) -> u32 {
        match self.family {
            ProxyAddressFamily::Inet => std::mem::size_of::<SOCKADDR_IN>() as u32,
            ProxyAddressFamily::Inet6 => std::mem::size_of::<SOCKADDR_IN6>() as u32,
            ProxyAddressFamily::Unix => 0,
        }
    }
}

impl Proxy for TsiStreamProxy {
    fn id(&self) -> u64 {
        self.id
    }

    fn as_pollable_fd(&self) -> PollableFd {
        self.sock.as_pollable_fd()
    }

    fn status(&self) -> ProxyStatus {
        self.status
    }

    fn connect(&mut self, _pkt: &VsockPacket, req: TsiConnectReq) -> ProxyUpdate {
        let mut update = ProxyUpdate::default();

        let (sa_buf, sa_len) = match sockaddr_from_storage(&req.addr) {
            Some(v) => v,
            None => {
                self.push_connect_rsp(-LINUX_EINVAL);
                return update;
            }
        };

        let ret = unsafe {
            connect(
                self.sock.raw(),
                sa_buf.as_ptr() as *const SOCKADDR,
                sa_len,
            )
        };

        let result = if ret == 0 {
            debug!("connect: Connected");
            self.switch_to_connected();
            0
        } else {
            let err = unsafe { WSAGetLastError() };
            if err == WSAEWOULDBLOCK || err == WSAEINPROGRESS {
                debug!("connect: Connecting (WSAEWOULDBLOCK/WSAEINPROGRESS)");
                self.status = ProxyStatus::Connecting;
                0
            } else {
                debug!("TcpProxy: Error connecting: wsa_err={err}");
                -LINUX_ECONNREFUSED
            }
        };

        if self.status == ProxyStatus::Connecting {
            update.polling = Some((
                self.id,
                self.sock.as_pollable_fd(),
                EventSet::OUT | EventSet::EDGE_TRIGGERED,
            ));
        } else {
            if self.status == ProxyStatus::Connected {
                update.polling = Some((self.id, self.sock.as_pollable_fd(), EventSet::IN));
            }
            self.push_connect_rsp(result);
        }

        update
    }

    fn confirm_connect(&mut self, pkt: &VsockPacket) -> Option<ProxyUpdate> {
        debug!(
            "confirm_connect: local_port={} peer_port={}, src_port={}, dst_port={}",
            pkt.dst_port(),
            pkt.src_port(),
            self.local_port,
            self.peer_port,
        );

        self.peer_buf_alloc = pkt.buf_alloc();
        self.peer_fwd_cnt = Wrapping(pkt.fwd_cnt());

        self.local_port = pkt.dst_port();
        self.peer_port = pkt.src_port();

        let rx = MuxerRx::OpResponse {
            local_port: pkt.dst_port(),
            peer_port: pkt.src_port(),
        };
        push_packet(self.cid, rx, &self.rxq, &self.queue, &self.mem);

        Some(ProxyUpdate {
            polling: Some((self.id, self.sock.as_pollable_fd(), EventSet::IN)),
            ..Default::default()
        })
    }

    fn getpeername(&mut self, pkt: &VsockPacket) {
        debug!("getpeername: id={}", self.id);

        let addr_len = self.get_addr_len_for_family();
        let mut sa_buf = vec![0u8; addr_len as usize];
        let mut sa_len = addr_len as i32;

        let ret = unsafe {
            getpeername(
                self.sock.raw(),
                sa_buf.as_mut_ptr() as *mut SOCKADDR,
                &mut sa_len,
            )
        };

        let (result, resp_addr_len, addr) = if ret == 0 {
            let storage = storage_from_sockaddr(&sa_buf, sa_len as usize);
            (0i32, sa_len as u32, storage)
        } else {
            (
                -LINUX_EINVAL,
                0u32,
                SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 0).into(),
            )
        };

        let data = TsiGetnameRsp {
            result,
            addr_len: resp_addr_len,
            addr,
        };

        debug!("getpeername: reply={data:?}");

        let rx = MuxerRx::GetnameResponse {
            local_port: pkt.dst_port(),
            peer_port: pkt.src_port(),
            data,
        };
        push_packet(self.cid, rx, &self.rxq, &self.queue, &self.mem);
    }

    fn sendmsg(&mut self, pkt: &VsockPacket) -> ProxyUpdate {
        debug!("sendmsg");

        let mut update = ProxyUpdate::default();

        let ret = if let Some(buf) = pkt.buf() {
            let sent = unsafe {
                send(
                    self.sock.raw(),
                    buf.as_ptr(),
                    buf.len() as i32,
                    0,
                )
            };
            if sent > 0 {
                if (sent as usize) != buf.len() {
                    error!(
                        "couldn't send everything: buf={}, sent={}",
                        buf.len(),
                        sent
                    );
                }
                self.tx_cnt += Wrapping(sent as u32);
                sent
            } else {
                let err = unsafe { WSAGetLastError() };
                debug!("send error: wsa_err={err}");
                -LINUX_EINVAL
            }
        } else {
            -LINUX_EINVAL
        };

        if ret > 0
            && (self.tx_cnt - self.last_tx_cnt_sent).0 as usize >= (defs::CONN_TX_BUF_SIZE / 2)
        {
            debug!(
                "sending credit update: id={}, tx_cnt={}, last_tx_cnt={}",
                self.id, self.tx_cnt, self.last_tx_cnt_sent
            );
            self.last_tx_cnt_sent = self.tx_cnt;
            let rx = MuxerRx::CreditUpdate {
                local_port: pkt.dst_port(),
                peer_port: pkt.src_port(),
                fwd_cnt: self.tx_cnt.0,
            };
            push_packet(self.cid, rx, &self.rxq, &self.queue, &self.mem);
            update.signal_queue = true;
        }

        debug!("sendmsg ret={ret}");
        update
    }

    fn sendto_addr(&mut self, _req: TsiSendtoAddr) -> ProxyUpdate {
        ProxyUpdate::default()
    }

    fn listen(
        &mut self,
        pkt: &VsockPacket,
        req: TsiListenReq,
        host_port_map: &Option<HashMap<u16, u16>>,
    ) -> ProxyUpdate {
        debug!(
            "listen: id={} addr={}, vm_port={} backlog={}",
            self.id, req.addr, req.vm_port, req.backlog
        );
        let mut update = ProxyUpdate::default();

        let result = self.try_listen(&req, host_port_map);

        let rx = MuxerRx::ListenResponse {
            local_port: pkt.dst_port(),
            peer_port: pkt.src_port(),
            result,
        };
        push_packet(self.cid, rx, &self.rxq, &self.queue, &self.mem);

        if result == 0 {
            self.peer_port = req.vm_port;
            self.status = ProxyStatus::Listening;
            update.polling = Some((self.id, self.sock.as_pollable_fd(), EventSet::IN));
        }

        update
    }

    fn accept(&mut self, req: TsiAcceptReq) -> ProxyUpdate {
        debug!("accept: id={} flags={}", req.peer_port, req.flags);

        let mut update = ProxyUpdate::default();

        if self.pending_accepts > 0 {
            self.pending_accepts -= 1;
            self.push_accept_rsp(0);
            update.signal_queue = true;
        } else if (req.flags & LINUX_O_NONBLOCK) != 0 {
            self.push_accept_rsp(-LINUX_EWOULDBLOCK);
            update.signal_queue = true;
        } else {
            self.status = ProxyStatus::WaitingOnAccept;
        }

        update
    }

    fn update_peer_credit(&mut self, pkt: &VsockPacket) -> ProxyUpdate {
        debug!(
            "update_credit: buf_alloc={} rx_cnt={} fwd_cnt={}",
            pkt.buf_alloc(),
            self.rx_cnt,
            pkt.fwd_cnt()
        );
        self.peer_buf_alloc = pkt.buf_alloc();
        self.peer_fwd_cnt = Wrapping(pkt.fwd_cnt());

        self.status = ProxyStatus::Connected;

        ProxyUpdate {
            polling: Some((self.id, self.sock.as_pollable_fd(), EventSet::IN)),
            ..Default::default()
        }
    }

    fn push_op_request(&self) {
        debug!(
            "push_op_request: id={}, local_port={} peer_port={}",
            self.id, self.local_port, self.peer_port
        );

        let rx = MuxerRx::OpRequest {
            local_port: self.local_port,
            peer_port: self.peer_port,
        };
        push_packet(self.cid, rx, &self.rxq, &self.queue, &self.mem);
    }

    fn process_op_response(&mut self, pkt: &VsockPacket) -> ProxyUpdate {
        debug!(
            "process_op_response: id={} src_port={} dst_port={}",
            self.id,
            pkt.src_port(),
            pkt.dst_port()
        );

        self.peer_buf_alloc = pkt.buf_alloc();
        self.peer_fwd_cnt = Wrapping(pkt.fwd_cnt());

        self.switch_to_connected();

        ProxyUpdate {
            polling: Some((self.id, self.sock.as_pollable_fd(), EventSet::IN)),
            push_accept: Some((self.id, self.parent_id)),
            ..Default::default()
        }
    }

    fn enqueue_accept(&mut self) {
        debug!("enqueue_accept: control_port: {}", self.control_port);

        if self.status == ProxyStatus::WaitingOnAccept {
            self.status = ProxyStatus::Listening;
            self.push_accept_rsp(0);
        } else {
            self.pending_accepts += 1;
        }
    }

    fn push_accept_rsp(&self, result: i32) {
        debug!(
            "push_accept_rsp: control_port: {}, result: {}",
            self.control_port, result
        );

        let rx = MuxerRx::AcceptResponse {
            local_port: 1030,
            peer_port: self.control_port,
            result,
        };
        push_packet(self.cid, rx, &self.rxq, &self.queue, &self.mem);
    }

    fn shutdown(&mut self, pkt: &VsockPacket) {
        let recv_off = pkt.flags() & uapi::VSOCK_FLAGS_SHUTDOWN_RCV != 0;
        let send_off = pkt.flags() & uapi::VSOCK_FLAGS_SHUTDOWN_SEND != 0;

        let how = if recv_off && send_off {
            SD_BOTH
        } else if recv_off {
            SD_RECEIVE
        } else {
            SD_SEND
        };

        let ret = unsafe { shutdown(self.sock.raw(), how) };
        if ret == SOCKET_ERROR {
            let err = unsafe { WSAGetLastError() };
            warn!("error sending shutdown to socket: wsa_err={err}");
        }
    }

    fn release(&mut self) -> ProxyUpdate {
        debug!(
            "release: id={}, tx_cnt={}, last_tx_cnt={}",
            self.id, self.tx_cnt, self.last_tx_cnt_sent
        );
        let remove_proxy = if self.status == ProxyStatus::Listening {
            ProxyRemoval::Immediate
        } else {
            ProxyRemoval::Deferred
        };
        ProxyUpdate {
            remove_proxy,
            ..Default::default()
        }
    }

    fn process_event(&mut self, evset: EventSet) -> ProxyUpdate {
        let mut update = ProxyUpdate::default();

        if evset.contains(EventSet::HANG_UP) {
            debug!("process_event: HANG_UP");
            if self.status == ProxyStatus::Connecting {
                self.push_connect_rsp(-LINUX_ECONNREFUSED);
            } else {
                self.push_reset();
            }

            self.status = ProxyStatus::Closed;
            update.polling = Some((self.id, self.sock.as_pollable_fd(), EventSet::empty()));
            update.signal_queue = true;
            update.remove_proxy = if self.status == ProxyStatus::Listening {
                ProxyRemoval::Immediate
            } else {
                ProxyRemoval::Deferred
            };
            return update;
        }

        if evset.contains(EventSet::IN) {
            debug!("process_event: IN");
            if self.status == ProxyStatus::Connected {
                let (signal_queue, wait_credit) = self.recv_pkt();
                update.signal_queue = signal_queue;

                if wait_credit && self.status != ProxyStatus::WaitingCreditUpdate {
                    self.status = ProxyStatus::WaitingCreditUpdate;
                    let rx = MuxerRx::CreditRequest {
                        local_port: self.local_port,
                        peer_port: self.peer_port,
                        fwd_cnt: self.tx_cnt.0,
                    };
                    update.push_credit_req = Some(rx);
                }

                if self.status == ProxyStatus::Closed {
                    debug!(
                        "process_event: endpoint closed, sending reset: id={}",
                        self.id
                    );
                    self.push_reset();
                    update.signal_queue = true;
                    update.polling =
                        Some((self.id(), self.sock.as_pollable_fd(), EventSet::empty()));
                    return update;
                } else if self.status == ProxyStatus::WaitingCreditUpdate {
                    debug!("process_event: WaitingCreditUpdate");
                    update.polling =
                        Some((self.id(), self.sock.as_pollable_fd(), EventSet::empty()));
                }
            } else if self.status == ProxyStatus::Listening
                || self.status == ProxyStatus::WaitingOnAccept
            {
                let accept_sock = unsafe { accept(self.sock.raw(), std::ptr::null_mut(), std::ptr::null_mut()) };
                if accept_sock != INVALID_SOCKET {
                    let new_fd = ProxyOwnedFd::from_raw_handle(accept_sock as isize);
                    update.new_proxy =
                        Some((self.peer_port, new_fd, self.family, NewProxyType::Tcp));
                } else {
                    let err = unsafe { WSAGetLastError() };
                    warn!(
                        "error accepting connection: id={}, wsa_err={}",
                        self.id, err
                    );
                }
                update.signal_queue = true;
                return update;
            } else {
                debug!("EventSet::IN while not connected: {:?}", self.status);
            }
        }

        if evset.contains(EventSet::OUT) {
            debug!("process_event: OUT");
            if self.status == ProxyStatus::Connecting {
                self.switch_to_connected();
                self.push_connect_rsp(0);
                update.signal_queue = true;
                update.polling =
                    Some((self.id(), self.sock.as_pollable_fd(), EventSet::empty()));
            } else {
                debug!("EventSet::OUT while not connecting");
            }
        }

        update
    }
}
