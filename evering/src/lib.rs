#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![cfg_attr(feature = "nightly", feature(allocator_api))]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub mod driver;
pub mod uring;

mod seal {
    pub trait Sealed {}
}
