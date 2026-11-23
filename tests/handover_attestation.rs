//! Which member a process stood in, as the host says it rather than as the process says it: a grant admits a
//! child into a rendezvous, the child that arrived writes down on that rendezvous which member it became, and
//! the parent that promised it a slot reads the answer back with the writer's identity attached by the kernel
//! rather than carried in the text. `announcement` stops one step earlier, at the promise going *out* — a slot
//! the parent named and no record of who filled it — and the two things are worth separating because only the
//! second can authorize a reap.
//!
//! Which platform leg runs is the host's choice rather than a case's: `unix` reads the receipt off a field
//! beside the datagram, `windows` off the pipe instance the child connected to, and each keeps its own account
//! of what that difference buys — a departed child is something a pipe can report and a datagram pair cannot.
//! `announcement` runs on both, since the same grant carries the promise either way. `labels` runs on neither
//! in particular: it asks only that the two tags a frame begins with are published from outside this crate and
//! differ from each other, which is the one wire fact a porter used to have to copy out of `src`.
//!
//! The row asks for `os`. The platform term stays in the manifest's place because Cargo has no key for it, and
//! saying it once at the root is what keeps a leg from looking like it compiled. The root also holds the
//! claim's only `role_entry`: the harness re-enters a child by exact item name, so a target can answer to one,
//! and it dispatches over whichever platform leg was compiled plus the announcement leg's roles.

#![cfg(any(unix, windows))]

mod support;

#[path = "handover_attestation/announcement.rs"]
mod announcement;
#[path = "handover_attestation/labels.rs"]
mod labels;
#[cfg(unix)]
#[path = "handover_attestation/unix.rs"]
mod unix;
#[cfg(windows)]
#[path = "handover_attestation/windows.rs"]
mod windows;

use crate::support::role;

/// The child's whole program: the roles this claim defines, and nothing else.
#[test]
fn role_entry() {
    #[cfg(unix)]
    let platform = unix::ROLES;
    #[cfg(windows)]
    let platform = windows::ROLES;
    role::dispatch_all(&[platform, announcement::ROLES]);
}
