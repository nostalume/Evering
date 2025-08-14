use core::ops::Deref;
use core::ptr::NonNull;

#[cfg(feature = "nightly")]
use alloc::alloc::{AllocError, Allocator};
#[cfg(not(feature = "nightly"))]
use allocator_api2::alloc::{AllocError, Allocator};

use memory_addr::MemoryAddr;

use crate::shm_area::ShmArea;
use crate::shm_area::ShmBackend;
use crate::shm_area::ShmSpec;

pub mod blink;
pub mod gma;
pub mod tlsf;

pub type ShmSpinTlsf<'a, S, M> = ShmAlloc<tlsf::SpinTlsf<'a>, S, M>;
pub type ShmSpinGma<S, M> = ShmAlloc<gma::SpinGma, S, M>;
pub type ShmBlinkGma<S, M> = ShmAlloc<blink::BlinkGma, S, M>;

pub enum ShmAllocError<S: ShmSpec, M: ShmBackend<S>> {
    UnenoughSpace,
    MapError(M::Error),
}

impl<S: ShmSpec, M: ShmBackend<S>> core::fmt::Debug for ShmAllocError<S, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnenoughSpace => write!(f, "UnenoughSpace"),
            Self::MapError(arg0) => f.debug_tuple("MapError").field(arg0).finish(),
        }
    }
}

pub struct ShmAlloc<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> {
    area: ShmArea<S, M>,
    phantom: core::marker::PhantomData<A>,
}

impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> Deref for ShmAlloc<A, S, M> {
    type Target = A;

    fn deref(&self) -> &Self::Target {
        let ptr = self.area.start().into() as *mut A;
        unsafe { &*ptr }
    }
}

impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> ShmAlloc<A, S, M> {
    pub fn allocator(&self) -> &A {
        self
    }

    pub fn map(
        state: M,
        start: S::Addr,
        size: usize,
        flags: S::Flags,
        cfg: M::Config,
    ) -> Result<Self, ShmAllocError<S, M>> {
        let align_start = start.align_up(A::MIN_ALIGNMENT);
        let end = align_start.add(size);
        let align_end = end.align_down(A::MIN_ALIGNMENT);
        let align_size = align_end.sub_addr(align_start);

        let area = state
            .map(align_start, align_size, flags, cfg)
            .map_err(ShmAllocError::MapError)?;
        Ok(Self::from_area(area)?)
    }

    pub fn from_area(area: ShmArea<S, M>) -> Result<Self, ShmAllocError<S, M>> {
        let start = area.start();
        // calculate size of allocator
        let alloc_start = start
            .add(A::ALLOCATOR_SIZE)
            .align_up(A::MIN_ALIGNMENT)
            .into();
        let alloc_size = match area.size().checked_sub(A::ALLOCATOR_SIZE) {
            Some(size) => size,
            _ => return Err(ShmAllocError::UnenoughSpace),
        };
        // Safety: The area must be valid to access and store the allocator.
        unsafe {
            let ptr = start.into() as *mut A;
            let a = A::init_addr(alloc_start, alloc_size);
            ptr.write(a);
        }
        Ok(Self {
            area,
            phantom: core::marker::PhantomData,
        })
    }
}

unsafe impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> Allocator for ShmAlloc<A, S, M> {
    fn allocate(&self, layout: core::alloc::Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.allocator().allocate(layout)
    }

    fn allocate_zeroed(&self, layout: core::alloc::Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.allocator().allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: core::alloc::Layout) {
        unsafe { self.allocator().deallocate(ptr, layout) }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: core::alloc::Layout,
        new_layout: core::alloc::Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.allocator().grow(ptr, old_layout, new_layout) }
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: core::alloc::Layout,
        new_layout: core::alloc::Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.allocator().grow_zeroed(ptr, old_layout, new_layout) }
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: core::alloc::Layout,
        new_layout: core::alloc::Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.allocator().shrink(ptr, old_layout, new_layout) }
    }
}

unsafe impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> ShmAllocator for ShmAlloc<A, S, M> {
    fn start_ptr(&self) -> *const u8 {
        self.area.start().into() as *const u8
    }
}

pub unsafe trait ShmInit: Allocator + Sized {
    const USIZE_ALIGNMENT: usize = core::mem::align_of::<usize>();
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();
    const ALLOCATOR_SIZE: usize = core::mem::size_of::<Self>();

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
