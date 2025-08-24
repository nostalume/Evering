use core::mem::ManuallyDrop;
use core::ops::Deref;
use core::ptr::NonNull;

#[cfg(feature = "nightly")]
use alloc::alloc::{AllocError, Allocator};
#[cfg(not(feature = "nightly"))]
use allocator_api2::alloc::{AllocError, Allocator};

use alloc::sync::Arc;
use memory_addr::MemoryAddr;

use crate::IAllocator;
use crate::seal::Sealed;
use crate::shm_area::ShmArea;
use crate::shm_area::ShmBackend;
use crate::shm_area::ShmSpec;
use crate::shm_box::ShmBox;
use crate::shm_header::Header;

pub mod blink;
pub mod gma;
pub mod tlsf;

pub type ShmSpinTlsf<S, M> = ShmAlloc<tlsf::SpinTlsf, S, M>;
pub type ShmSpinGma<S, M> = ShmAlloc<gma::SpinGma, S, M>;
pub type ShmBlinkGma<S, M> = ShmAlloc<blink::BlinkGma, S, M>;

pub enum ShmAllocError<S: ShmSpec, M: ShmBackend<S>> {
    UnenoughSpace,
    InvalidHeader,
    MapError(M::Error),
}

impl<S: ShmSpec, M: ShmBackend<S>> core::error::Error for ShmAllocError<S, M> {}

impl<S: ShmSpec, M: ShmBackend<S>> core::fmt::Display for ShmAllocError<S, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnenoughSpace => write!(f, "UnenoughSpace"),
            Self::InvalidHeader => write!(f, "InvalidHeader"),
            Self::MapError(arg0) => f.debug_tuple("MapError").field(arg0).finish(),
        }
    }
}

impl<S: ShmSpec, M: ShmBackend<S>> core::fmt::Debug for ShmAllocError<S, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnenoughSpace => write!(f, "UnenoughSpace"),
            Self::InvalidHeader => write!(f, "InvalidHeader"),
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
        // TODO! better layout check
        let ptr = self
            .area
            .start()
            .add(Header::HEADER_SIZE)
            .align_up(Header::HEADER_ALIGN)
            .into() as *mut A;
        unsafe { &*ptr }
    }
}

impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> Clone for ShmAlloc<A, S, M>
where
    M: Clone,
{
    fn clone(&self) -> Self {
        Self {
            area: self.area.clone(),
            phantom: self.phantom,
        }
    }
}

impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> Drop for ShmAlloc<A, S, M> {
    fn drop(&mut self) {
        let header_write = self.header().write();
        if header_write.decre_rc() == 1 {
            drop(header_write);
            let _ = M::unmap(&mut self.area);
        }
    }
}

impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> ShmAlloc<A, S, M> {
    pub fn allocator(&self) -> &A {
        self
    }

    pub fn init_or_load(
        state: M,
        start: Option<S::Addr>,
        size: usize,
        flags: S::Flags,
        cfg: M::Config,
    ) -> Result<Self, ShmAllocError<S, M>> {
        let area = state
            .map(start, size, flags, cfg)
            .map_err(ShmAllocError::MapError)?;
        Self::from_area(area)
    }

    pub fn from_area(area: ShmArea<S, M>) -> Result<Self, ShmAllocError<S, M>> {
        let start = area.start();
        unsafe {
            let (header, h_start, _) = area
                .acquire::<Header>(start)
                .ok_or(ShmAllocError::UnenoughSpace)?;
            let header_ref = &mut *header;
            let mut header_write = header_ref.write();

            // TODO!
            use crate::shm_header::ShmStatus;
            if !header_write.valid_magic() {
                header_write.intializing();
                let (a_ptr, a_start, a_size) = area
                    .acquire_raw::<A>(h_start, A::ALLOCATOR_SIZE, A::MIN_ALIGNMENT)
                    .ok_or(ShmAllocError::UnenoughSpace)?;
                let a = A::init_addr(a_start.into(), a_size);
                a_ptr.write(a);
                header_write.with_status(ShmStatus::Initialized);
            } else {
                match header_write.status() {
                    ShmStatus::Initializing => {
                        drop(header_write);
                        loop {
                            // repeat acquire header until initialized
                            let header_read = header_ref.read();
                            if header_read.status() == ShmStatus::Initialized {
                                break;
                            } else {
                                core::hint::spin_loop();
                            }
                        }
                    }
                    ShmStatus::Initialized => {}
                    _ => return Err(ShmAllocError::InvalidHeader),
                }
                header_ref.write().incre_rc();
            }
        }

        Ok(Self {
            area,
            phantom: core::marker::PhantomData,
        })
    }
}

impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> Sealed for ShmAlloc<A, S, M> {}

unsafe impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> IAllocator for ShmAlloc<A, S, M> {
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

unsafe impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> ShmHeader for ShmAlloc<A, S, M> {
    /// preload spec
    fn spec_raw<T>(&self, idx: usize) -> Option<NonNull<T>> {
        let header = self.header();
        let offset = header.read().spec(idx);

        offset.map(|offset| unsafe { self.get_aligned_ptr_mut::<T>(offset) })
    }

    unsafe fn init_spec_raw<T>(&self, spec: &T, idx: usize) -> bool {
        let header = self.header();
        let mut header = header.write();
        let offset = unsafe { self.offset(spec) };
        header.with_spec(offset, idx)
    }

    fn header(&self) -> &Header {
        let ptr = self.area.start().into() as *mut Header;
        unsafe { &*ptr }
    }
}

pub unsafe trait ShmInit: IAllocator + Sized {
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
    unsafe fn spec<T>(&self, idx: usize) -> Option<ShmBox<T, &Self>>
    where
        Self: ShmAllocator + Sized,
    {
        self.spec_raw(idx)
            .map(|ptr| unsafe { ShmBox::from_raw_in(ptr.as_ptr(), self) })
    }
    unsafe fn spec_in<T>(&self, idx: usize) -> Option<ShmBox<T, Self>>
    where
        Self: ShmAllocator + Sized + Clone,
    {
        self.spec_raw(idx)
            .map(|ptr| unsafe { ShmBox::from_raw_in(ptr.as_ptr(), self.clone()) })
    }
    unsafe fn init_spec_raw<T>(&self, spec: &T, idx: usize) -> bool;
    fn init_spec<T, A: ShmAllocator>(&self, spec: ShmBox<T, A>, idx: usize) -> bool
    where
        Self: ShmAllocator + Sized,
    {
        // manually drop to elide deconstructor after store.
        let spec = ManuallyDrop::new(spec);
        unsafe { self.init_spec_raw(spec.as_ref(), idx) }
    }

    unsafe fn clean_spec<T>(&self, idx: usize)
    where
        Self: ShmAllocator + Sized,
    {
        unsafe { self.spec::<T>(idx).map(|b| drop(b)) };
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
