use std::collections::HashMap;
use std::fmt;
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};

use super::muxer::MuxerRx;
use super::packet::{TsiAcceptReq, TsiConnectReq, TsiListenReq, TsiSendtoAddr, VsockPacket};
#[cfg(unix)]
use nix::sys::socket::AddressFamily;
use utils::epoll::EventSet;

/// Cross-platform file descriptor / handle type used for polling.
#[cfg(unix)]
pub type PollableFd = RawFd;
#[cfg(target_os = "windows")]
pub type PollableFd = isize;

/// Cross-platform address family representation.
/// On Unix this wraps nix::sys::socket::AddressFamily.
/// On Windows it uses a simple enum with the same variants.
#[cfg(unix)]
pub type ProxyAddressFamily = AddressFamily;

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyAddressFamily {
    Inet,
    Inet6,
    Unix,
}

/// Cross-platform owned file descriptor / handle type.
#[cfg(unix)]
pub type ProxyOwnedFd = OwnedFd;

#[cfg(target_os = "windows")]
#[derive(Debug)]
pub struct ProxyOwnedFd {
    _handle: isize,
}

#[cfg(target_os = "windows")]
impl ProxyOwnedFd {
    pub fn invalid() -> Self {
        ProxyOwnedFd { _handle: -1 }
    }

    /// Create from a raw Winsock SOCKET handle.
    pub fn from_raw_handle(handle: isize) -> Self {
        ProxyOwnedFd { _handle: handle }
    }

    /// Consume the wrapper and return the raw handle without closing it.
    pub fn into_raw_handle(self) -> isize {
        let h = self._handle;
        std::mem::forget(self);
        h
    }
}

#[derive(Debug)]
pub enum RecvPkt {
    Close,
    Error,
    Read(usize),
    WaitForCredit,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum ProxyError {
    #[cfg(unix)]
    CreatingSocket(nix::errno::Errno),
    #[cfg(target_os = "windows")]
    CreatingSocket(std::io::Error),
    InvalidFamily,
    #[cfg(unix)]
    SettingReuseAddr(nix::errno::Errno),
    #[cfg(target_os = "windows")]
    SettingReuseAddr(std::io::Error),
    #[cfg(unix)]
    SettingReusePort(nix::errno::Errno),
    #[cfg(target_os = "windows")]
    SettingReusePort(std::io::Error),
}

#[derive(Eq, PartialEq, Clone, Copy, Debug)]
pub enum ProxyStatus {
    Idle,
    Connecting,
    Connected,
    Listening,
    Closed,
    WaitingCreditUpdate,
    ReverseInit,
    WaitingOnAccept,
}

#[derive(Default)]
pub enum ProxyRemoval {
    #[default]
    Keep,
    Immediate,
    Deferred,
}

#[derive(Default)]
pub enum NewProxyType {
    #[default]
    Tcp,
    Unix,
}

#[derive(Default)]
pub struct ProxyUpdate {
    pub signal_queue: bool,
    pub remove_proxy: ProxyRemoval,
    pub polling: Option<(u64, PollableFd, EventSet)>,
    pub new_proxy: Option<(u32, ProxyOwnedFd, ProxyAddressFamily, NewProxyType)>,
    pub push_accept: Option<(u64, u64)>,
    pub push_credit_req: Option<MuxerRx>,
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(unix)]
pub trait Proxy: Send + AsRawFd {
    fn id(&self) -> u64;
    #[allow(dead_code)]
    fn status(&self) -> ProxyStatus;
    fn connect(&mut self, pkt: &VsockPacket, req: TsiConnectReq) -> ProxyUpdate;
    fn confirm_connect(&mut self, _pkt: &VsockPacket) -> Option<ProxyUpdate> {
        None
    }
    fn getpeername(&mut self, pkt: &VsockPacket);
    fn sendmsg(&mut self, pkt: &VsockPacket) -> ProxyUpdate;
    fn sendto_addr(&mut self, req: TsiSendtoAddr) -> ProxyUpdate;
    fn sendto_data(&mut self, _pkt: &VsockPacket) {}
    fn listen(
        &mut self,
        pkt: &VsockPacket,
        req: TsiListenReq,
        host_port_map: &Option<HashMap<u16, u16>>,
    ) -> ProxyUpdate;
    fn accept(&mut self, req: TsiAcceptReq) -> ProxyUpdate;
    fn update_peer_credit(&mut self, pkt: &VsockPacket) -> ProxyUpdate;
    fn push_op_request(&self) {}
    fn process_op_response(&mut self, pkt: &VsockPacket) -> ProxyUpdate;
    fn enqueue_accept(&mut self) {}
    fn push_accept_rsp(&self, _result: i32) {}
    fn shutdown(&mut self, _pkt: &VsockPacket) {}
    fn release(&mut self) -> ProxyUpdate;
    fn process_event(&mut self, evset: EventSet) -> ProxyUpdate;
}

#[cfg(target_os = "windows")]
pub trait Proxy: Send {
    fn id(&self) -> u64;
    fn as_pollable_fd(&self) -> PollableFd;
    #[allow(dead_code)]
    fn status(&self) -> ProxyStatus;
    fn connect(&mut self, pkt: &VsockPacket, req: TsiConnectReq) -> ProxyUpdate;
    fn confirm_connect(&mut self, _pkt: &VsockPacket) -> Option<ProxyUpdate> {
        None
    }
    fn getpeername(&mut self, pkt: &VsockPacket);
    fn sendmsg(&mut self, pkt: &VsockPacket) -> ProxyUpdate;
    fn sendto_addr(&mut self, req: TsiSendtoAddr) -> ProxyUpdate;
    fn sendto_data(&mut self, _pkt: &VsockPacket) {}
    fn listen(
        &mut self,
        pkt: &VsockPacket,
        req: TsiListenReq,
        host_port_map: &Option<HashMap<u16, u16>>,
    ) -> ProxyUpdate;
    fn accept(&mut self, req: TsiAcceptReq) -> ProxyUpdate;
    fn update_peer_credit(&mut self, pkt: &VsockPacket) -> ProxyUpdate;
    fn push_op_request(&self) {}
    fn process_op_response(&mut self, pkt: &VsockPacket) -> ProxyUpdate;
    fn enqueue_accept(&mut self) {}
    fn push_accept_rsp(&self, _result: i32) {}
    fn shutdown(&mut self, _pkt: &VsockPacket) {}
    fn release(&mut self) -> ProxyUpdate;
    fn process_event(&mut self, evset: EventSet) -> ProxyUpdate;
}
