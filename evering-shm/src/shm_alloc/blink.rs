use alloc::alloc::AllocError;
use alloc::alloc::Allocator;
use blink_alloc::SyncBlinkAlloc;
use core::alloc::Layout;
use core::ptr::NonNull;
use good_memory_allocator::SpinLockedAllocator as GmaSpinAllocator;

use crate::align::align_up;
use crate::shm_alloc::gma::ShmGma;
use crate::shm_alloc::ShmAllocator;
use crate::shm_alloc::ShmInit;

type GmaBlink = SyncBlinkAlloc<GmaSpinAllocator>;

pub struct ShmGmaBlink(GmaBlink, usize, usize);

impl ShmGmaBlink {
    pub unsafe fn raw_new(start: usize, size: usize) -> Self {
        let aligned_start = align_up(start, ShmGma::MIN_ALIGNMENT);
        let gma = GmaSpinAllocator::empty();
        unsafe { gma.init(aligned_start, size) };

        let blink = SyncBlinkAlloc::new_in(gma);
        
        Self(blink, aligned_start, size)
    }
}

unsafe impl Allocator for ShmGmaBlink {
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

unsafe impl ShmAllocator for ShmGmaBlink {
    fn start_ptr(&self) -> *const u8 {
        self.1 as *const u8
    }
}

unsafe impl ShmInit for ShmGmaBlink {
    unsafe fn init_addr(start: usize, size: usize) -> Self {
        unsafe { ShmGmaBlink::raw_new(start, size) }
    }
}
