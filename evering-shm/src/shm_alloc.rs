use core::ops::Deref;
use core::ptr::NonNull;

use alloc::alloc::AllocError;
use alloc::alloc::Allocator;
use memory_addr::MemoryAddr;
use memory_set::MappingBackend;
use memory_set::MemoryArea;

pub mod blink;
pub mod gma;
pub mod tlsf;

pub type ShmSpinTlsf<'a, B> = ShmAlloc<tlsf::SpinTlsf<'a>, B>;
pub type ShmSpinGma<B> = ShmAlloc<gma::SpinGma, B>;
pub type ShmBlinkGma<B> = ShmAlloc<blink::BlinkGma, B>;

pub struct ShmAlloc<A: ShmInit, B: MappingBackend> {
    alloc: A,
    area: MemoryArea<B>,
}

impl<A: ShmInit, B: MappingBackend> Deref for ShmAlloc<A, B> {
    type Target = A;

    fn deref(&self) -> &Self::Target {
        &self.alloc
    }
}

impl<A: ShmInit, B: MappingBackend> ShmAlloc<A, B> {
    pub fn from_map(start: B::Addr, size: usize, flags: B::Flags, bk: B) -> Self {
        let align_start = start.align_up(A::MIN_ALIGNMENT);
        let end = align_start.add(size);
        let align_end =  end.align_down(A::MIN_ALIGNMENT);
        let align_size = align_end.sub_addr(align_start);

        let area = MemoryArea::new(align_start, align_size, flags, bk);
        Self::from_area(area)
    }

    pub fn from_area(area: MemoryArea<B>) -> Self {
        Self {
            alloc: A::init_area(&area),
            area,
        }
    }
}

unsafe impl<A: ShmInit, B: MappingBackend> Allocator for ShmAlloc<A, B> {
    fn allocate(&self, layout: core::alloc::Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.alloc.allocate(layout)
    }

    fn allocate_zeroed(&self, layout: core::alloc::Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.alloc.allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: core::alloc::Layout) {
        unsafe { self.alloc.deallocate(ptr, layout) }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: core::alloc::Layout,
        new_layout: core::alloc::Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.alloc.grow(ptr, old_layout, new_layout) }
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: core::alloc::Layout,
        new_layout: core::alloc::Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.alloc.grow_zeroed(ptr, old_layout, new_layout) }
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: core::alloc::Layout,
        new_layout: core::alloc::Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.alloc.shrink(ptr, old_layout, new_layout) }
    }
}

unsafe impl<A: ShmInit, B: MappingBackend> ShmAllocator for ShmAlloc<A, B> {
    fn start_ptr(&self) -> *const u8 {
        self.area.start().into() as *const u8
    }
}

pub unsafe trait ShmInit: Allocator {
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

    /// Initializes the allocator by addr and size.
    ///
    /// ## Safety
    /// user must ensure that the provided memory are valid and ready for allocation.
    unsafe fn init_addr(start: usize, size: usize) -> Self;

    /// Initializes the allocator by a block of memory.
    #[inline]
    fn init_ptr(blk: NonNull<[u8]>) -> Self
    where
        Self: Sized,
    {
        unsafe { Self::init_addr(blk.as_ptr().addr(), blk.len()) }
    }

    #[inline]
    fn init_area<B: MappingBackend>(area: &MemoryArea<B>) -> Self
    where
        Self: Sized,
    {
        unsafe { Self::init_addr(area.start().into(), area.size()) }
    }
}

pub unsafe trait ShmAllocator: Allocator {
    // Returns the number of bytes that are reserved by the allocator.
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
    //
    /// Returns the start pointer of the main memory of the allocator.
    fn start_ptr(&self) -> *const u8;

    /// Returns the start pointer of the main memory of the allocator.
    /// 
    /// ## Safety
    /// The `ptr` should be correctly modified.
    #[inline]
    unsafe fn start_mut_ptr(&self) -> *mut u8 {
        self.start_ptr().cast_mut()
    }

    /// Returns the offset to the start of the allocator.
    ///
    /// ## Safety
    /// - `ptr` must be allocated by this allocator.
    #[inline]
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

unsafe impl<A: ShmAllocator> ShmAllocator for &A {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        (**self).start_ptr()
    }
}
