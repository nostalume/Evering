//! The unix-but-not-Linux arm of `crate::os::event`: a `UnixStream` pair standing in for a latch,
//! where `Ring` writes a byte per kick and `Event` reads until the stream reports `WouldBlock`, so one
//! `clear` drains every kick rather than one of them. `event` marks both halves nonblocking;
//! `Ring::notify` treats a full buffer (`WouldBlock`) as a successful notification, and `Event::clear`
//! treats a peer's closed end (`Ok(0)`) as `BrokenPipe` rather than as drained.
//!
//! The closed-end answer exists only on this arm — neither the counter nor the Windows bit can report a
//! departed notifier, so anything the wait layer does with that error is behaviour this host can show
//! and the others cannot. And `from_owned_fd` adopts a stream without setting it nonblocking, unlike
//! `event`: a received blocking latch turns `clear`'s drain loop into a park, which no test here can
//! reach because every latch in this file was made by `event`.

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
