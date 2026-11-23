//! What a porter may rely on without being asked which kernel object it arrived through: the bytes an
//! identity is made of, the cut of the one word a member's state lives in, the two shapes the type system
//! answers with nothing running, the seam an extent already inside this binary's own image is held to,
//! and — on a Windows host only — the named section that is a session source by itself.
//!
//! The row asks for `map` because the section leg cannot compile without it and the other four ride
//! along. That is the trade made on purpose: a feature written in the manifest is read by the same reader
//! who reads the claim list, while a feature written as `#![cfg(feature = "…")]` at the top of a target
//! hides the whole target from *running* and still reports green. Every command that runs tests in CI
//! asks for `os`, which implies `map`, so nothing that used to execute stops executing there.

// Why the `#[path]` attributes are here rather than bare `mod` lines: see the note above `autotests`.
#[path = "portable_contracts/identity_bytes.rs"]
mod identity_bytes;
#[path = "portable_contracts/member_word.rs"]
mod member_word;
#[path = "portable_contracts/section_source.rs"]
#[cfg(windows)]
mod section_source;
#[path = "portable_contracts/static_source.rs"]
mod static_source;
#[path = "portable_contracts/type_shape.rs"]
mod type_shape;
