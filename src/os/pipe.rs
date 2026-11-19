use std::{
    io::{self, ErrorKind, Read, Write},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd},
        unix::net::UnixStream,
    },
};

use crate::notify::Notify;

#[derive(Debug)]
pub struct Ring(UnixStream);

#[derive(Debug)]
pub struct Event(UnixStream);

/// Creates the two local owners of one sticky socket latch.
pub fn event() -> io::Result<(Ring, Event)> {
    let (ring, event) = UnixStream::pair()?;
    ring.set_nonblocking(true)?;
    event.set_nonblocking(true)?;
    Ok((Ring(ring), Event(event)))
}

impl Notify for Ring {
    type Error = io::Error;

    fn notify(&self) -> Result<(), Self::Error> {
        match (&self.0).write(&[1]) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl Ring {
    pub fn from_owned_fd(fd: OwnedFd) -> Self {
        Self(UnixStream::from(fd))
    }
}

impl Event {
    pub fn from_owned_fd(fd: OwnedFd) -> Self {
        Self(UnixStream::from(fd))
    }

    pub(crate) fn clear(&self) -> io::Result<()> {
        let mut bytes = [0; 64];
        loop {
            match (&self.0).read(&mut bytes) {
                Ok(0) => return Err(ErrorKind::BrokenPipe.into()),
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }
}

impl AsFd for Event {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl AsRawFd for Event {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl AsFd for Ring {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, event};
    use crate::notify::Notify;

    use std::io::{ErrorKind, Read};

    /// Reads what the latch holds without the backend's tolerance for an empty one,
    /// which the wake-set clause needs. A pipe shows its state by what it gives back:
    /// bytes mean pending, `WouldBlock` means drained.
    fn pending(event: &Event) -> bool {
        let mut bytes = [0; 64];
        match (&event.0).read(&mut bytes) {
            Ok(_) => true,
            Err(error) if error.kind() == ErrorKind::WouldBlock => false,
            Err(error) => panic!("latch unreadable: {error}"),
        }
    }

    #[test]
    fn notification_is_sticky_and_coalesced() {
        let (ring, event) = event().unwrap();
        ring.notify().unwrap();
        ring.notify().unwrap();
        event.clear().unwrap();
        event.clear().unwrap();
    }

    #[test]
    fn one_clear_consumes_every_pending_kick() {
        let (ring, event) = event().unwrap();
        assert!(!pending(&event), "a fresh latch holds nothing");
        ring.notify().unwrap();
        ring.notify().unwrap();
        ring.notify().unwrap();
        event.clear().unwrap();
        assert!(
            !pending(&event),
            "one clear must drain every kick, not one byte of them"
        );
    }
}
