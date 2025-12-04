use core::marker::PhantomData;
use core::ptr::{self, NonNull};

use crate::boxed::PBox;
use crate::mem::{IsMetaSpanOf, MemAllocator, Meta, MetaSpanOf};
use crate::msg::{Envelope, Message, Tag, TagRef, TypeId, TypeTag};

#[derive(Clone, Copy, Debug)]
enum Metadata {
    Sized,
    Slice(usize),
}

impl Metadata {
    #[inline(always)]
    const fn from_ptr<T>(_ptr: *const T) -> Self {
        Metadata::Sized
    }

    #[inline(always)]
    const fn from_slice<T>(ptr: *const [T]) -> Self {
        Metadata::Slice(ptr.len())
    }
}

pub struct TokenOf<T: ?Sized + ptr::Pointee, M> {
    span: M,
    metadata: Metadata,
    _marker: PhantomData<T>,
}

impl<T, M> TokenOf<T, M> {
    #[inline]
    pub fn as_ptr<A: MemAllocator>(&self, alloc: A) -> (NonNull<T>, A)
    where
        M: IsMetaSpanOf<A> + Clone,
    {
        let meta = unsafe { IsMetaSpanOf::recall(self.span.clone(), alloc.base_ptr()) };
        match self.metadata {
            Metadata::Sized => {
                let ptr = unsafe { meta.as_ptr::<T>() };
                unsafe { (NonNull::new_unchecked(ptr), alloc) }
            }
            _ => unreachable!(),
        }
    }

    #[inline(always)]
    pub const unsafe fn from_raw(span: M, ptr: *const T) -> Self {
        let metadata = Metadata::from_ptr(ptr);
        TokenOf {
            span,
            metadata,
            _marker: PhantomData,
        }
    }

    pub unsafe fn detokenize<A: MemAllocator, Out>(
        self,
        alloc: A,
        f: impl FnOnce(A::Meta, *mut T, A) -> Out,
    ) -> Out
    where
        M: IsMetaSpanOf<A>,
    {
        let TokenOf { span, metadata, .. } = self;
        let meta = unsafe { IsMetaSpanOf::recall(span, alloc.base_ptr()) };
        match metadata {
            Metadata::Sized => {
                let ptr = unsafe { meta.as_ptr::<T>() };
                f(meta, ptr, alloc)
            }
            _ => unreachable!(),
        }
    }

    pub fn detoken<A: MemAllocator>(self, alloc: A) -> PBox<T, A>
    where
        M: IsMetaSpanOf<A>,
    {
        PBox::<T, A>::detoken_of(self, alloc)
    }
}

impl<T, M> TokenOf<[T], M> {
    #[inline]
    pub fn as_ptr<A: MemAllocator>(&self, alloc: A) -> (NonNull<[T]>, A)
    where
        M: IsMetaSpanOf<A> + Clone,
    {
        let meta = unsafe { IsMetaSpanOf::recall(self.span.clone(), alloc.base_ptr()) };
        match self.metadata {
            Metadata::Slice(len) => {
                let ptr = unsafe { meta.as_slice::<T>(len) };
                unsafe { (NonNull::new_unchecked(ptr), alloc) }
            }
            _ => unreachable!(),
        }
    }

    #[inline(always)]
    pub const unsafe fn from_slice(span: M, ptr: *const [T]) -> Self {
        let metadata = Metadata::from_slice(ptr);
        TokenOf {
            span,
            metadata,
            _marker: PhantomData,
        }
    }

    pub unsafe fn detokenize<A: MemAllocator, Out>(
        self,
        alloc: A,
        f: impl FnOnce(A::Meta, *mut [T], A) -> Out,
    ) -> Out
    where
        M: IsMetaSpanOf<A>,
    {
        let TokenOf { span, metadata, .. } = self;
        let meta = unsafe { IsMetaSpanOf::recall(span, alloc.base_ptr()) };
        match metadata {
            Metadata::Slice(len) => {
                let ptr = unsafe { meta.as_slice::<T>(len) };
                f(meta, ptr, alloc)
            }
            _ => unreachable!(),
        }
    }

    pub fn detoken<A: MemAllocator>(self, alloc: A) -> PBox<[T], A>
    where
        M: IsMetaSpanOf<A>,
    {
        PBox::<[T], A>::detoken_of(self, alloc)
    }
}

pub type AllocToken<A> = Token<MetaSpanOf<A>>;
pub struct Token<M> {
    span: M,
    metadata: Metadata,
    id: TypeId,
}

impl<M> core::fmt::Debug for Token<M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Token")
            .field("metadata", &self.metadata)
            .field("id", &self.id)
            .finish()
    }
}

impl<M> Token<M> {
    #[inline]
    pub const fn empty() -> Self
    where
        M: [const] crate::mem::Span,
    {
        Self {
            span: M::null(),
            metadata: Metadata::Sized,
            id: <() as TypeTag>::TYPE_ID,
        }
    }

    #[inline(always)]
    pub fn pack_default<H: Envelope + Default>(self) -> PackToken<H, M> {
        PackToken {
            header: H::default(),
            token: self,
        }
    }

    #[inline(always)]
    pub const fn pack<H: Envelope>(self, header: H) -> PackToken<H, M> {
        PackToken {
            header,
            token: self,
        }
    }

    #[inline]
    pub fn recall<T: Message + ?Sized>(token: Self) -> Option<TokenOf<T, M>> {
        let Self { span, metadata, id } = token;
        if id == T::TYPE_ID {
            let token_of = TokenOf {
                span,
                metadata,
                _marker: PhantomData,
            };
            Some(token_of)
        } else {
            None
        }
    }

    #[inline]
    pub fn forget<T: Message + ?Sized>(token_of: TokenOf<T, M>) -> Self {
        let TokenOf { span, metadata, .. } = token_of;
        let id = T::TYPE_ID;

        Self { span, metadata, id }
    }
}

pub struct PackToken<H: Envelope, M> {
    header: H,
    token: Token<M>,
}

// impl<H: Envelope, M> core::fmt::Debug for PackToken<H, M> {
//     fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
//         f.debug_struct("PackToken")
//             .field("header", &self.header)
//             .field("token", &self.token)
//             .finish()
//     }
// }

impl<H: Envelope, M> PackToken<H, M> {
    #[inline]
    pub fn into_parts(self) -> (Token<M>, H) {
        let Self { header, token } = self;
        (token, header)
    }

    #[inline]
    pub fn map_header_in<F: FnOnce(&mut H, &Token<M>)>(&mut self, f: F) {
        f(&mut self.header, &self.token)
    }

    #[inline]
    pub fn map_header<T: Envelope, F: FnOnce(H, &Token<M>) -> T>(self, f: F) -> PackToken<T, M>
    {
        let (token, header) = self.into_parts();
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
    pub fn with_tag_in<T>(&mut self, value: T)
    where
        H: TagRef<T>,
    {
        self.header.with_tag_in(value);
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
