#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![cfg_attr(feature = "nightly", feature(allocator_api))]
#![feature(const_trait_impl)]
#![feature(const_index, slice_ptr_get, sized_type_properties, layout_for_ptr)]

extern crate alloc;

mod area;
pub mod boxed;
mod header;
mod malloc;
pub mod os;
pub mod perlude;
mod tests;

mod seal {
    pub trait Sealed {}
}
