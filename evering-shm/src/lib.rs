#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![cfg_attr(feature = "nightly", feature(allocator_api))]
#![feature(
    const_trait_impl,
    const_convert,
    const_try,
    const_index,
    const_result_trait_fn,
    const_option_ops
)]
#![feature(sized_type_properties, layout_for_ptr, ptr_metadata, slice_ptr_get)]

extern crate alloc;

#[cfg(feature = "tracing")]
extern crate tracing;

mod area;
mod arena;
pub mod boxed;
mod header;
mod malloc;
pub mod os;
mod queue;
mod msg;
mod reg;
mod tests;

mod seal {
    pub trait Sealed {}
}

mod numeric {
    #[const_trait]
    pub trait CastFrom<T> {
        fn cast_from(t: T) -> Self;
    }

    #[const_trait]
    pub trait CastInto<T> {
        fn cast_into(self) -> T;
    }

    impl<T, U> const CastInto<U> for T
    where
        U: const CastFrom<T>,
    {
        #[inline]
        fn cast_into(self) -> U {
            U::cast_from(self)
        }
    }

    macro_rules! cast {
        (from:$from:ty, to:$to:ty) => {
            paste::paste! {
                impl const crate::numeric::CastFrom<$from> for $to {
                    #[inline]
                    fn cast_from(t: $from) -> Self {
                        t as $to
                    }
                }
            }
        };
    }

    #[cfg(target_pointer_width = "32")]
    mod target32 {
        cast!(from:usize, to:u64);
        cast!(from:usize, to:u32);
        cast!(from:usize, to:usize);
        cast!(from:u32, to:usize);
        cast!(from:u32, to:u64);
    }
    #[cfg(target_pointer_width = "32")]
    pub use target32::*;

    #[cfg(target_pointer_width = "64")]
    mod target64 {
        cast!(from:usize, to:u128);
        cast!(from:usize, to:u64);
        cast!(from:usize, to:usize);
        cast!(from:u64, to:usize);
        cast!(from:u32, to:usize);
        cast!(from:u32, to:u64);
    }

    #[const_trait]
    pub trait Alignable {
        fn size_of<T>() -> Self;
        fn align_of<T>() -> Self;
        fn align_down(self, other: Self) -> Self;
        fn align_down_of<T>(self) -> Self;
        fn align_up(self, other: Self) -> Self;
        fn align_up_of<T>(self) -> Self;
        fn align_offset(self, other: Self) -> Self;
        fn align_offset_of<T>(self) -> Self;
        fn is_aligned(self, other: Self) -> bool;
        fn is_aligned_of<T>(self) -> bool;
    }

    macro_rules! align {
        ($ty:ty) => {
            impl const crate::numeric::Alignable for $ty {
                #[inline(always)]
                fn size_of<T>() -> Self {
                    let size = core::mem::size_of::<T>();
                    assert!(size <= Self::MAX as usize, "size_of::<T>() is too large");
                    size as Self
                }
                #[inline(always)]
                fn align_of<T>() -> Self {
                    let size = core::mem::align_of::<T>();
                    assert!(size <= Self::MAX as usize, "size_of::<T>() is too large");
                    size as Self
                }
                #[inline(always)]
                fn align_down(self, other: Self) -> Self {
                    self & !(other - 1)
                }
                #[inline(always)]
                fn align_down_of<T>(self) -> Self {
                    let align = core::mem::align_of::<T>();
                    assert!(align <= Self::MAX as usize, "align_of::<T>() is too large");
                    self.align_down(align as Self)
                }
                #[inline(always)]
                fn align_up(self, other: Self) -> Self {
                    (self + other - 1) & !(other - 1)
                }
                #[inline(always)]
                fn align_up_of<T>(self) -> Self {
                    let align = core::mem::align_of::<T>();
                    assert!(align <= Self::MAX as usize, "align_of::<T>() is too large");
                    self.align_up(align as Self)
                }
                #[inline(always)]
                fn align_offset(self, other: Self) -> Self {
                    self & (other - 1)
                }
                fn align_offset_of<T>(self) -> Self {
                    let align = core::mem::align_of::<T>();
                    assert!(align <= Self::MAX as usize, "align_of::<T>() is too large");
                    self.align_offset(align as Self)
                }
                #[inline(always)]
                fn is_aligned(self, other: Self) -> bool {
                    self.align_offset(other) == 0
                }
                #[inline(always)]
                fn is_aligned_of<T>(self) -> bool {
                    let align = core::mem::align_of::<T>();
                    assert!(align <= Self::MAX as usize, "align_of::<T>() is too large");
                    self.is_aligned(align as Self)
                }
            }
        };
    }

    align!(usize);
    align!(u64);
    align!(u32);
    align!(u16);
    align!(u8);

    #[const_trait]
    pub trait Packable: Sized {
        type Packed;

        fn pack(first: Self, second: Self) -> Self::Packed;
        fn unpack(packed: Self::Packed) -> (Self, Self);
    }

    macro_rules! pack_bits {
        (unpack:$unpack:ty, pack:$pack:ty) => {
            impl const crate::numeric::Packable for $unpack {
                type Packed = $pack;

                fn pack(first: Self, second: Self) -> Self::Packed {
                    const BITS: u32 = <$unpack>::BITS;
                    ((first as Self::Packed) << BITS) | (second as Self::Packed)
                }

                fn unpack(packed: Self::Packed) -> (Self, Self) {
                    const BITS: u32 = <$unpack>::BITS;
                    let first = (packed >> BITS) as Self;
                    let second = (packed & ((1 << BITS) - 1)) as Self;
                    (first, second)
                }
            }
        };
    }

    pack_bits!(unpack:u16, pack:u32);
    pack_bits!(unpack:u32, pack:u64);
    pack_bits!(unpack:u64, pack:u128);
}
