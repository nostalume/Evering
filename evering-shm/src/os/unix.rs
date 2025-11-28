#![cfg(feature = "unix")]

use core::{ffi::c_void, ptr::NonNull};
pub use nix::{
    libc::off_t,
    sys::mman::{MapFlags, ProtFlags},
    unistd,
};
use std::{
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::{Path, PathBuf},
};

use crate::{
    area::{Access, Mmap, Mprotect, RawMemBlk, SharedMmap},
    os::FdBackend,
};

type Addr = usize;
type Size = usize;

unsafe fn as_c_void(ptr: Addr) -> NonNull<c_void> {
    let ptr = ptr as *mut c_void;
    unsafe { NonNull::new_unchecked(ptr) }
}

fn shm_path<P: AsRef<Path> + ?Sized>(name: &P) -> PathBuf {
    const SHM_BASE: &str = "/dev/shm";
    const TMP_BASE: &str = "/tmp";
    let base = {
        let sbase = Path::new(SHM_BASE);
        if sbase.exists() {
            sbase
        } else {
            Path::new(TMP_BASE)
        }
    };

    base.join(name)
}

#[derive(Debug, Clone)]
enum FdKind {
    MemFd(String),    // ephemeral
    Shm(PathBuf),     // persistent in RAM
    Regular(PathBuf), //persistent in FS
    FromFd,           // Adopted
}

pub struct UnixFd<F: AsFd> {
    fd: F,
    size: usize,
    kind: FdKind,
}

impl<F: AsFd> core::fmt::Debug for UnixFd<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UnixFd")
            .field("size", &self.size)
            .field("fdkind", &self.kind)
            .finish()
    }
}

impl UnixFd<OwnedFd> {
    /// Creates an anonymous file in memory (memfd_create).
    pub fn memfd(name: &str, size: usize, sealing: bool) -> nix::Result<Self> {
        use nix::sys::memfd;
        let flags = if sealing {
            memfd::MFdFlags::MFD_ALLOW_SEALING
        } else {
            memfd::MFdFlags::empty()
        };

        let fd = memfd::memfd_create(name, flags)?;
        unistd::ftruncate(fd.as_fd(), size as off_t)?;
        Ok(Self {
            fd,
            kind: FdKind::MemFd(name.to_string()),
            size,
        })
    }

    pub fn shm_create<P: AsRef<Path> + ?Sized>(name: &P, size: usize) -> nix::Result<Self> {
        use nix::fcntl;
        use nix::sys::stat;
        let path = shm_path(name);
        let oflags = fcntl::OFlag::O_RDWR
            .union(fcntl::OFlag::O_CREAT)
            .union(fcntl::OFlag::O_EXCL);
        let mode = stat::Mode::from_bits_truncate(0o600);
        let fd = fcntl::open(&path, oflags, mode)?;
        unistd::ftruncate(fd.as_fd(), size as off_t)?;
        Ok(Self {
            fd,
            kind: FdKind::Shm(path),
            size,
        })
    }

    pub fn shm_open<P: AsRef<Path> + ?Sized>(name: &P) -> nix::Result<Self> {
        use nix::fcntl;
        use nix::sys::stat;
        let path = shm_path(name);
        let fd = fcntl::open(&path, fcntl::OFlag::O_RDWR, stat::Mode::empty())?;
        let size = stat::fstat(fd.as_fd())?.st_size as usize;
        Ok(Self {
            fd,
            kind: FdKind::Shm(path),
            size,
        })
    }

    pub fn shm_unlink<P: AsRef<Path> + ?Sized>(name: &P) -> nix::Result<()> {
        let path = shm_path(name);
        unistd::unlink(&path)
    }

    pub fn from_fd(fd: OwnedFd) -> nix::Result<Self> {
        use nix::sys::stat;
        let size = stat::fstat(fd.as_fd())?.st_size as usize;
        Ok(Self {
            fd,
            kind: FdKind::FromFd,
            size,
        })
    }
}

impl<F: AsFd> UnixFd<F> {
    pub fn borrow(&self) -> UnixFd<BorrowedFd<'_>> {
        UnixFd {
            fd: self.fd.as_fd(),
            kind: self.kind.clone(),
            size: self.size,
        }
    }

    pub fn dup(&self) -> nix::Result<UnixFd<OwnedFd>> {
        let fd = unistd::dup(self.fd.as_fd())?;
        Ok(UnixFd {
            fd,
            kind: self.kind.clone(),
            size: self.size,
        })
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    pub fn size(&self) -> usize {
        self.size
    }
}

pub struct AddrSpec;

impl crate::area::AddrSpec for AddrSpec {
    type Addr = Addr;
    type Flags = ProtFlags;
}

impl const From<Access> for ProtFlags {
    fn from(value: Access) -> Self {
        match value {
            Access::Write => ProtFlags::PROT_WRITE,
            Access::Read => ProtFlags::PROT_READ,
            Access::ReadWrite => ProtFlags::PROT_WRITE.union(ProtFlags::PROT_READ),
        }
    }
}

impl<F: AsFd> Mmap<AddrSpec> for FdBackend<F> {
    type Handle = UnixFd<F>;

    type MapFlags = MapFlags;

    type Error = nix::Error;

    fn map(
        self,
        start: Option<<AddrSpec as crate::area::AddrSpec>::Addr>,
        size: usize,
        mflags: Self::MapFlags,
        pflags: <AddrSpec as crate::area::AddrSpec>::Flags,
        conf: Self::Handle,
    ) -> Result<RawMemBlk<AddrSpec, Self>, Self::Error> {
        use core::num::NonZeroUsize;
        use nix::sys::mman;

        let fd = conf.fd.as_fd();
        let fsize = nix::sys::stat::fstat(fd)?.st_size;
        let rsize = size as off_t;
        if fsize < rsize {
            unistd::ftruncate(fd, rsize)?;
        }

        let start = start.and_then(NonZeroUsize::new);
        let size = NonZeroUsize::new(size).ok_or(nix::Error::EINVAL)?;

        unsafe {
            let ptr = mman::mmap(start, size, pflags, mflags, fd, 0)?;
            Ok(RawMemBlk::from_ptr(ptr, size.get(), pflags, self))
        }
    }

    fn unmap(area: &mut RawMemBlk<AddrSpec, Self>) -> Result<(), Self::Error> {
        let start = unsafe { as_c_void(area.a.start()) };
        let size = area.a.size();
        unsafe { nix::sys::mman::munmap(start, size) }
    }
}

impl<F: AsFd> Mprotect<AddrSpec> for FdBackend<F> {
    fn protect(
        area: &mut RawMemBlk<AddrSpec, Self>,
        pflags: <AddrSpec as crate::area::AddrSpec>::Flags,
    ) -> Result<(), Self::Error> {
        let start = unsafe { as_c_void(area.a.start()) };
        let size = area.a.size();
        unsafe { nix::sys::mman::mprotect(start, size, pflags) }
    }
}

impl<F: AsFd> SharedMmap<AddrSpec> for FdBackend<F> {
    fn shared(
        self,
        size: usize,
        access: crate::area::Access,
        handle: Self::Handle,
    ) -> Result<RawMemBlk<AddrSpec, Self>, Self::Error> {
        let pflags = access.into();
        let mflags = MapFlags::MAP_SHARED;
        self.map(None, size, mflags, pflags, handle)
    }
}
