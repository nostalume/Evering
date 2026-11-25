//! Authors the transfer locator's bytes — pool identity, class and slot, generation, metadata tag,
//! type id — and the mechanical size rule the value behind them follows. `Block::transfer_with` is the
//! only thing that mints one; `map` and `update` rewrite the header half; `adopt_id` and `takeover`
//! read it back. The two `Shape` impls below already cover every sized type and every slice, so a
//! caller satisfies that bound rather than declaring one, and a third impl would collide with them.
//!
//! The tag encoding sits outside the composed schema key, so reshaping these fields keeps every schema
//! valid while an unrecognised tag simply decodes as no metadata and is refused as a type mismatch —
//! no size or offset assertion catches the reorder. That is tolerable only because the locator is not a
//! reconstruction capability: `Token` is deliberately unpublished and `layout::Shape` is the half a
//! caller ever holds.
use core::{alloc::Layout, mem, ptr};

use crate::{
    msg::{Repr, TypeId},
    place::LayoutId,
    schema::{SchemaKey, SharedSchema, compose_schema, schema_id},
};

/// Which slot a locator names: the class it belongs to, then the slot inside that class. A pair of
/// indices, never an address range: those are `mem::AddrSpan` and the spans `dir` stages.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClassSlot {
    pub(crate) class: usize,
    pub(crate) slot: usize,
}

/// A locator carried by the queue. It never authorizes pointer reconstruction.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct PoolToken {
    pub(crate) pool: LayoutId,
    pub(crate) id: TypeId,
    pub(crate) at: ClassSlot,
    pub(crate) generation: u32,
    metadata_kind: usize,
    metadata_len: usize,
}

impl PoolToken {
    pub(crate) const fn new(
        pool: LayoutId,
        at: ClassSlot,
        generation: u32,
        metadata: Metadata,
        id: TypeId,
    ) -> Self {
        let (metadata_kind, metadata_len) = match metadata {
            Metadata::Sized => (0, 0),
            Metadata::Slice(len) => (1, len),
        };
        Self {
            pool,
            id,
            at,
            generation,
            metadata_kind,
            metadata_len,
        }
    }

    pub(crate) const fn metadata(&self) -> Option<Metadata> {
        match self.metadata_kind {
            0 if self.metadata_len == 0 => Some(Metadata::Sized),
            1 => Some(Metadata::Slice(self.metadata_len)),
            _ => None,
        }
    }
}

#[repr(C)]
pub struct Token<H: Repr> {
    pub(crate) token: PoolToken,
    pub(crate) header: H,
}

impl<H: Repr> SharedSchema for Token<H> {
    const SCHEMA: SchemaKey =
        compose_schema(SchemaKey::new(schema_id("evering.transfer"), 1), H::SCHEMA);
}

unsafe impl<H: Repr> Repr for Token<H> {}

impl<H: Repr + core::fmt::Debug> core::fmt::Debug for Token<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Token")
            .field("header", &self.header)
            .field("token", &self.token)
            .finish()
    }
}

impl<H: Repr> Token<H> {
    pub(crate) fn map<T: Repr>(self, map: impl FnOnce(H) -> T) -> Token<T> {
        Token {
            token: self.token,
            header: map(self.header),
        }
    }

    pub(crate) fn update(&mut self, update: impl FnOnce(&mut H)) {
        update(&mut self.header);
    }
}

/// Mechanical metadata for sized values and slices.
///
/// There is nothing for a caller to implement: the two blanket impls below cover every sized type and
/// every slice, so an attempt to write a third is a coherence conflict. A `Sealed` supertrait would have
/// repeated that guarantee without adding it, which is why none is here.
pub trait Shape {
    fn metadata(ptr: *const Self) -> Metadata;
    fn layout(metadata: Metadata) -> Result<Layout, MetadataError>;
}

impl<T> Shape for T {
    #[inline(always)]
    fn metadata(_: *const Self) -> Metadata {
        Metadata::Sized
    }

    fn layout(metadata: Metadata) -> Result<Layout, MetadataError> {
        match metadata {
            Metadata::Sized => Ok(Layout::new::<T>()),
            Metadata::Slice(_) => Err(MetadataError::WrongKind),
        }
    }
}

impl<T> Shape for [T] {
    #[inline(always)]
    fn metadata(ptr: *const Self) -> Metadata {
        Metadata::Slice(ptr.len())
    }

    fn layout(metadata: Metadata) -> Result<Layout, MetadataError> {
        match metadata {
            Metadata::Slice(len) => Layout::array::<T>(len).map_err(|_| MetadataError::Overflow),
            Metadata::Sized => Err(MetadataError::WrongKind),
        }
    }
}

/// What [`Shape`] says travels beside a value's address: nothing, or a slice's element count.
///
/// This is the metadata half of a `*const T where T: ?Sized`, read off the pointer by
/// [`Shape::metadata`] and turned back into bytes by [`Shape::layout`]. It is a `MetadataError`
/// away from a layout rather than an option because the count arrived through a queue header and a
/// caller has to be able to tell "not a slice" from "too many elements to name".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metadata {
    /// The value is sized, so the pointer alone describes it.
    Sized,
    /// The value is a slice of this many elements.
    Slice(usize),
}

/// Why [`Shape::layout`] refuses to turn [`Metadata`] into a layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataError {
    /// The metadata is for the other kind — a length where the type has none, or none where the
    /// type is a slice. This is what a payload whose element type differs reports.
    WrongKind,
    /// The element count times the element size does not fit in `usize`.
    Overflow,
}

impl Metadata {
    pub(crate) unsafe fn as_ptr<T: ?Sized>(self, raw: *mut u8) -> *mut T {
        match self {
            Metadata::Sized => unsafe { mem::transmute_copy(&raw) },
            Metadata::Slice(len) => {
                let slice = ptr::slice_from_raw_parts_mut(raw as *mut (), len);
                unsafe { mem::transmute_copy(&slice) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::alloc::Layout;

    use super::{Metadata, Shape, Token};
    use crate::msg::Repr;

    #[test]
    fn a_token_is_a_repr_whose_shape_refuses_wrong_metadata() {
        fn assert_repr<T: Repr>() {}
        assert_repr::<Token<()>>();
        assert_eq!(
            <u64 as Shape>::layout(Metadata::Sized),
            Ok(Layout::new::<u64>())
        );
        assert!(<u64 as Shape>::layout(Metadata::Slice(1)).is_err());
        assert_eq!(
            <[u32] as Shape>::layout(Metadata::Slice(3)),
            Ok(Layout::array::<u32>(3).unwrap())
        );
        assert!(<[u64] as Shape>::layout(Metadata::Slice(usize::MAX)).is_err());
    }
}
