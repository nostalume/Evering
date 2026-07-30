use core::alloc::Layout;
use core::marker::PhantomData;
use core::mem;
use core::ops::Deref;
use core::ptr::{self, NonNull};

use crate::boxed::PBoxIn;
use crate::channel::driver::Identified;
use crate::mem::{Meta, TransferAllocator, TransferError};
use crate::msg::{Repr, Tag, TagId, TagRef, TypeId, type_id};
use crate::numeric::Id;
use crate::schema::{LayoutId, SchemaKey, SharedSchema, compose_schema, schema_id};

/// Mechanical geometry for the standard sized and slice representations.
///
/// ```compile_fail
/// use evering::token::Shape;
///
/// struct Custom;
/// impl Shape for Custom {}
/// ```
pub const trait Shape: crate::seal::Sealed {
    fn metadata(ptr: *const Self) -> Metadata;
    fn layout(metadata: Metadata) -> Result<Layout, MetadataError>;
}

impl<T> crate::seal::Sealed for T {}

impl<T> Shape for T {
    #[inline(always)]
    fn metadata(_ptr: *const Self) -> Metadata {
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
    #[inline(always)]
    const fn from_ptr<T: [const] Shape + ?Sized>(ptr: *const T) -> Self {
        T::metadata(ptr)
    }

    #[inline]
    pub(crate) unsafe fn as_ptr<T: ?Sized>(self, raw: *mut u8) -> *mut T {
        // Safety: interprets the ptr by `transmute_copy`.
        // thin ptr or slice ptr will match the size by Metadata context.
        //
        // Don't use `transmute` due to size check hack.
        match self {
            Metadata::Sized => unsafe { mem::transmute_copy(&raw) },
            Metadata::Slice(len) => {
                let slice = ptr::slice_from_raw_parts_mut(raw as *mut (), len);
                unsafe { mem::transmute_copy(&slice) }
            }
        }
    }
}

pub struct TokenOf<T: ?Sized, M: Meta> {
    owner: LayoutId,
    meta: M,
    metadata: Metadata,
    _marker: PhantomData<T>,
}

impl<T: ?Sized, M: Meta> TokenOf<T, M> {
    #[inline]
    pub(crate) fn boxed<A: TransferAllocator<Meta = M>>(
        self,
        alloc: A,
    ) -> Result<PBoxIn<T, A>, TokenOfError<T, M>>
    where
        T: Shape,
    {
        let ptr = match self.as_ptr(&alloc) {
            Ok(ptr) => ptr,
            Err(kind) => {
                return Err(TokenOfError { kind, token: self });
            }
        };
        let Self { meta, .. } = self;
        Ok(unsafe { PBoxIn::from_raw_ptr(ptr.as_ptr(), meta, alloc) })
    }

    #[inline]
    pub(crate) fn as_ptr<A: TransferAllocator<Meta = M>>(
        &self,
        alloc: &A,
    ) -> Result<NonNull<T>, ReconstructError>
    where
        T: Shape,
    {
        let layout = T::layout(self.metadata).map_err(ReconstructError::Metadata)?;
        let raw = alloc
            .admit(self.owner, &self.meta, layout)
            .map_err(ReconstructError::Transfer)?;
        let ptr = unsafe { self.metadata.as_ptr(raw.as_ptr()) };
        NonNull::new(ptr).ok_or(ReconstructError::Transfer(TransferError::Null))
    }

    #[inline(always)]
    pub fn pack<P: Repr>(self, header: P) -> PackToken<P, M>
    where
        T: Repr,
    {
        let () = T::VALID;
        let () = P::VALID;
        let () = M::VALID;
        PackToken {
            header,
            token: Token {
                owner: self.owner,
                meta: self.meta,
                metadata: self.metadata,
                id: type_id::<P, T>(),
            },
        }
    }
}

impl<T: ?Sized + Shape, M: Meta> TokenOf<T, M> {
    #[inline(always)]
    pub(crate) unsafe fn from_raw(owner: LayoutId, meta: M, ptr: *const T) -> Self {
        let metadata = Metadata::from_ptr(ptr);
        TokenOf {
            owner,
            meta,
            metadata,
            _marker: PhantomData,
        }
    }
}

pub struct Token<M: Meta> {
    owner: LayoutId,
    meta: M,
    metadata: Metadata,
    id: TypeId,
}

impl<M: Meta> core::fmt::Debug for Token<M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Token")
            .field("metadata", &self.metadata)
            .field("id", &self.id)
            .finish()
    }
}

impl<M: Meta> Token<M> {
    #[inline(always)]
    pub(crate) const fn with<H: Repr>(self, header: H) -> PackToken<H, M> {
        let () = H::VALID;
        let () = M::VALID;
        PackToken {
            header,
            token: self,
        }
    }

    #[inline]
    pub(crate) fn identify<P: Repr, T: Repr + ?Sized>(self) -> Result<TokenOf<T, M>, Self> {
        if self.id != type_id::<P, T>() {
            return Err(self);
        }
        Ok(TokenOf {
            owner: self.owner,
            meta: self.meta,
            metadata: self.metadata,
            _marker: PhantomData,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconstructError {
    Metadata(MetadataError),
    Transfer(TransferError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenKind {
    Unknown,
    Reconstruct(ReconstructError),
}

pub struct OpenError<R> {
    pub kind: OpenKind,
    pub record: R,
}

pub struct DiscardError<R> {
    pub kind: TransferError,
    pub record: R,
}

impl<R> core::fmt::Debug for DiscardError<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DiscardError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl<R> core::fmt::Debug for OpenError<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OpenError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

pub(crate) struct TokenOfError<T: ?Sized, M: Meta> {
    pub kind: ReconstructError,
    pub token: TokenOf<T, M>,
}

impl<T: ?Sized, M: Meta> core::fmt::Debug for TokenOfError<T, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TokenOfError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl<M: Meta> SharedSchema for Token<M> {
    const SCHEMA: SchemaKey =
        compose_schema(SchemaKey::new(schema_id("evering.token"), 2), M::SCHEMA);
}

pub type ReqToken<T, M> = PackToken<ReqId<T>, M>;
pub struct PackToken<H: Repr, M: Meta> {
    header: H,
    token: Token<M>,
}

impl<H: Repr, M: Meta> SharedSchema for PackToken<H, M> {
    const SCHEMA: SchemaKey = compose_schema(
        compose_schema(
            SchemaKey::new(schema_id("evering.pack-token"), 2),
            H::SCHEMA,
        ),
        Token::<M>::SCHEMA,
    );
}

unsafe impl<H: Repr, M: Meta> Repr for PackToken<H, M> {
    const SCHEMA: SchemaKey = <Self as SharedSchema>::SCHEMA;
}

impl<H: Repr + core::fmt::Debug, M: Meta> core::fmt::Debug for PackToken<H, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PackToken")
            .field("header", &self.header)
            .field("token", &self.token)
            .finish()
    }
}

impl<H: Repr, M: Meta> PackToken<H, M> {
    #[inline]
    pub(crate) fn unpack(self) -> (Token<M>, H) {
        (self.token, self.header)
    }

    #[inline]
    pub fn update<F: FnOnce(&mut H, &Token<M>)>(&mut self, f: F) {
        f(&mut self.header, &self.token)
    }

    #[inline]
    pub fn map<T: Repr, F: FnOnce(H, &Token<M>) -> T>(self, f: F) -> PackToken<T, M> {
        let (token, header) = self.unpack();
        PackToken {
            header: f(header, &token),
            token,
        }
    }

    #[inline]
    pub fn with_tag<T>(self, value: T) -> Self
    where
        H: Tag<T>,
    {
        let header = self.header.with_tag(value);
        PackToken {
            header,
            token: self.token,
        }
    }

    #[inline]
    pub fn set_tag<T>(&mut self, value: T)
    where
        H: TagRef<T>,
    {
        self.header.set_tag(value);
    }

    #[inline]
    pub fn tag<T>(&self) -> T
    where
        H: Tag<T>,
    {
        self.header.tag()
    }

    #[inline]
    pub fn tag_ref<T>(&self) -> &T
    where
        H: TagRef<T>,
    {
        self.header.tag_ref()
    }
}

impl<P: Repr, M: Meta> PackToken<P, M> {
    pub(crate) fn discard_with<A>(self, alloc: A) -> Result<P, DiscardError<Self>>
    where
        A: TransferAllocator<Meta = M>,
    {
        let Self { header, token } = self;
        let layout = token.meta.layout_bytes();
        if let Err(kind) = alloc.admit(token.owner, &token.meta, layout) {
            return Err(DiscardError {
                kind,
                record: Self { header, token },
            });
        }
        if layout.size() != 0
            && let Err(meta) = alloc.dealloc(token.meta, layout)
        {
            return Err(DiscardError {
                kind: TransferError::Busy,
                record: Self {
                    header,
                    token: Token { meta, ..token },
                },
            });
        }
        Ok(header)
    }

    pub fn open<T: ?Sized + Repr + Shape, A>(
        self,
        alloc: A,
    ) -> Result<(P, PBoxIn<T, A>), OpenError<Self>>
    where
        A: TransferAllocator<Meta = M>,
    {
        let () = P::VALID;
        let () = T::VALID;
        let Self { header, token } = self;
        let token = match token.identify::<P, T>() {
            Ok(token) => token,
            Err(token) => {
                return Err(OpenError {
                    kind: OpenKind::Unknown,
                    record: Self { header, token },
                });
            }
        };
        match token.boxed(alloc) {
            Ok(value) => Ok((header, value)),
            Err(error) => {
                let TokenOf {
                    owner,
                    meta,
                    metadata,
                    ..
                } = error.token;
                Err(OpenError {
                    kind: OpenKind::Reconstruct(error.kind),
                    record: Self {
                        header,
                        token: Token {
                            owner,
                            meta,
                            metadata,
                            id: type_id::<P, T>(),
                        },
                    },
                })
            }
        }
    }
}

pub type ReqNull = ReqId<()>;
pub struct ReqId<T: Repr> {
    id: Id,
    header: T,
}

impl<T: Repr> const Deref for ReqId<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.header
    }
}

impl<T: Repr> SharedSchema for ReqId<T> {
    const SCHEMA: SchemaKey =
        compose_schema(SchemaKey::new(schema_id("evering.req-id"), 1), T::SCHEMA);
}

unsafe impl<T: Repr> Repr for ReqId<T> {
    const SCHEMA: SchemaKey = <Self as SharedSchema>::SCHEMA;
}

impl<T: Repr> TagId for ReqId<T> {
    #[inline]
    fn with_id(self, value: Id) -> Self
    where
        Self: Sized,
    {
        Self { id: value, ..self }
    }
    #[inline]
    fn id(&self) -> Id {
        self.id
    }
}

impl<H: Tag<T>, T> Tag<T> for ReqId<H> {
    #[inline]
    fn with_tag(self, value: T) -> Self
    where
        Self: Sized,
    {
        Self {
            id: self.id,
            header: self.header.with_tag(value),
        }
    }

    #[inline]
    fn tag(&self) -> T {
        self.header.tag()
    }
}

impl<T: Repr> ReqId<T> {
    pub fn header(&self) -> &T {
        &self.header
    }
}

impl<T: Repr, M: Meta> Identified<ReqToken<T, M>> for PackToken<T, M> {
    fn compose(self, id: Id) -> ReqToken<T, M> {
        let (token, header) = self.unpack();
        let header = ReqId { header, id };
        token.with(header)
    }

    fn decompose(token: ReqToken<T, M>) -> (Self, Id) {
        let (token, header) = token.unpack();
        let ReqId { id, header } = header;
        (token.with(header), id)
    }
}

#[cfg(test)]
mod representation_tests {
    use super::PackToken;
    use crate::msg::Repr;
    use crate::talc;

    #[test]
    fn packed_token_is_a_repr() {
        fn assert_repr<T: Repr>() {}

        assert_repr::<PackToken<(), talc::Meta>>();
    }
}

#[cfg(test)]
mod tests {
    use core::alloc::Layout;

    use super::{Metadata, Shape};

    #[test]
    fn pointee_layout_rejects_wrong_metadata_and_slice_overflow() {
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
