//! The published vocabulary of one mapping — `Request`, `Access`, `Source`, `Map`, `Peer`,
//! `LayoutField`, `Error` and the `REGION_ALIGN` a mapping is arranged to — over the private
//! implementation module `mem`, which is their only address. Nothing in this file computes: a `Map`
//! arrives from a caller's `Source::map` and is consumed once by the composition, which erases the
//! backend it came from. It lives behind a path rather than at the root because these are the words for
//! *how* a region gets mapped, not nouns held across an exchange.
//!
//! What the shim cannot enforce is still the obligation it assumes: a `Source` written outside the
//! crate may hand back a map whose base carries a `REGION_ALIGN` residue, and nothing here runs it.
//! Publishing the bound is the whole of what this file can do about that: the multiple is what
//! `header::recorded_at` refuses a member for missing, and `pool::Geometry` refuses a class wider
//! than it.
pub use crate::mem::{Access, Error, LayoutField, Map, Peer, REGION_ALIGN, Request, Source};
