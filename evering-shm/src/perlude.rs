use core::ops::Deref;
use core::ptr::{self, NonNull};

use memory_addr::MemoryAddr;

use crate::area::ShmArea;
pub use crate::area::{ShmBackend, ShmProtect, ShmSpec};
use crate::header::Header;
pub use crate::malloc::{AllocError, IAllocator, ShmAllocator, ShmHeader, ShmInit};
use crate::malloc::{blink, gma, tlsf};
use crate::seal::Sealed;

#[cfg(feature = "unix")]
pub type DefaultShmSpec = crate::os::unix::UnixShm;
#[cfg(feature = "windows")]
pub type DefaultShmSpec = crate::os::windows::WinShm;
#[cfg(not(any(feature = "unix", feature = "windows")))]
pub type DefaultShmSpec = ();

pub type DefaultAlloc = tlsf::SpinTlsf;

pub type ShmSpinTlsf<M, S = DefaultShmSpec> = ShmAlloc<tlsf::SpinTlsf, S, M>;
pub type ShmSpinGma<M, S = DefaultShmSpec> = ShmAlloc<gma::SpinGma, S, M>;
pub type ShmBlinkGma<M, S = DefaultShmSpec> = ShmAlloc<blink::BlinkGma, S, M>;

pub type AsShmAlloc<M, A = DefaultAlloc, S = DefaultShmSpec> = ShmAlloc<A, S, M>;
pub type AsShmAllocError<M, S = DefaultShmSpec> = ShmAllocError<S, M>;

pub enum ShmAllocError<S: ShmSpec, M: ShmBackend<S>> {
    UnenoughSpace,
    InvalidHeader,
    MapError(M::Error),
    AllocError(AllocError),
}

impl<S: ShmSpec, M: ShmBackend<S>> From<AllocError> for ShmAllocError<S, M> {
    fn from(err: AllocError) -> Self {
        Self::AllocError(err)
    }
}

impl<S: ShmSpec, M: ShmBackend<S>> core::error::Error for ShmAllocError<S, M> {}

impl<S: ShmSpec, M: ShmBackend<S>> alloc::fmt::Debug for ShmAllocError<S, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnenoughSpace => write!(f, "Not enough space available"),
            Self::InvalidHeader => write!(f, "Invalid header detected"),
            Self::MapError(err) => write!(f, "Mapping error: {:?}", err),
            Self::AllocError(err) => write!(f, "Allocation error: {:?}", err),
        }
    }
}

impl<S: ShmSpec, M: ShmBackend<S>> core::fmt::Display for ShmAllocError<S, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnenoughSpace => write!(f, "Not enough space available"),
            Self::InvalidHeader => write!(f, "Invalid header detected"),
            Self::MapError(err) => write!(f, "Mapping error: {:?}", err),
            Self::AllocError(err) => write!(f, "Allocation error: {:?}", err),
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
        self.allocator()
    }
}

impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> Clone for ShmAlloc<A, S, M>
where
    M: Clone,
{
    fn clone(&self) -> Self {
        self.header().write().inc_rc();
        Self {
            area: self.area.clone(),
            phantom: self.phantom,
        }
    }
}

impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> Drop for ShmAlloc<A, S, M> {
    fn drop(&mut self) {
        let rc = self.header().write().dec_rc();

        rc.map(|s| {
            if s == 1 {
                let alloc = self.allocator();
                unsafe { ptr::drop_in_place(alloc as *const A as *mut A) };
                let _ = M::unmap(&mut self.area);
            }
        });
    }
}

impl<A: ShmInit, S: ShmSpec, M: ShmBackend<S>> ShmAlloc<A, S, M> {
    pub fn allocator(&self) -> &A {
        let ptr = self
            .area
            .start()
            .add(Header::HEADER_SIZE)
            .align_up(Header::HEADER_ALIGN)
            .into() as *mut A;
        unsafe { &*ptr }
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
        unsafe {
            let (header, h_offset) = area
                .acquire_by_offset::<Header>(0)
                .ok_or(ShmAllocError::UnenoughSpace)?;
            let header_ref = &mut *header;
            let header_read = header_ref.read();

            // TODO!
            use crate::header::ShmStatus;
            if header_read.valid_magic() {
                match header_read.status() {
                    ShmStatus::Initialized => {
                        header_read.inc_rc();
                    }
                    ShmStatus::Initializing => {
                        drop(header_read);
                        loop {
                            let header_read_again = header_ref.read();
                            match header_read_again.status() {
                                ShmStatus::Initialized => {
                                    header_read_again.inc_rc();
                                    break;
                                }
                                _ => core::hint::spin_loop(),
                            }
                        }
                    }
                    _ => return Err(ShmAllocError::InvalidHeader),
                }
            } else {
                drop(header_read);
                header_ref.init(|| -> Result<(), ShmAllocError<S, M>> {
                    let (a_ptr, a_offset) = area
                        .acquire_by_offset::<A>(h_offset)
                        .ok_or(ShmAllocError::UnenoughSpace)?;
                    let (a_start, a_size) =
                        area.as_addr(a_offset).ok_or(ShmAllocError::UnenoughSpace)?;
                    let a = A::init_addr(a_start.into(), a_size);
                    a_ptr.write(a);
                    Ok(())
                })?;
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

    unsafe fn init_spec_raw<T>(&self, spec: &T, idx: usize) {
        let mut header = self.header().write();
        let offset = unsafe { self.offset(spec) };
        header.with_spec(offset, idx);
    }

    fn header(&self) -> &Header {
        let ptr = self.area.start().into() as *mut Header;
        unsafe { &*ptr }
    }
}
