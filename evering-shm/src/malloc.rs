use core::alloc::Layout;
use core::mem::ManuallyDrop;
use core::ops::Deref;
use core::{ptr, ptr::NonNull};

#[cfg(feature = "nightly")]
pub use alloc::alloc::{AllocError, handle_alloc_error};
#[cfg(not(feature = "nightly"))]
pub use allocator_api2::alloc::{AllocError, handle_alloc_error};

use alloc::sync::Arc;

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