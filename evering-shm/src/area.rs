use core::ptr::NonNull;
use core::{ops::Deref, sync::atomic::AtomicU32};

use crate::header::HeaderStatus;

pub trait AddrSpec {
    type Addr: memory_addr::MemoryAddr;
    type Flags: Copy;
}

pub trait Mmap<S: AddrSpec>: Sized {
    type Handle;
    type MapFlags: Copy;
    type Error: core::fmt::Debug;

    fn map(
        self,
        start: Option<S::Addr>,
        size: usize,
        mflags: Self::MapFlags,
        pflags: S::Flags,
        handle: Self::Handle,
    ) -> Result<RawMemBlk<S, Self>, Self::Error>;
    fn unmap(area: &mut RawMemBlk<S, Self>) -> Result<(), Self::Error>;
}

pub trait Mprotect<S: AddrSpec>: Mmap<S> {
    fn protect(area: &mut RawMemBlk<S, Self>, new_flags: S::Flags) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Copy)]
pub enum Access {
    Write,
    Read,
    ReadWrite,
}

pub trait SharedMmap<S: AddrSpec>: Mmap<S> {
    fn shared(
        self,
        size: usize,
        access: Access,
        handle: Self::Handle,
    ) -> Result<RawMemBlk<S, Self>, Self::Error>;
}

macro_rules! addr_span {
    ($ty:ty) => {
        impl AddrSpan<$ty> {
            #[inline]
            pub const fn null() -> Self {
                Self {
                    start_offset: 0,
                    size: 0,
                }
            }

            #[inline]
            pub const fn is_null(&self) -> bool {
                self.start_offset == 0 || self.size == 0
            }

            #[inline]
            pub const fn end_offset(&self) -> $ty {
                self.start_offset + self.size
            }

            #[inline]
            pub const fn align_of<T>(&self) -> Self {
                use crate::numeric::Alignable;
                Self {
                    start_offset: self.start_offset.align_up_of::<T>(),
                    size: <$ty>::size_of::<T>(),
                }
            }

            #[inline]
            pub const fn align_to(&self, align: $ty) -> Self {
                use crate::numeric::Alignable;
                let aligned_offset = self.start_offset.align_up(align);
                let new_size = self.end_offset() - aligned_offset;

                Self {
                    start_offset: aligned_offset,
                    size: new_size,
                }
            }

            #[inline]
            pub const fn align_to_of<T>(&self) -> Self {
                use crate::numeric::Alignable;
                let aligned_offset = self.start_offset.align_up_of::<T>();
                let new_size = self.end_offset() - aligned_offset;

                Self {
                    start_offset: aligned_offset,
                    size: new_size,
                }
            }

            #[inline]
            pub const fn shift(&self, delta: $ty) -> Self {
                Self {
                    start_offset: self.start_offset.saturating_add(delta),
                    size: self.size,
                }
            }

            #[inline]
            pub const unsafe fn as_ptr(&self, base_ptr: *const u8) -> *const u8 {
                use crate::numeric::CastInto;
                unsafe { base_ptr.add(self.start_offset.cast_into()) }
            }

            #[inline]
            pub const unsafe fn as_mut_ptr(&self, base_ptr: *mut u8) -> *mut u8 {
                use crate::numeric::CastInto;
                unsafe { base_ptr.add(self.start_offset.cast_into()) }
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddrSpan<T> {
    pub start_offset: T,
    pub size: T,
}

impl<T> AddrSpan<T> {
    #[inline]
    pub const fn new(offset: T, size: T) -> Self {
        Self {
            start_offset: offset,
            size,
        }
    }
}

addr_span!(u32);
addr_span!(usize);

pub unsafe trait MemBlkOps {
    #[inline]
    fn start<Addr: memory_addr::MemoryAddr>(&self) -> Addr {
        self.start_ptr().addr().into()
    }

    /// Returns the start pointer of the memory block.
    fn start_ptr(&self) -> *const u8;

    #[inline]
    fn end<Addr: memory_addr::MemoryAddr>(&self) -> Addr {
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
    unsafe fn offset<T: ?Sized>(&self, ptr: *const T) -> usize {
        // Safety: `ptr` must has address greater than `self.start_ptr()`.
        unsafe { ptr.byte_offset_from_unsigned(self.start_ptr()) }
    }

    /// Returns a pointer to the memory at the given offset.
    #[inline]
    fn get_ptr(&self, offset: usize) -> *const u8 {
        unsafe { self.start_ptr().add(offset) }
    }

    /// Returns a pointer to the memory at the given offset.
    #[inline]
    fn get_mut_ptr(&self, offset: usize) -> *mut u8 {
        unsafe { self.start_mut_ptr().add(offset) }
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
    unsafe fn obtain_by_offset<T>(&self, offset: usize) -> Result<(*mut T, usize), usize> {
        use memory_addr::MemoryAddr;
        let t_size = core::mem::size_of::<T>();
        let t_align = core::mem::align_of::<T>();

        let start = self.start_ptr().addr();
        let t_start = start.add(offset).align_up(t_align);
        let new_offset = t_start.add(t_size) - start;
        if new_offset > self.size() {
            return Err(new_offset);
        }
        let ptr = (t_start as *const u8).cast::<T>().cast_mut();
        Ok((ptr, new_offset))
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
    unsafe fn obtain_by_addr<T, Addr: memory_addr::MemoryAddr>(
        &self,
        start: Addr,
    ) -> Result<(*mut T, usize), Option<usize>> {
        if start < self.start() {
            return Err(None);
        }
        let offset = start.sub_addr(self.start());
        unsafe { self.obtain_by_offset(offset).map_err(Option::Some) }
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
    ) -> Result<(*mut [T], usize), Option<usize>> {
        use alloc::alloc::Layout;
        use memory_addr::MemoryAddr;

        let layout = Layout::array::<T>(len).map_err(|_| Option::None)?;
        let arr_size = layout.size();
        let arr_align = layout.align();

        unsafe {
            let start = self.start_ptr().addr();
            let t_start = start.add(offset).align_up(arr_align);
            let new_offset = t_start.add(arr_size) - start;
            if new_offset > self.size() {
                return Err(Some(new_offset));
            }
            let ptr = (t_start as *const u8).cast::<T>().cast_mut();
            let ptr = core::slice::from_raw_parts_mut(ptr, len);
            Ok((ptr, new_offset))
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
    unsafe fn obtain_slice_by_addr<T, Addr: memory_addr::MemoryAddr>(
        &self,
        start: Addr,
        len: usize,
    ) -> Result<(*mut [T], usize), Option<usize>> {
        if start < self.start() {
            return Err(None);
        }
        let offset = start.sub_addr(self.start());
        unsafe { self.obtain_slice_by_offset(offset, len) }
    }
}

unsafe impl<M: MemBlkOps> MemBlkOps for &M {
    fn start_ptr(&self) -> *const u8 {
        (*self).start_ptr()
    }

    fn end_ptr(&self) -> *const u8 {
        (*self).end_ptr()
    }

    fn size(&self) -> usize {
        (*self).size()
    }
}

pub struct MemBlkSpec<S: AddrSpec> {
    range: memory_addr::AddrRange<S::Addr>,
    flags: S::Flags,
}

impl<S: AddrSpec> Clone for MemBlkSpec<S> {
    fn clone(&self) -> Self {
        Self {
            range: self.range.clone(),
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
        let va_range = memory_addr::AddrRange::from_start_size(start, size);
        Self {
            range: va_range,
            flags,
        }
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
    pub const fn va_range(&self) -> memory_addr::AddrRange<S::Addr> {
        self.range
    }

    /// Returns the memory flags, e.g., the permission bits.
    #[inline]
    pub const fn flags(&self) -> S::Flags {
        self.flags
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
    magic: MAGIC,
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
    const MAGIC_VALUE: MAGIC = 0x7203;
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

/// Area without certainty on map, unmap
pub struct RawMemBlk<S: AddrSpec, M: Mmap<S>> {
    pub a: MemBlkSpec<S>,
    pub bk: M,
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
        Self { a, bk }
    }

    pub unsafe fn init_header<T: Layout>(
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
                HeaderStatus::Initializing => return Err(Error::Contention),
                _ => return Err(Error::InvalidHeader),
            }
        }
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

use crate::header::Layout;
use crate::header::MAGIC;
/// A handle of **mapped** memory block.
pub struct MemBlk<S: AddrSpec, M: Mmap<S>> {
    a: MemBlkSpec<S>,
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

impl<S: AddrSpec, M: Mmap<S>> TryFrom<RawMemBlk<S, M>> for MemBlk<S, M> {
    type Error = Error<S, M>;

    fn try_from(area: RawMemBlk<S, M>) -> Result<Self, Self::Error> {
        unsafe { area.init_header::<Header>(0, ())? };
        Ok(unsafe { MemBlk::from_raw(area) })
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
    #[inline(always)]
    pub unsafe fn from_raw(raw: RawMemBlk<S, M>) -> MemBlk<S, M> {
        let RawMemBlk { a, bk } = raw;
        Self { a, bk }
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
        unsafe { self.0.release_of() }
    }
}

impl<S: AddrSpec, M: Mmap<S>> Deref for MemBlkHandle<S, M> {
    type Target = MemBlk<S, M>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<S: AddrSpec, M: Mmap<S>> TryFrom<RawMemBlk<S, M>> for MemBlkHandle<S, M> {
    type Error = Error<S, M>;

    fn try_from(area: RawMemBlk<S, M>) -> Result<Self, Self::Error> {
        let blk = MemBlk::try_from(area)?;
        Ok(MemBlkHandle::from(blk))
    }
}

impl<S: AddrSpec, M: Mmap<S>> From<MemBlk<S, M>> for MemBlkHandle<S, M> {
    fn from(value: MemBlk<S, M>) -> Self {
        Self(crate::counter::CounterOf::suspend(value))
    }
}
