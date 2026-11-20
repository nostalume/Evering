//! The platform's object names and one attested identity: which primitive backs `Event`, `Ring` and
//! `Shared` on each target, and the `Peer` a completed handover reports. `Peer` exists only through
//! `introduced`, which `Handoff::receive` reaches once the kernel's answer and the introduction agree
//! — an impostor is an `Err`, never a value of the type.
//!
//! Resources travel positionally: the order a sender puts the mapping, the event and the ring into a
//! handover is matched by index on the receiving side, and `ReceivedResources` carries no tag saying
//! which is which, so a sender and a reader disagreeing about that order receive each other's objects
//! with nothing to complain. The whole map is also gated on `std`, so it simply is not there under
//! `--no-default-features`. Each backend keeps its own obligations — the unix leg reserves a window
//! before placing the file, the Windows leg goes through a section and a view — and only the paths a
//! suite actually reaches on a given host have been executed.
#![cfg(feature = "std")]

#[cfg(all(feature = "map", unix))]
pub mod unix;
#[cfg(all(feature = "map", windows))]
pub mod windows;

#[cfg(all(feature = "notify", target_os = "linux"))]
#[path = "os/eventfd.rs"]
mod event;
#[cfg(all(feature = "notify", unix, not(target_os = "linux")))]
#[path = "os/pipe.rs"]
mod event;
#[cfg(all(feature = "notify", windows))]
#[path = "os/event.rs"]
mod event;

#[cfg(any(all(feature = "notify", unix), all(feature = "notify", windows)))]
pub use event::{Event, Ring, event};

#[cfg(all(feature = "process", unix))]
#[path = "os/unix/handoff.rs"]
mod handoff;
#[cfg(all(feature = "process", windows))]
#[path = "os/windows/handoff.rs"]
mod handoff;
#[cfg(all(feature = "process", any(unix, windows)))]
pub use handoff::{Grant, Handoff, Shared, shared};

/// The process at the other end of a completed handover, as the host kernel reported it and as
/// checked against the process that admitted the grant.
///
/// This is a *process* identity, and is not the `Peer` of [`crate::mapping`], which names a
/// participant of a shared region by slot and generation. A handover produces both: this one says
/// who the bytes arrived from, that one says which directory member may claim them.
///
/// `Handoff::receive` returns one only after the kernel's answer and the introduction agree, so an
/// impostor is an `Err` rather than a variant of this type: there is no unattested value to
/// overlook.
#[cfg(all(feature = "process", any(unix, windows)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    pid: u32,
}

#[cfg(all(feature = "process", any(unix, windows)))]
impl Peer {
    pub(crate) const fn introduced(pid: u32) -> Self {
        Self { pid }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

#[cfg(all(feature = "process", any(unix, windows)))]
pub struct ReceivedResources {
    pub bootstrap: crate::process::Bootstrap,
    pub mapping: Shared,
    pub event: Event,
    pub ring: Ring,
}

#[cfg(all(test, feature = "notify"))]
mod tests {
    use super::*;
    use crate::{mem::Source, notify::Notify};
    use std::io;

    /// A doorbell failure is stored in `Committed<T, N::Error>`, and a mapping
    /// failure in `SessionError<S::Error>`, so both types reach a caller's own
    /// error handling. Each native backend is required to report them as
    /// `io::Error`, which is the one carrier available on every platform.
    fn portable_doorbell<N: Notify<Error = io::Error>>() {}

    fn portable_source<S: Source<Error = io::Error>>() {}

    #[test]
    fn a_native_failure_reaches_the_caller_as_one_error_type() {
        portable_doorbell::<Ring>();
        #[cfg(all(unix, feature = "map"))]
        portable_source::<unix::UnixFd<std::fs::File>>();
        #[cfg(all(windows, feature = "map"))]
        portable_source::<windows::Section<std::fs::File>>();
    }
}
