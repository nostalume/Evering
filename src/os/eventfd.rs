use std::{
    io,
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd},
};

use nix::{
    errno::Errno,
    sys::eventfd::{EfdFlags, EventFd},
};

use crate::notify::Notify;

#[derive(Debug)]
pub struct Ring(EventFd);

#[derive(Debug)]
pub struct Event(EventFd);

/// Creates the two local owners of one sticky eventfd latch.
pub fn event() -> io::Result<(Ring, Event)> {
    let event = EventFd::from_flags(EfdFlags::EFD_CLOEXEC | EfdFlags::EFD_NONBLOCK)
        .map_err(io::Error::from)?;
    let owned: OwnedFd = event.into();
    let ring = owned.as_fd().try_clone_to_owned()?;
    Ok(unsafe {
        (
            Ring(EventFd::from_owned_fd(ring)),
            Event(EventFd::from_owned_fd(owned)),
        )
    })
}

impl Notify for Ring {
    type Error = io::Error;

    fn notify(&self) -> Result<(), Self::Error> {
        match self.0.write(1) {
            Ok(_) | Err(Errno::EAGAIN) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

impl Ring {
    /// Takes ownership of a received notification descriptor.
    pub fn from_owned_fd(fd: OwnedFd) -> Self {
        Self(unsafe { EventFd::from_owned_fd(fd) })
    }
}

impl Event {
    /// Takes ownership of a received wait descriptor.
    pub fn from_owned_fd(fd: OwnedFd) -> Self {
        Self(unsafe { EventFd::from_owned_fd(fd) })
    }

    pub(crate) fn clear(&self) -> Result<(), Errno> {
        match self.0.read() {
            Ok(_) | Err(Errno::EAGAIN) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl AsFd for Event {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl AsFd for Ring {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl AsRawFd for Event {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, event};
    use crate::notify::Notify;

    use nix::errno::Errno;

    /// Reads the latch as the kernel sees it: a counter read returns the pending total
    /// and leaves nothing behind. `Event::clear` cannot report this, because the
    /// wake-set clause needs an already-consumed latch to succeed, so consumption is
    /// probed at the primitive instead of inferred from a park that stayed asleep.
    fn pending(event: &Event) -> u64 {
        match event.0.read() {
            Ok(counter) => counter,
            Err(Errno::EAGAIN) => 0,
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
        assert_eq!(pending(&event), 0, "a fresh latch holds nothing");
        ring.notify().unwrap();
        ring.notify().unwrap();
        ring.notify().unwrap();
        event.clear().unwrap();
        assert_eq!(
            pending(&event),
            0,
            "one clear must take the whole counter, not one kick"
        );
        ring.notify().unwrap();
        // The primitive itself counts, which is what the wait layer collapses: the
        // read that observes the count is also the read that consumes it.
        assert_eq!(pending(&event), 1, "a pending kick is a counter, not a bit");
    }
}
