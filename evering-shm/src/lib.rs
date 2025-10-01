#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![cfg_attr(feature = "nightly", feature(allocator_api))]
#![feature(const_trait_impl)]
#![feature(sized_type_properties, layout_for_ptr)]
#![feature(const_index, slice_ptr_get)]

extern crate alloc;

mod area;
mod arena;
mod header;
pub mod boxed;
mod malloc;
pub mod os;
pub mod perlude;
mod tests;

mod seal {
    pub trait Sealed {}
}
