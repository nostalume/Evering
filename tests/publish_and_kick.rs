//! Publishing and kicking are two promises, and this claim keeps them apart in both directions: a commit
//! that has not become visible may not ring, a batch that already rang may not ring twice, a refusal owes
//! no ring at all, and a contention some policy stopped on is reported rather than swallowed. The other
//! half is geometry: two agents that publish before they read each other stall at the ring limit, and the
//! same window drains as soon as one read is attempted between publishes.
//!
//! `commit_policy` holds the ordering arms and the `Patience` ladder; `ring_limit` holds the two arms
//! whose only difference is whether a receive happens between publishes.

mod support;

#[path = "publish_and_kick/commit_policy.rs"]
mod commit_policy;
#[path = "publish_and_kick/ring_limit.rs"]
mod ring_limit;
