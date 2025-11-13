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
#![feature(
    sized_type_properties,
    layout_for_ptr,
    ptr_metadata,
    slice_ptr_get,
    get_mut_unchecked,
    unsafe_cell_access
)]

use core::ptr::NonNull;

pub use crate::arena::{Optimistic, Pessimistic};

use crate::{
    area::{AddrSpec, MemBlkHandle, Mmap},
    arena::{ARENA_MAX_CAPACITY, Strategy, UInt, max_bound},
    numeric::CastInto,
};

extern crate alloc;

#[cfg(feature = "tracing")]
extern crate tracing;

mod area;
mod arena;
pub mod boxed;
mod header;
mod malloc;
mod msg;
pub mod os;
mod queue;
mod reg;
mod tests;

fn tracing_init() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .try_init();
}

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

#[derive(Clone)]
pub struct ArenaMemBlk<S: AddrSpec, M: Mmap<S>, G: Strategy> {
    m: MemBlkHandle<S, M>,
    header: NonNull<crate::area::Header>,
    alloc: NonNull<crate::arena::Header<G>>,
    size: UInt,
}

impl<S: AddrSpec, M: Mmap<S>, G: Strategy> ArenaMemBlk<S, M, G> {
    pub fn header(&self) -> &crate::area::Header {
        unsafe { self.header.as_ref() }
    }

    fn alloc_header(&self) -> &crate::arena::Header<G> {
        unsafe { self.alloc.as_ref() }
    }

    pub fn arena(&self) -> crate::arena::Arena<'_, G> {
        let cfg = crate::arena::Config::default();
        crate::arena::Arena::from_header(self.alloc_header(), self.size, cfg)
    }

    pub fn init(
        bk: M,
        start: Option<S::Addr>,
        size: usize,
        flags: S::Flags,
        mcfg: M::Config,
    ) -> Result<Self, area::Error<S, M>> {
        max_bound(size).ok_or(area::Error::OutofSize {
            requested: size,
            bound: ARENA_MAX_CAPACITY.cast_into(),
        })?;

        let area = bk
            .map(start, size, flags, mcfg)
            .map_err(area::Error::MapError)?;
        unsafe {
            let (header, hoffset) = area.init_header::<crate::area::Header>(0, ())?;
            let (alloc, aoffset) = area.init_header::<crate::arena::Header<G>>(
                hoffset,
                crate::arena::Header::<G>::MIN_SEGMENT_SIZE,
            )?;
            let size = (area.size() - aoffset) as UInt;

            Ok(Self {
                m: area.into(),
                header,
                alloc,
                // Safety: Previous arithmetic check
                size,
            })
        }
    }
}

#[derive(Clone)]
struct RegMemBlk<S: AddrSpec, M: Mmap<S>, T, const N: usize> {
    m: MemBlkHandle<S, M>,
    header: NonNull<crate::area::Header>,
    reg: NonNull<crate::reg::Registry<T, N>>,
}

impl<S: AddrSpec, M: Mmap<S>, T, const N: usize> RegMemBlk<S, M, T, N> {
    pub fn header(&self) -> &crate::area::Header {
        unsafe { self.header.as_ref() }
    }

    pub fn reg(&self) -> &crate::reg::Registry<T, N> {
        unsafe { self.reg.as_ref() }
    }

    pub fn init(
        bk: M,
        start: Option<S::Addr>,
        size: usize,
        flags: S::Flags,
        mcfg: M::Config,
    ) -> Result<Self, area::Error<S, M>> {
        let area = bk
            .map(start, size, flags, mcfg)
            .map_err(area::Error::MapError)?;
        unsafe {
            let (header, hoffset) = area.init_header::<crate::area::Header>(0, ())?;
            let (reg, _) = area.init_header::<crate::reg::Registry<T, N>>(hoffset, ())?;

            Ok(Self {
                m: area.into(),
                header,
                reg,
            })
        }
    }
}
