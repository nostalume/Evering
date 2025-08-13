use alloc::alloc::Allocator;
use core::alloc::Layout;
use core::ptr::NonNull;
use good_memory_allocator::SpinLockedAllocator as GmaSpinAllocator;

use crate::align::align_up;
use crate::shm_alloc::ShmAllocator;
use crate::shm_alloc::ShmInit;

pub struct ShmGma(GmaSpinAllocator, usize);

impl ShmGma {
    pub unsafe fn raw_new(start: usize, size: usize) -> Self {
        let aligned_heap_start_addr = align_up(start, Self::MIN_ALIGNMENT);

        let alloc = GmaSpinAllocator::empty();
        unsafe { alloc.init(start, size) };
        Self(alloc, aligned_heap_start_addr)
    }
}

unsafe impl Allocator for ShmGma {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, alloc::alloc::AllocError> {
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

unsafe impl ShmAllocator for ShmGma {
    fn start_ptr(&self) -> *const u8 {
        self.1 as *const u8
    }
}

unsafe impl ShmInit for ShmGma {
    unsafe fn init_addr(start: usize, size: usize) -> Self {
        unsafe { Self::raw_new(start, size) }
    }

    fn init_ptr(blk: NonNull<[u8]>) -> Self {
        unsafe { Self::init_addr(blk.as_ptr().addr(), blk.len()) }
    }
}