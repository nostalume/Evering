use core::ptr::NonNull;
use core::{ops::Deref, sync::atomic::AtomicU32};

use super::{AddrSpec, MemBlkOps, Mmap, Mprotect};
use crate::header::{HeaderStatus, Layout, Magic};

pub struct MemBlkSpec<S: AddrSpec> {
    range: memory_addr::AddrRange<S::Addr>,
    flags: S::Flags,
}

impl<S: AddrSpec> Clone for MemBlkSpec<S> {
    fn clone(&self) -> Self {
        Self {
            range: self.range,
            flags: self.flags,
        }
    }
}

impl<S: AddrSpec> AddrSpec for MemBlkSpec<S> {
    type Addr = S::Addr;
    type Flags = S::Flags;
}

impl<S: AddrSpec> MemBlkSpec<S> {
    /// Create a memory area spec.
    pub(crate) fn new(start: S::Addr, size: usize, flags: S::Flags) -> Self {
        let va_range = memory_addr::AddrRange::from_start_size(start, size);
        Self {
            range: va_range,
            flags,
        }
    }
}

impl<S: AddrSpec> MemBlkSpec<S> {
    /// Returns the virtual address range.
    #[inline]
    pub const fn va_range(&self) -> memory_addr::AddrRange<S::Addr> {
        self.range
    }

    /// Returns the memory flags, e.g., the permission bits.
    #[inline]
    pub const fn flags(&self) -> S::Flags {
        self.flags
    }

    #[inline]
    pub const fn with_flags(&mut self, flags: S::Flags) {
        self.flags = flags
    }

    /// Returns the start address of the memory area.
    #[inline]
    pub const fn start(&self) -> S::Addr {
        self.range.start
    }

    /// Returns the end address of the memory area.
    #[inline]
    pub const fn end(&self) -> S::Addr {
        self.range.end
    }

    /// Returns the size of the memory area.
    #[inline]
    pub fn size(&self) -> usize {
        self.range.size()
    }
}

#[repr(C, align(8))]
pub struct Metadata {
    magic: Magic,
    // own counts
    rc: AtomicU32,
}

pub type Header = crate::header::Header<Metadata>;

impl alloc::fmt::Debug for Metadata {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Metadata of Header")
            .field("magic", &self.magic)
            .field(
                "reference count",
                &self.rc.load(core::sync::atomic::Ordering::Relaxed),
            )
            .finish()
    }
}

impl core::fmt::Display for Metadata {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        alloc::fmt::Debug::fmt(self, f)
    }
}

impl crate::header::Layout for Metadata {
    type Config = ();
    #[inline]
    fn init(&mut self, _cfg: ()) -> HeaderStatus {
        use crate::header::Metadata;
        self.with_magic();
        self.rc.store(1, core::sync::atomic::Ordering::Relaxed);
        HeaderStatus::Initialized
    }

    #[inline]
    fn attach(&self) -> HeaderStatus {
        use crate::header::Metadata;
        if self.valid_magic() {
            self.inc_rc();
            HeaderStatus::Initialized
        } else {
            HeaderStatus::Uninitialized
        }
    }
}

impl crate::header::Metadata for Metadata {
    const MAGIC_VALUE: Magic = 0x7203;
    #[inline]
    fn valid_magic(&self) -> bool {
        self.magic == Self::MAGIC_VALUE
    }

    #[inline]
    fn with_magic(&mut self) {
        self.magic = Self::MAGIC_VALUE
    }
}

impl Metadata {
    #[inline]
    pub fn inc_rc(&self) -> u32 {
        self.rc.fetch_add(1, core::sync::atomic::Ordering::AcqRel)
    }

    #[inline]
    pub fn dec_rc(&self) -> Option<u32> {
        use core::sync::atomic::Ordering;
        self.rc
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                if cur == 0 { None } else { Some(cur - 1) }
            })
            .ok()
    }
}

pub enum Error<S: AddrSpec, M: Mmap<S>> {
    OutofSize { requested: usize, bound: usize },
    UnenoughSpace { requested: usize, allocated: usize },
    Contention,
    InvalidHeader,
    MapError(M::Error),
}

impl<S: AddrSpec, M: Mmap<S>> core::error::Error for Error<S, M> {}

impl<S: AddrSpec, M: Mmap<S>> alloc::fmt::Debug for Error<S, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnenoughSpace {
                requested,
                allocated,
            } => write!(
                f,
                "Not enough space available, requested {}, allocated {}",
                requested, allocated
            ),
            Self::OutofSize { requested, bound } => write!(
                f,
                "Out of upper bounded size, requested {}, upper bound {}",
                requested, bound
            ),
            Self::Contention => write!(f, "Contention"),
            Self::InvalidHeader => write!(f, "Header initialization failed"),
            Self::MapError(err) => write!(f, "Mapping error: {:?}", err),
        }
    }
}

impl<S: AddrSpec, M: Mmap<S>> core::fmt::Display for Error<S, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        alloc::fmt::Debug::fmt(self, f)
    }
}

pub struct RawMemBlk<S: AddrSpec, M: Mmap<S>> {
    pub spec: MemBlkSpec<S>,
    pub bk: M,
}

impl<S: AddrSpec, M: Mmap<S>> RawMemBlk<S, M> {
    pub fn drop_in(area: Self) -> Result<(), M::Error> {
        let mut area = area;
        M::unmap(&mut area)
    }
}

impl<S: AddrSpec, M: Mprotect<S>> RawMemBlk<S, M> {
    pub unsafe fn protect(&mut self, flags: S::Flags) -> Result<(), M::Error> {
        (unsafe { M::protect(self, flags) })?;
        self.spec.flags = flags;
        Ok(())
    }
}

impl<S: AddrSpec, M: Mmap<S>> RawMemBlk<S, M> {
    pub unsafe fn from_ptr<T>(start: NonNull<T>, size: usize, flags: S::Flags, bk: M) -> Self {
        unsafe { Self::from_raw(start.addr().get().into(), size, flags, bk) }
    }

    /// Create a hallow memory area without any map operation.
    ///
    /// You should only use in `Mmap` trait.
    #[inline]
    pub unsafe fn from_raw(start: S::Addr, size: usize, flags: S::Flags, bk: M) -> Self {
        let a = MemBlkSpec::new(start, size, flags);
        Self { spec: a, bk }
    }

    unsafe fn init_header<T: Layout>(
        &self,
        offset: usize,
        cfg: T::Config,
    ) -> Result<(NonNull<T>, usize), Error<S, M>> {
        use crate::header::Layout;
        unsafe {
            let (header, hoffset) =
                self.obtain_by_offset::<T>(offset)
                    .map_err(|new_offset| Error::UnenoughSpace {
                        requested: new_offset,
                        allocated: self.size(),
                    })?;
            let header_ref = Layout::from_raw(header);
            match header_ref.attach_or_init(cfg) {
                HeaderStatus::Initialized => Ok((NonNull::new_unchecked(header), hoffset)),
                HeaderStatus::Initializing => Err(Error::Contention),
                _ => Err(Error::InvalidHeader),
            }
        }
    }
}

impl<S: AddrSpec, M: Mmap<S>> Deref for RawMemBlk<S, M> {
    type Target = MemBlkSpec<S>;

    fn deref(&self) -> &Self::Target {
        &self.spec
    }
}

unsafe impl<S: AddrSpec, M: Mmap<S>> MemBlkOps for RawMemBlk<S, M> {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.spec.start().into() as *const u8
    }

    #[inline]
    fn end_ptr(&self) -> *const u8 {
        self.spec.start().into() as *const u8
    }

    #[inline]
    fn size(&self) -> usize {
        self.spec.size()
    }
}

pub struct MemBlkLayout<S: AddrSpec, M: Mmap<S>> {
    pub area: RawMemBlk<S, M>,
    offset: usize,
}

impl<S: AddrSpec, M: Mmap<S>> MemBlkLayout<S, M> {
    pub const fn from_raw(area: RawMemBlk<S, M>, offset: usize) -> Self {
        Self { area, offset }
    }

    pub const fn new(area: RawMemBlk<S, M>) -> Self {
        Self::from_raw(area, 0)
    }

    pub fn push<T: Layout>(&mut self, conf: T::Config) -> Result<NonNull<T>, Error<S, M>> {
        let (ptr, next) = unsafe { self.area.init_header::<T>(self.offset, conf) }?;
        self.offset = next;
        Ok(ptr)
    }

    pub fn finish(self) -> (MemBlk<S, M>, usize) {
        let Self { area, offset } = self;
        (unsafe { MemBlk::from_raw(area) }, offset)
    }
}

/// A handle of **mapped** memory block.
pub struct MemBlk<S: AddrSpec, M: Mmap<S>> {
    spec: MemBlkSpec<S>,
    bk: M,
}

pub struct MemBlkHandle<S: AddrSpec, M: Mmap<S>>(crate::counter::CounterOf<MemBlk<S, M>>);

impl<S: AddrSpec, M: Mmap<S>> Drop for MemBlk<S, M> {
    fn drop(&mut self) {
        self.header().read().dec_rc();
        let blk = self.as_raw();
        let _ = M::unmap(blk);
    }
}

impl<S: AddrSpec, M: Mmap<S>> TryFrom<MemBlkLayout<S, M>> for MemBlk<S, M> {
    type Error = Error<S, M>;

    fn try_from(area: MemBlkLayout<S, M>) -> Result<Self, Self::Error> {
        let mut area = area;
        area.push::<Header>(())?;
        let (area, _) = area.finish();
        Ok(area)
    }
}

unsafe impl<S: AddrSpec, M: Mmap<S>> MemBlkOps for MemBlk<S, M> {
    fn start_ptr(&self) -> *const u8 {
        self.spec.start().into() as *const u8
    }

    fn end_ptr(&self) -> *const u8 {
        self.spec.end().into() as *const u8
    }

    fn size(&self) -> usize {
        self.spec.size()
    }
}

impl<S: AddrSpec, M: Mmap<S>> MemBlk<S, M> {
    #[inline(always)]
    pub unsafe fn from_raw(raw: RawMemBlk<S, M>) -> MemBlk<S, M> {
        let RawMemBlk { spec: a, bk } = raw;
        Self { spec: a, bk }
    }

    #[inline(always)]
    fn as_raw(&mut self) -> &mut RawMemBlk<S, M> {
        unsafe { &mut *(self as *mut _ as *mut _) }
    }

    #[inline(always)]
    pub fn header(&self) -> &Header {
        unsafe {
            let ptr = self.start_ptr() as *const Header;
            &*ptr
        }
    }
}

impl<S: AddrSpec, M: Mmap<S>> Clone for MemBlkHandle<S, M> {
    fn clone(&self) -> Self {
        Self(self.0.acquire())
    }
}

impl<S: AddrSpec, M: Mmap<S>> Drop for MemBlkHandle<S, M> {
    fn drop(&mut self) {
        unsafe { self.0.release() }
    }
}

impl<S: AddrSpec, M: Mmap<S>> Deref for MemBlkHandle<S, M> {
    type Target = MemBlk<S, M>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<S: AddrSpec, M: Mmap<S>> TryFrom<MemBlkLayout<S, M>> for MemBlkHandle<S, M> {
    type Error = Error<S, M>;

    fn try_from(area: MemBlkLayout<S, M>) -> Result<Self, Self::Error> {
        let blk = MemBlk::try_from(area)?;
        Ok(MemBlkHandle::from(blk))
    }
}

impl<S: AddrSpec, M: Mmap<S>> From<MemBlk<S, M>> for MemBlkHandle<S, M> {
    fn from(value: MemBlk<S, M>) -> Self {
        Self(crate::counter::CounterOf::suspend(value))
    }
}
