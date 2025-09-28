use core::{fmt::Debug, ops::Deref, ptr::NonNull, sync::atomic::AtomicU32};
use memory_addr::{AddrRange, MemoryAddr};

use spin::RwLock;

pub trait AddrSpec {
    type Addr: MemoryAddr;
    type Flags: Copy;
}

pub trait Mmap<S: AddrSpec>: Sized {
    type Config;
    type Error: core::fmt::Debug;

    fn map(
        self,
        start: Option<S::Addr>,
        size: usize,
        flags: S::Flags,
        cfg: Self::Config,
    ) -> Result<RawMemBlk<S, Self>, Self::Error>;
    fn unmap(area: &mut RawMemBlk<S, Self>) -> Result<(), Self::Error>;
}

pub trait Mprotect<S: AddrSpec>: Mmap<S> {
    fn protect(area: &mut RawMemBlk<S, Self>, new_flags: S::Flags) -> Result<(), Self::Error>;
}

pub unsafe trait MemBlkOps {
    #[inline]
    fn start<Addr: MemoryAddr>(&self) -> Addr {
        self.start_ptr().addr().into()
    }

    /// Returns the start pointer of the memory block.
    fn start_ptr(&self) -> *const u8;

    #[inline]
    fn end<Addr: MemoryAddr>(&self) -> Addr {
        self.end_ptr().addr().into()
    }

    fn end_ptr(&self) -> *const u8;

    /// Returns the byte size of the memory block.
    fn size(&self) -> usize;

    /// Returns the start pointer of the memory block.
    ///
    /// ## Safety
    /// The `ptr` should be correctly modified.
    #[inline]
    unsafe fn start_mut_ptr(&self) -> *mut u8 {
        self.start_ptr().cast_mut()
    }

    /// Returns the offset to the start of the memory block.
    ///
    /// ## Safety
    /// - `ptr` must be allocated in the memory.
    #[inline]
    unsafe fn offset<T: ?Sized>(&self, ptr: *const T) -> isize {
        // Safety: `ptr` must has address greater than `self.raw_ptr()`.
        unsafe { ptr.byte_offset_from(self.start_ptr()) }
    }

    /// Returns a pointer to the memory at the given offset.
    #[inline]
    fn get_ptr(&self, offset: usize) -> *const u8 {
        unsafe { self.start_ptr().add(offset as usize) }
    }

    /// Returns a pointer to the memory at the given offset.
    #[inline]
    fn get_ptr_mut(&self, offset: usize) -> *mut u8 {
        unsafe { self.start_mut_ptr().add(offset as usize) }
    }

    /// Given a offset related to start, acquire the `Sized` instance.
    /// from the area.
    ///
    /// ## Panics
    /// `start.add(size_of<T>())` overflows.
    ///
    /// ## Safety
    /// User should ensure the validity of memory area and instance.
    ///
    /// ## Returns
    /// (`*mut T`, `next_offset`)
    /// - `*mut T`: the pointer to the instance.
    /// - `next_offset`: `offset + size_of<T>()`
    #[inline]
    unsafe fn obtain_by_offset<T>(&self, offset: usize) -> Option<(*mut T, usize)> {
        let t_size = core::mem::size_of::<T>();
        let t_align = core::mem::align_of::<T>();

        unsafe {
            let t_start = self.start_ptr().add(offset);
            let new_offset = offset.add(t_size).align_up(t_align);
            if new_offset > self.size() {
                return None;
            }
            let ptr = t_start.cast::<T>().cast_mut();
            Some((ptr, new_offset))
        }
    }

    /// Given a absolute addr, acquire the `Sized` instance.
    /// from the area.
    ///
    /// ## Panics
    /// `start.add(size_of<T>())` overflows.
    ///
    /// ## Safety
    /// User should ensure the validity of memory area and instance.
    ///
    /// ## Returns
    /// (`*mut T`, `next_offset`)
    /// - `*mut T`: the pointer to the instance.
    /// - `next_offset`: `offset + size_of<T>()`
    #[inline]
    unsafe fn obtain_by_addr<T, Addr: MemoryAddr>(&self, start: Addr) -> Option<(*mut T, usize)> {
        if start < self.start() {
            return None;
        }
        let offset = start.sub_addr(self.start());
        unsafe { self.obtain_by_offset(offset) }
    }

    /// Given a offset related to start, acquire the slice instance.
    /// from the area.
    ///
    /// ## Panics
    /// `start.add(size_of<T>())` overflows.
    ///
    /// ## Safety
    /// User should ensure the validity of memory area and instance.
    ///
    /// ## Returns
    /// (`*mut [T]`, `next_offset`)
    /// - `*mut [T]`: the pointer to the slice.
    /// - `next_offset`: `offset + size_of<T>() * len`
    #[inline]
    unsafe fn obtain_slice_by_offset<T>(
        &self,
        offset: usize,
        len: usize,
    ) -> Option<(*mut [T], usize)> {
        let t_size = core::mem::size_of::<T>();
        let t_align = core::mem::align_of::<T>();

        unsafe {
            let t_start = self.start_ptr().add(offset);
            let new_offset = offset.add(t_size).align_up(t_align);
            if new_offset > self.size() {
                return None;
            }
            let ptr = t_start.cast::<T>().cast_mut();
            let ptr = core::slice::from_raw_parts_mut(ptr, len);
            Some((ptr, new_offset))
        }
    }

    /// Given a offset related to start, acquire the slice instance.
    /// from the area.
    ///
    /// ## Panics
    /// `start.add(size_of<T>())` overflows.
    ///
    /// ## Safety
    /// User should ensure the validity of memory area and instance.
    ///
    /// ## Returns
    /// (`*mut [T]`, `next_offset`)
    /// - `*mut [T]`: the pointer to the slice.
    /// - `next_offset`: `offset + size_of<T>() * len`
    #[inline]
    unsafe fn obtain_slice_by_addr<T, Addr: MemoryAddr>(
        &self,
        start: Addr,
        len: usize,
    ) -> Option<(*mut [T], usize)> {
        if start < self.start() {
            return None;
        }
        let offset = start.sub_addr(self.start());
        unsafe { self.obtain_slice_by_offset(offset, len) }
    }
}

pub struct MemBlkSpec<S: AddrSpec> {
    va_range: AddrRange<S::Addr>,
    flags: S::Flags,
}

impl<S: AddrSpec> Clone for MemBlkSpec<S> {
    fn clone(&self) -> Self {
        Self {
            va_range: self.va_range.clone(),
            flags: self.flags.clone(),
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
        let va_range = AddrRange::from_start_size(start, size);
        Self { va_range, flags }
    }

    // #[inline]
    // pub(crate) fn as_addr(&self, offset: usize) -> Option<(S::Addr, usize)> {
    //     let addr = self.start().add(offset);
    //     let size = self.end().checked_sub_addr(addr)?;
    //     Some((addr, size))
    // }
}

impl<S: AddrSpec> MemBlkSpec<S> {
    /// Returns the virtual address range.
    #[inline]
    pub const fn va_range(&self) -> AddrRange<S::Addr> {
        self.va_range
    }

    /// Returns the memory flags, e.g., the permission bits.
    #[inline]
    pub const fn flags(&self) -> S::Flags {
        self.flags
    }

    /// Returns the start address of the memory area.
    #[inline]
    pub const fn start(&self) -> S::Addr {
        self.va_range.start
    }

    /// Returns the end address of the memory area.
    #[inline]
    pub const fn end(&self) -> S::Addr {
        self.va_range.end
    }

    /// Returns the size of the memory area.
    #[inline]
    pub fn size(&self) -> usize {
        self.va_range.size()
    }
}

#[repr(C)]
pub struct Metadata {
    magic: u16,
    status: MapStatus,
    // own counts
    rc: AtomicU32,
}

#[repr(transparent)]
pub struct Header(RwLock<Metadata>);

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapStatus {
    Uninitialized = 0,
    Initializing = 1,
    Initialized = 2,
    Corrupted = 3, // optional
}

impl alloc::fmt::Debug for Metadata {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Metadata of Header")
            .field("magic", &self.magic)
            .field("status", &self.status)
            .field("reference count", &self.rc)
            .finish()
    }
}

impl core::fmt::Display for Metadata {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        alloc::fmt::Debug::fmt(self, f)
    }
}

impl Metadata {
    // TODO
    pub const MAGIC_VALUE: u16 = 0x7203;

    #[inline]
    pub fn attempt_init(&mut self) {
        self.with_magic();
        self.with_status(MapStatus::Initializing);
        self.rc.store(1, core::sync::atomic::Ordering::Relaxed);
    }

    #[inline]
    pub const fn with_magic(&mut self) {
        self.magic = Self::MAGIC_VALUE;
    }

    #[inline]
    pub const fn valid_magic(&self) -> bool {
        self.magic == Self::MAGIC_VALUE
    }

    #[inline]
    pub const fn status(&self) -> MapStatus {
        self.status
    }

    #[inline]
    pub const fn with_status(&mut self, status: MapStatus) {
        self.status = status;
    }

    #[inline]
    pub fn inc_rc(&self) -> u32 {
        self.rc.fetch_add(1, core::sync::atomic::Ordering::AcqRel)
    }

    #[inline]
    pub fn dec_rc(&self) -> Option<u32> {
        use core::sync::atomic::Ordering;
        match self
            .rc
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                if cur == 0 { None } else { Some(cur - 1) }
            }) {
            Ok(prev) => Some(prev),
            Err(_) => None,
        }
    }
}

impl alloc::fmt::Debug for Header {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let h = self.read();
        f.debug_struct("Metadata of Header")
            .field("magic", &h.magic)
            .field("status", &h.status)
            .field("reference count", &h.rc)
            .finish()
    }
}

impl core::fmt::Display for Header {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        alloc::fmt::Debug::fmt(self, f)
    }
}

impl Header {
    const TRYTIMES: u8 = 50;

    pub fn init_with<E, F: FnOnce() -> Result<(), E>>(&self, f: F) -> Result<(), E> {
        let mut inner = self.0.write();
        if inner.valid_magic() {
            inner.inc_rc();
            Ok(())
        } else {
            inner.attempt_init();
            let res = f();
            if res.is_ok() {
                inner.with_status(MapStatus::Initialized);
            } else {
                inner.with_status(MapStatus::Corrupted);
            }
            res
        }
    }

    pub fn claim(&self) -> MapStatus {
        let header_read = self.0.read();
        if header_read.valid_magic() {
            match header_read.status() {
                MapStatus::Initialized => {
                    header_read.inc_rc();
                    return MapStatus::Initialized;
                }
                MapStatus::Initializing => {
                    drop(header_read);
                    for _ in 0..Self::TRYTIMES {
                        let header_try = self.read();
                        match header_try.status() {
                            MapStatus::Initialized => {
                                header_try.inc_rc();
                                return MapStatus::Initialized;
                            }
                            _ => core::hint::spin_loop(),
                        }
                    }
                    return MapStatus::Initializing;
                }
                _ => return MapStatus::Corrupted,
            }
        } else {
            return MapStatus::Uninitialized;
        }
    }
}

impl Deref for Header {
    type Target = RwLock<Metadata>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub enum MmapError<S: AddrSpec, M: Mmap<S>> {
    UnenoughSpace,
    InvalidHeader,
    Contention,
    MapError(M::Error),
}

impl<S: AddrSpec, M: Mmap<S>> core::error::Error for MmapError<S, M> {}

impl<S: AddrSpec, M: Mmap<S>> alloc::fmt::Debug for MmapError<S, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnenoughSpace => write!(f, "Not enough space available"),
            Self::InvalidHeader => write!(f, "Invalid header detected"),
            Self::Contention => write!(f, "Attempt failed due to Contention"),
            Self::MapError(err) => write!(f, "Mapping error: {:?}", err),
        }
    }
}

impl<S: AddrSpec, M: Mmap<S>> core::fmt::Display for MmapError<S, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        alloc::fmt::Debug::fmt(self, f)
    }
}

/// Area without certainty on map, unmap
pub(crate) struct RawMemBlk<S: AddrSpec, M: Mmap<S>> {
    pub a: MemBlkSpec<S>,
    pub bk: M,
}

impl<S: AddrSpec, M: Mmap<S>> RawMemBlk<S, M> {
    /// Create a hallow memory area without any map operation.
    ///
    /// You should only use in `Mmap` trait.
    pub(crate) fn from_raw(start: S::Addr, size: usize, flags: S::Flags, bk: M) -> Self {
        let a = MemBlkSpec::new(start, size, flags);
        Self { a, bk }
    }
}

impl<S: AddrSpec, M: Mmap<S>> Deref for RawMemBlk<S, M> {
    type Target = MemBlkSpec<S>;

    fn deref(&self) -> &Self::Target {
        &self.a
    }
}

unsafe impl<S: AddrSpec, M: Mmap<S>> MemBlkOps for RawMemBlk<S, M> {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.a.start().into() as *const u8
    }

    #[inline]
    fn end_ptr(&self) -> *const u8 {
        self.a.start().into() as *const u8
    }

    #[inline]
    fn size(&self) -> usize {
        self.a.size()
    }
}

pub struct MemBlk<S: AddrSpec, M: Mmap<S>> {
    a: MemBlkSpec<S>,
    bk: M,
}

impl<S: AddrSpec, M: Mmap<S>> Into<MemBlk<S, M>> for RawMemBlk<S, M> {
    fn into(self) -> MemBlk<S, M> {
        let Self { a, bk } = self;
        MemBlk { a, bk }
    }
}

impl<S: AddrSpec, M: Mmap<S>> Clone for MemBlk<S, M>
where
    M: Clone,
{
    fn clone(&self) -> Self {
        Self {
            a: self.a.clone(),
            bk: self.bk.clone(),
        }
    }
}

impl<S: AddrSpec, M: Mmap<S>> Drop for MemBlk<S, M> {
    fn drop(&mut self) {
        let rc = self.header().write().dec_rc();

        rc.map(|s| {
            if s == 1 {
                unsafe {
                    let ptr = self as *mut Self as *mut RawMemBlk<S, M>;
                    let _ = M::unmap(&mut *ptr);
                }
            }
        });
    }
}

unsafe impl<S: AddrSpec, M: Mmap<S>> MemBlkOps for MemBlk<S, M> {
    fn start_ptr(&self) -> *const u8 {
        self.a.start().into() as *const u8
    }

    fn end_ptr(&self) -> *const u8 {
        self.a.end().into() as *const u8
    }

    fn size(&self) -> usize {
        self.a.size()
    }
}

impl<S: AddrSpec, M: Mmap<S>> MemBlk<S, M> {
    // After init_map!
    pub fn header(&self) -> &Header {
        unsafe {
            let ptr = self.a.start().into() as *const Header;
            &*ptr
        }
    }
    pub fn init_map<F: FnOnce() -> Result<(), MmapError<S, M>>>(
        bk: M,
        start: Option<S::Addr>,
        size: usize,
        flags: S::Flags,
        cfg: M::Config,
        f: F,
    ) -> Result<Self, MmapError<S, M>> {
        let area = bk
            .map(start, size, flags, cfg)
            .map_err(MmapError::MapError)?;
        unsafe {
            let (header, _) = area
                .obtain_by_offset::<Header>(0)
                .ok_or(MmapError::UnenoughSpace)?;
            let header_ref = &mut *header;

            match header_ref.claim() {
                MapStatus::Initialized => {}
                MapStatus::Corrupted | MapStatus::Uninitialized => {
                    header_ref.init_with(f)?;
                }
                MapStatus::Initializing => return Err(MmapError::Contention),
            }
        }

        Ok(area.into())
    }
}
