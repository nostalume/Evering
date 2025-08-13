#![cfg(feature = "nightly")]

use alloc::alloc::{AllocError, Allocator};

use blink_alloc::SyncBlinkAlloc;
use core::alloc::Layout;
use core::ptr::NonNull;
use good_memory_allocator::SpinLockedAllocator as GmaSpinAllocator;

use crate::shm_alloc::ShmInit;

type GmaBlinkIn = SyncBlinkAlloc<GmaSpinAllocator>;

pub struct BlinkGma(GmaBlinkIn);

impl BlinkGma {
    pub unsafe fn raw_new(start: usize, size: usize) -> Self {
        let gma = GmaSpinAllocator::empty();
        unsafe { gma.init(start, size) };

        let blink = SyncBlinkAlloc::new_in(gma);
        Self(blink)
    }
}

unsafe impl Allocator for BlinkGma {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.0.allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let size = layout.size();
        unsafe { self.0.deallocate(ptr, size) }
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.0.shrink(ptr, old_layout, new_layout) }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.0.grow(ptr, old_layout, new_layout) }
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.0.grow_zeroed(ptr, old_layout, new_layout) }
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.0.allocate_zeroed(layout)
    }
}

unsafe impl ShmInit for BlinkGma {
    unsafe fn init_addr(start: usize, size: usize) -> Self {
        unsafe { BlinkGma::raw_new(start, size) }
    }
}
