use core::ops::Deref;

use crate::{
    boxed::PBox,
    mem::MemAllocator,
    numeric::Id,
    token::{AllocToken, PackToken},
};

pub mod type_id {
    pub type TypeId = u64;

    pub trait TypeTag {
        // `core::any::TypeId` can't be same across different compilation.
        // Thus choose manual definition strategy.
        const TYPE_ID: TypeId;
    }

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
        bool,
        ()
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
        use super::{TypeTag, combine, type_id};

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

pub trait TransitMove: MoveMessage {
    type Portable<A: MemAllocator>;
    fn tokens<A: MemAllocator>(self, alloc: A) -> (Self::Portable<A>, A);
    fn detokens<A: MemAllocator>(repr: Self::Portable<A>, alloc: A) -> Option<(Self, A)>
    where
        Self: Sized;
}

pub trait Envelope {}

impl Envelope for () {}

pub trait Tag<T>: Envelope {
    fn with_tag(self, value: T) -> Self
    where
        Self: Sized;
    fn tag(&self) -> T;
}

pub trait TagId: Envelope {
    fn with_id(self, value: Id) -> Self
    where
        Self: Sized;
    fn id(&self) -> Id;
}

pub trait TagRef<T>: Envelope {
    fn with_tag_in(&mut self, value: T);
    fn tag_ref(&self) -> &T;
}

pub struct Operation<T: Envelope> {
    id: Id,
    header: T,
}

impl<T: Envelope> Deref for Operation<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.header
    }
}

impl<T: Envelope> Envelope for Operation<T> {}

impl<T: Envelope> TagId for Operation<T> {
    fn with_id(self, value: Id) -> Self
    where
        Self: Sized,
    {
        Self { id: value, ..self }
    }
    fn id(&self) -> Id {
        self.id
    }
}

impl<T: Tag<H>, H> Tag<H> for Operation<T> {
    fn with_tag(self, value: H) -> Self
    where
        Self: Sized,
    {
        Self {
            id: self.id,
            header: self.header.with_tag(value),
        }
    }

    fn tag(&self) -> H {
        self.header.tag()
    }
}

impl<T: Envelope> Operation<T> {
    pub fn header(&self) -> &T {
        &self.header
    }

    pub fn compose<M>(id: Id, token: PackToken<T, M>) -> PackToken<Operation<T>, M> {
        let (token, header) = token.into_parts();
        let header = Self { header, id };
        token.pack(header)
    }
    pub fn decompose<M>(token: PackToken<Operation<T>, M>) -> (Id, PackToken<T, M>) {
        let (token, header) = token.into_parts();
        let Self { id, header } = header;
        (id, token.pack(header))
    }
}
