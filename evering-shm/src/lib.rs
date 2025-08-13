#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![feature(allocator_api)]
#![feature(ptr_as_uninit)]

extern crate alloc;

use alloc::alloc::AllocError;
use alloc::alloc::Allocator;
use core::alloc::Layout;
use core::mem::MaybeUninit;
use core::ptr;
use core::ptr::NonNull;
use good_memory_allocator::SpinLockedAllocator;
use rlsf::Tlsf;

use crate::align::align_up;

mod align;
pub mod shm_box;

unsafe trait ShmAllocator: Allocator {
    /// Returns the number of bytes that are reserved by the allocator.
    // fn reserved(&self) -> usize;
    // /// Returns the data offset of the allocator. The offset is the end of the reserved bytes of the allocator.
    // fn reserved_offset(&self) -> usize;
    // /// Returns the number of bytes allocated by the allocator.
    // fn allocated(&self) -> usize;
    // /// Returns the offset of the allocator, where the offset is the end of reserved bytes of allocator.
    // #[inline]
    // fn allocated_slice(&self) -> &[u8] {
    //     unsafe {
    //         let offset = self.reserved_offset();
    //         let ptr = self.raw_ptr().add(offset);
    //         let allocated = self.allocated();
    //         core::slice::from_raw_parts(ptr, allocated - offset)
    //     }
    // }
    //
    /// Returns the start pointer of the main memory of the allocator.
    ///
    /// ## Default
    /// Returns `self as *const Self as *const u8`.
    fn start_ptr(&self) -> *const u8;
    /// Returns the start pointer of the main memory of the allocator.
    unsafe fn start_mut_ptr(&self) -> *mut u8 {
        self.start_ptr().cast_mut()
    }

    /// Returns the offset to the start of the allocator.
    ///
    /// ## Safety
    /// - `ptr` must be allocated by this allocator.
    unsafe fn offset<T>(&self, ptr: *const T) -> isize {
        // Safety: `ptr` must has address greater than `self.raw_ptr()`.
        unsafe { ptr.byte_offset_from(self.start_ptr()) }
    }

    /// Returns a pointer to the memory at the given offset.
    #[inline]
    fn get_ptr(&self, offset: isize) -> *const u8 {
        unsafe {
            if offset == 0 {
                return self.start_ptr();
            }
            if offset < 0 {
                return self.start_ptr().sub(-offset as usize);
            }
            self.start_ptr().add(offset as usize)
        }
    }

    /// Returns a pointer to the memory at the given offset.
    #[inline]
    fn get_ptr_mut(&self, offset: isize) -> *mut u8 {
        unsafe {
            if offset == 0 {
                return self.start_mut_ptr();
            }
            if offset < 0 {
                return self.start_mut_ptr().sub(-offset as usize);
            }
            self.start_mut_ptr().add(offset as usize)
        }
    }
    /// Returns an aligned pointer to the memory at the given offset.
    ///
    /// ## Safety
    /// - `offset..offset + mem::size_of::<T>() + padding` must be allocated memory.
    #[inline]
    unsafe fn get_aligned_ptr<T>(&self, offset: isize) -> *const T {
        
        self.get_ptr(offset).cast()
    }
    /// Returns an aligned pointer to the memory at the given offset.
    ///
    /// ## Safety
    /// - `offset..offset + mem::size_of::<T>() + padding` must be allocated memory.
    #[inline]
    unsafe fn get_aligned_ptr_mut<T>(&self, offset: isize) -> NonNull<T> {
        unsafe {
            if offset == 0 {
                return NonNull::dangling();
            }

            let ptr = self.get_ptr_mut(offset).cast();
            NonNull::new_unchecked(ptr)
        }
    }
}

unsafe impl<A:ShmAllocator> ShmAllocator for &A {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        (**self).start_ptr()
    }
}

unsafe trait ShmInit : ShmAllocator {
    const USIZE_ALIGNMENT: usize = core::mem::align_of::<usize>();
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();

    // IMPORTANT:
    // `MIN_ALIGNMENT` must be larger than 4, so that storing the size as a
    // `DivisibleBy4Usize` is safe.
    const MIN_ALIGNMENT: usize = if Self::USIZE_ALIGNMENT < 8 {
        8
    } else {
        Self::USIZE_ALIGNMENT
    };
    unsafe fn init_addr(start: usize, size: usize) -> Self;

    #[inline]
    fn init_ptr(blk: NonNull<[u8]>) -> Self
    where
        Self: Sized,
    {
        unsafe { Self::init_addr(blk.as_ptr().addr(), blk.len()) }
    }
}
struct ShmHeap(SpinLockedAllocator, usize);

impl ShmHeap {
    pub unsafe fn raw_new(start: usize, size: usize) -> Self {
        let aligned_heap_start_addr = align_up(start, Self::MIN_ALIGNMENT);

        let alloc = SpinLockedAllocator::empty();
        unsafe { alloc.init(start, size) };
        Self(alloc, aligned_heap_start_addr)
    }
}

unsafe impl Allocator for ShmHeap {
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

unsafe impl ShmAllocator for ShmHeap {
    fn start_ptr(&self) -> *const u8 {
        self.1 as *const u8
    }
}

unsafe impl ShmInit for ShmHeap {
    unsafe fn init_addr(start: usize, size: usize) -> Self {
        unsafe { Self::raw_new(start, size) }
    }

    fn init_ptr(blk: NonNull<[u8]>) -> Self {
        unsafe { Self::init_addr(blk.as_ptr().addr(), blk.len()) }
    }

}

type MyTlsf<'a> = Tlsf<'a, u32, u32, 24, 8>;
struct SpinTlsf<'a>(spin::Mutex<MyTlsf<'a>>, usize);

impl<'a> SpinTlsf<'a> {
    pub unsafe fn raw_new(start: usize, size: usize) -> Self {
        let s = Self(spin::Mutex::new(MyTlsf::new()), start);
        let blk = unsafe {
            NonNull::new_unchecked(ptr::slice_from_raw_parts_mut(s.start_mut_ptr(), size))
        };
        unsafe { s.0.lock().insert_free_block_ptr(blk) };
        s
    }
    pub fn new(blk: NonNull<[u8]>) -> Self {
        let start = blk.as_ptr().addr();
        let s = Self(spin::Mutex::new(MyTlsf::new()), start);
        unsafe { s.0.lock().insert_free_block_ptr(blk) };
        s
    }

    pub fn new_blk(blk: &'a mut [MaybeUninit<u8>]) -> Self {
        let start = blk.as_ptr().addr();
        let s = Self(spin::Mutex::new(MyTlsf::new()), start);
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

unsafe impl<'a> ShmAllocator for SpinTlsf<'a> {
    fn start_ptr(&self) -> *const u8 {
        self.1 as *const u8
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