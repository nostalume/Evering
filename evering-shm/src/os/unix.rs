#![cfg(feature = "unix")]

use core::{ffi::c_void, num::NonZeroUsize, ptr::NonNull};
pub use nix::{
    libc::off_t,
    sys::memfd::MFdFlags,
    sys::mman::{MapFlags, ProtFlags},
};
use std::{
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::{Path, PathBuf},
};

use crate::{
    area::{AddrSpec, Mmap, Mprotect, RawMemBlk},
    os::FdBackend,
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
    const BASE_MFLAGS: MapFlags = MapFlags::MAP_SHARED;
    fn resolve_named<P: AsRef<Path> + ?Sized>(name: &P) -> PathBuf {
        const BASE: &str = "/dev/shm";
        const TMP: &str = "/tmp";

        let base = Path::new(BASE);
        let path = if base.exists() {
            base.join(name.as_ref())
        } else {
            Path::new(TMP).join(name.as_ref())
        };
        path
    }
    pub fn named<P: AsRef<Path> + ?Sized>(name: &P, size: usize) -> Result<Self, nix::Error> {
        use nix::fcntl::OFlag;
        let path = Self::resolve_named(name);
        let oflags = OFlag::O_RDWR | OFlag::O_CREAT;
        let fd = nix::fcntl::open(
            &path,
            oflags,
            nix::sys::stat::Mode::from_bits_truncate(0o600),
        )?;
        nix::unistd::ftruncate(&fd, size as i64)?;

        Ok(Self::new(fd, UnixFdConf::BASE_MFLAGS, 0))
    }

    pub fn clean_named<P: AsRef<Path> + ?Sized>(name: &P) -> Result<(), nix::Error> {
        let path = Self::resolve_named(name);
        nix::unistd::unlink(&path)
    }

    pub fn default_mem<P: nix::NixPath + ?Sized>(
        path: &P,
        size: usize,
        mfd_flags: nix::sys::memfd::MFdFlags,
    ) -> Result<Self, nix::Error> {
        Self::mem(path, size, mfd_flags, UnixFdConf::BASE_MFLAGS, 0)
    }

    pub fn mem<P: nix::NixPath + ?Sized>(
        path: &P,
        size: usize,
        mfd_flags: nix::sys::memfd::MFdFlags,
        mflags: MapFlags,
        offset: off_t,
    ) -> Result<Self, nix::Error> {
        let mflags = mflags.union(UnixFdConf::BASE_MFLAGS);
        let f = nix::sys::memfd::memfd_create(path, mfd_flags)?;
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

pub struct UnixAddrSpec;

impl AddrSpec for UnixAddrSpec {
    type Addr = UnixAddr;
    type Flags = ProtFlags;
}

impl<F: AsFd> Mmap<UnixAddrSpec> for FdBackend<F> {
    type Config = UnixFdConf<F>;
    type Error = nix::Error;

    fn map(
        self,
        start: Option<<UnixAddrSpec as AddrSpec>::Addr>,
        size: usize,
        flags: <UnixAddrSpec as AddrSpec>::Flags,
        cfg: UnixFdConf<F>,
    ) -> Result<RawMemBlk<UnixAddrSpec, Self>, Self::Error> {
        let start = start.and_then(NonZeroUsize::new);
        let len = size as i64;
        let size = match NonZeroUsize::new(size) {
            Some(size) => size,
            _ => return Err(nix::Error::EINVAL),
        };

        let UnixFdConf {
            ref f,
            mflags,
            offset,
        } = cfg;

        unsafe {
            let stat = nix::sys::stat::fstat(f.as_fd())?;
            let f_size = offset + len;
            if stat.st_size < f_size {
                nix::unistd::ftruncate(f.as_fd(), f_size)?;
            }
            nix::sys::mman::mmap(start, size, flags, mflags, f.as_fd(), offset)
                .map(|ptr| RawMemBlk::from_raw(ptr.addr().into(), size.get(), flags, self))
        }
    }

    fn unmap(area: &mut RawMemBlk<UnixAddrSpec, Self>) -> Result<(), Self::Error> {
        let start = unsafe { as_c_void(area.a.start()) };
        let size = area.a.size();
        unsafe { nix::sys::mman::munmap(start, size) }
    }
}

impl<F: AsFd> Mprotect<UnixAddrSpec> for FdBackend<F> {
    fn protect(
        area: &mut RawMemBlk<UnixAddrSpec, Self>,
        new_flags: <UnixAddrSpec as AddrSpec>::Flags,
    ) -> Result<(), Self::Error> {
        let start = unsafe { as_c_void(area.a.start()) };
        let size = area.a.size();
        unsafe { nix::sys::mman::mprotect(start, size, new_flags) }
    }
}
