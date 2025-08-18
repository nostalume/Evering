#![cfg(feature = "nightly")]

use alloc::alloc::Allocator;

use core::alloc::Layout;
use core::ptr::NonNull;
use good_memory_allocator::SpinLockedAllocator as GmaSpinAllocator;

use crate::seal::Sealed;
use crate::shm_alloc::ShmInit;
use crate::{IAllocator,AllocError};

pub struct SpinGma(GmaSpinAllocator);

impl SpinGma {
    pub unsafe fn raw_new(start: usize, size: usize) -> Self {
        let alloc = GmaSpinAllocator::empty();
        unsafe { alloc.init(start, size) };
        Self(alloc)
    }
}

impl Sealed for SpinGma {}

unsafe impl IAllocator for SpinGma {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.0.allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        unsafe {
            self.0.deallocate(ptr, layout);
        }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, alloc::alloc::AllocError> {
        unsafe { self.0.grow(ptr, old_layout, new_layout) }
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, alloc::alloc::AllocError> {
        unsafe { self.0.shrink(ptr, old_layout, new_layout) }
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, alloc::alloc::AllocError> {
        self.0.allocate_zeroed(layout)
    }
}

unsafe impl ShmInit for SpinGma {
    unsafe fn init_addr(start: usize, size: usize) -> Self {
        unsafe { Self::raw_new(start, size) }
    }

    fn init_ptr(blk: NonNull<[u8]>) -> Self {
        unsafe { Self::init_addr(blk.as_ptr().addr(), blk.len()) }
    }
}