#![feature(local_waker)]
#![feature(associated_type_defaults)]
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod driver;
pub mod resource;
pub mod uring;
mod uring_array;
mod uring_depr;
