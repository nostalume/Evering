//! Two state models that run against nothing but themselves. Neither file imports the crate: what is
//! frozen here is the schedule and the ownership outcome, written before the owner that must match it
//! existed and kept running after, so an implementation may be rewritten underneath them and the law
//! stays checked rather than being re-derived from the code it governs.
//!
//! The row carries no `required-features` for the same reason there is no kernel object to open, no
//! process to spawn and no runtime to drive: a target that asks for a feature it never uses is a target
//! that silently stops running the moment a narrower command forgets to ask.

// Why the `#[path]` attributes are here rather than bare `mod` lines: see the note above `autotests`.
#[path = "protocol_oracles/fail_stop.rs"]
mod fail_stop;
#[path = "protocol_oracles/notification.rs"]
mod notification;
