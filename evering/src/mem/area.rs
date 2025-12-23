use core::ops::Deref;
use core::ptr::NonNull;

use super::{AddrSpec, MemOps, Mmap, Mprotect};
use crate::{
    counter,
    header::{self, Layout, RcHeader, Status},
    mem::{Access, Accessible, Error},
};

pub struct MapSpec<S: AddrSpec> {
    range: memory_addr::AddrRange<S::Addr>,
    flags: S::Flags,
}

impl<S: AddrSpec> core::fmt::Debug for MapSpec<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemBlkSpec")
            .field("start", &self.range.start.into())
            .field("size", &self.range.size())
            .finish()
    }
}

impl<S: AddrSpec> Clone for MapSpec<S> {
    fn clone(&self) -> Self {
        Self {
            range: self.range,
            flags: self.flags,
        }
    }
}

impl<S: AddrSpec> AddrSpec for MapSpec<S> {
    type Addr = S::Addr;
    type Flags = S::Flags;
}

impl<S: AddrSpec> MapSpec<S> {
    pub fn new(start: S::Addr, size: usize, flags: S::Flags) -> Self {
        let va_range = memory_addr::AddrRange::from_start_size(start, size);
        Self {
            range: va_range,
            flags,
        }
    }
}

impl<S: AddrSpec> MapSpec<S> {
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

pub struct RawMap<S: AddrSpec, M: Mmap<S>> {
    pub spec: MapSpec<S>,
    pub bk: M,
}

impl<S: AddrSpec, M: Mmap<S>> core::fmt::Debug for RawMap<S, M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawMap").field("area", &self.spec).finish()
    }
}

unsafe impl<S: AddrSpec, M: Mmap<S>> MemOps for RawMap<S, M> {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.spec.start().into() as *const u8
    }

    #[inline]
    fn end_ptr(&self) -> *const u8 {
        self.spec.end().into() as *const u8
    }

    #[inline]
    fn size(&self) -> usize {
        self.spec.size()
    }
}

impl<S: AddrSpec, M: Mmap<S>> Deref for RawMap<S, M> {
    type Target = MapSpec<S>;

    fn deref(&self) -> &Self::Target {
        &self.spec
    }
}

impl<S: AddrSpec, M: Mmap<S>> RawMap<S, M> {
    pub fn unmap(area: Self) -> Result<(), M::Error> {
        let mut area = area;
        M::unmap(&mut area)
    }
}

impl<S: AddrSpec, M: Mprotect<S>> RawMap<S, M> {
    pub unsafe fn protect(&mut self, flags: S::Flags) -> Result<(), M::Error> {
        (unsafe { M::protect(self, flags) })?;
        self.spec.flags = flags;
        Ok(())
    }
}

impl<S: AddrSpec, M: Mmap<S>> RawMap<S, M> {
    pub unsafe fn from_ptr<T>(start: NonNull<T>, size: usize, flags: S::Flags, bk: M) -> Self {
        unsafe { Self::from_raw(start.addr().get().into(), size, flags, bk) }
    }

    /// Create a hallow memory area without any map operation.
    ///
    /// You should only use in `Mmap` trait.
    #[inline]
    pub unsafe fn from_raw(start: S::Addr, size: usize, flags: S::Flags, bk: M) -> Self {
        let spec = MapSpec::new(start, size, flags);
        Self { spec, bk }
    }

    #[inline]
    pub fn permits(&self, access: Access) -> Result<(), Error<S, M>> {
        if !self.spec.flags.permits(access) {
            return Err(Error::PermissionDenied { requested: access });
        }
        Ok(())
    }

    #[inline]
    unsafe fn reserve<T: Layout>(&self, offset: usize) -> Result<(*mut T, usize), Error<S, M>> {
        self.permits(Access::WRITE)?;
        unsafe {
            let (ptr, hoffset) =
                self.obtain_by_offset::<T>(offset)
                    .map_err(|new_offset| Error::UnenoughSpace {
                        requested: new_offset,
                        allocated: self.size(),
                    })?;

            #[cfg(feature = "tracing")]
            tracing::debug!("[Area]: reserve offset, old {}, new {}", offset, hoffset);

            Ok((ptr, hoffset))
        }
    }

    #[inline]
    unsafe fn commit<T: Layout>(
        &self,
        header: *mut T,
        conf: T::Config,
    ) -> Result<NonNull<T>, Error<S, M>> {
        self.permits(Access::WRITE)?;
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

struct Map<S: AddrSpec, M: Mmap<S>> {
    raw: RawMap<S, M>,
    header: NonNull<RcHeader>,
}

unsafe impl<S: AddrSpec, M: Mmap<S>> MemOps for Map<S, M> {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.raw.spec.start().into() as *const u8
    }

    #[inline]
    fn end_ptr(&self) -> *const u8 {
        self.raw.spec.end().into() as *const u8
    }

    #[inline]
    fn size(&self) -> usize {
        self.raw.spec.size()
    }
}

impl<S: AddrSpec, M: Mmap<S>> core::fmt::Debug for Map<S, M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Map")
            .field("raw", &self.raw)
            .field("header", &self.header)
            .finish()
    }
}

impl<S: AddrSpec, M: Mmap<S>> Drop for Map<S, M> {
    fn drop(&mut self) {
        use header::Finalize;
        unsafe { self.header().finalize() };

        #[cfg(feature = "tracing")]
        tracing::debug!("[Area]: header: {:?} unmap...", self.header());

        M::unmap(&mut self.raw).unwrap();
    }
}

impl<S: AddrSpec, M: Mmap<S>> Map<S, M> {
    fn new(raw: RawMap<S, M>) -> Result<(Self, usize), Error<S, M>> {
        let (header, offset) = unsafe { raw.push::<RcHeader>(0, ())? };
        Ok((Self { raw, header }, offset))
    }

    fn header(&self) -> &RcHeader {
        unsafe { self.header.as_ref() }
    }
}

pub struct Reserve<T> {
    ptr: *mut T,
    next: usize,
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
    pub const fn size(&self) -> usize {
        use core::mem;
        mem::size_of::<T>()
    }
}

#[repr(transparent)]
struct SuspendMap<S: AddrSpec, M: Mmap<S>>(counter::CounterOf<Map<S, M>>);

impl<S: AddrSpec, M: Mmap<S>> core::fmt::Debug for SuspendMap<S, M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        core::fmt::Debug::fmt(&**self, f)
    }
}

impl<S: AddrSpec, M: Mmap<S>> Deref for SuspendMap<S, M> {
    type Target = Map<S, M>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<S: AddrSpec, M: Mmap<S>> Clone for SuspendMap<S, M> {
    fn clone(&self) -> Self {
        Self(self.0.acquire())
    }
}

impl<S: AddrSpec, M: Mmap<S>> Drop for SuspendMap<S, M> {
    fn drop(&mut self) {
        unsafe { self.0.release() }
    }
}

impl<S: AddrSpec, M: Mmap<S>> SuspendMap<S, M> {
    fn new(area: Map<S, M>) -> Self {
        Self(counter::CounterOf::suspend(area))
    }
}

/// Manages the incremental layout and allocation of objects within a memory-mapped area,
/// allowing reservation of space and commitment with configuration.
pub struct MapLayout<S: AddrSpec, M: Mmap<S>> {
    area: SuspendMap<S, M>,
    offset: usize,
}

unsafe impl<S: AddrSpec, M: Mmap<S>> MemOps for MapLayout<S, M> {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.area.start_ptr()
    }

    #[inline]
    fn end_ptr(&self) -> *const u8 {
        self.area.end_ptr()
    }

    #[inline]
    fn size(&self) -> usize {
        self.area.size()
    }
}

impl<S: AddrSpec, M: Mmap<S>> MapLayout<S, M> {
    /// Returns a reference to the underlying raw map.
    #[inline]
    fn as_raw(&self) -> &RawMap<S, M> {
        &self.area.raw
    }

    /// Creates a new layout manager from a raw map, initializing the header and offset.
    #[inline]
    pub fn new(raw: RawMap<S, M>) -> Result<Self, Error<S, M>> {
        let (area, offset) = Map::new(raw)?;
        let area = SuspendMap::new(area);
        Ok(Self { area, offset })
    }

    /// Advances the current offset by the specified amount, returning a new layout.
    #[inline]
    pub fn forward(self, forward: usize) -> Self {
        Self {
            area: self.area,
            offset: self.offset + forward,
        }
    }

    /// Returns the current offset within the memory area.
    #[inline]
    pub const fn cur_offset(&self) -> usize {
        self.offset
    }

    /// Calculates the offset of the given reserve's pointer relative to the layout's start.
    #[inline]
    pub fn ptr_offset<T>(&self, reserve: &Reserve<T>) -> usize {
        unsafe { self.offset(reserve.ptr) }
    }

    /// Returns the remaining size available for allocation.
    #[inline]
    pub fn rest_size(&self) -> usize {
        self.area.size() - self.offset
    }

    /// Reserves space for a type `T` at the current offset, advancing the offset.
    #[inline]
    pub fn reserve<T: Layout>(&mut self) -> Result<Reserve<T>, Error<S, M>> {
        let (ptr, next) = unsafe { self.as_raw().reserve::<T>(self.offset) }?;
        let reserve = Reserve { ptr, next };
        self.offset = next;
        Ok(reserve)
    }

    /// Commits a reserved space with the given configuration, returning a handle to the object.
    #[inline]
    pub fn commit<T: Layout>(
        &mut self,
        reserve: Reserve<T>,
        conf: T::Config,
    ) -> Result<MapHandle<T, S, M>, Error<S, M>> {
        let ptr = unsafe { self.as_raw().commit(reserve.ptr, conf) }?;
        let handle = unsafe { MapHandle::from_raw(self.area.clone(), ptr) };
        Ok(handle)
    }

    /// Reserves and commits space for a type `T` in one step, advancing the offset.
    #[inline]
    pub fn push<T: Layout>(&mut self, conf: T::Config) -> Result<MapHandle<T, S, M>, Error<S, M>> {
        let (ptr, next) = unsafe { self.as_raw().push::<T>(self.offset, conf) }?;
        self.offset = next;
        let handle = unsafe { MapHandle::from_raw(self.area.clone(), ptr) };
        Ok(handle)
    }

    /// Finalizes the layout and returns the total offset used.
    pub fn finish(self) -> usize {
        self.offset
    }
}

pub struct MapHandle<T: ?Sized, S: AddrSpec, M: Mmap<S>> {
    handle: SuspendMap<S, M>,
    ptr: NonNull<T>,
}
pub type MapView<S, M> = MapHandle<(), S, M>;

unsafe impl<T: ?Sized + Send, S: AddrSpec, M: Mmap<S>> Send for MapHandle<T, S, M> {}
unsafe impl<T: ?Sized, S: AddrSpec, M: Mmap<S>> Sync for MapHandle<T, S, M> {}

impl<T: ?Sized + core::fmt::Debug, S: AddrSpec, M: Mmap<S>> core::fmt::Debug
    for MapHandle<T, S, M>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapHandle")
            .field("handle", &self.handle)
            .field("ptr", &self.ptr)
            .finish()
    }
}

impl<T: ?Sized, S: AddrSpec, M: Mmap<S>> const Deref for MapHandle<T, S, M> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: ?Sized, S: AddrSpec, M: Mmap<S>> Clone for MapHandle<T, S, M> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            ptr: self.ptr,
        }
    }
}

impl<S: AddrSpec, M: Mmap<S>> TryFrom<MapLayout<S, M>> for MapView<S, M> {
    type Error = Error<S, M>;

    fn try_from(mut value: MapLayout<S, M>) -> Result<Self, Self::Error> {
        let area = value.push::<()>(())?;
        Ok(area)
    }
}

impl<S: AddrSpec, M: Mmap<S>> MapView<S, M> {
    #[inline]
    pub fn header(&self) -> &RcHeader {
        self.handle.header()
    }
}

impl<T: ?Sized, S: AddrSpec, M: Mmap<S>> MapHandle<T, S, M> {
    unsafe fn from_raw(handle: SuspendMap<S, M>, ptr: NonNull<T>) -> Self {
        Self { handle, ptr }
    }

    pub fn map<U>(&self, f: impl FnOnce(&T) -> &U) -> MapHandle<U, S, M> {
        let u = f(self);
        MapHandle {
            handle: self.handle.clone(),
            ptr: u.into(),
        }
    }

    pub fn try_map<E, U>(
        &self,
        f: impl FnOnce(&T) -> Result<&U, E>,
    ) -> Result<MapHandle<U, S, M>, E> {
        let u = f(self)?;
        Ok(MapHandle {
            handle: self.handle.clone(),
            ptr: u.into(),
        })
    }

    pub fn may_map<U>(&self, f: impl FnOnce(&T) -> Option<&U>) -> Option<MapHandle<U, S, M>> {
        let u = f(self)?;
        Some(MapHandle {
            handle: self.handle.clone(),
            ptr: u.into(),
        })
    }
}
