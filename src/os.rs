//! The platform's object names and one attested identity: which primitive backs `Event`, `Ring` and
//! `Shared` on each target, and the `Peer` a completed handover reports. `Peer` exists only through
//! `introduced`, which `Handoff::receive` reaches once the kernel's answer and the introduction agree
//! — an impostor is an `Err`, never a value of the type.
//!
//! Resources travel positionally: the order a sender puts the mapping, the event and the ring into a
//! handover is matched by index on the receiving side, and `ReceivedResources` carries no tag saying
//! which is which, so a sender and a reader disagreeing about that order receive each other's objects
//! with nothing to complain. The promised member slot beside them is the one thing the map carries
//! that is not positional — it is announced through the environment, and only the roster decides
//! whether it was ever promised. The one thing that goes back the other way is a receipt, and it is
//! named by two types and no resource: `Receipt` is the admitted process's means of reporting the
//! member it became, `Attendance` is the parent's bounded read of that report, and what makes a report
//! evidence is that the host names its writer — a claim the writer cannot choose, and the reason no
//! object travels with it. The whole map is also gated on `std`, so it simply is not there under
//! `--no-default-features`. Each backend keeps its own obligations — the unix leg reserves a window
//! before placing the file, the Windows leg goes through a section and a view — and only the paths a
//! suite actually reaches on a given host have been executed.
//!
//! A target that is neither of those two hosts replaces this map rather than a piece of it: bytes that
//! live in the program's own image answer for themselves through [`crate::mapping::Source`], whose
//! safety text names [`crate::mapping::REGION_ALIGN`] as the residue to arrange, and a doorbell that
//! arrives by an interrupt or a scheduled callback implements [`crate::notify::Notify`] and
//! [`crate::notify::Wait`] on its own type. Nothing in `session`, `pool` or `channel` names anything in
//! this module, so a target with no descriptor, no handle and no process to hand one to is not missing
//! a port here — it has no reason to compile one.
//!
//! The one operation both unix arms share is the park over a set of latches (`mux`), because a latch is a
//! readable descriptor on either: `Event::wait_any` is the arm's own method over that leg, which is why
//! the ceiling a Windows wait array imposes — 64 objects, and a refusal above it — is written on that arm
//! alone rather than in the seam. Editing the shared leg therefore changes two hosts at once, and no run
//! on the third can show it.
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

#[cfg(all(unix, feature = "notify"))]
mod mux;

#[cfg(all(feature = "process", unix))]
#[path = "os/unix/handoff.rs"]
mod handoff;
#[cfg(all(feature = "process", windows))]
#[path = "os/windows/handoff.rs"]
mod handoff;
#[cfg(all(feature = "process", any(unix, windows)))]
pub use handoff::{Attendance, Grant, Handoff, Receipt, Shared, shared};

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
    /// The member slot the admitted process was told it holds, absent when nothing was promised.
    ///
    /// It comes through the environment beside the rendezvous rather than among the four fields
    /// above, so it is the one part of a handover that is announced instead of delivered: the objects
    /// travel positionally and are the sender's to hand over, while this only says which member word
    /// the sender left open. The roster is what answers for it, and it answers by refusing an arrival
    /// nobody promised — which is why an attested handover and an unmade-up number are not the same
    /// evidence, and why this is a claim a caller hands to [`crate::Session::open_as`] rather than a
    /// fact the handover vouches for.
    pub seat: Option<crate::process::Introduction>,
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
