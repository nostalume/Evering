//! What a handoff will answer to. A grant is spent into a command, and everything the introducer announced
//! goes with it: the process that made the grant has no rendezvous of its own to receive at, a process that
//! reaches a rendezvous and names an introducer it was not given is refused before a byte of the offer is
//! read, and only the process the grant was admitted into meets anybody — which is the one answer of the three
//! that says who was on the other end.
//!
//! Both legs measure those three, off what the host will attest: `unix` classifies the descriptor number the
//! grant announced and asks a connected datagram pair who wrote, `windows` asks the pipe instance which client
//! it accepted. Where a kernel cannot answer, the introducer's policy word is what decides, so each leg also
//! shows that word refusing to open a door no grant opened — the half `Provenance` exists to make visible.
//!
//! The refusals of a process that was never admitted came from the files that borrowed a handoff to move a
//! region, which is not where a claim about who may meet belongs. Unix holds two of them, because a granted
//! descriptor the introducer still holds is a thing worth refusing on its own; each leg's header says what its
//! own refusals distinguish.
//!
//! The row asks for `process`. The platform term stays in this file because Cargo has no key for it, and the
//! root holds the claim's only `role_entry`: the harness re-enters a child by exact item name, so a target can
//! answer to one, and it runs whichever platform leg was compiled.

#![cfg(any(unix, windows))]

mod support;

#[cfg(unix)]
#[path = "handover_admission/unix.rs"]
mod unix;
#[cfg(windows)]
#[path = "handover_admission/windows.rs"]
mod windows;

use crate::support::role;

/// The child's whole program: the roles this claim defines, and nothing else.
#[test]
fn role_entry() {
    #[cfg(unix)]
    role::dispatch(unix::ROLES);
    #[cfg(windows)]
    role::dispatch(windows::ROLES);
}
