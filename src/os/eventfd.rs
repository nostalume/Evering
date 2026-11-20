//! The Linux arm of `crate::os::event`: one `eventfd` counter held by two local owners, where `Ring`
//! adds and `Event` consumes. `event` makes both ends `EFD_NONBLOCK | EFD_CLOEXEC`, `Ring::notify`
//! writes one and `Event::clear` reads the whole counter, and both count the full and empty answers
//! (`EAGAIN`) as success — a saturated counter is already a pending notification, and a read that found
//! nothing is already clear.
//!
//! Those two tolerances are only non-blocking because of the flags, and `from_owned_fd` adopts a
//! descriptor without looking at them: a latch that arrives from elsewhere blocking turns `clear` into
//! a park instead of a no-op, which no test here can show because `event` always sets them. The same
//! file is also the only arm that *counts* — the pipe arm holds bytes and the Windows arm one bit — so a
//! change that makes `clear` consume one notification instead of the total is correct here and wrong on
//! the other two hosts. `Event::wait_any` inherits that collapse: a park over a set reports which latch
//! answered and never how many kicks it was holding.

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

    /// Parks this thread until one of `events` is ready, consumes that latch's readiness and reports
    /// which index it was.
    ///
    /// One park reports one latch and never a count: this counter coalesces every kick into one
    /// readiness, so an answer that also said how many peers had rung would report something the latch
    /// stopped recording the moment it was rung. The rest of a set keeps its readiness while one latch is
    /// being served, so a caller loops and the next park answers at once until the set is drained. The
    /// slice is borrowed exclusively because readiness taken from a latch a second park was watching is
    /// a wake that went missing, and nothing but the borrow says no second park exists.
    pub fn wait_any(events: &mut [Event]) -> io::Result<usize> {
        crate::os::mux::park(events)
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
