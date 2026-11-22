//! The harness the multi-process claims share, so a helper a second claim file needs is written once
//! rather than copied: the extent a claim runs on ([`region`]), the waiting around an introduction
//! ([`introduce`]), the entry a re-spawned process runs ([`role`]) and the crash number a parent hands
//! its child ([`cuts`]) are here. What one claim counts alone — the doorbells it rings, the verdict names
//! it asserts — joins when a second claim file starts agreeing by hand.
//! Nothing here is a test: a claim file names the law and calls into this subtree.
//!
//! Two rules own what lives here.
//!
//! - A helper joins once two claim files hold the same body. A helper one claim holds alone stays with that
//!   claim, including when its two platform legs differ: forcing those together is how a harness starts
//!   describing a platform it never ran.
//! - A module opens at the feature its cheapest item needs, and an item that reaches higher says so on
//!   itself. A claim that stops at the queue must not be made to ask for a process it never spawns. Every
//!   build row says `os` or `all-features` today, so a harness reaching past its own items is unnoticed
//!   until a narrower row is added — and narrower rows cannot be added freely while `--features map` alone
//!   fails inside `src/os/unix.rs`, which asks `nix` for a `sysconf` the `map` feature never declares.
//!
//! The manifest side of this layout is `[package] autotests = false` and the `[[test]]` rows: nothing under
//! `tests/` is discovered any more, and `tests/verification_layout.rs` fails the suite when a file and a row
//! stop agreeing. That check exists because a dropped row is not an error but a green run over one claim less.

// An attribute on this module, so a claim file's own dead code is still reported: the harness may hold a
// helper no claim calls yet, and that is not a licence for a claim to hold dead code.
#![allow(dead_code)]

// A crash number travels as text, so this module needs nothing from the crate either.
pub mod cuts;

// The queue-draining pair needs only a channel, so the module opens at `map`; the two items that wait on a
// second process say so on themselves.
#[cfg(feature = "map")]
pub mod introduce;

#[cfg(feature = "map")]
pub mod region;

// The re-entry needs nothing from the crate: a role is a function, a key and an exit code.
pub mod role;
