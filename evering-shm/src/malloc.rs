use core::alloc::Layout;
use core::mem::ManuallyDrop;
use core::ops::Deref;
use core::{ptr, ptr::NonNull};

#[cfg(feature = "nightly")]
pub use alloc::alloc::{AllocError, handle_alloc_error};
#[cfg(not(feature = "nightly"))]
pub use allocator_api2::alloc::{AllocError, handle_alloc_error};

use alloc::sync::Arc;

use crate::boxed::ShmBox;
use crate::header::Header;
use crate::seal::Sealed;

pub mod blink;
pub mod gma;
pub mod tlsf;

pub unsafe trait ShmInit: IAllocator + Sized {
    const USIZE_ALIGNMENT: usize = core::mem::align_of::<usize>();
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();
    const ALLOCATOR_SIZE: usize = core::mem::size_of::<Self>();

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

pub unsafe trait IAllocator: Sealed {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError>;
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let ptr = self.allocate(layout)?;
        // SAFETY: `alloc` returns a valid memory block
        unsafe { ptr.as_non_null_ptr().as_ptr().write_bytes(0, ptr.len()) }
        Ok(ptr)
    }
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout);
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        debug_assert!(
            new_layout.size() >= old_layout.size(),
            "`new_layout.size()` must be greater than or equal to `old_layout.size()`"
        );

        let new_ptr = self.allocate(new_layout)?;
        // SAFETY: because `new_layout.size()` must be greater than or equal to
        // `old_layout.size()`, both the old and new memory allocation are valid for reads and
        // writes for `old_layout.size()` bytes. Also, because the old allocation wasn't yet
        // deallocated, it cannot overlap `new_ptr`. Thus, the call to `copy_nonoverlapping` is
        // safe. The safety contract for `dealloc` must be upheld by the caller.
        unsafe {
            ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_mut_ptr(), old_layout.size());
            self.deallocate(ptr, old_layout);
        }

        Ok(new_ptr)
    }
    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        debug_assert!(
            new_layout.size() >= old_layout.size(),
            "`new_layout.size()` must be greater than or equal to `old_layout.size()`"
        );

        let new_ptr = self.allocate_zeroed(new_layout)?;
        unsafe {
            ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_mut_ptr(), old_layout.size());
            self.deallocate(ptr, old_layout);
        }

        Ok(new_ptr)
    }
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        debug_assert!(
            new_layout.size() <= old_layout.size(),
            "`new_layout.size()` must be smaller than or equal to `old_layout.size()`"
        );

        let new_ptr = self.allocate(new_layout)?;

        // SAFETY: because `new_layout.size()` must be lower than or equal to
        // `old_layout.size()`, both the old and new memory allocation are valid for reads and
        // writes for `new_layout.size()` bytes. Also, because the old allocation wasn't yet
        // deallocated, it cannot overlap `new_ptr`. Thus, the call to `copy_nonoverlapping` is
        // safe. The safety contract for `dealloc` must be upheld by the caller.
        unsafe {
            ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_mut_ptr(), new_layout.size());
            self.deallocate(ptr, old_layout);
        }

        Ok(new_ptr)
    }
    #[inline(always)]
    fn by_ref(&self) -> &Self
    where
        Self: Sized,
    {
        self
    }
}

impl<A: IAllocator> Sealed for &A {}

unsafe impl<A: IAllocator> IAllocator for &A {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        (*self).allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        unsafe { (*self).deallocate(ptr, layout) };
    }
}

impl<A: IAllocator> Sealed for Arc<A> {}

unsafe impl<A: IAllocator> IAllocator for Arc<A> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.deref().allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        unsafe { self.deref().deallocate(ptr, layout) };
    }
}

pub unsafe trait ShmAllocator: IAllocator {
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
    unsafe fn offset<T: ?Sized>(&self, ptr: *const T) -> isize {
        // Safety: `ptr` must has address greater than `self.raw_ptr()`.
        unsafe { ptr.byte_offset_from(self.start_ptr()) }
    }

    /// Returns a pointer to the memory at the given offset.
    #[inline]
    fn get_ptr(&self, offset: isize) -> *const u8 {
        unsafe {
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
            let ptr = self.get_ptr_mut(offset).cast();
            NonNull::new_unchecked(ptr)
        }
    }

    #[inline]
    unsafe fn get_aligned_slice_mut<T>(&self, offset: isize, len: usize) -> NonNull<[T]> {
        unsafe {
            let ptr = self.get_ptr_mut(offset).cast();
            NonNull::new_unchecked(core::slice::from_raw_parts_mut(ptr, len))
        }
    }
}

unsafe impl<A: ShmAllocator> ShmAllocator for &A {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        (**self).start_ptr()
    }
}

unsafe impl<A: ShmAllocator> ShmAllocator for Arc<A> {
    fn start_ptr(&self) -> *const u8 {
        (**self).start_ptr()
    }
}

pub unsafe trait ShmHeader {
    fn header(&self) -> &Header;
    fn spec_raw<T>(&self, idx: usize) -> Option<NonNull<T>>;
    unsafe fn spec<T>(&self, idx: usize) -> Option<ShmBox<T, Self>>
    where
        Self: ShmAllocator + Sized + Clone,
    {
        self.spec_raw(idx)
            .map(|ptr| unsafe { ShmBox::from_raw_in(ptr.as_ptr(), self.clone()) })
    }
    unsafe fn spec_ref<T>(&self, idx: usize) -> Option<ShmBox<T, &Self>>
    where
        Self: ShmAllocator + Sized,
    {
        self.spec_raw(idx)
            .map(|ptr| unsafe { ShmBox::from_raw_in(ptr.as_ptr(), self) })
    }
    unsafe fn init_spec_raw<T>(&self, spec: &T, idx: usize) -> bool;
    fn init_spec<T, A: ShmAllocator>(&self, spec: ShmBox<T, A>, idx: usize) -> bool
    where
        Self: ShmAllocator + Sized,
    {
        // manually drop to elide deallocation after store.
        let spec = ManuallyDrop::new(spec);
        unsafe { self.init_spec_raw(spec.as_ref(), idx) }
    }
}

unsafe impl<A: ShmHeader> ShmHeader for &A {
    fn header(&self) -> &Header {
        (**self).header()
    }

    fn spec_raw<T>(&self, idx: usize) -> Option<NonNull<T>> {
        (**self).spec_raw(idx)
    }

    unsafe fn init_spec_raw<T>(&self, spec: &T, idx: usize) -> bool {
        unsafe { (**self).init_spec_raw(spec, idx) }
    }
}

unsafe impl<A: ShmHeader> ShmHeader for Arc<A> {
    fn header(&self) -> &Header {
        (**self).header()
    }

    fn spec_raw<T>(&self, idx: usize) -> Option<NonNull<T>> {
        (**self).spec_raw(idx)
    }

    unsafe fn init_spec_raw<T>(&self, spec: &T, idx: usize) -> bool {
        unsafe { (**self).init_spec_raw(spec, idx) }
    }
}
