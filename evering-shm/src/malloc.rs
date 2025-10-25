use core::alloc::Layout;
use core::mem::{self};
use core::{ptr, ptr::NonNull};

#[cfg(feature = "nightly")]
pub use alloc::alloc::AllocError;
#[cfg(not(feature = "nightly"))]
pub use allocator_api2::alloc::{AllocError, handle_alloc_error};

use memory_addr::MemoryAddr;

use crate::area::MemBlkOps;
use crate::seal::Sealed;

pub trait MemAllocInfo: MemBlkOps {
    type Header;
    fn header(&self) -> &Self::Header;
    fn allocated(&self) -> usize;
    fn remained(&self) -> usize;
    fn discarded(&self) -> usize;
    unsafe fn alloc_ptr(&self) -> *const u8 {
        self.get_ptr(mem::size_of::<Self::Header>().align_up(mem::align_of::<Self::Header>()))
    }
    unsafe fn alloc_mut_ptr(&self) -> *mut u8 {
        self.get_mut_ptr(mem::size_of::<Self::Header>().align_up(mem::align_of::<Self::Header>()))
    }
}

/// Allocate or deallocate raw type `T` in persistence.
pub unsafe trait MemAlloc<T> : MemBlkOps {
    type Error;
    fn base_ptr(&self) -> *const u8;
    fn malloc_by(&self, layout: Layout) -> Result<T, Self::Error>;
    fn malloc_of<H>(&self) -> Result<T, Self::Error> {
        let layout = Layout::new::<H>();
        self.malloc_by(layout)
    }
    fn malloc_bytes(&self, size: usize) -> Result<T, Self::Error> {
        let layout = Layout::array::<u8>(size).unwrap();
        self.malloc_by(layout)
    }
}

pub unsafe trait MemDealloc<T>: MemAlloc<T> {
    // deallocate meta `T` in persistence.
    fn demalloc(&self, meta: T) -> bool;
}

pub unsafe trait MemDeallocBy<T>: MemAlloc<T> {
    fn demalloc_by(&self, meta: T, layout: Layout) -> bool;
}

unsafe impl<A: MemAlloc<T>, T> MemAlloc<T> for &A {
    type Error = A::Error;

    fn base_ptr(&self) -> *const u8 {
        (*self).base_ptr()   
    }
    fn malloc_by(&self, layout: Layout) -> Result<T, Self::Error> {
        (*self).malloc_by(layout)
    }
}

unsafe impl<A: MemDealloc<T>, T> MemDealloc<T> for &A {
    fn demalloc(&self, meta: T) -> bool {
        (*self).demalloc(meta)
    }
}

unsafe impl<A: MemDeallocBy<T>, T> MemDeallocBy<T> for &A {
    fn demalloc_by(&self, meta: T, layout: Layout) -> bool {
        (*self).demalloc_by(meta, layout)
    }
}