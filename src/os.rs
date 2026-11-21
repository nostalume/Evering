//! The platform's object names and one identity a handover can claim: which primitive backs `Event`,
//! `Ring` and `Shared` on each target, and the `Peer` a completed handover reports. `Peer` exists either
//! through `introduced`, which `Handoff::receive` reaches once the kernel's answer and the introduction
//! agree, or through `unattested`, which is the whole say a policy has in this map: it releases a
//! silent rendezvous only when the grant that admitted this process asked for that and the descriptor is
//! already established as one that was handed over. An impostor — a peer that answered and disagreed —
//! is an `Err` under every policy, and so is any silence on a pair whose kernel always answers, which is
//! why no value here can be overlooked into a handover.
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

#[cfg(all(feature = "process", any(unix, windows)))]
use std::io;

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
/// [`Handoff::receive`] reaches one of two ways. It names the peer and the introduction agrees with
/// the name, which is [`Provenance::Attested`] and the only arm that carries a process id. Or the
/// rendezvous answers nothing and the grant that admitted this process said so out loud, which is one
/// of the other two arms and carries no id at all. A peer that answered and disagreed is neither: it
/// is an `Err` under every policy, because that is the case a policy has nothing to add to.
#[cfg(all(feature = "process", any(unix, windows)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    pid: Option<u32>,
    provenance: Provenance,
}

/// Which assertion a completed handover is entitled to make about the peer behind its rendezvous.
///
/// The arms differ in who answered, not in how sure the receiver feels:
///
/// * `Attested` — the kernel named the peer and the name is the one the introduction expected. The
///   only arm with a [`Peer::pid`].
/// * `Delegated` — this target's rendezvous answers nothing about its other end and never will, and the
///   introducer's grant said to complete anyway: every authority for who the peer is came from the step
///   that admitted this process. Card 3.4's no-second-rendezvous-party target is what the arm is for.
/// * `Unavailable` — a peer that could be asked is there and this handover got no answer, and the
///   introducer's policy carried it through regardless.
///
/// `Delegated` and `Unavailable` say which question went unanswered, which is why a caller that has to
/// refuse one can tell the other apart from a check that failed: that one never becomes a value.
#[cfg(all(feature = "process", any(unix, windows)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    Attested,
    Delegated,
    Unavailable,
}

#[cfg(all(feature = "process", any(unix, windows)))]
impl Peer {
    pub(crate) const fn introduced(pid: u32) -> Self {
        Self {
            pid: Some(pid),
            provenance: Provenance::Attested,
        }
    }

    /// Decide what a handover may claim about a peer whose rendezvous answered nothing about it.
    ///
    /// The three inputs each own one part of the answer, which is why the answer is not spelled out at
    /// any one call site. `kind` says which kind of nothing this is: a target whose rendezvous answers
    /// nothing, or a meeting that was not asked. `established` says whether the rendezvous is known to
    /// be one this process was *given*, and that is the only thing a policy can speak about — on a unix
    /// pair the kernel's own answer is what establishes it, so silence there indicts the descriptor
    /// rather than the asking, and no introduction releases it. `trusts` is the introducer's word read
    /// off the announcement its grant wrote: the receiver reads that policy and never owns it, because
    /// a process that sets its own proof burden has set none.
    ///
    /// The two refusals name different missing things, because the two failures ask a caller for
    /// different repairs: one has no policy to read and can be given one, while the other has a policy
    /// that cannot apply here, so no further word from the same introducer changes the answer.
    pub(crate) fn unattested(
        kind: Provenance,
        established: bool,
        trusts: bool,
    ) -> io::Result<Self> {
        if established && trusts {
            return Ok(Self {
                pid: None,
                provenance: kind,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            if established {
                "this rendezvous answers nothing about the peer behind it, and the grant that admitted \
                 this process said nothing about completing without an attestation"
            } else {
                "a rendezvous that was granted always answers who its peer is, so the trusting \
                 introduction this process holds releases nothing: the descriptor behind the \
                 announcement is not one that was handed over"
            },
        ))
    }

    /// The process the kernel named at the other end, when it named one.
    ///
    /// `None` is not a number to fall back on: a peer this handover cannot name is a peer whose id
    /// would be invented, and a relay that keys on an invented one fails silently rather than loudly.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Which of the three assertions this identity rests on.
    pub fn provenance(&self) -> Provenance {
        self.provenance
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

#[cfg(all(test, feature = "process", any(unix, windows)))]
#[test]
fn an_unreleased_no_answer_is_refused_by_the_thing_that_is_missing() {
    for kind in [Provenance::Delegated, Provenance::Unavailable] {
        let error = Peer::unattested(kind, true, false)
            .err()
            .unwrap_or_else(|| panic!("{kind:?} completed a handover nobody released"));
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            error.to_string().contains("said nothing"),
            "the refusal has to name the introducer's silence as what is missing, since no evidence is \
             owed on a target that answers nothing: {error}"
        );
    }
}

#[cfg(all(test, feature = "process", any(unix, windows)))]
#[test]
fn a_policy_releases_no_handover_whose_rendezvous_was_never_established_as_a_granted_one() {
    let error = Peer::unattested(Provenance::Unavailable, false, true).expect_err(
        "an unestablished rendezvous completes nothing, however trusting the introducer",
    );
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(
        error.to_string().contains("releases nothing"),
        "this is the impostor's refusal, and it has to say the descriptor is what is wrong rather than \
         borrow the sentence about an introducer who did speak: {error}"
    );
}

#[cfg(all(test, feature = "process", any(unix, windows)))]
#[test]
fn a_released_no_answer_still_says_which_question_went_unanswered() {
    let delegated = Peer::unattested(Provenance::Delegated, true, true).unwrap();
    let unavailable = Peer::unattested(Provenance::Unavailable, true, true).unwrap();
    assert_eq!(delegated.pid(), None);
    assert_eq!(unavailable.pid(), None);
    assert_ne!(
        delegated, unavailable,
        "one target answers nothing about its peer end and the other answered nothing this time, which \
         are two different things for a caller to have agreed to"
    );
    let met = Peer::introduced(7);
    assert_eq!(met.provenance(), Provenance::Attested);
    assert_eq!(met.pid(), Some(7));
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
