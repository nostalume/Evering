#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![feature(allocator_api)]
#![feature(
    const_trait_impl,
    const_convert,
    const_try,
    const_index,
    const_result_trait_fn,
    const_option_ops,
    const_cmp
)]
#![feature(
    sized_type_properties,
    layout_for_ptr,
    ptr_metadata,
    slice_ptr_get,
    get_mut_unchecked,
    unsafe_cell_access
)]

extern crate alloc;

#[cfg(feature = "tracing")]
extern crate tracing;

mod arena;
pub mod boxed;
mod channel;
mod header;
mod mem;
pub mod msg;
pub mod os;
pub mod perlude;
mod reg;
mod tests;
mod token;

mod seal {
    pub trait Sealed {}
}

mod numeric {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[repr(C)]
    pub struct Id {
        pub idx: usize,
        pub live: u32,
    }

    impl Id {
        pub const HEAD: usize = 0;
        pub const NONE: usize = usize::MAX;
        pub const fn null() -> Self {
            Self {
                idx: Self::NONE,
                live: 0,
            }
        }

        pub const fn is_null(&self) -> bool {
            self.idx == Self::NONE
        }
    }

    pub const trait CastFrom<T> {
        fn cast_from(t: T) -> Self;
    }

    pub const trait CastInto<T> {
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

    pub const trait Alignable {
        fn size_of<T>() -> Self;
        fn align_of<T>() -> Self;
        fn align_down(self, other: Self) -> Self;
        fn align_down_of<T>(self) -> Self;
        fn align_up(self, other: Self) -> Self;
        fn align_up_of<T>(self) -> Self;
        fn align_offset(&self, other: &Self) -> Self;
        fn align_offset_of<T>(&self) -> Self;
        fn is_aligned(&self, other: &Self) -> bool;
        fn is_aligned_of<T>(&self) -> bool;
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
                fn align_offset(&self, other: &Self) -> Self {
                    *self & (*other - 1)
                }
                fn align_offset_of<T>(&self) -> Self {
                    let align = core::mem::align_of::<T>();
                    assert!(align <= Self::MAX as usize, "align_of::<T>() is too large");
                    self.align_offset(&(align as Self))
                }
                #[inline(always)]
                fn is_aligned(&self, other: &Self) -> bool {
                    self.align_offset(other) == 0
                }
                #[inline(always)]
                fn is_aligned_of<T>(&self) -> bool {
                    let align = core::mem::align_of::<T>();
                    assert!(align <= Self::MAX as usize, "align_of::<T>() is too large");
                    self.is_aligned(&(align as Self))
                }
            }
        };
    }

    align!(usize);
    align!(u64);
    align!(u32);
    align!(u16);
    align!(u8);

    pub const trait Packable: Sized {
        type Packed;

        fn pack(first: Self, second: Self) -> Self::Packed;
        fn unpack(packed: Self::Packed) -> (Self, Self);
    }

    pub const trait AtomicPackable: Sized + Packable {
        type AtomicSelf;
        type AtomicPacked;
    }

    pub type Pack<T> = <T as Packable>::Packed;
    pub type Atomic<T> = <T as AtomicPackable>::AtomicSelf;
    pub type AtomicPack<T> = <T as AtomicPackable>::AtomicPacked;

    macro_rules! pack_bits {
        (unpack:$unpack:ty, pack:$pack:ty) => {
            impl const Packable for $unpack {
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

    macro_rules! atomic_pack_bits {
        (unpack:$unpack:ty, atomic:$atomic:ty, atomic_pack:$atomic_pack:ty) => {
            impl const AtomicPackable for $unpack {
                type AtomicSelf = $atomic;
                type AtomicPacked = $atomic_pack;
            }
        };
    }

    use core::sync::atomic;

    pack_bits!(unpack:u16, pack:u32);
    atomic_pack_bits!(unpack:u16, atomic: atomic::AtomicU16, atomic_pack: atomic::AtomicU32);
    pack_bits!(unpack:u32, pack:u64);
    #[cfg(all(target_has_atomic = "32", target_has_atomic = "64"))]
    atomic_pack_bits!(unpack:u32, atomic: atomic::AtomicU32, atomic_pack: atomic::AtomicU64);
    pack_bits!(unpack:u64, pack:u128);
    #[cfg(all(target_has_atomic = "64", target_has_atomic = "128"))]
    atomic_pack_bits!(unpack:u64, atomic: atomic::AtomicU64, atomic_pack: atomic::AtomicU128);
}

mod counter {
    use core::{
        ops::Deref,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Counter<T> {
        counts: AtomicUsize,
        data: T,
    }

    pub struct CounterOf<T> {
        counter: *mut Counter<T>,
    }

    unsafe impl<T: Send> Send for CounterOf<T> {}
    unsafe impl<T: Sync> Sync for CounterOf<T> {}

    impl<T> CounterOf<T> {
        pub fn suspend(data: T) -> Self {
            let counter = Box::into_raw(Box::new(Counter {
                counts: AtomicUsize::new(1),
                data,
            }));
            Self { counter }
        }

        const fn counter(&self) -> &Counter<T> {
            unsafe { &*self.counter }
        }

        const fn data(&self) -> &mut T {
            let counter = unsafe { &mut *self.counter };
            &mut counter.data
        }

        pub fn acquire(&self) -> Self {
            let count = self.counter().counts.fetch_add(1, Ordering::Relaxed);

            // Cloning senders and calling `mem::forget` on the clones could potentially overflow the
            // counter. It's very difficult to recover sensibly from such degenerate scenarios so we
            // just abort when the count becomes very large.
            if count > isize::MAX as usize {
                core::panic!("counts exceed `isize::MAX`")
            }

            Self {
                counter: self.counter,
            }
        }

        pub unsafe fn release_by<F: FnOnce(*mut T)>(&self, dispose: F) {
            if self.counter().counts.fetch_sub(1, Ordering::AcqRel) == 1 {
                dispose(self.data());
                drop(unsafe { Box::from_raw(self.counter) });
            }
        }

        pub unsafe fn release(&self) {
            if self.counter().counts.fetch_sub(1, Ordering::AcqRel) == 1 {
                drop(unsafe { Box::from_raw(self.counter) });
            }
        }
    }

    impl<T: core::fmt::Debug> core::fmt::Debug for CounterOf<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            core::fmt::Debug::fmt(&**self, f)
        }
    }

    impl<T> const Deref for CounterOf<T> {
        type Target = T;

        fn deref(&self) -> &T {
            &self.counter().data
        }
    }

    impl<T> PartialEq for CounterOf<T> {
        fn eq(&self, other: &CounterOf<T>) -> bool {
            self.counter == other.counter
        }
    }
}
