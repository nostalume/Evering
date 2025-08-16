#![cfg(feature = "unix")]

use core::{ffi::c_void, num::NonZeroUsize, ptr::NonNull};
pub use nix::{
    libc::off_t,
    sys::memfd::MFdFlags,
    sys::mman::{MapFlags, ProtFlags},
};
use std::os::fd::{AsFd, OwnedFd};

use crate::shm_area::{ShmArea, ShmBackend, ShmProtect, ShmSpec};

type UnixAddr = usize;

fn c_void_as_usize(ptr: *const c_void) -> UnixAddr {
    ptr as UnixAddr
}

unsafe fn usize_as_c_void(ptr: UnixAddr) -> NonNull<c_void> {
    let ptr = ptr as *mut c_void;
    unsafe { NonNull::new_unchecked(ptr) }
}

pub struct FdConfig<F: AsFd> {
    f: F,
    mflags: MapFlags,
    offset: off_t,
}

impl FdConfig<OwnedFd> {
    pub fn default_from_mem_fd<P: nix::NixPath + ?Sized>(
        name: &P,
        mfd_flags: nix::sys::memfd::MFdFlags,
    ) -> Result<Self, nix::Error> {
        Self::from_mem_fd(name, mfd_flags, MapFlags::MAP_SHARED, 0)
    }

    pub fn from_mem_fd<P: nix::NixPath + ?Sized>(
        name: &P,
        mfd_flags: nix::sys::memfd::MFdFlags,
        mflags: MapFlags,
        offset: off_t,
    ) -> Result<Self, nix::Error> {
        let f = nix::sys::memfd::memfd_create(name, mfd_flags)?;
        Ok(Self::new(f, mflags, offset))
    }
}

impl<F: AsFd> FdConfig<F> {
    pub const fn new(f: F, mflags: MapFlags, offset: off_t) -> Self {
        Self { f, mflags, offset }
    }
}

pub struct FdShmSpec<F: AsFd>(core::marker::PhantomData<F>);

impl<F: AsFd> ShmSpec for FdShmSpec<F> {
    type Addr = UnixAddr;
    type Flags = ProtFlags;
}

pub struct FdBackend;

impl<F: AsFd> ShmBackend<FdShmSpec<F>> for FdBackend {
    type Config = FdConfig<F>;
    type Error = nix::Error;

    fn map(
        self,
        start: Option<<FdShmSpec<F> as ShmSpec>::Addr>,
        size: usize,
        flags: <FdShmSpec<F> as ShmSpec>::Flags,
        cfg: FdConfig<F>,
    ) -> Result<ShmArea<FdShmSpec<F>, Self>, Self::Error> {
        let start = start.and_then(NonZeroUsize::new);
        let len = size as i64;
        let size = match NonZeroUsize::new(size) {
            Some(size) => size,
            _ => return Err(nix::Error::EINVAL),
        };

        let FdConfig {
            f,
            mflags: mflag,
            offset,
        } = cfg;

        unsafe {
            let stat = nix::sys::stat::fstat(f.as_fd())?;
            let m = core::cmp::max(len, offset);
            if stat.st_size < m {
                nix::unistd::ftruncate(f.as_fd(), m)?;
            }
            nix::sys::mman::mmap(start, size, flags, mflag, f.as_fd(), offset).map(|ptr| {
                let start = ptr.addr().into();
                ShmArea::new(start, size.get(), flags, self)
            })
        }
    }

    fn unmap(area: &mut ShmArea<FdShmSpec<F>, Self>) -> Result<(), Self::Error> {
        let addr = unsafe { usize_as_c_void(area.start().into()) };
        let size = area.size();
        unsafe { nix::sys::mman::munmap(addr, size) }
    }
}

impl<F: AsFd> ShmProtect<FdShmSpec<F>> for FdBackend {
    fn protect(
        area: &mut ShmArea<FdShmSpec<F>, Self>,
        new_flags: <FdShmSpec<F> as ShmSpec>::Flags,
    ) -> Result<(), Self::Error> {
        let start = unsafe { usize_as_c_void(area.start().into()) };
        let size = area.size();
        unsafe { nix::sys::mman::mprotect(start, size, new_flags) }
    }
}
