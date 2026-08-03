use std::{
    io,
    os::fd::AsFd,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use crate::{
    os::{
        Event, ReceivedResources, Ring,
        unix::{UnixFd, process::Socket},
    },
    process::{Bootstrap, Supervisor},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub type Shared = UnixFd<std::os::fd::OwnedFd>;

pub fn shared(name: &str, size: usize, _access: crate::mapping::Access) -> io::Result<Shared> {
    UnixFd::memfd(name, size, false).map_err(io::Error::from)
}

pub struct Handoff {
    address: String,
}

impl Handoff {
    pub fn bind() -> io::Result<Self> {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("evering-{}-{serial}.sock", std::process::id()));
        let address = path
            .into_os_string()
            .into_string()
            .map_err(|_| io::ErrorKind::InvalidData)?;
        let _ = std::fs::remove_file(&address);
        Ok(Self { address })
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn send<F: AsFd>(
        self,
        _child: &Supervisor,
        deadline: Instant,
        bootstrap: &Bootstrap,
        mapping: &UnixFd<F>,
        event: &Event,
        ring: &Ring,
    ) -> io::Result<()> {
        let socket = loop {
            match Socket::connect(&self.address) {
                Ok(socket) => break socket,
                Err(_) if Instant::now() < deadline => std::thread::yield_now(),
                Err(error) => return Err(error),
            }
        };
        socket.send(bootstrap, &[mapping.as_fd(), event.as_fd(), ring.as_fd()])
    }

    pub fn receive(address: &str) -> io::Result<ReceivedResources> {
        let (bootstrap, resources) = Socket::bind(address)?.recv(3)?.into_parts();
        let mut resources = resources.into_vec();
        Ok(ReceivedResources {
            bootstrap,
            mapping: UnixFd::from_fd(resources.remove(0)).map_err(io::Error::from)?,
            event: Event::from_owned_fd(resources.remove(0)),
            ring: Ring::from_owned_fd(resources.remove(0)),
        })
    }
}

impl Drop for Handoff {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.address);
    }
}
