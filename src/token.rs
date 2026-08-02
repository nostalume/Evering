use core::{alloc::Layout, mem, ptr};

use crate::{
    msg::{Repr, TypeId},
    schema::{LayoutId, SchemaKey, SharedSchema, compose_schema, schema_id},
};

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) class: usize,
    pub(crate) slot: usize,
}

/// A locator carried by the queue. It never authorizes pointer reconstruction.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct PoolToken {
    pub(crate) pool: LayoutId,
    pub(crate) id: TypeId,
    pub(crate) span: Span,
    pub(crate) generation: u32,
    metadata_kind: usize,
    metadata_len: usize,
}

impl PoolToken {
    pub(crate) const fn new(
        pool: LayoutId,
        span: Span,
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
            span,
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

unsafe impl<H: Repr> Repr for Token<H> {
    const SCHEMA: SchemaKey = <Self as SharedSchema>::SCHEMA;
}

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
pub const trait Shape: crate::seal::Sealed {
    fn metadata(ptr: *const Self) -> Metadata;
    fn layout(metadata: Metadata) -> Result<Layout, MetadataError>;
}

impl<T> crate::seal::Sealed for T {}

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

impl<T> crate::seal::Sealed for [T] {}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metadata {
    Sized,
    Slice(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataError {
    WrongKind,
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
    fn token_is_a_repr_and_shape_rejects_wrong_metadata() {
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
