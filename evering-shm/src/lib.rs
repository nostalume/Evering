#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![cfg_attr(feature = "nightly", feature(allocator_api))]
#![feature(const_index,const_trait_impl)]
#![feature(ptr_as_uninit)]

extern crate alloc;

pub mod os;
pub mod shm_alloc;
pub mod shm_area;
pub mod shm_box;
pub mod shm_header;
mod tests;
