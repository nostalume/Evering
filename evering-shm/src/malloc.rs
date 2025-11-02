use core::alloc::Layout;
use core::mem::{self, MaybeUninit};
use core::ptr::{self, NonNull};

#[cfg(feature = "nightly")]
pub use alloc::alloc::{AllocError, handle_alloc_error};
#[cfg(not(feature = "nightly"))]
pub use allocator_api2::alloc::{AllocError, handle_alloc_error};

use memory_addr::MemoryAddr;

use crate::area::MemBlkOps;

pub trait MemAllocInfo: MemBlkOps {
    type Header;
    fn header(&self) -> &Self::Header;
    fn allocated(&self) -> usize;
    fn remained(&self) -> usize;
    fn discarded(&self) -> usize;
    unsafe fn alloc_ptr(&self) -> *const u8 {
        self.get_ptr(mem::size_of::<Self::Header>().align_up(mem::align_of::<Self::Header>()))
    }
    unsafe fn alloc_mut_ptr(&self) -> *mut u8 {
        self.get_mut_ptr(mem::size_of::<Self::Header>().align_up(mem::align_of::<Self::Header>()))
    }
}

pub type MetaOf<A> = <A as MemAlloc>::Meta;
pub type SpanOf<M> = <M as Meta>::SpanMeta;

#[const_trait]
pub unsafe trait Meta: Clone {
    type SpanMeta: Clone;
    fn null() -> Self;
    fn is_null(&self) -> bool;
    fn as_uninit<T>(&self) -> NonNull<MaybeUninit<T>>;
    unsafe fn as_ptr<T>(&self) -> *mut T {
        self.as_uninit::<T>().as_ptr().cast()
    }
    fn as_uninit_slice<T>(&self, len: usize) -> NonNull<[MaybeUninit<T>]> {
        let ptr = self.as_uninit::<T>();
        let slice = NonNull::slice_from_raw_parts(ptr, len);
        slice
    }
    unsafe fn as_slice<T>(&self, len: usize) -> *mut [T] {
        let ptr = unsafe { self.as_ptr::<T>() };
        let slice = ptr::slice_from_raw_parts_mut(ptr, len);
        slice
    }
    fn forget(self) -> Self::SpanMeta;
    unsafe fn resolve(span: Self::SpanMeta, base_ptr: *const u8) -> Self;
}

pub trait MemAllocator: MemAlloc + MemDealloc {}
pub trait MemAllocator2: MemAlloc + MemDeallocBy {}

/// Allocate or deallocate raw type `T` in persistence.
pub unsafe trait MemAlloc: MemBlkOps {
    type Meta: Meta;
    type Error;
    fn base_ptr(&self) -> *const u8;
    fn malloc_by(&self, layout: Layout) -> Result<Self::Meta, Self::Error>;
    fn malloc_of<H>(&self) -> Result<Self::Meta, Self::Error> {
        let layout = Layout::new::<H>();
        self.malloc_by(layout)
    }
    fn malloc_bytes(&self, size: usize) -> Result<Self::Meta, Self::Error> {
        let layout = Layout::array::<u8>(size).unwrap();
        self.malloc_by(layout)
    }
}

pub unsafe trait MemDealloc: MemAlloc {
    // deallocate meta `T` in persistence.
    fn demalloc(&self, meta: Self::Meta) -> bool;
}

pub unsafe trait MemDeallocBy: MemAlloc {
    fn demalloc_by(&self, meta: Self::Meta, layout: Layout) -> bool;
}

unsafe impl<A: MemAlloc> MemAlloc for &A {
    type Meta = A::Meta;
    type Error = A::Error;

    fn base_ptr(&self) -> *const u8 {
        (*self).base_ptr()
    }
    fn malloc_by(&self, layout: Layout) -> Result<Self::Meta, Self::Error> {
        (*self).malloc_by(layout)
    }
}

unsafe impl<A: MemDealloc> MemDealloc for &A {
    fn demalloc(&self, meta: Self::Meta) -> bool {
        (*self).demalloc(meta)
    }
}

unsafe impl<A: MemDeallocBy> MemDeallocBy for &A {
    fn demalloc_by(&self, meta: Self::Meta, layout: Layout) -> bool {
        (*self).demalloc_by(meta, layout)
    }
}
