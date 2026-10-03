// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::fmt::Formatter;
use std::io;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};

use utils::epoll::{self, Epoll, EpollEvent};

pub type Result<T> = std::result::Result<T, Error>;

/// Platform-specific pollable type.
/// On Unix this is a file descriptor (i32), on Windows it's a HANDLE (isize).
#[cfg(unix)]
pub type Pollable = RawFd;
#[cfg(target_os = "windows")]
pub type Pollable = isize;

/// Errors associated with epoll events handling.
pub enum Error {
    /// Cannot create epoll fd.
    EpollCreate(io::Error),
    /// Polling I/O error.
    Poll(io::Error),
    /// The specified pollable already registered.
    AlreadyExists(Pollable),
    /// The specified pollable is not registered.
    NotFound(Pollable),
}

impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        use self::Error::*;

        match self {
            EpollCreate(err) => write!(f, "Unable to create epoll fd: {err}"),
            Poll(err) => write!(f, "Error during epoll call: {err}"),
            AlreadyExists(pollable) => write!(
                f,
                "A handler for the specified pollable {pollable} already exists."
            ),
            NotFound(pollable) => write!(
                f,
                "A handler for the specified pollable {pollable} was not found."
            ),
        }
    }
}

/// A trait to express the ability to respond to I/O event readiness
/// using callbacks.
pub trait Subscriber {
    /// Callback called when an event is available.
    fn process(&mut self, event: &EpollEvent, event_manager: &mut EventManager);

    /// Returns a list of `EpollEvent` that this subscriber is interested in.
    fn interest_list(&self) -> Vec<EpollEvent>;
}

/// Manages I/O notifications using epoll mechanism (or equivalent on Windows).
pub struct EventManager {
    epoll: Epoll,
    subscribers: HashMap<Pollable, Arc<Mutex<dyn Subscriber>>>,
    ready_events: Vec<EpollEvent>,
}

#[cfg(unix)]
impl AsRawFd for EventManager {
    fn as_raw_fd(&self) -> RawFd {
        self.epoll.as_raw_fd()
    }
}

impl EventManager {
    const EVENT_BUFFER_SIZE: usize = 128;

    /// Create a new EventManager.
    pub fn new() -> Result<EventManager> {
        let epoll_fd = epoll::Epoll::new().map_err(Error::EpollCreate)?;

        Ok(EventManager {
            epoll: epoll_fd,
            subscribers: HashMap::new(),
            ready_events: vec![epoll::EpollEvent::default(); EventManager::EVENT_BUFFER_SIZE],
        })
    }

    /// Returns a clone of the subscriber associated with the `fd`.
    pub fn subscriber(&self, fd: Pollable) -> Result<Arc<Mutex<dyn Subscriber>>> {
        self.subscribers
            .get(&fd)
            .ok_or(Error::NotFound(fd))
            .cloned()
    }

    /// Register a new subscriber. All events that the subscriber is interested are registered.
    pub fn add_subscriber(&mut self, subscriber: Arc<Mutex<dyn Subscriber>>) -> Result<()> {
        let interest_list = subscriber.lock().unwrap().interest_list();

        for event in interest_list {
            self.register(event.fd(), event, subscriber.clone())?
        }

        Ok(())
    }

    /// Register a new `pollable` file descriptor with the corresponding `epoll_event`
    /// for `subscriber`.
    pub fn register(
        &mut self,
        pollable: Pollable,
        epoll_event: EpollEvent,
        subscriber: Arc<Mutex<dyn Subscriber>>,
    ) -> Result<()> {
        if self.subscribers.contains_key(&pollable) {
            return Err(Error::AlreadyExists(pollable));
        };

        self.epoll
            .ctl(epoll::ControlOperation::Add, pollable, &epoll_event)
            .map_err(Error::Poll)?;

        self.subscribers.insert(pollable, subscriber);
        Ok(())
    }

    /// Unregister the `pollable` file descriptor.
    pub fn unregister(&mut self, pollable: Pollable) -> Result<()> {
        match self.subscribers.remove(&pollable) {
            Some(_) => {
                self.epoll
                    .ctl(
                        epoll::ControlOperation::Delete,
                        pollable,
                        &epoll::EpollEvent::default(),
                    )
                    .map_err(Error::Poll)?;
            }
            None => {
                return Err(Error::NotFound(pollable));
            }
        }
        Ok(())
    }

    /// Update the events monitored by `pollable`.
    pub fn modify(&mut self, pollable: Pollable, epoll_event: EpollEvent) -> Result<()> {
        if self.subscribers.contains_key(&pollable) {
            self.epoll
                .ctl(epoll::ControlOperation::Modify, pollable, &epoll_event)
                .map_err(Error::Poll)?;
        } else {
            return Err(Error::NotFound(pollable));
        }

        Ok(())
    }

    /// Check if a file descriptor is pollable
    pub fn is_pollable(&mut self, pollable: Pollable) -> bool {
        self.epoll
            .ctl(
                epoll::ControlOperation::Add,
                pollable,
                &epoll::EpollEvent::default(),
            )
            .is_ok_and(|_| {
                self.epoll
                    .ctl(
                        epoll::ControlOperation::Delete,
                        pollable,
                        &epoll::EpollEvent::default(),
                    )
                    .unwrap();
                true
            })
    }

    /// Wait for events, then dispatch to the registered event handlers.
    pub fn run(&mut self) -> Result<usize> {
        self.run_with_timeout(-1)
    }

    /// Wait for events for a maximum timeout of `miliseconds`. Dispatch the events to the
    /// registered signal handlers.
    pub fn run_with_timeout(&mut self, milliseconds: i32) -> Result<usize> {
        let event_count = match self.epoll.wait(
            EventManager::EVENT_BUFFER_SIZE,
            milliseconds,
            &mut self.ready_events[..],
        ) {
            Ok(event_count) => event_count,
            #[cfg(unix)]
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => 0,
            Err(e) => return Err(Error::Poll(e)),
        };
        self.dispatch_events(event_count);

        Ok(event_count)
    }

    fn dispatch_events(&mut self, event_count: usize) {
        for ev_index in 0..event_count {
            let event = &self.ready_events[ev_index].clone();
            let pollable = event.fd();

            if self.subscribers.contains_key(&pollable) {
                self.subscribers
                    .get_mut(&pollable)
                    .unwrap()
                    .clone()
                    .lock()
                    .unwrap()
                    .process(event, self);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use utils::epoll::EventSet;
    use utils::eventfd::EventFd;

    struct DummySubscriber {
        event_fd_1: EventFd,
        event_fd_2: EventFd,

        processed_ev1_out: bool,
        processed_ev2_out: bool,
        processed_ev1_in: bool,

        register_ev2: bool,
        unregister_ev1: bool,
        modify_ev1: bool,
    }

    impl DummySubscriber {
        fn new() -> Self {
            DummySubscriber {
                event_fd_1: EventFd::new(0).unwrap(),
                event_fd_2: EventFd::new(0).unwrap(),
                processed_ev1_out: false,
                processed_ev2_out: false,
                processed_ev1_in: false,
                register_ev2: false,
                unregister_ev1: false,
                modify_ev1: false,
            }
        }
    }

    impl DummySubscriber {
        fn register_ev2(&mut self) {
            self.register_ev2 = true;
        }

        fn unregister_ev1(&mut self) {
            self.unregister_ev1 = true;
        }

        fn modify_ev1(&mut self) {
            self.modify_ev1 = true;
        }

        fn processed_ev1_out(&self) -> bool {
            self.processed_ev1_out
        }

        fn processed_ev2_out(&self) -> bool {
            self.processed_ev2_out
        }

        fn processed_ev1_in(&self) -> bool {
            self.processed_ev1_in
        }

        fn reset_state(&mut self) {
            self.processed_ev1_out = false;
            self.processed_ev2_out = false;
            self.processed_ev1_in = false;
        }

        #[cfg(unix)]
        fn fd1_pollable(&self) -> Pollable {
            self.event_fd_1.as_raw_fd()
        }
        #[cfg(unix)]
        fn fd2_pollable(&self) -> Pollable {
            self.event_fd_2.as_raw_fd()
        }

        #[cfg(target_os = "windows")]
        fn fd1_pollable(&self) -> Pollable {
            self.event_fd_1.as_pollable()
        }
        #[cfg(target_os = "windows")]
        fn fd2_pollable(&self) -> Pollable {
            self.event_fd_2.as_pollable()
        }

        fn handle_updates(&mut self, event_manager: &mut EventManager) {
            if self.register_ev2 {
                let fd2 = self.fd2_pollable();
                let fd1 = self.fd1_pollable();
                event_manager
                    .register(
                        fd2,
                        EpollEvent::new(EventSet::OUT, fd2 as u64),
                        event_manager.subscriber(fd1).unwrap(),
                    )
                    .unwrap();
                self.register_ev2 = false;
            }

            if self.unregister_ev1 {
                event_manager.unregister(self.fd1_pollable()).unwrap();
                self.unregister_ev1 = false;
            }

            if self.modify_ev1 {
                let fd1 = self.fd1_pollable();
                event_manager
                    .modify(fd1, EpollEvent::new(EventSet::IN, fd1 as u64))
                    .unwrap();
                self.modify_ev1 = false;
            }
        }

        fn handle_in(&mut self, source: Pollable) {
            if self.fd1_pollable() == source {
                self.processed_ev1_in = true;
            }
        }

        fn handle_out(&mut self, source: Pollable) {
            if self.fd1_pollable() == source {
                self.processed_ev1_out = true;
            } else if self.fd2_pollable() == source {
                self.processed_ev2_out = true;
            }
        }
    }

    impl Subscriber for DummySubscriber {
        fn process(&mut self, event: &EpollEvent, event_manager: &mut EventManager) {
            let source = event.fd();
            let event_set = EventSet::from_bits(event.events()).unwrap();

            let all_but_in_out = EventSet::all() - EventSet::OUT - EventSet::IN;
            if event_set.intersects(all_but_in_out) {
                return;
            }

            self.handle_updates(event_manager);

            match event_set {
                EventSet::IN => self.handle_in(source),
                EventSet::OUT => self.handle_out(source),
                _ => {}
            }
        }

        fn interest_list(&self) -> Vec<EpollEvent> {
            vec![EpollEvent::new(
                EventSet::OUT,
                self.fd1_pollable() as u64,
            )]
        }
    }

    #[test]
    fn test_register() {
        let mut event_manager = EventManager::new().unwrap();
        let dummy_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));

        event_manager
            .add_subscriber(dummy_subscriber.clone())
            .unwrap();

        dummy_subscriber.lock().unwrap().register_ev2();

        event_manager.run().unwrap();
        assert!(dummy_subscriber.lock().unwrap().processed_ev1_out());
        assert!(!dummy_subscriber.lock().unwrap().processed_ev2_out());

        dummy_subscriber.lock().unwrap().reset_state();
        event_manager.run().unwrap();
        assert!(dummy_subscriber.lock().unwrap().processed_ev1_out());
        assert!(dummy_subscriber.lock().unwrap().processed_ev2_out());
    }

    #[test]
    fn test_unregister() {
        let mut event_manager = EventManager::new().unwrap();
        let dummy_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));

        event_manager
            .add_subscriber(dummy_subscriber.clone())
            .unwrap();

        dummy_subscriber.lock().unwrap().unregister_ev1();

        event_manager.run().unwrap();
        assert!(dummy_subscriber.lock().unwrap().processed_ev1_out());

        dummy_subscriber.lock().unwrap().reset_state();

        event_manager.run_with_timeout(100).unwrap();
        assert!(!dummy_subscriber.lock().unwrap().processed_ev1_out());
    }

    #[test]
    fn test_modify() {
        let mut event_manager = EventManager::new().unwrap();
        let dummy_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));

        event_manager
            .add_subscriber(dummy_subscriber.clone())
            .unwrap();

        dummy_subscriber.lock().unwrap().modify_ev1();
        event_manager.run().unwrap();
        assert!(dummy_subscriber.lock().unwrap().processed_ev1_out());
        assert!(!dummy_subscriber.lock().unwrap().processed_ev2_out());

        dummy_subscriber.lock().unwrap().reset_state();

        dummy_subscriber
            .lock()
            .unwrap()
            .event_fd_1
            .write(1)
            .unwrap();

        event_manager.run().unwrap();
        assert!(!dummy_subscriber.lock().unwrap().processed_ev1_out());
        assert!(!dummy_subscriber.lock().unwrap().processed_ev2_out());
        assert!(dummy_subscriber.lock().unwrap().processed_ev1_in());

        let event_fd = EventFd::new(0).unwrap();
        #[cfg(unix)]
        let fd = event_fd.as_raw_fd();
        #[cfg(target_os = "windows")]
        let fd = event_fd.as_pollable();
        let event = EpollEvent::new(EventSet::IN, fd as u64);
        let result = event_manager.modify(fd, event);
        match result {
            Err(Error::NotFound(_)) => {}
            _ => panic!("Modifying event did not fail with expected error."),
        };
    }

    #[test]
    fn test_register_errors() {
        let mut event_manager = EventManager::new().unwrap();
        let dummy_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));

        event_manager
            .add_subscriber(dummy_subscriber.clone())
            .unwrap();

        assert!(event_manager.add_subscriber(dummy_subscriber).is_err())
    }

    #[test]
    fn test_unregister_errors() {
        let mut event_manager = EventManager::new().unwrap();
        let dummy_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));

        event_manager
            .add_subscriber(dummy_subscriber.clone())
            .unwrap();

        assert!(event_manager
            .unregister(dummy_subscriber.lock().unwrap().fd2_pollable())
            .is_err());

        assert!(event_manager
            .unregister(dummy_subscriber.lock().unwrap().fd1_pollable())
            .is_ok());
        assert!(event_manager
            .unregister(dummy_subscriber.lock().unwrap().fd1_pollable())
            .is_err());
    }

    #[test]
    fn test_get_handler() {
        let mut event_manager = EventManager::new().unwrap();
        let dummy_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));

        event_manager
            .add_subscriber(dummy_subscriber.clone())
            .unwrap();

        let dummy_fd = dummy_subscriber.lock().unwrap().fd1_pollable();
        assert!(event_manager.subscriber(dummy_fd).is_ok());
        assert!(event_manager.subscriber(-1).is_err());
    }
}
