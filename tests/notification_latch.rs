//! The latch is the one kernel object the notification protocol trusts to be remembered, and this claim
//! is what that trust buys: a kick that arrives before any waiter exists is not lost, repeated kicks
//! coalesce and one park consumes them all, a park given up on leaves the kick for the next park, and one
//! kick releases every waiter registered before it. The cross-process legs add the half one process
//! cannot show — that a peer's death cancels the wait without fabricating readiness.
//!
//! `latch_clauses` holds the four backend clauses, which is what a new adapter, including a kernel-side
//! provider, is checked against. `cross_process` holds the two legs that hand a doorbell's number to a
//! second process, and the roles those legs run.
//!
//! The row asks for `notify` and `tokio`. The platform term cannot go in the manifest, so it stays here:
//! on a host that is neither Unix nor Windows there is no latch to park on, and saying it once at the root
//! is what keeps a leg from looking like it compiled.

#![cfg(any(unix, windows))]

mod support;

#[path = "notification_latch/cross_process.rs"]
mod cross_process;
#[path = "notification_latch/latch_clauses.rs"]
mod latch_clauses;

use crate::support::role;

/// The child's whole program: the roles this claim defines, and nothing else.
#[test]
fn role_entry() {
    role::dispatch(&[
        ("parks", cross_process::parks),
        ("leaves", cross_process::leaves),
    ]);
}
