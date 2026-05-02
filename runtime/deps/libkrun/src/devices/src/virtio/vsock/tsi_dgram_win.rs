// Windows implementation of TsiDgramProxy using Winsock2 via windows-sys.
//
// This file provides the same TsiDgramProxy interface as the Unix version
// (tsi_dgram.rs), but uses Winsock2 APIs for socket operations.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::num::Wrapping;
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Networking::WinSock::{
    bind, closesocket, connect, getpeername, ioctlsocket, recv, send, sendto, socket, AF_INET,
    AF_INET6, FIONBIO, INVALID_SOCKET, IPPROTO_UDP, SOCKADDR, SOCKADDR_IN, SOCKET, SOCKET_ERROR,
    SOCK_DGRAM, WSAEWOULDBLOCK, WSAGetLastError,
};

use super::super::Queue as VirtQueue;
use super::defs;
use super::defs::uapi;
use super::muxer::{push_packet, MuxerRx};
use super::muxer_rxq::MuxerRxQ;
use super::packet::{
    SockaddrStorage, TsiAcceptReq, TsiConnectReq, TsiGetnameRsp, TsiListenReq, TsiSendtoAddr,
    VsockPacket,
};
use super::proxy::{
    PollableFd, Proxy, ProxyAddressFamily, ProxyError, ProxyRemoval, ProxyStatus, ProxyUpdate,
    RecvPkt,
};
use utils::epoll::EventSet;

use vm_memory::GuestMemoryMmap;

// Linux errno values for the guest protocol.
const LINUX_EINVAL: i32 = 22;

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

/// Build a SOCKADDR + length from the raw bytes stored in our SockaddrStorage compat type.
fn sockaddr_from_storage(storage: &SockaddrStorage) -> Option<(Vec<u8>, i32)> {
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
    let linux_family = u16::from_le_bytes([buf[0], buf[1]]);
    let win_family: u16 = match linux_family {
        2 => AF_INET as u16,
        10 => AF_INET6 as u16,
        1 => 1,
        other => other,
    };
    buf[0..2].copy_from_slice(&win_family.to_le_bytes());

    Some((buf, len as i32))
}

/// Create a SockaddrStorage response from a Windows SOCKADDR buffer.
fn storage_from_sockaddr(buf: &[u8], len: usize) -> SockaddrStorage {
    let mut out = vec![0u8; len];
    out[..len].copy_from_slice(&buf[..len]);

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

    SockaddrStorage::from_raw_bytes(&out, len as u32).unwrap_or_default()
}

pub struct TsiDgramProxy {
    pub id: u64,
    cid: u64,
    local_port: u32,
    peer_port: u32,
    sock: WinSocket,
    pub status: ProxyStatus,
    sendto_addr: Option<(Vec<u8>, i32)>,
    listening: bool,
    mem: GuestMemoryMmap,
    queue: Arc<Mutex<VirtQueue>>,
    rxq: Arc<Mutex<MuxerRxQ>>,
    rx_cnt: Wrapping<u32>,
    tx_cnt: Wrapping<u32>,
    peer_buf_alloc: u32,
    peer_fwd_cnt: Wrapping<u32>,
}

impl TsiDgramProxy {
    pub fn new(
        id: u64,
        cid: u64,
        family: u16,
        peer_port: u32,
        mem: GuestMemoryMmap,
        queue: Arc<Mutex<VirtQueue>>,
        rxq: Arc<Mutex<MuxerRxQ>>,
    ) -> Result<Self, ProxyError> {
        let af = match family {
            defs::LINUX_AF_INET => AF_INET as i32,
            defs::LINUX_AF_INET6 => AF_INET6 as i32,
            _ => return Err(ProxyError::InvalidFamily),
        };

        let raw_sock = unsafe { socket(af, SOCK_DGRAM as i32, IPPROTO_UDP as i32) };
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

        Ok(TsiDgramProxy {
            id,
            cid,
            local_port: 0,
            peer_port,
            sock,
            status: ProxyStatus::Idle,
            sendto_addr: None,
            listening: false,
            mem,
            queue,
            rxq,
            rx_cnt: Wrapping(0),
            tx_cnt: Wrapping(0),
            peer_buf_alloc: 0,
            peer_fwd_cnt: Wrapping(0),
        })
    }

    fn init_pkt(&self, pkt: &mut VsockPacket) {
        debug!(
            "init_pkt: id={}, src_port={}, dst_port={}",
            self.id, self.local_port, self.peer_port
        );
        pkt.set_op(uapi::VSOCK_OP_RW)
            .set_src_cid(self.cid)
            .set_dst_cid(uapi::VSOCK_HOST_CID)
            .set_dst_port(self.peer_port)
            .set_src_port(0)
            .set_type(uapi::VSOCK_TYPE_DGRAM)
            .set_buf_alloc(defs::CONN_TX_BUF_SIZE as u32)
            .set_fwd_cnt(self.tx_cnt.0);
    }

    fn recv_to_pkt(&self, pkt: &mut VsockPacket) -> RecvPkt {
        if let Some(buf) = pkt.buf_mut() {
            let max_len = buf.len();

            let cnt = unsafe { recv(self.sock.raw(), buf.as_mut_ptr(), max_len as i32, 0) };

            if cnt > 0 {
                debug!("recv cnt={cnt}");
                RecvPkt::Read(cnt as usize)
            } else if cnt == 0 {
                RecvPkt::Close
            } else {
                let err = unsafe { WSAGetLastError() };
                if err == WSAEWOULDBLOCK {
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
        let wait_credit = false;
        let mut queue = self.queue.lock().unwrap();

        while let Some(head) = queue.pop(&self.mem) {
            let len = match VsockPacket::from_rx_virtq_head(&head) {
                Ok(mut pkt) => match self.recv_to_pkt(&mut pkt) {
                    RecvPkt::WaitForCredit => 0,
                    RecvPkt::Read(cnt) => {
                        self.rx_cnt += Wrapping(cnt as u32);
                        self.init_pkt(&mut pkt);
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
                debug!("recv_pkt: pushing packet with {len} bytes");
                if let Err(e) = queue.add_used(&self.mem, head.index, len as u32) {
                    error!("failed to add used elements to the queue: {e:?}");
                }
            }
        }

        debug!("recv_pkt: have_used={have_used}");
        (have_used, wait_credit)
    }
}

impl Proxy for TsiDgramProxy {
    fn id(&self) -> u64 {
        self.id
    }

    fn as_pollable_fd(&self) -> PollableFd {
        self.sock.as_pollable_fd()
    }

    fn status(&self) -> ProxyStatus {
        self.status
    }

    fn connect(&mut self, pkt: &VsockPacket, req: TsiConnectReq) -> ProxyUpdate {
        debug!("connect: addr={}", req.addr);

        let (sa_buf, sa_len) = match sockaddr_from_storage(&req.addr) {
            Some(v) => v,
            None => {
                let rx = MuxerRx::ConnResponse {
                    local_port: pkt.dst_port(),
                    peer_port: pkt.src_port(),
                    result: -LINUX_EINVAL,
                };
                push_packet(self.cid, rx, &self.rxq, &self.queue, &self.mem);
                return ProxyUpdate::default();
            }
        };

        let ret = unsafe {
            connect(
                self.sock.raw(),
                sa_buf.as_ptr() as *const SOCKADDR,
                sa_len,
            )
        };

        let res = if ret == 0 {
            debug!("connect: Connected");
            self.status = ProxyStatus::Connected;
            0
        } else {
            let err = unsafe { WSAGetLastError() };
            debug!("Error connecting: wsa_err={err}");
            -LINUX_EINVAL
        };

        self.peer_buf_alloc = pkt.buf_alloc();
        self.peer_fwd_cnt = Wrapping(pkt.fwd_cnt());

        let rx = MuxerRx::ConnResponse {
            local_port: pkt.dst_port(),
            peer_port: pkt.src_port(),
            result: res,
        };
        push_packet(self.cid, rx, &self.rxq, &self.queue, &self.mem);

        let mut update = ProxyUpdate::default();
        if res == 0 && !self.listening {
            update.polling = Some((self.id, self.sock.as_pollable_fd(), EventSet::IN));
        }
        update
    }

    fn getpeername(&mut self, pkt: &VsockPacket) {
        debug!("process_getpeername");

        let mut sa_buf = vec![0u8; 128];
        let mut sa_len: i32 = 128;

        let ret = unsafe {
            getpeername(
                self.sock.raw(),
                sa_buf.as_mut_ptr() as *mut SOCKADDR,
                &mut sa_len,
            )
        };

        let (result, addr) = if ret == 0 {
            let storage = storage_from_sockaddr(&sa_buf, sa_len as usize);
            (0i32, storage)
        } else {
            let _err = unsafe { WSAGetLastError() };
            (
                -LINUX_EINVAL,
                SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 0).into(),
            )
        };

        let data = TsiGetnameRsp {
            result,
            addr_len: addr.len(),
            addr,
        };

        let rx = MuxerRx::GetnameResponse {
            local_port: pkt.dst_port(),
            peer_port: pkt.src_port(),
            data,
        };
        push_packet(self.cid, rx, &self.rxq, &self.queue, &self.mem);
    }

    fn sendmsg(&mut self, pkt: &VsockPacket) -> ProxyUpdate {
        debug!("sendmsg");

        let ret = if let Some(buf) = pkt.buf() {
            let sent =
                unsafe { send(self.sock.raw(), buf.as_ptr(), buf.len() as i32, 0) };
            if sent > 0 {
                self.tx_cnt += Wrapping(sent as u32);
                sent
            } else {
                -LINUX_EINVAL
            }
        } else {
            -LINUX_EINVAL
        };

        debug!("sendmsg ret={ret}");

        ProxyUpdate::default()
    }

    fn sendto_addr(&mut self, req: TsiSendtoAddr) -> ProxyUpdate {
        debug!("sendto_addr: addr={}", req.addr);

        let mut update = ProxyUpdate::default();

        self.sendto_addr = sockaddr_from_storage(&req.addr);
        if !self.listening {
            // Bind to 0.0.0.0:0 (INADDR_ANY, port 0).
            let sin = SOCKADDR_IN {
                sin_family: AF_INET as u16,
                sin_port: 0,
                sin_addr: windows_sys::Win32::Networking::WinSock::IN_ADDR {
                    S_un: windows_sys::Win32::Networking::WinSock::IN_ADDR_0 { S_addr: 0 },
                },
                sin_zero: [0; 8],
            };
            let ret = unsafe {
                bind(
                    self.sock.raw(),
                    &sin as *const SOCKADDR_IN as *const SOCKADDR,
                    std::mem::size_of::<SOCKADDR_IN>() as i32,
                )
            };
            if ret == 0 {
                self.listening = true;
                update.polling = Some((self.id, self.sock.as_pollable_fd(), EventSet::IN));
            } else {
                let err = unsafe { WSAGetLastError() };
                debug!("couldn't bind socket: wsa_err={err}");
            }
        }

        update
    }

    fn sendto_data(&mut self, pkt: &VsockPacket) {
        debug!("sendto_data");

        self.peer_buf_alloc = pkt.buf_alloc();
        self.peer_fwd_cnt = Wrapping(pkt.fwd_cnt());

        if let Some((ref sa_buf, sa_len)) = self.sendto_addr {
            if let Some(buf) = pkt.buf() {
                let sent = unsafe {
                    sendto(
                        self.sock.raw(),
                        buf.as_ptr(),
                        buf.len() as i32,
                        0,
                        sa_buf.as_ptr() as *const SOCKADDR,
                        sa_len,
                    )
                };
                if sent > 0 {
                    self.tx_cnt += Wrapping(sent as u32);
                } else {
                    let err = unsafe { WSAGetLastError() };
                    debug!("error in sendto: wsa_err={err}");
                }
            } else {
                debug!("sendto_data pkt without buffer");
            }
        } else {
            debug!("sendto_data without sendto_addr");
        }
    }

    fn listen(
        &mut self,
        _pkt: &VsockPacket,
        _req: TsiListenReq,
        _host_port_map: &Option<HashMap<u16, u16>>,
    ) -> ProxyUpdate {
        ProxyUpdate::default()
    }

    fn accept(&mut self, _req: TsiAcceptReq) -> ProxyUpdate {
        ProxyUpdate::default()
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

        ProxyUpdate {
            polling: Some((self.id, self.sock.as_pollable_fd(), EventSet::IN)),
            ..Default::default()
        }
    }

    fn process_op_response(&mut self, _pkt: &VsockPacket) -> ProxyUpdate {
        ProxyUpdate::default()
    }

    fn release(&mut self) -> ProxyUpdate {
        debug!("release");
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
            update.remove_proxy = if self.status == ProxyStatus::Listening {
                ProxyRemoval::Immediate
            } else {
                ProxyRemoval::Deferred
            };
            return update;
        }

        if evset.contains(EventSet::IN) {
            let (signal_queue, wait_credit) = self.recv_pkt();
            update.signal_queue = signal_queue || wait_credit;

            if wait_credit && self.status != ProxyStatus::WaitingCreditUpdate {
                self.status = ProxyStatus::WaitingCreditUpdate;
                let rx = MuxerRx::CreditRequest {
                    local_port: self.local_port,
                    peer_port: self.peer_port,
                    fwd_cnt: self.tx_cnt.0,
                };
                update.push_credit_req = Some(rx);
            }

            if self.status == ProxyStatus::WaitingCreditUpdate {
                debug!("process_event: WaitingCreditUpdate");
                update.polling = Some((self.id(), self.sock.as_pollable_fd(), EventSet::empty()));
            }
        }

        if evset.contains(EventSet::OUT) {
            error!("EventSet::OUT unexpected");
        }

        update
    }
}
