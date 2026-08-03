use std::{io, os::windows::io::AsHandle, time::Instant};

use crate::{
    os::{
        Event, ReceivedResources, Ring,
        windows::{
            Section,
            process::{Listener, Socket},
        },
    },
    process::{Bootstrap, Supervisor},
};

pub type Shared = Section<std::os::windows::io::OwnedHandle>;

pub fn shared(_name: &str, size: usize, access: crate::mapping::Access) -> io::Result<Shared> {
    Section::anonymous(size, access)
}

pub struct Handoff {
    listener: Listener,
    address: String,
}

impl Handoff {
    pub fn bind() -> io::Result<Self> {
        let listener = Listener::bind()?;
        let address = listener.name().to_owned();
        Ok(Self { listener, address })
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn send<F: AsHandle>(
        self,
        child: &Supervisor,
        deadline: Instant,
        bootstrap: &Bootstrap,
        mapping: &Section<F>,
        event: &Event,
        ring: &Ring,
    ) -> io::Result<()> {
        self.listener.accept(child, deadline)?.send(
            child,
            bootstrap,
            &[mapping.as_handle(), event.as_handle(), ring.as_handle()],
        )
    }

    pub fn receive(address: &str) -> io::Result<ReceivedResources> {
        let (bootstrap, resources) = Socket::connect(address)?.recv(3)?.into_parts();
        let mut resources = resources.into_vec();
        Ok(ReceivedResources {
            bootstrap,
            mapping: Section::from_owned_handle(resources.remove(0)),
            event: Event::from_owned_handle(resources.remove(0)),
            ring: Ring::from_owned_handle(resources.remove(0)),
        })
    }
}
