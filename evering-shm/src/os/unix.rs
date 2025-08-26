#![cfg(feature = "unix")]

use core::{ffi::c_void, num::NonZeroUsize, ptr::NonNull};
pub use nix::{
    libc::off_t,
    sys::memfd::MFdFlags,
    sys::mman::{MapFlags, ProtFlags},
};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use crate::{
    os::FdBackend,
    area::{ShmArea, ShmBackend, ShmProtect, ShmSpec},
};

type UnixAddr = usize;

unsafe fn as_c_void(ptr: UnixAddr) -> NonNull<c_void> {
    let ptr = ptr as *mut c_void;
    unsafe { NonNull::new_unchecked(ptr) }
}

pub struct UnixFdConf<F: AsFd> {
    f: F,
    mflags: MapFlags,
    offset: off_t,
}

impl UnixFdConf<OwnedFd> {
    pub fn default_mem_fd<P: nix::NixPath + ?Sized>(
        name: &P,
        size: usize,
        mfd_flags: nix::sys::memfd::MFdFlags,
    ) -> Result<Self, nix::Error> {
        Self::mem_fd(name, size, mfd_flags, MapFlags::MAP_SHARED, 0)
    }

    pub fn mem_fd<P: nix::NixPath + ?Sized>(
        name: &P,
        size: usize,
        mfd_flags: nix::sys::memfd::MFdFlags,
        mflags: MapFlags,
        offset: off_t,
    ) -> Result<Self, nix::Error> {
        let f = nix::sys::memfd::memfd_create(name, mfd_flags)?;
        nix::unistd::ftruncate(f.as_fd(), size as i64)?;
        Ok(Self::new(f, mflags, offset))
    }

    pub fn as_ref<'f>(&'f self) -> UnixFdConf<BorrowedFd<'f>> {
        UnixFdConf::new(self.f.as_fd(), self.mflags, self.offset)
    }

    pub fn dup(&self) -> Result<UnixFdConf<OwnedFd>, nix::Error> {
        let owned = nix::unistd::dup(self.f.as_fd())?;
        Ok(UnixFdConf::new(owned, self.mflags, self.offset))
    }
}

impl Clone for UnixFdConf<OwnedFd> {
    fn clone(&self) -> Self {
        self.dup().unwrap()
    }
}

impl<F: AsFd> UnixFdConf<F> {
    pub const fn new(f: F, mflags: MapFlags, offset: off_t) -> Self {
        Self { f, mflags, offset }
    }
}

pub struct UnixShm;

impl ShmSpec for UnixShm {
    type Addr = UnixAddr;
    type Flags = ProtFlags;
}

impl<F: AsFd> ShmBackend<UnixShm> for FdBackend<F> {
    type Config = UnixFdConf<F>;
    type Error = nix::Error;

    fn map(
        self,
        start: Option<<UnixShm as ShmSpec>::Addr>,
        size: usize,
        flags: <UnixShm as ShmSpec>::Flags,
        cfg: UnixFdConf<F>,
    ) -> Result<ShmArea<UnixShm, Self>, Self::Error> {
        let start = start.and_then(NonZeroUsize::new);
        let len = size as i64;
        let size = match NonZeroUsize::new(size) {
            Some(size) => size,
            _ => return Err(nix::Error::EINVAL),
        };

        let UnixFdConf {
            ref f,
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

    fn unmap(area: &mut ShmArea<UnixShm, Self>) -> Result<(), Self::Error> {
        let addr = unsafe { as_c_void(area.start()) };
        let size = area.size();
        unsafe { nix::sys::mman::munmap(addr, size) }
    }
}

impl<F: AsFd> ShmProtect<UnixShm> for FdBackend<F> {
    fn protect(
        area: &mut ShmArea<UnixShm, Self>,
        new_flags: <UnixShm as ShmSpec>::Flags,
    ) -> Result<(), Self::Error> {
        let start = unsafe { as_c_void(area.start()) };
        let size = area.size();
        unsafe { nix::sys::mman::mprotect(start, size, new_flags) }
    }
}
