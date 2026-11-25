//! Where bytes are and what may be done to that place: the 128-bit `RegionId`, the `LayoutId`
//! coordinate that adds an offset to it, the `LayoutContext` a caller hands to an admission, and the
//! `RegionAdmission` a `Session` chooses once at `create` or `open`.
//!
//! This page answers position and standing only. It never states what the bytes *are* — that is
//! `crate::schema`, and a `LayoutId` is a place a caller must already have been given, not a way to
//! claim one. The two are separate modules precisely so the identity of a region and its address
//! cannot be confused for each other, which is what a `record` re-deriving from a hand-written
//! coordinate would allow.
//!
//! What is not enforced here: every field of `LayoutContext` is writable by the caller who builds it,
//! so `region` and `offset` reach a verdict as input rather than as conclusion. The `allow_init` field
//! is an operation-level property (whether this admission may initialize an uninitialized record), not
//! a session-level property. Callers must set it explicitly based on the operation type — see the
//! `LayoutContext` doc for details.

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegionId {
    pub high: u64,
    pub low: u64,
}

impl RegionId {
    pub const fn new(high: u64, low: u64) -> Self {
        Self { high, low }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LayoutId {
    pub region: RegionId,
    pub offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegionAdmission {
    Create(RegionId),
    Expect(RegionId),
}

/// Context passed to layout admission.
///
/// # Fields
///
/// - `region` / `offset`: The coordinate this admission is working at. The two consumers are the
///   writer, which copies them into a new record, and the verifier, which refuses a record whose
///   stored coordinate differs. `Layout::info` receives the context but no layout in this crate
///   derives identity from it: an `Info` is a shape of the value, not of its address.
/// - `allow_init`: Whether this admission is permitted to initialize an `Uninitialized` record.
///
/// # The `allow_init` Field
///
/// `allow_init` is an **operation-level** property: it answers "may THIS admission initialize
/// an uninitialized record?" It is NOT derived from the session's relationship to the region.
///
/// Callers must set this explicitly based on the operation type:
/// - `true` for operations that create new records (root header by region creator, new layout
///   in directory, new composition element)
/// - `false` for operations that attach to existing records (open, read, inspect)
///
/// The region creator's special privilege (initializing the root header) is expressed by
/// passing `allow_init: true` when constructing the root's `LayoutContext`, not by storing
/// a flag on `Region`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LayoutContext {
    pub(crate) region: RegionId,
    pub(crate) offset: u64,
    pub(crate) allow_init: bool,
}
