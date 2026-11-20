//! The file-descriptor side of a region on Unix: `UnixFd` holds the bytes — a memfd, a `/dev/shm` file,
//! or an fd adopted from elsewhere — and `FdMap` decides where they enter this address space.
//! `memfd`/`shm_create`/`shm_open`/`from_fd` make or take the object, `mapping`/`at`/`with_flags` state
//! the placement, and `Source::map` is the only path that calls `mmap`: it grows the file if the request
//! is longer than it, reserves an anonymous window, places the file at the aligned address inside that
//! window with `MAP_FIXED`, and hands back the two remainders as unmapped.
//!
//! The reserve-then-place order is what makes an aligned block address reproducible rather than lucky,
//! and it is the reason `mapping`'s residue and `mem::REGION_ALIGN` agree for every mapper: address and
//! file offset are one decision, and trimming an existing file mapping moves the surviving pages'
//! offsets along with them. A caller that reaches the platform a different way — `with_flags` replacing
//! `MAP_SHARED` with a private mapping, or an `mmap` of its own at the same fd — silently opts out of
//! that agreement, and the directory's compare-exchanges then change one process's copy while the record
//! claims otherwise. Nothing here can see that happen; only `header` and `pool` read the addresses back.

use core::ptr::NonNull;
pub use nix::{
    libc::off_t,
    sys::mman::{MapFlags, ProtFlags},
    unistd,
};
use std::{
    io,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::{Path, PathBuf},
};

use crate::mem::{Access, Map, Request, Source};

#[cfg(feature = "process")]
pub mod process;

type Addr = usize;

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
    MemFd,
    Shm,
    FromFd,
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
            kind: FdKind::MemFd,
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
            kind: FdKind::Shm,
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
            kind: FdKind::Shm,
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

const impl From<Access> for ProtFlags {
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

pub struct FdMap<F: AsFd> {
    fd: UnixFd<F>,
    start: Option<Addr>,
    flags: MapFlags,
}

impl<F: AsFd> UnixFd<F> {
    pub fn mapping(self) -> FdMap<F> {
        FdMap {
            fd: self,
            start: None,
            flags: MapFlags::MAP_SHARED,
        }
    }
}

impl<F: AsFd> FdMap<F> {
    pub fn at(mut self, address: usize) -> Self {
        self.start = Some(address);
        self
    }

    pub fn with_flags(mut self, flags: MapFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Maps in the backend's own error type, so the `Source` boundary converts
    /// once instead of every syscall site.
    fn mapping(self, request: Request) -> Result<Map, nix::Error> {
        use core::num::NonZeroUsize;
        use nix::sys::mman;

        let fd = self.fd.fd.as_fd();
        let fsize = nix::sys::stat::fstat(fd)?.st_size;
        let rsize = request.len as off_t;
        if fsize < rsize {
            unistd::ftruncate(fd, rsize)?;
        }

        let access = request.access;
        let want = NonZeroUsize::new(request.len).ok_or(nix::Error::EINVAL)?;
        let page = nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE)?
            .map(|bytes| bytes as usize)
            .unwrap_or(4096);
        let bound = crate::mem::REGION_ALIGN;
        debug_assert!(
            bound.is_multiple_of(page),
            "a bound the kernel cannot address"
        );
        // Two processes mapping one region have to agree on which byte of the file each address names,
        // and `mmap` alone gives neither: it promises only page alignment and treats a hint as
        // advisory. Trimming an existing file mapping is not the way out either, because splitting the
        // mapping moves the surviving pages' file offsets along with them. So the window is reserved
        // first, and the file is then placed at the reserved address that satisfies the bound.
        let slack = bound - 1;
        let hint = self
            .start
            .map(|address| address & !slack)
            .and_then(NonZeroUsize::new);
        let keep = request.len.next_multiple_of(page);
        let span = NonZeroUsize::new(keep + bound + page).ok_or(nix::Error::EINVAL)?;

        unsafe {
            let room =
                mman::mmap_anonymous(hint, span, ProtFlags::PROT_NONE, MapFlags::MAP_PRIVATE)?;
            let room_start = room.cast::<u8>().addr().get();
            let aligned = room_start.wrapping_add(slack) & !slack;
            mman::mmap(
                NonZeroUsize::new(aligned),
                want,
                access.into(),
                self.flags.union(MapFlags::MAP_FIXED),
                fd,
                0,
            )?;
            if aligned > room_start {
                mman::munmap(room, aligned - room_start)?;
            }
            let tail = aligned + keep;
            let end = room_start + span.get();
            if tail < end {
                mman::munmap(
                    NonNull::new_unchecked(tail as *mut core::ffi::c_void),
                    end - tail,
                )?;
            }
            Ok(Map::from_raw_parts(
                NonNull::new_unchecked(aligned as *mut u8),
                want.get(),
                access,
                release,
            ))
        }
    }
}

unsafe fn release(start: NonNull<u8>, len: usize) -> bool {
    unsafe { nix::sys::mman::munmap(start.cast(), len) }.is_ok()
}

unsafe impl<F: AsFd> Source for FdMap<F> {
    type Error = io::Error;

    fn map(self, request: Request) -> Result<Map, Self::Error> {
        self.mapping(request).map_err(io::Error::from)
    }
}

unsafe impl<F: AsFd> Source for UnixFd<F> {
    type Error = io::Error;

    fn map(self, request: Request) -> Result<Map, Self::Error> {
        self.mapping().map(request)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::UnixFd;

    use crate::mem::{Access, Map, Request, Source};
    use crate::tests::MemBlkTestIO;

    use nix::libc::off_t;
    use nix::unistd;

    struct TestMap;

    impl TestMap {
        fn shared(
            self,
            size: usize,
            access: Access,
            fd: UnixFd<std::os::fd::OwnedFd>,
        ) -> std::io::Result<Map> {
            fd.map(Request::new(size, access))
        }
    }

    #[test]
    fn memfd_rw() {
        const SIZE: usize = 4096;
        const NAME: &str = "fd";
        const VALUE: &[u8] = b"hello";

        let fd = UnixFd::memfd(NAME, SIZE, false).expect("should create");
        let blk = TestMap
            .shared(SIZE, Access::READ | Access::WRITE, fd)
            .expect("should create");

        unsafe {
            blk.write(VALUE);
            let buf = blk.read(VALUE.len());
            assert_eq!(buf, VALUE)
        }

        drop(blk);
    }

    #[test]
    fn memfd_resize() {
        const SIZE: usize = 1024;
        const GROW_SIZE: usize = SIZE * 4;
        const NAME: &str = "grow";
        const VALUE: &[u8] = b"hello";

        let fd = UnixFd::memfd(NAME, SIZE, false).expect("should create");
        let bk = TestMap;

        unistd::ftruncate(fd.as_fd(), GROW_SIZE as off_t).unwrap();

        let blk = bk
            .shared(GROW_SIZE, Access::READ | Access::WRITE, fd)
            .expect("should create");

        unsafe {
            blk.write(VALUE);
            let buf = blk.read(VALUE.len());
            assert_eq!(buf, VALUE)
        }

        drop(blk);
    }

    #[test]
    fn memfd_dup() {
        const SIZE: usize = 4096;
        const NAME: &str = "dup";
        const VALUE: &[u8] = b"hello";

        let fd1 = UnixFd::memfd(NAME, SIZE, false).expect("should create");
        let fd2 = fd1.dup().expect("should dup");

        let bk = TestMap;
        let blk1 = bk
            .shared(SIZE, Access::READ | Access::WRITE, fd1)
            .expect("should create");

        unsafe {
            blk1.write(VALUE);
        }

        drop(blk1);

        let bk2 = TestMap;
        let blk2 = bk2
            .shared(SIZE, Access::READ | Access::WRITE, fd2)
            .expect("should create");

        unsafe {
            let buf = blk2.read(VALUE.len());
            assert_eq!(&buf, VALUE)
        }

        drop(blk2);
    }

    #[test]
    fn shm_persist() {
        const NAME: &str = "shm_persist";
        const SIZE: usize = 4096;
        const VALUE: &[u8] = b"hello";

        let fd1 = UnixFd::shm_create(NAME, SIZE).expect("should create");
        let bk = TestMap;
        let blk1 = bk
            .shared(SIZE, Access::READ | Access::WRITE, fd1)
            .expect("should create");
        unsafe {
            blk1.write(VALUE);
        }
        drop(blk1);

        let fd2 = UnixFd::shm_open(NAME).expect("should open");
        let bk2 = TestMap;
        let blk2 = bk2
            .shared(SIZE, Access::READ | Access::WRITE, fd2)
            .expect("should create");
        unsafe {
            let buf = blk2.read(VALUE.len());
            assert_eq!(buf, VALUE)
        }
        drop(blk2);

        UnixFd::shm_unlink(NAME).expect("should unlink")
    }

    #[test]
    fn shm_unlink() {
        const NAME: &str = "shm_unlink";
        const SIZE: usize = 4096;
        const VALUE: &[u8] = b"hello";

        let fd = UnixFd::shm_create(NAME, SIZE).expect("should create");
        let bk = TestMap;
        let blk = bk
            .shared(SIZE, Access::READ | Access::WRITE, fd)
            .expect("should create");
        unsafe {
            blk.write(VALUE);
        }
        drop(blk);

        UnixFd::shm_unlink(NAME).expect("should unlink");
        assert!(UnixFd::shm_open(NAME).is_err())
    }

    #[test]
    fn zero_size() {
        const NAME: &str = "zero_size";
        const SIZE: usize = 1;

        let fd = UnixFd::shm_create(NAME, SIZE).expect("should create");
        let bk = TestMap;
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
        let blk1 = TestMap
            .shared(SIZE, Access::READ | Access::WRITE, fd1)
            .unwrap();
        let blk2 = TestMap
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

        drop(blk1);
        drop(blk2);
        let _ = UnixFd::shm_unlink(NAME);
    }
}
