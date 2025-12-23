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
    mem::{self, Access, RawMap},
    os::FdBackend,
};

type Addr = usize;

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

impl const From<Access> for ProtFlags {
    fn from(value: Access) -> Self {
        let mut prot = ProtFlags::empty();
        if value.contains(Access::READ) {
            prot = prot.union(ProtFlags::PROT_READ);
        }
        if value.contains(Access::WRITE) {
            prot = prot.union(ProtFlags::PROT_WRITE);
        }
        if value.contains(Access::EXEC) {
            prot = prot.union(ProtFlags::PROT_EXEC);
        }
        prot
    }
}

impl mem::Accessible for ProtFlags {
    fn permits(self, access: Access) -> bool {
        let access = Self::from(access);
        self.contains(access)
    }
}

pub struct AddrSpec;

impl mem::AddrSpec for AddrSpec {
    type Addr = Addr;
    type Flags = ProtFlags;
}

impl mem::Mmap<AddrSpec> for FdBackend {
    type Handle = UnixFd<OwnedFd>;

    type MapFlags = MapFlags;

    type Error = nix::Error;

    fn map(
        self,
        start: Option<<AddrSpec as mem::AddrSpec>::Addr>,
        size: usize,
        mflags: Self::MapFlags,
        pflags: <AddrSpec as mem::AddrSpec>::Flags,
        conf: Self::Handle,
    ) -> Result<RawMap<AddrSpec, Self>, Self::Error> {
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
            Ok(RawMap::from_ptr(ptr, size.get(), pflags, self))
        }
    }

    fn unmap(area: &mut RawMap<AddrSpec, Self>) -> Result<(), Self::Error> {
        let start = unsafe { as_c_void(area.spec.start()) };
        let size = area.spec.size();
        unsafe { nix::sys::mman::munmap(start, size) }
    }
}

impl mem::Mprotect<AddrSpec> for FdBackend {
    unsafe fn protect(
        area: &mut RawMap<AddrSpec, Self>,
        pflags: <AddrSpec as mem::AddrSpec>::Flags,
    ) -> Result<(), Self::Error> {
        let start = unsafe { as_c_void(area.spec.start()) };
        let size = area.spec.size();
        unsafe { nix::sys::mman::mprotect(start, size, pflags) }
    }
}

impl mem::SharedMmap<AddrSpec> for FdBackend {
    fn shared(
        self,
        size: usize,
        access: Access,
        handle: Self::Handle,
    ) -> Result<RawMap<AddrSpec, Self>, Self::Error> {
        use mem::Mmap;
        let pflags = access.into();
        let mflags = MapFlags::MAP_SHARED;
        self.map(None, size, mflags, pflags, handle)
    }
}

#[cfg(test)]
mod tests {
    #![cfg(target_os = "linux")]

    use super::super::FdBackend;
    use super::UnixFd;

    use crate::mem::{Access, Mmap, SharedMmap};
    use crate::tests::MemBlkTestIO;

    use nix::libc::off_t;
    use nix::unistd;

    #[test]
    fn memfd_rw() {
        const SIZE: usize = 4096;
        const NAME: &str = "fd";
        const VALUE: &[u8] = b"hello";

        let fd = UnixFd::memfd(NAME, SIZE, false).expect("should create");
        let mut blk = FdBackend
            .shared(SIZE, Access::READ | Access::WRITE, fd)
            .expect("should create");

        unsafe {
            blk.write(VALUE);
            let buf = blk.read(VALUE.len());
            assert_eq!(buf, VALUE)
        }

        let _ = Mmap::unmap(&mut blk);
    }

    #[test]
    fn memfd_resize() {
        const SIZE: usize = 1024;
        const GROW_SIZE: usize = SIZE * 4;
        const NAME: &str = "grow";
        const VALUE: &[u8] = b"hello";

        let fd = UnixFd::memfd(NAME, SIZE, false).expect("should create");
        let bk = FdBackend;

        unistd::ftruncate(fd.as_fd(), GROW_SIZE as off_t).unwrap();

        let mut blk = bk
            .shared(GROW_SIZE, Access::READ | Access::WRITE, fd)
            .expect("should create");

        unsafe {
            blk.write(VALUE);
            let buf = blk.read(VALUE.len());
            assert_eq!(buf, VALUE)
        }

        let _ = Mmap::unmap(&mut blk);
    }

    #[test]
    fn memfd_dup() {
        const SIZE: usize = 4096;
        const NAME: &str = "dup";
        const VALUE: &[u8] = b"hello";

        let fd1 = UnixFd::memfd(NAME, SIZE, false).expect("should create");
        let fd2 = fd1.dup().expect("should dup");

        let bk = FdBackend;
        let mut blk1 = bk
            .shared(SIZE, Access::READ | Access::WRITE, fd1)
            .expect("should create");

        unsafe {
            blk1.write(VALUE);
        }

        let _ = Mmap::unmap(&mut blk1);

        let bk2 = FdBackend;
        let mut blk2 = bk2
            .shared(SIZE, Access::READ | Access::WRITE, fd2)
            .expect("should create");

        unsafe {
            let buf = blk2.read(VALUE.len());
            assert_eq!(&buf, VALUE)
        }

        let _ = Mmap::unmap(&mut blk2);
    }

    #[test]
    fn shm_persist() {
        const NAME: &str = "shm_persist";
        const SIZE: usize = 4096;
        const VALUE: &[u8] = b"hello";

        let fd1 = UnixFd::shm_create(NAME, SIZE).expect("should create");
        let bk = FdBackend;
        let mut blk1 = bk
            .shared(SIZE, Access::READ | Access::WRITE, fd1)
            .expect("should create");
        unsafe {
            blk1.write(VALUE);
        }
        let _ = Mmap::unmap(&mut blk1).unwrap();

        let fd2 = UnixFd::shm_open(NAME).expect("should open");
        let bk2 = FdBackend;
        let mut blk2 = bk2
            .shared(SIZE, Access::READ | Access::WRITE, fd2)
            .expect("should create");
        unsafe {
            let buf = blk2.read(VALUE.len());
            assert_eq!(buf, VALUE)
        }
        let _ = Mmap::unmap(&mut blk2);

        UnixFd::shm_unlink(NAME).expect("should unlink")
    }

    #[test]
    fn shm_unlink() {
        const NAME: &str = "shm_unlink";
        const SIZE: usize = 4096;
        const VALUE: &[u8] = b"hello";

        let fd = UnixFd::shm_create(NAME, SIZE).expect("should create");
        let bk = FdBackend;
        let mut blk = bk
            .shared(SIZE, Access::READ | Access::WRITE, fd)
            .expect("should create");
        unsafe {
            blk.write(VALUE);
        }
        let _ = Mmap::unmap(&mut blk);

        UnixFd::shm_unlink(NAME).expect("should unlink");
        assert!(UnixFd::shm_open(NAME).is_err())
    }

    #[test]
    fn zero_size() {
        const NAME: &str = "zero_size";
        const SIZE: usize = 1;

        let fd = UnixFd::shm_create(NAME, SIZE).expect("should create");
        let bk = FdBackend;
        let res = bk.shared(0, Access::READ | Access::WRITE, fd);
        assert!(res.is_err());

        UnixFd::shm_unlink(NAME).expect("should unlink");
    }

    #[test]
    fn multiple_map() {
        const NAME: &str = "multi";
        const SIZE: usize = 1024;
        const VALUE: &[u8] = b"hello";
        const VALUE2: &[u8] = b"hello2";

        let fd1 = UnixFd::shm_create(NAME, SIZE).expect("should create");
        let fd2 = fd1.dup().expect("should dup");
        let mut blk1 = FdBackend
            .shared(SIZE, Access::READ | Access::WRITE, fd1)
            .unwrap();
        let mut blk2 = FdBackend
            .shared(SIZE, Access::READ | Access::WRITE, fd2)
            .unwrap();

        unsafe {
            blk1.write_in(VALUE, 0);
            blk2.write_in(VALUE2, VALUE.len());

            let buf1 = blk1.read_in(VALUE.len(), 0);
            let buf2 = blk2.read_in(VALUE2.len(), VALUE.len());
            assert_eq!(buf1, VALUE);
            assert_eq!(buf2, VALUE2);
        }

        let _ = Mmap::unmap(&mut blk1);
        let _ = Mmap::unmap(&mut blk2);
        let _ = UnixFd::shm_unlink(NAME);
    }

    #[test]
    #[should_panic]
    fn perm_change() {
        use nix::sys::mman::ProtFlags;
        const NAME: &str = "perm";
        const SIZE: usize = 1024;
        const VALUE: &[u8] = b"hello";

        let fd = UnixFd::shm_create(NAME, SIZE).expect("should create");
        let mut blk = FdBackend
            .shared(SIZE, Access::READ | Access::WRITE, fd)
            .unwrap();

        unsafe { blk.protect(ProtFlags::PROT_READ).unwrap() };

        // should panic:
        unsafe { blk.write_in(VALUE, 0) };

        let _ = Mmap::unmap(&mut blk);
        let _ = UnixFd::shm_unlink(NAME);
    }
}
