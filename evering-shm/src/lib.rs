#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![feature(allocator_api)]
#![feature(ptr_as_uninit)]

extern crate alloc;

pub mod shm_alloc;
mod align;
pub mod shm_box;


