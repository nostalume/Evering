//! What crosses a process boundary together, and what a refusal owes back: an offer's opaque bytes and its
//! descriptors arrive as one thing or not at all, a receive that counted wrong closes every descriptor it
//! took in, and a local bound rejects without ever reaching the transport. Nothing here asks what the region
//! on the other end of that pipe *means* — only that what is handed over arrives whole, and that a refusal
//! leaves no handle behind.
//!
//! The two legs are the same claim over two different pipes, and they are separate files rather than one
//! file with two arms because the mechanism differs below the assertion: the unix leg binds its rendezvous
//! in the child and connects back from the parent through a filesystem name, while the windows leg owns the
//! pipe instance in the parent and can therefore report *which* client dialed it — the face that lets an
//! `accept` refuse a child that is not the one it was told about, and which no unix pair in this claim can
//! imitate. Each leg says what it alone shows.
//!
//! The row asks for `process`, measured against both legs' code rather than inherited from their old gates:
//! the `notify` and `map` terms the two files carried were vacuous, since `process` already pulls in the
//! mapping and notification layers. The platform term stays in the manifest's place, said once at the root,
//! which is what keeps a leg from looking like it compiled.
//!
//! The root also holds the claim's only `role_entry`: the harness re-enters a child by exact item name, so a
//! target can answer to one, and it dispatches over whichever platform leg was compiled.

#![cfg(any(unix, windows))]

mod support;

#[cfg(unix)]
#[path = "resource_transfer/unix.rs"]
mod unix;
#[cfg(windows)]
#[path = "resource_transfer/windows.rs"]
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
