#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![feature(allocator_api)]
#![feature(const_trait_impl, const_convert, const_cmp)]
#![feature(layout_for_ptr, slice_ptr_get, unsafe_cell_access)]

extern crate alloc;

#[cfg(feature = "tracing")]
extern crate tracing;

mod boxed;
mod channel;
mod dir;
mod header;
mod mem;
pub mod msg;
pub mod notify;
pub mod os;
pub mod perlude;
#[cfg(feature = "process")]
pub mod process;
#[cfg(feature = "tokio")]
pub mod runtime;
mod schema;
mod talc;
mod tests;
mod token;

/// A live value owned by the shared heap borrowed for `'a`.
///
/// The allocator implementation is deliberately absent from the public type.
///
/// ```compile_fail
/// use evering::boxed::PBox;
/// ```
///
/// ```compile_fail
/// # use evering::PBox;
/// # fn cannot_bypass_heap<'a, A>(allocator: A) {
/// let _ = PBox::<'a, u64>::new_in(7, allocator);
/// # }
/// ```
pub use boxed::PBox;
pub use channel::{
    Claim, ClaimError, QueueChannel, Receiver, ReserveError, Reserved, Sender, Staged,
    TryRecvError, TrySendError,
};
#[doc(hidden)]
pub use header::AdmitLayout;
pub use header::{Layout, Magic as LayoutMagic, RcHeader, Status as LayoutStatus};
pub use mem::{
    Error as MapError, LayoutField, Map, MapLayout, Mapped, OpenError, Peer, Recovery, Ref, Region,
    RegionCloseError, Request, Source,
};
pub use msg::{Encoded, Repr};
pub use notify::{Async, Done, Listen, Notify, Pending, RecvError, SendError};
pub use schema::{
    LayoutContext, LayoutId, LayoutInfo, RegionAdmission, RegionId, SchemaId, SchemaKey,
    SharedSchema,
};
pub use token::Shape;

mod seal {
    pub trait Sealed {}
}

mod numeric {
    pub mod bit {
        pub type Word = usize;
        pub type Bit = usize;
        pub const WORD_ALIGN: usize = core::mem::align_of::<Word>();
        pub const WORD_BITS: usize = Word::BITS as usize;

        #[inline]
        pub const fn bit_check(word: Word, bit: Bit) -> bool {
            ((word >> bit) & 1) != 0
        }

        #[inline]
        pub const fn bit_flip(word: &mut Word, bit: Bit) {
            *word ^= 1usize << bit;
        }
    }

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

    pub const trait Alignable {
        fn align_down(self, align: Self) -> Self;
        fn align_down_of<T>(self) -> Self;
        fn align_up(self, align: Self) -> Self;
        fn align_up_of<T>(self) -> Self;
    }

    macro_rules! align {
        ($ty:ty) => {
            impl const Alignable for $ty {
                #[inline(always)]
                fn align_down(self, align: Self) -> Self {
                    debug_assert!(align.is_power_of_two());
                    self & !(align - 1)
                }
                #[inline(always)]
                fn align_down_of<T>(self) -> Self {
                    let align = core::mem::align_of::<T>();
                    self.align_down(align as Self)
                }
                #[inline(always)]
                fn align_up(self, align: Self) -> Self {
                    debug_assert!(align.is_power_of_two());
                    debug_assert!(Self::MAX - self > align - 1, "align up overflow");
                    (self + align - 1) & !(align - 1)
                }
                #[inline(always)]
                fn align_up_of<T>(self) -> Self {
                    let align = core::mem::align_of::<T>();
                    self.align_up(align as Self)
                }
            }
        };
    }

    align!(usize);

    pub trait AlignPtr: Sized {
        fn align_up(self, align: usize) -> Self;
        #[inline]
        fn align_up_of<T>(self) -> Self {
            self.align_up(core::mem::align_of::<T>())
        }
    }

    impl AlignPtr for *const u8 {
        #[inline]
        fn align_up(self, align: usize) -> Self {
            debug_assert!(align.is_power_of_two());
            let addr = self.addr();
            debug_assert!(addr <= usize::MAX - (align - 1));
            ((addr + align - 1) & !(align - 1)) as *const u8
        }
    }

    impl AlignPtr for *mut u8 {
        #[inline]
        fn align_up(self, align: usize) -> Self {
            debug_assert!(align.is_power_of_two());
            let addr = self.addr();
            debug_assert!(addr <= usize::MAX - (align - 1));
            ((addr + align - 1) & !(align - 1)) as *mut u8
        }
    }
}

mod counter {
    use alloc::boxed::Box;
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

        pub unsafe fn release(&self) {
            if self.counter().counts.fetch_sub(1, Ordering::AcqRel) == 1 {
                drop(unsafe { Box::from_raw(self.counter) });
            }
        }
    }

    impl<T: core::fmt::Debug> core::fmt::Debug for CounterOf<T> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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
