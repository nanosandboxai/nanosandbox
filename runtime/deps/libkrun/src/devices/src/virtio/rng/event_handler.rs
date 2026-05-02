#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use polly::event_manager::{EventManager, Subscriber};
use utils::epoll::{EpollEvent, EventSet};

use super::device::{Rng, REQ_INDEX};
use crate::virtio::device::VirtioDevice;

#[cfg(unix)]
macro_rules! pollable {
    ($efd:expr) => {
        $efd.as_raw_fd()
    };
}

#[cfg(target_os = "windows")]
macro_rules! pollable {
    ($efd:expr) => {
        $efd.as_pollable()
    };
}

impl Rng {
    pub(crate) fn handle_req_event(&mut self, event: &EpollEvent) {
        debug!("rng: request queue event");

        let event_set = event.event_set();
        if event_set != EventSet::IN {
            warn!("rng: request queue unexpected event {event_set:?}");
            return;
        }

        if let Err(e) = self.queue_event(REQ_INDEX).read() {
            error!("Failed to read request queue event: {e:?}");
        } else if self.process_req() {
            self.device_state.signal_used_queue();
        }
    }

    fn handle_activate_event(&self, event_manager: &mut EventManager) {
        debug!("rng: activate event");
        if let Err(e) = self.activate_evt.read() {
            error!("Failed to consume rng activate event: {e:?}");
        }

        // The subscriber must exist as we previously registered activate_evt via
        // `interest_list()`.
        let self_subscriber = event_manager
            .subscriber(pollable!(self.activate_evt))
            .unwrap();

        event_manager
            .register(
                pollable!(self.queue_event(REQ_INDEX)),
                EpollEvent::new(EventSet::IN, pollable!(self.queue_event(REQ_INDEX)) as u64),
                self_subscriber.clone(),
            )
            .unwrap_or_else(|e| {
                error!("Failed to register rng frq with event manager: {e:?}");
            });

        event_manager
            .unregister(pollable!(self.activate_evt))
            .unwrap_or_else(|e| {
                error!("Failed to unregister rng activate evt: {e:?}");
            })
    }
}

impl Subscriber for Rng {
    fn process(&mut self, event: &EpollEvent, event_manager: &mut EventManager) {
        let source = event.fd();
        let req = pollable!(self.queue_event(REQ_INDEX));
        let activate_evt = pollable!(self.activate_evt);

        if self.is_activated() {
            match source {
                _ if source == req => self.handle_req_event(event),
                _ if source == activate_evt => {
                    self.handle_activate_event(event_manager);
                }
                _ => warn!("Unexpected rng event received: {source:?}"),
            }
        } else {
            warn!("rng: The device is not yet activated. Spurious event received: {source:?}");
        }
    }

    fn interest_list(&self) -> Vec<EpollEvent> {
        vec![EpollEvent::new(
            EventSet::IN,
            pollable!(self.activate_evt) as u64,
        )]
    }
}
