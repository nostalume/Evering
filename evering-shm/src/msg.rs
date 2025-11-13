use core::marker::PhantomData;
use core::ptr::{self, NonNull};

use crate::boxed::PBox;
use crate::malloc::{IsMetaSpanOf, MemAllocator, Meta, MetaSpanOf};

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

    // &T
    impl<T: TypeTag + ?Sized> TypeTag for &mut T {
        const TYPE_ID: TypeId = combine(fnv1a64("ref mut"), T::TYPE_ID);
    }

    // Option<T>
    impl<T: TypeTag> TypeTag for Option<T> {
        const TYPE_ID: TypeId = combine(fnv1a64("option"), T::TYPE_ID);
    }

    #[inline]
    const fn fnv1a64(s: &str) -> TypeId {
        let mut hash = 0xcbf29ce484222325;
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

    #[cfg(test)]
    mod tests {
        use super::{TypeId, TypeTag, combine, type_id};
        macro_rules! type_tag {
            ($($ty:ty),*) => {
                $(
                    impl TypeTag for $ty {
                        const TYPE_ID: TypeId = type_id(stringify!($ty));
                    }
                )*
            };
        }

        type_tag! {
            u8,
            u32,
            f64,
            i32,
            bool
        }

        #[test]
        fn hash_consistent() {
            let id1 = type_id("u32");
            let id2 = type_id("u32");
            let id3 = type_id("u64");

            assert_eq!(id1, id2, "Type id should be same for same type");
            assert_ne!(id1, id3, "Type id shouldn't be same for different type");
        }

        #[test]
        fn combine_consistent() {
            let a = type_id("A");
            let b = type_id("B");
            let ab = combine(a, b);
            let ba = combine(b, a);
            assert_ne!(ab, ba, "Combination must be order dependent");
            let aa = combine(a, a);
            assert_ne!(aa, 0, "Combination must not be zero");
        }

        #[test]
        fn basic_type_consistent() {
            assert_ne!(u8::TYPE_ID, u32::TYPE_ID);
            assert_ne!(u32::TYPE_ID, i32::TYPE_ID);
            assert_ne!(i32::TYPE_ID, bool::TYPE_ID);
            assert_ne!(bool::TYPE_ID, f64::TYPE_ID);
        }

        #[test]
        fn wrap_type_consistent() {
            // 1. Test Option<T>
            type OptU32 = Option<u32>;
            type OptU8 = Option<u8>;
            assert_ne!(
                OptU32::TYPE_ID,
                OptU8::TYPE_ID,
                "Option types must differ based on inner type"
            );
            assert_ne!(
                u32::TYPE_ID,
                OptU32::TYPE_ID,
                "Inner type and Option should be different"
            );

            // 2. Test Slices [T]
            type SliceU32 = [u32];
            type SliceF64 = [f64];
            assert_ne!(
                SliceU32::TYPE_ID,
                SliceF64::TYPE_ID,
                "Slice types must differ based on inner type"
            );
            assert_ne!(
                u32::TYPE_ID,
                SliceU32::TYPE_ID,
                "Inner type and slice should be different"
            );

            // 3. Test References &T
            type RefU32<'a> = &'a u32;
            type RefMutU32<'a> = &'a mut u32;
            assert_ne!(
                RefU32::TYPE_ID,
                RefMutU32::TYPE_ID,
                "Immutable and mutable references must be distinct"
            );
            assert_ne!(
                RefU32::TYPE_ID,
                u32::TYPE_ID,
                "Reference and value type must be distinct"
            );

            // 4. Test nested generics
            type OptRefU32<'a> = Option<&'a u32>;
            type RefOptU32<'a> = &'a Option<u32>;
            assert_ne!(
                OptRefU32::TYPE_ID,
                RefOptU32::TYPE_ID,
                "Option<&T> must differ from &Option<T>"
            );
        }
    }
}

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

pub use self::type_id::{TypeId, TypeTag};

pub trait Message: TypeTag {
    type Semantics;
}

impl<T: Message> Message for [T] {
    type Semantics = T::Semantics;
}

pub struct Move;
pub trait MoveMessage: Message<Semantics = Move> {
    #[inline]
    fn token<A: MemAllocator>(self, alloc: A) -> (AllocToken<A>, A)
    where
        Self: Sized,
    {
        PBox::new_in(self, alloc).token_with()
    }

    #[inline]
    fn copied_slice_token<A: MemAllocator>(t: &[Self], alloc: A) -> (AllocToken<A>, A)
    where
        Self: Sized,
    {
        PBox::copy_from_slice(t, alloc).token_with()
    }

    #[inline]
    fn slice_token<A: MemAllocator, F: FnMut(usize) -> Self>(
        len: usize,
        f: F,
        alloc: A,
    ) -> (AllocToken<A>, A)
    where
        Self: Sized,
    {
        PBox::new_slice_in(len, f, alloc).token_with()
    }

    #[inline]
    fn detoken<A: MemAllocator>(t: AllocToken<A>, alloc: A) -> Option<PBox<Self, A>>
    where
        Self: Sized,
    {
        PBox::<Self, A>::detoken(t, alloc)
    }

    #[inline]
    fn slice_detoken<A: MemAllocator>(t: AllocToken<A>, alloc: A) -> Option<PBox<[Self], A>>
    where
        Self: Sized,
    {
        PBox::<[Self], A>::detoken(t, alloc)
    }
}

impl<T: Message<Semantics = Move>> MoveMessage for T {}

impl<T: MoveMessage> MoveMessage for [T] {}

type AllocToken<A> = Token<MetaSpanOf<A>>;
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
    #[inline(always)]
    pub fn pack<H: Envelope>(self) -> PackToken<H, M> {
        PackToken::from_token(self)
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
    pub fn erase<T: Message + ?Sized>(token_of: TokenOf<T, M>) -> Self {
        let TokenOf { span, metadata, .. } = token_of;
        let id = T::TYPE_ID;

        Self { span, metadata, id }
    }
}

pub(crate) trait Envelope: core::fmt::Debug {
    fn init_by<M>(t: &Token<M>) -> Self;
}

impl Envelope for () {
    fn init_by<M>(_t: &Token<M>) -> Self {
        ()
    }
}

pub struct PackToken<H: Envelope, M> {
    h: H,
    token: Token<M>,
}

impl<H: Envelope, M> core::fmt::Debug for PackToken<H, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PackToken")
            .field("header", &self.h)
            .field("token", &self.token)
            .finish()
    }
}

impl<H: Envelope, M> PackToken<H, M> {
    #[inline]
    pub fn unpack_with(self) -> (Token<M>, H) {
        let Self { h, token } = self;
        (token, h)
    }

    #[inline]
    pub fn unpack(self) -> Token<M> {
        self.token
    }

    #[inline]
    fn from_token(t: Token<M>) -> Self {
        Self {
            h: H::init_by(&t),
            token: t,
        }
    }

    #[inline]
    fn from_token_of<T: Message + ?Sized>(t: TokenOf<T, M>) -> Self {
        let t = Token::erase(t);
        Self::from_token(t)
    }
}
