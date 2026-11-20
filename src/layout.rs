//! The one address for what a type claims to be: `Layout`, `RcHeader`, `Magic`, `Status`,
//! `AbiProfile` and the recovery types from `header`, `Repr` from `msg`, the schema and region keys
//! from `schema`, `Shape` from `token`, plus `recovery_handler`, which is how a caller mints the
//! handler `Recovery::reap_with` matches against. Those four modules are private, so this is where
//! their vocabulary is published; nothing here is a second name for something the root already holds.
//!
//! A handler is frozen against `AbiProfile::NATIVE` when it is made, and recovery compares against
//! what a member's header recorded rather than what the caller expects. Bumping a revision while
//! leaving the bytes alone passes unnoticed — a changed `Layout` body without a revision bump is
//! refused, but nothing checks the other direction, and the profile itself is target-dependent.
pub use crate::header::{
    AbiProfile, Layout, Magic, RcHeader, RecoveryContext, RecoveryHandler, Status,
};
pub use crate::msg::Repr;
// The two keys a caller can defend are derived, not typed in: `SchemaId(pub u64)` accepts any
// integer, so without these the only reachable advice is to guess a number and hope no other crate
// guesses the same one.
pub use crate::schema::{
    LayoutContext, LayoutId, LayoutInfo, RegionAdmission, RegionId, SchemaId, SchemaKey,
    SharedSchema,
};
pub use crate::schema::{compose_schema, schema_id};
pub use crate::token::Shape;

pub const fn recovery_handler<H: Repr>() -> RecoveryHandler {
    RecoveryHandler::of::<crate::channel::Duplex<H>>()
}
