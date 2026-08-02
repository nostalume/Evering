#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![cfg_attr(test, feature(allocator_api))]
#![feature(const_trait_impl, const_convert, const_cmp)]
#![feature(layout_for_ptr, slice_ptr_get, unsafe_cell_access)]

extern crate alloc;

#[cfg(feature = "tracing")]
extern crate tracing;

mod boxed;
mod channel;
mod dir;
mod header;
pub mod layout;
pub mod mapping;
mod mem;
mod msg;
pub mod notify;
pub mod os;
mod pool;
#[cfg(feature = "process")]
pub mod process;
mod queue;
#[cfg(feature = "tokio")]
pub mod runtime;
mod schema;
mod session;
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
///
/// A transfer locator is deliberately not a public reconstruction capability.
/// ```compile_fail
/// use evering::Token;
/// ```
pub use boxed::PBox;
pub use channel::{
    AdoptError, Channel, Id as ChannelId, Port, ReceiveError, Received, Rx, SendReserveError,
    TransferReserved, TransferStaged, TrySendError, Tx,
};
pub use msg::Encoded;
pub use pool::{
    Block, BlockRange, ClassInfo, Pool, PoolCreateError, PoolId, PoolRef,
    RangeError as BlockRangeError, ReserveError as PoolReserveError, Transfer, Vacant,
};
pub use session::{
    AdmitError, ChannelCreateError, GeneralHeap, OpenPoolError, PutError, Recovery, RemoveError,
    Session, SessionError, SessionOptions,
};
pub use talc::{
    Geometry as HeapGeometry, GeometryError as HeapGeometryError, MutationError as GeneralHeapError,
};

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
