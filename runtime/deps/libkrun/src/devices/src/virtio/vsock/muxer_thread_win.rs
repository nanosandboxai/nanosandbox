// Windows implementation of MuxerThread.
//
// On Windows the Epoll shim uses WaitForMultipleObjects and requires &mut self
// for ctl() operations. This version wraps the Epoll in a Mutex to allow
// mutable access from the thread, while the VsockMuxer only holds the Arc for
// registration (update_polling is a no-op on VsockMuxer on Windows since the
// real polling happens here in the thread).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use super::super::Queue as VirtQueue;
use super::muxer::{push_packet, MuxerRx, ProxyMap};
use super::muxer_rxq::MuxerRxQ;
use super::proxy::{NewProxyType, PollableFd, Proxy, ProxyRemoval, ProxyUpdate};
use super::tsi_stream::TsiStreamProxy;

use crate::virtio::InterruptTransport;
use crossbeam_channel::Sender;
use rand::{rng, rngs::ThreadRng, Rng};
use utils::epoll::{ControlOperation, Epoll, EpollEvent, EventSet};
use vm_memory::GuestMemoryMmap;

pub struct MuxerThread {
    cid: u64,
    pub epoll: Arc<Mutex<Epoll>>,
    rxq: Arc<Mutex<MuxerRxQ>>,
    proxy_map: ProxyMap,
    mem: GuestMemoryMmap,
    queue: Arc<Mutex<VirtQueue>>,
    interrupt: InterruptTransport,
    reaper_sender: Sender<u64>,
    _unix_ipc_port_map: HashMap<u32, (PathBuf, bool)>,
}

impl MuxerThread {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cid: u64,
        epoll: Epoll,
        rxq: Arc<Mutex<MuxerRxQ>>,
        proxy_map: ProxyMap,
        mem: GuestMemoryMmap,
        queue: Arc<Mutex<VirtQueue>>,
        interrupt: InterruptTransport,
        reaper_sender: Sender<u64>,
        unix_ipc_port_map: HashMap<u32, (PathBuf, bool)>,
    ) -> Self {
        MuxerThread {
            cid,
            epoll: Arc::new(Mutex::new(epoll)),
            rxq,
            proxy_map,
            mem,
            queue,
            interrupt,
            reaper_sender,
            _unix_ipc_port_map: unix_ipc_port_map,
        }
    }

    pub fn run(self) {
        thread::Builder::new()
            .name("vsock muxer".into())
            .spawn(|| self.work())
            .unwrap();
    }

    fn send_credit_request(&self, credit_rx: MuxerRx) {
        debug!("send_credit_request");
        push_packet(self.cid, credit_rx, &self.rxq, &self.queue, &self.mem);
    }

    pub fn update_polling(&self, id: u64, fd: PollableFd, evset: EventSet) {
        debug!("update_polling id={id} fd={fd:?} evset={evset:?}");
        let mut epoll = self.epoll.lock().unwrap();
        let _ = epoll.ctl(ControlOperation::Delete, fd, &EpollEvent::default());
        if !evset.is_empty() {
            let _ = epoll.ctl(ControlOperation::Add, fd, &EpollEvent::new(evset, id));
        }
    }

    fn process_proxy_update(&self, id: u64, update: ProxyUpdate, thread_rng: &mut ThreadRng) {
        if let Some(polling) = update.polling {
            self.update_polling(polling.0, polling.1, polling.2);
        }

        if let Some(credit_rx) = update.push_credit_req {
            debug!("send_credit_request");
            self.send_credit_request(credit_rx);
        }

        match update.remove_proxy {
            ProxyRemoval::Keep => {}
            ProxyRemoval::Immediate => {
                warn!("immediately removing proxy: {id}");
                self.proxy_map.write().unwrap().remove(&id);
            }
            ProxyRemoval::Deferred => {
                warn!("deferring proxy removal: {id}");
                if self.reaper_sender.send(id).is_err() {
                    self.proxy_map.write().unwrap().remove(&id);
                }
            }
        }

        let mut should_signal = update.signal_queue;

        if let Some((peer_port, accept_fd, family, proxy_type)) = update.new_proxy {
            let local_port: u32 = thread_rng.random_range(1024..u32::MAX);
            let new_id: u64 = ((peer_port as u64) << 32) | (local_port as u64);
            let new_proxy: Box<dyn Proxy> = match proxy_type {
                NewProxyType::Tcp => Box::new(TsiStreamProxy::new_reverse(
                    new_id,
                    self.cid,
                    id,
                    family,
                    local_port,
                    peer_port,
                    accept_fd,
                    self.mem.clone(),
                    self.queue.clone(),
                    self.rxq.clone(),
                )),
                NewProxyType::Unix => {
                    // Unix sockets are not supported on Windows.
                    warn!("Unix proxy not supported on Windows");
                    return;
                }
            };
            self.proxy_map
                .write()
                .unwrap()
                .insert(new_id, Mutex::new(new_proxy));
            if let Some(proxy) = self.proxy_map.read().unwrap().get(&new_id) {
                proxy.lock().unwrap().push_op_request();
            };
            should_signal = true;
        }

        if should_signal {
            debug!("signal IRQ");
            self.interrupt.signal_used_queue();
        }
    }

    fn work(self) {
        let mut thread_rng = rng();
        // Note: Unix IPC listening sockets are not supported on Windows,
        // so we skip create_listening_ipc_sockets().
        loop {
            let epoll_events;
            let ev_cnt;

            // Scope the epoll lock to just the wait call.
            {
                let epoll = self.epoll.lock().unwrap();
                let mut events = vec![EpollEvent::new(EventSet::empty(), 0); 32];
                match epoll.wait(events.len(), -1, events.as_mut_slice()) {
                    Ok(cnt) => {
                        epoll_events = events;
                        ev_cnt = cnt;
                    }
                    Err(e) => {
                        debug!("failed to consume muxer epoll event: {e}");
                        continue;
                    }
                }
            }

            for ev in &epoll_events[0..ev_cnt] {
                debug!("Event: ev.data={} ev.fd={}", ev.data(), ev.fd());
                let evset = EventSet::from_bits(ev.events).unwrap();
                let id = ev.data();

                let update = self.proxy_map.read().unwrap().get(&id).map(|proxy_lock| {
                    let mut proxy = proxy_lock.lock().unwrap();
                    proxy.process_event(evset)
                });

                if let Some(update) = update {
                    self.process_proxy_update(id, update, &mut thread_rng);
                }
            }
        }
    }
}
