//! The published vocabulary of one mapping — `Request`, `Access`, `Source`, `Map`, `Peer`,
//! `LayoutField` and `Error` — over the private implementation module `mem`, which is their only
//! address. Nothing in this file computes: a `Map` arrives from a caller's `Source::map` and is
//! consumed once by the composition, which erases the backend it came from. It lives behind a path
//! rather than at the root because these are the words for *how* a region gets mapped, not nouns held
//! across an exchange.
//!
//! What the shim cannot enforce is the obligation it assumes: a `Source` written outside the crate may
//! hand back a `Map` built from raw parts whose base carries no `REGION_ALIGN` residue, and the
//! trait's safety text asks only for something suitably aligned. No test runs a non-conforming source.
pub use crate::mem::{Access, Error, LayoutField, Map, Peer, Request, Source};
