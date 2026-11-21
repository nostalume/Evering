//! A bounded, typed shared-memory substrate: a pool hands out blocks, a directory records what a
//! block is, a channel moves one between members of a session, and a notification object says when
//! to look again. The region is the only transport, so every type above it is a promise about the
//! bytes in somebody else's mapping.
//!
//! # Where a name lives
//!
//! There are two maps, and a name belongs to exactly one of them.
//!
//! * **The crate root is what a caller holds across an exchange**: `Session`, `Observer`, `Pool`,
//!   `Channel`, `Port`, `Tx`/`Rx`, `Block`, `Transfer`, `PBox`, and the refusals of that same
//!   exchange — `BlockRangeError`, `ReceiveError`, `RemoveError`, `OpenPoolError`, `ObserverError`.
//!   A refusal that hands something of the caller's back answers with one shared vocabulary instead of
//!   a shape per module: `Decline`, `DeclineCause`, `RoomWait`. These come out of implementation
//!   modules that are private (`pool`, `channel`, `session`, `queue`, `boxed`), so the root is their
//!   only address, and where two modules use the same noun the root's alias settles it:
//!   `pool::RangeError` becomes `BlockRangeError`, `talc::Geometry` becomes `HeapGeometry`. Rustc
//!   enforces the private half — a type from a private module cannot appear in a public signature
//!   without being re-exported.
//! * **A `pub mod` is the map of one layer**, named by path and deliberately not duplicated at the
//!   root: `layout` for the identity a type claims (`SchemaKey`, `Repr`, `RegionId`,
//!   `recovery_handler`), `mapping` for how a region gets mapped (`Access`, `Request`, `Map`),
//!   `notify` for when to look again (`Signals`, `Wait`, `Patience`, `ProgressError`), `os` for the
//!   platform's objects, `process` and `runtime` for spawning and for driving a wait.
//!
//! So `evering::Pool` and `evering::notify::Signals` are not two styles of the same thing: the first
//! is a noun of the exchange, the second is a member of the waiting layer. A layer's own refusals
//! travel with it for the same reason — `notify::ProgressError` and `process::TooLarge` have no root
//! name. `layout` and `mapping` are re-export shims over private modules; they are public so a
//! vocabulary has one address, not because their implementation is reachable.
//!
//! Two nouns still mean more than one thing, and the rule names them rather than fixing them:
//! `Geometry` is both the pool's block-class layout and the heap's capacity (`HeapGeometry`), and
//! `Peer` is both `mapping::Peer` (a mapped peer region) and `os::Peer` (a process). The second is
//! open work, not a documentation choice.
//!
//! # What a module header says
//!
//! Three sentences, in this order: what fact or state the module owns, who may change it and under
//! which protocol, and the one obligation a change here could break that no test would catch. The
//! layer narrative lives in `docs/architecture.md`; a header does not retell it.

#![cfg_attr(not(any(test, feature = "std")), no_std)]

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
    AdoptError, Channel, Id as ChannelId, Port, PublishError, ReceiveError, Received, Rx,
    TransferReserved, TransferStaged, Tx,
};
/// Why a request was declined, and the two tables that say what would change the answer.
///
/// One shape covers every authority that answers no while still holding something of the caller's — a
/// block request, a queue reservation, a heap mutation — because the reporting layer must not depend
/// on what the payload can render.
pub use mem::{Decline, DeclineCause, RoomWait};
/// A standpoint on a region, held by a process that reads it and never joined it.
///
/// Neither the mapping it reads through nor a member's authority is reachable from it, which is why a
/// reader cannot commit anything by accident — the refusal is a missing method, not a runtime error.
///
/// ```compile_fail
/// # use evering::Observer;
/// # fn cannot_hand_out_the_mapping(observer: &Observer) { let _map = observer.map(); }
/// ```
///
/// ```compile_fail
/// # use evering::Observer;
/// # fn cannot_speak_for_a_member(observer: &Observer) { let _peer = observer.peer(); }
/// ```
pub use mem::{Observer, ObserverError};
pub use msg::Encoded;
pub use pool::{
    Block, BlockRange, ClassInfo, Pool, PoolCreateError, PoolId, PoolRef,
    RangeError as BlockRangeError, Transfer, Vacant,
};
pub use session::{
    AdmitError, ChannelCreateError, GeneralHeap, OpenPoolError, Recovery, RemoveError, Seat,
    SeatError, Session, SessionError, SessionOptions,
};
pub use talc::{Geometry as HeapGeometry, GeometryError as HeapGeometryError};

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

    // `usize::min` and `usize::max` are only conditionally const, so a `const fn` cannot reach them
    // without a feature gate; these do the same thing and are const on their own.
    pub const fn min_usize(a: usize, b: usize) -> usize {
        if a < b { a } else { b }
    }

    pub const fn max_usize(a: usize, b: usize) -> usize {
        if a > b { a } else { b }
    }

    pub trait Alignable {
        fn align_down(self, align: Self) -> Self;
        fn align_down_of<T>(self) -> Self;
        fn align_up(self, align: Self) -> Self;
        fn align_up_of<T>(self) -> Self;
    }

    macro_rules! align {
        ($ty:ty) => {
            impl Alignable for $ty {
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
