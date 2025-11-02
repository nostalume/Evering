use core::marker::PhantomData;
use core::ptr;

use crate::boxed::PBox;
use crate::malloc::{MemAllocator, Meta, MetaOf, SpanOf};

pub mod type_id {
    pub type TypeId = u64;

    pub trait TypeTag {
        // `core::any::TypeId` can't be same across different compilation.
        // Thus choose manual definition strategy.
        const TYPE_ID: TypeId;
    }

    // [T]
    impl<T: TypeTag> TypeTag for [T] {
        const TYPE_ID: TypeId = combine(fnv1a64("slice"), T::TYPE_ID);
    }

    // &T
    impl<T: TypeTag + ?Sized> TypeTag for &T {
        const TYPE_ID: TypeId = combine(fnv1a64("ref"), T::TYPE_ID);
    }

    // Option<T>
    impl<T: TypeTag> TypeTag for Option<T> {
        const TYPE_ID: TypeId = combine(fnv1a64("option"), T::TYPE_ID);
    }

    #[inline]
    const fn fnv1a64(s: &str) -> TypeId {
        let mut hash = 0xcbf29ce484222325u64;
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u64;
            hash = hash.wrapping_mul(0x100000001b3);
            i += 1;
        }
        hash
    }

    #[inline]
    const fn combine(a: TypeId, b: TypeId) -> TypeId {
        let mixed = a ^ b.rotate_left(17);
        mixed.wrapping_mul(0x9E3779B97F4A7C15)
    }

    pub const fn type_id(s: &str) -> TypeId {
        fnv1a64(s)
    }
}

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

pub type ATokenOf<T, A> = TokenOf<T, SpanOf<MetaOf<A>>>;

pub struct TokenOf<T: ?Sized + ptr::Pointee, M> {
    span: M,
    metadata: Metadata,
    _marker: PhantomData<T>,
}

impl<T, M> TokenOf<T, M> {
    #[inline(always)]
    pub const unsafe fn from_ptr(span: M, ptr: *const T) -> Self {
        let metadata = Metadata::from_ptr(ptr);
        TokenOf {
            span,
            metadata,
            _marker: PhantomData,
        }
    }

    pub unsafe fn detokenize<A: MemAllocator, Out>(
        t: ATokenOf<T, A>,
        alloc: A,
        f: impl FnOnce(A::Meta, *mut T, A) -> Out,
    ) -> Out {
        let TokenOf { span, metadata, .. } = t;
        let meta = unsafe { A::Meta::resolve(span, alloc.base_ptr()) };
        match metadata {
            Metadata::Sized => {
                let ptr = unsafe { meta.as_ptr::<T>() };
                f(meta, ptr, alloc)
            }
            _ => unreachable!(),
        }
    }
}

impl<T, M> TokenOf<[T], M> {
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
        t: ATokenOf<[T], A>,
        alloc: A,
        f: impl FnOnce(A::Meta, *mut [T], A) -> Out,
    ) -> Out {
        let TokenOf { span, metadata, .. } = t;
        let meta = unsafe { A::Meta::resolve(span, alloc.base_ptr()) };
        match metadata {
            Metadata::Slice(len) => {
                let ptr = unsafe { meta.as_slice::<T>(len) };
                f(meta, ptr, alloc)
            }
            _ => unreachable!(),
        }
    }
}

pub use self::type_id::{TypeId, TypeTag, type_id};

trait Message: TypeTag {
    type Semantics;
}
impl<T: Message> Message for [T] {
    type Semantics = T::Semantics;
}

pub struct Move;
pub trait MoveMessage: Message<Semantics = Move> {}
impl<T: MoveMessage> MoveMessage for [T] {}

type AToken<A> = Token<SpanOf<MetaOf<A>>>;
struct Token<M> {
    span: M,
    metadata: Metadata,
    id: TypeId,
}

impl<M> Token<M> {
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
    pub fn erase<T: Message + ?Sized>(token_of: TokenOf<T, M>) -> Self {
        let TokenOf { span, metadata, .. } = token_of;
        let id = T::TYPE_ID;

        Self { span, metadata, id }
    }
}

impl Move {
    pub fn token<T: MoveMessage, A: MemAllocator>(t: T, alloc: A) -> AToken<A> {
        let b = PBox::new_in(t, alloc);
        let token = Token::erase(b.token());
        token
    }

    pub fn copied_slice_token<T: MoveMessage, A: MemAllocator>(t: &[T], alloc: A) -> AToken<A> {
        let b = PBox::copy_from_slice(t, alloc);
        let token = Token::erase(b.token());
        token
    }

    pub fn detoken<T: MoveMessage, A: MemAllocator>(t: AToken<A>, alloc: A) -> Option<PBox<T, A>> {
        let Some(token_of) = Token::recall::<T>(t) else {
            return None;
        };
        let b = PBox::<T, A>::detoken(token_of, alloc);
        Some(b)
    }

    pub fn slice_detoken<T: MoveMessage, A: MemAllocator>(
        t: AToken<A>,
        alloc: A,
    ) -> Option<PBox<[T], A>> {
        let Some(token_of) = Token::recall::<[T]>(t) else {
            return None;
        };
        let b = PBox::<[T], A>::detoken(token_of, alloc);
        Some(b)
    }
}
