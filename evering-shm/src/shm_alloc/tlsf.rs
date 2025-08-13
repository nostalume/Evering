use alloc::alloc::AllocError;
use alloc::alloc::Allocator;
use core::alloc::Layout;
use core::mem::MaybeUninit;
use core::ptr;
use core::ptr::NonNull;
use rlsf::Tlsf;

use crate::shm_alloc::ShmInit;

type MyTlsf<'a> = Tlsf<'a, u32, u32, 24, 8>;
pub struct SpinTlsf<'a>(spin::Mutex<MyTlsf<'a>>);

impl<'a> SpinTlsf<'a> {
    pub unsafe fn raw_new(start: usize, size: usize) -> Self {
        let s = Self(spin::Mutex::new(MyTlsf::new()));
        let blk = unsafe {
            NonNull::new_unchecked(ptr::slice_from_raw_parts_mut(start as *mut u8, size))
        };
        unsafe { s.0.lock().insert_free_block_ptr(blk) };
        s
    }
    pub fn new(blk: NonNull<[u8]>) -> Self {
        let s = Self(spin::Mutex::new(MyTlsf::new()));
        unsafe { s.0.lock().insert_free_block_ptr(blk) };
        s
    }

    pub fn new_blk(blk: &'a mut [MaybeUninit<u8>]) -> Self {
        let s = Self(spin::Mutex::new(MyTlsf::new()));
        s.0.lock().insert_free_block(blk);
        s
    }
}

unsafe impl<'a> Allocator for SpinTlsf<'a> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if let Some(alloc) = self.0.lock().allocate(layout).map(|p| unsafe {
            NonNull::new_unchecked(ptr::slice_from_raw_parts_mut(p.as_ptr(), layout.size()))
        }) {
            Ok(alloc)
        } else {
            Err(AllocError)
        }
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let align = layout.align();
        unsafe { self.0.lock().deallocate(ptr, align) };
    }
}

unsafe impl<'a> ShmInit for SpinTlsf<'a> {
    unsafe fn init_addr(start: usize, size: usize) -> Self {
        unsafe { Self::raw_new(start, size) }
    }

    fn init_ptr(blk: NonNull<[u8]>) -> Self {
        Self::new(blk)
    }
}