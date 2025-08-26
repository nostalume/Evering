use core::ops::Deref;
use core::ptr::NonNull;

use memory_addr::MemoryAddr;

use crate::area::ShmArea;
pub use crate::area::{ShmBackend, ShmProtect, ShmSpec};
use crate::header::Header;
pub use crate::malloc::{AllocError, IAllocator, ShmAllocator, ShmHeader, ShmInit};
use crate::malloc::{blink, gma, tlsf};
use crate::seal::Sealed;

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
        self.header().write().incre_rc();
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
            use crate::header::ShmStatus;
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
