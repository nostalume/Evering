use core::ops::Deref;
use core::ptr::NonNull;

use super::{AddrSpec, MemBlkOps, Mmap, Mprotect};
use crate::{header::{self, Layout, Status}, mem::Error};

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

    #[inline]
    unsafe fn reserve<T: Layout>(&self, offset: usize) -> Result<(*mut T, usize), Error<S, M>> {
        unsafe {
            let (ptr, hoffset) =
                self.obtain_by_offset::<T>(offset)
                    .map_err(|new_offset| Error::UnenoughSpace {
                        requested: new_offset,
                        allocated: self.size(),
                    })?;
            Ok((ptr, hoffset))
        }
    }

    #[inline]
    unsafe fn commit<T: Layout>(
        &self,
        header: *mut T,
        conf: T::Config,
    ) -> Result<NonNull<T>, Error<S, M>> {
        unsafe {
            let header_ref = Layout::from_raw(header);
            match header_ref.attach_or_init(conf) {
                Status::Initialized => Ok(NonNull::new_unchecked(header)),
                Status::Initializing => Err(Error::Contention),
                _ => Err(Error::InvalidHeader),
            }
        }
    }

    unsafe fn push<T: Layout>(
        &self,
        offset: usize,
        conf: T::Config,
    ) -> Result<(NonNull<T>, usize), Error<S, M>> {
        unsafe {
            let (ptr, next) = self.reserve::<T>(offset)?;
            let ptr = self.commit(ptr, conf)?;
            Ok((ptr, next))
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

pub struct Reserve<T> {
    ptr: *mut T,
    next: usize,
}

impl<S: AddrSpec, M: Mmap<S>> MemBlkLayout<S, M> {
    #[inline]
    pub fn new(area: RawMemBlk<S, M>) -> Result<Self, Error<S, M>> {
        let (_, offset) = unsafe { area.push::<RcHeader>(0, ()) }?;
        Ok(Self { area, offset })
    }

    #[inline]
    pub fn forward(self, forward: usize) -> Self {
        Self {
            area: self.area,
            offset: self.offset + forward,
        }
    }

    #[inline]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    #[inline]
    pub fn reserve<T: Layout>(&mut self) -> Result<Reserve<T>, Error<S, M>> {
        let (ptr, next) = unsafe { self.area.reserve::<T>(self.offset) }?;
        let reserve = Reserve { ptr, next };
        self.offset = next;
        Ok(reserve)
    }

    #[inline]
    pub fn commit<T: Layout>(
        &mut self,
        reserve: Reserve<T>,
        conf: T::Config,
    ) -> Result<NonNull<T>, Error<S, M>> {
        let ptr = unsafe { self.area.commit(reserve.ptr, conf) }?;
        Ok(ptr)
    }

    #[inline]
    pub fn push<T: Layout>(&mut self, conf: T::Config) -> Result<NonNull<T>, Error<S, M>> {
        let (ptr, next) = unsafe { self.area.push::<T>(self.offset, conf) }?;
        self.offset = next;
        Ok(ptr)
    }

    pub fn finish(self) -> (MemBlk<S, M>, usize) {
        let Self { area, offset } = self;
        (unsafe { MemBlk::from_raw(area) }, offset)
    }
}

impl<T: Layout> Reserve<T> {
    #[inline]
    pub const fn next(&self) -> usize {
        self.next
    }

    #[inline]
    pub const fn as_ptr(&self) -> *mut T {
        self.ptr
    }

    #[inline]
    pub fn as_offset(&self) -> usize {
        self.ptr.addr()
    }

    #[inline]
    pub const fn size(&self) -> usize {
        use core::mem;
        mem::size_of::<T>()
    }
}

/// A handle of **mapped** memory block.
pub struct MemBlk<S: AddrSpec, M: Mmap<S>> {
    spec: MemBlkSpec<S>,
    bk: M,
}

pub type RcHeader = header::Header<header::RcMeta>;

pub struct MemBlkHandle<S: AddrSpec, M: Mmap<S>>(crate::counter::CounterOf<MemBlk<S, M>>);

impl<S: AddrSpec, M: Mmap<S>> Drop for MemBlk<S, M> {
    fn drop(&mut self) {
        use header::Finalize;
        // Safety: finalize only once and always true!
        let _ = unsafe { self.header().finalize() };
        let blk = unsafe { self.as_raw() };
        let _ = M::unmap(blk);
    }
}

impl<S: AddrSpec, M: Mmap<S>> TryFrom<MemBlkLayout<S, M>> for MemBlk<S, M> {
    type Error = Error<S, M>;

    fn try_from(area: MemBlkLayout<S, M>) -> Result<Self, Self::Error> {
        let mut area = area;
        area.push::<RcHeader>(())?;
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
    unsafe fn as_raw(&mut self) -> &mut RawMemBlk<S, M> {
        unsafe { &mut *(self as *mut _ as *mut _) }
    }

    #[inline(always)]
    pub fn header(&self) -> &RcHeader {
        unsafe {
            let ptr = self.start_ptr() as *const RcHeader;
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

pub struct MemRef<T: ?Sized, S: AddrSpec, M: Mmap<S>> {
    handle: MemBlkHandle<S, M>,
    ptr: NonNull<T>,
}

impl<T: ?Sized + core::fmt::Debug, S: AddrSpec, M: Mmap<S>> Clone for MemRef<T, S, M> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            ptr: self.ptr,
        }
    }
}

impl<T: ?Sized + core::fmt::Debug, S: AddrSpec, M: Mmap<S>> core::fmt::Debug for MemRef<T, S, M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ptr = unsafe { self.ptr.as_ref() };
        core::fmt::Debug::fmt(ptr, f)
    }
}

impl<T: ?Sized, S: AddrSpec, M: Mmap<S>> const Deref for MemRef<T, S, M> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: ?Sized, S: AddrSpec, M: Mmap<S>> MemRef<T, S, M> {
    pub unsafe fn from_raw(handle: MemBlkHandle<S, M>, ptr: NonNull<T>) -> Self {
        Self { handle, ptr }
    }

    pub fn map<U>(&self, f: impl FnOnce(&T) -> &U) -> MemRef<U, S, M> {
        let u = f(self);
        MemRef {
            handle: self.handle.clone(),
            ptr: u.into(),
        }
    }

    pub fn try_map<E, U>(&self, f: impl FnOnce(&T) -> Result<&U, E>) -> Result<MemRef<U, S, M>, E> {
        let u = f(self)?;
        Ok(MemRef {
            handle: self.handle.clone(),
            ptr: u.into(),
        })
    }

    pub fn may_map<U>(&self, f: impl FnOnce(&T) -> Option<&U>) -> Option<MemRef<U, S, M>> {
        let u = f(self)?;
        Some(MemRef {
            handle: self.handle.clone(),
            ptr: u.into(),
        })
    }
}
