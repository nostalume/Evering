use core::marker::PhantomData;
use core::mem;
use core::ops::Deref;
use core::ptr::{self, NonNull};

use crate::boxed::PBox;
use crate::channel::driver::Identified;
use crate::mem::{MemAlloc, MemAllocator, Meta};
use crate::msg::{Envelope, Message, Tag, TagId, TagRef, TypeId, TypeTag};
use crate::numeric::Id;

pub const trait PointeeIn {
    fn metadata(ptr: *const Self) -> Metadata;
}

impl<T> PointeeIn for T {
    #[inline(always)]
    fn metadata(_ptr: *const Self) -> Metadata {
        Metadata::Sized
    }
}

impl<T> PointeeIn for [T] {
    #[inline(always)]
    fn metadata(ptr: *const Self) -> Metadata {
        Metadata::Slice(ptr.len())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metadata {
    Sized,
    Slice(usize),
}

impl Metadata {
    #[inline(always)]
    const fn from_ptr<T: [const] PointeeIn + ?Sized>(ptr: *const T) -> Self {
        T::metadata(ptr)
    }

    #[inline]
    pub unsafe fn as_ptr<T: ?Sized>(self, raw: *mut u8) -> *mut T {
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
    meta: M,
    metadata: Metadata,
    _marker: PhantomData<T>,
}

impl<T: ?Sized, M: Meta> TokenOf<T, M> {
    #[inline]
    pub fn boxed<A: MemAllocator<Meta = M>>(self, alloc: A) -> PBox<T, A> {
        let ptr = unsafe { self.metadata.as_ptr(self.meta.recall_by(&alloc).as_ptr()) };

        unsafe { PBox::from_raw_ptr(ptr, self.meta, alloc) }
    }

    #[inline]
    pub fn as_ptr<A: MemAlloc>(&self, alloc: &A) -> NonNull<T> {
        let ptr = unsafe { self.metadata.as_ptr(self.meta.recall_by(&alloc).as_ptr()) };
        unsafe { NonNull::new_unchecked(ptr) }
    }

    #[inline]
    pub unsafe fn detokenize<A: MemAlloc, H>(
        self,
        alloc: A,
        f: impl FnOnce(M, *mut T, A) -> H,
    ) -> H {
        let ptr = self.as_ptr(&alloc);
        let Self {
            meta, metadata: _, ..
        } = self;
        f(meta, ptr.as_ptr(), alloc)
    }
}

impl<T: ?Sized + PointeeIn, M: Meta> TokenOf<T, M> {
    #[inline(always)]
    pub unsafe fn from_raw(meta: M, ptr: *const T) -> Self {
        let metadata = Metadata::from_ptr(ptr);
        TokenOf {
            meta,
            metadata,
            _marker: PhantomData,
        }
    }
}

pub struct Token<M: Meta> {
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

impl<M: Meta, T: ?Sized + Message> From<TokenOf<T, M>> for Token<M> {
    fn from(value: TokenOf<T, M>) -> Self {
        Self {
            meta: value.meta,
            metadata: value.metadata,
            id: T::TYPE_ID,
        }
    }
}

impl<M: Meta> Token<M> {
    #[inline]
    pub fn null() -> Self
    where
        M: Meta,
    {
        Self {
            meta: M::null(),
            metadata: Metadata::Sized,
            id: <() as TypeTag>::TYPE_ID,
        }
    }

    #[inline(always)]
    pub fn with_default<H: Envelope + Default>(self) -> PackToken<H, M> {
        PackToken {
            header: H::default(),
            token: self,
        }
    }

    #[inline(always)]
    pub const fn with<H: Envelope>(self, header: H) -> PackToken<H, M> {
        PackToken {
            header,
            token: self,
        }
    }

    #[inline]
    pub fn identify<T: Message + ?Sized>(self) -> Option<TokenOf<T, M>> {
        (self.id == T::TYPE_ID).then_some(TokenOf {
            meta: self.meta,
            metadata: self.metadata,
            _marker: PhantomData,
        })
    }
}

pub type ReqToken<T, M> = PackToken<ReqId<T>, M>;
pub struct PackToken<H: Envelope, M: Meta> {
    header: H,
    token: Token<M>,
}

impl<H: Envelope + core::fmt::Debug, M: Meta> core::fmt::Debug for PackToken<H, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PackToken")
            .field("header", &self.header)
            .field("token", &self.token)
            .finish()
    }
}

impl<H: Envelope, M: Meta> PackToken<H, M> {
    #[inline]
    pub fn unpack(self) -> (Token<M>, H) {
        (self.token, self.header)
    }

    #[inline]
    pub fn update<F: FnOnce(&mut H, &Token<M>)>(&mut self, f: F) {
        f(&mut self.header, &self.token)
    }

    #[inline]
    pub fn map<T: Envelope, F: FnOnce(H, &Token<M>) -> T>(self, f: F) -> PackToken<T, M> {
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

pub type ReqNull = ReqId<()>;
pub struct ReqId<T: Envelope> {
    id: Id,
    header: T,
}

impl<T: Envelope> const Deref for ReqId<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.header
    }
}

impl<T: Envelope> Envelope for ReqId<T> {}

impl<T: Envelope> TagId for ReqId<T> {
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

impl<T: Envelope> ReqId<T> {
    pub fn header(&self) -> &T {
        &self.header
    }
}

impl<T: Envelope, M: Meta> Identified<ReqToken<T, M>> for PackToken<T, M> {
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
