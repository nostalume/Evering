#![feature(local_waker)]
#![feature(associated_type_defaults)]
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod driver;
mod layout;
pub mod op;
mod queue;
pub mod resource;
pub mod uring;
