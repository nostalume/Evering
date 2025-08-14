#![cfg(feature = "unix")]

use core::{num::NonZeroUsize, ptr::NonNull};
use nix::{
    libc::off_t,
    sys::mman::{MapFlags, ProtFlags},
};
use std::os::fd::AsFd;

use crate::shm_area::{ShmArea, ShmBackend, ShmSpec, ShmProtect};

#[repr(transparent)]
#[derive(Debug, Clone, Copy, Eq, PartialEq, PartialOrd, Ord)]
pub struct UnixAddr(Option<NonZeroUsize>);

impl UnixAddr {
    const fn from_usize(addr: usize) -> Self {
        Self(NonZeroUsize::new(addr))
    }
}

impl From<UnixAddr> for usize {
    fn from(addr: UnixAddr) -> Self {
        match addr.0 {
            Some(addr) => addr.get(),
            _ => 0,
        }
    }
}

impl From<usize> for UnixAddr {
    fn from(addr: usize) -> Self {
        Self::from_usize(addr)
    }
}

impl<T> From<NonNull<T>> for UnixAddr {
    fn from(addr: NonNull<T>) -> Self {
        Self::from_usize(addr.as_ptr() as usize)
    }
}

impl<T> From<UnixAddr> for NonNull<T> {
    fn from(value: UnixAddr) -> Self {
        let addr: usize = value.into();
        let ptr = addr as *mut T;
        match NonNull::new(ptr) {
            Some(ptr) => ptr,
            _ => NonNull::dangling(),
        }
    }
}

pub struct FdConfig<F: AsFd> {
    f: F,
    mflag: MapFlags,
    offset: off_t,
}

impl<F:AsFd> FdConfig<F> {
	pub const fn new(f: F, mflag: MapFlags, offset: off_t) -> Self {
		Self { f, mflag, offset }
	}
}

struct FdShmSpec<F:AsFd>(core::marker::PhantomData<F>);

impl<F:AsFd> ShmSpec for FdShmSpec<F> {
    type Addr = UnixAddr;
    type Flags = ProtFlags;
}

pub struct FdBackend;

impl<F: AsFd> ShmBackend<FdShmSpec<F>> for FdBackend {
    type Config = FdConfig<F>;
    type Error = nix::Error;

    fn map(
        self,
        start: <FdShmSpec<F> as ShmSpec>::Addr,
        size: usize,
        flags: <FdShmSpec<F> as ShmSpec>::Flags,
		cfg: FdConfig<F>,
    ) -> Result<ShmArea<FdShmSpec<F>, Self>, Self::Error> {
        let size = match NonZeroUsize::new(size) {
            Some(size) => size,
            _ => return Err(nix::Error::EINVAL),
        };

        let FdConfig { f, mflag, offset } = cfg;

        unsafe {
            nix::sys::mman::mmap(start.0, size, flags, mflag, f.as_fd(), offset).map(|ptr| {
                let start = ptr.into();
                ShmArea::new(start, size.get(), flags, self)
            })
        }
    }

    fn unmap(area: &mut ShmArea<FdShmSpec<F>, Self>) -> Result<(), Self::Error> {
        let addr = area.start().into();
        let size = area.size();
        unsafe { nix::sys::mman::munmap(addr, size) }
    }
}

impl<F: AsFd> ShmProtect<FdShmSpec<F>> for FdBackend {
    fn protect(
        area: &mut ShmArea<FdShmSpec<F>, Self>,
        new_flags: <FdShmSpec<F> as ShmSpec>::Flags,
    ) -> Result<(), Self::Error> {
        let start = area.start().into();
        let size = area.size();
        unsafe { nix::sys::mman::mprotect(start, size, new_flags) }
    }
}