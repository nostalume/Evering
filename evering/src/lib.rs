#![feature(associated_type_defaults)]
#![cfg_attr(not(any(test, feature = "std")), no_std)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub mod driver;
pub mod uring;
