//! What a region owes after one of its members dies, asked three ways. `at_every_cut` asks for the number:
//! a peer carries one record as far as one of the four cuts a transfer can stop at and leaves without
//! unwinding, and the survivor's ledger has to balance — accepted equals validated plus recovered loss, with
//! no duplicate and nothing fabricated — before its slot is reusable again. The `unix` and `windows` legs ask
//! what only a second process can put in front of the layout and recovery layers: a mapping joined at another
//! base, a root naming a different region, a read-only attachment, geometry or schema from a different build,
//! and the refusals of a reap short of the exact handler, up to the pool blocks a doomed member died holding.
//!
//! Which leg runs is the host's choice rather than a case's, and the two differ in transport rather than in
//! what a recovery table owes: the unix leg hands the region over as a POSIX shared-memory name plus a base
//! read out of the environment, the windows leg as a duplicated section handle over a control pipe that only
//! the child it was opened for can use. Several of the unix leg's cases assert where a mapping landed and so
//! have no Windows face at all; the legs say so themselves rather than leaving the gap to be inferred from an
//! absent test.
//!
//! The row asks for `process` and `tokio` — a spawned peer for every leg and a runtime doorbell for the
//! arithmetic one — measured rather than inherited: the legs' own `os` and `map` terms were each wider than
//! the code under them needs, since `process` already pulls in the mapping and notification layers. The
//! platform term stays in the file because Cargo has no key for it, and saying it once at the root is what
//! keeps a leg from looking like it compiled.
//!
//! The root also holds the claim's only `role_entry`: the harness re-enters a child by exact item name, so a
//! target can answer to one, and it dispatches over the four cut roles plus whichever platform leg was
//! compiled.

#![cfg(any(unix, windows))]

mod support;

#[path = "recovery_conserves/at_every_cut.rs"]
mod at_every_cut;
#[cfg(unix)]
#[path = "recovery_conserves/unix.rs"]
mod unix;
#[cfg(windows)]
#[path = "recovery_conserves/windows.rs"]
mod windows;

use crate::support::role;

/// The child's whole program: the roles this claim defines, and nothing else.
#[test]
fn role_entry() {
    #[cfg(unix)]
    role::dispatch_all(&[at_every_cut::ROLES, unix::ROLES]);
    #[cfg(windows)]
    role::dispatch_all(&[at_every_cut::ROLES, windows::ROLES]);
}
