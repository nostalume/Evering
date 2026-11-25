//! The one address for what a type claims to be, arranged by the question each name answers.
//!
//! The vocabulary below is grouped into four slots — **Identity** (what the bytes are), **Place**
//! (where they are), **Shape** (how big, how cut, and what may be matched against a member's
//! header), and **Cause** (why a request was refused, which is not published here — it lives at the
//! crate root). The five underlying modules (`header`, `msg`, `place`, `schema`, `token`) are
//! private, so this file is where their vocabulary is published; nothing here is a second name for
//! something the root already holds.
//!
//! What a region is *made of* is not published: the layout trait, a record's admission context and
//! the header type itself stay inside the crate, because every layout a region composes is one the
//! crate wrote, and the choice a caller actually makes is a payload — `Repr` with `SharedSchema` —
//! plus a capacity. Two numbers a port still has to agree with are handed over below as data rather
//! than by reaching through the type they come from.
//!
//! A handler is frozen against `AbiProfile::NATIVE` when it is made, and recovery compares against
//! what a member's header recorded rather than what the caller expects. Bumping a revision while
//! leaving the bytes alone passes unnoticed — a changed layout body without a revision bump is
//! refused, but nothing checks the other direction, and the profile itself is target-dependent.

// Identity: what a piece of bytes claims to be — the schema key, the mixing rule, and the marker
// that lets a value of that identity be handed out by copy.
pub use crate::msg::Repr;
pub use crate::schema::{SchemaId, SchemaKey, SharedSchema, compose_schema, schema_id};

// Place: a coordinate on a region, with no claim about what is stored there.
pub use crate::place::RegionId;

// Shape: the profile a record persists and the handler a caller mints to match against what a
// member's header recorded.
pub use crate::header::{AbiProfile, RecoveryHandler};
// A `?Sized` payload's extent arrives and fails as these two, so whoever can name the bound has to
// be able to name what it answers with.
pub use crate::token::{Metadata, MetadataError, Shape};

// The member word's cut as published data rather than as a header internal: a process that joins a
// region without this crate writes that word, and the numbers it has to match were only readable in
// `src`. `tests/portable_contracts/member_word.rs` pins them.
pub use crate::header::{
    MEMBER_DEAD, MEMBER_FREE, MEMBER_GENERATION_MAX, MEMBER_LIVE, MEMBER_RESERVED, MEMBER_RETIRED,
    MEMBER_STATE_BITS, MEMBER_STATE_MASK,
};

/// What a region's header occupies before any layout body: the record, the one atomic, the identity
/// words and the writer's info schema.
///
/// A port that assembles an initial image without this crate measures its own copy of that order
/// against this number — `probe/bare_port/src/const_image.rs` is that port. The value is derived
/// rather than written out because it is not a total anyone copies: the image a port publishes is
/// sized by its own fields, and this is the check that the two have not aged apart.
pub const HEADER_PREFIX_BYTES: usize = core::mem::size_of::<crate::header::RcHeader<()>>();
/// How a region's header prefix has to be aligned, for the same reason and to the same reader.
pub const HEADER_PREFIX_ALIGN: usize = core::mem::align_of::<crate::header::RcHeader<()>>();

/// A handler that recovers a channel member carrying payload `H`, as this build implements it.
pub const fn recovery_handler<H: Repr>() -> RecoveryHandler {
    RecoveryHandler::of::<crate::channel::Duplex<H>>()
}
