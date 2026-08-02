use alloc::sync::Arc;
use core::ptr::NonNull;
use core::{marker::PhantomData, ops::Deref};

use crate::{
    header::{AdmitError, AdmitLayout, Layout, Member, RootHeader},
    schema::{LayoutContext, RegionAdmission, RegionId},
};

pub use crate::header::LayoutField;
#[cfg(test)]
pub use alloc::alloc::AllocError;

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Access: u8 {
        const READ = 0x1;
        const WRITE = 0x1 << 1;
        const EXEC = 0x1 << 2;
    }
}

impl core::fmt::Display for Access {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub len: usize,
    pub access: Access,
}

impl Request {
    pub const fn new(len: usize, access: Access) -> Self {
        Self { len, access }
    }
}

/// Produces one exclusively owned process-local mapping.
///
/// # Safety
///
/// An implementation must return a valid, suitably aligned mapping with the
/// requested access and extent. Its release function must remain callable and
/// release that mapping exactly once from any thread that may own it.
pub unsafe trait Source: Sized {
    type Error: core::fmt::Debug;

    fn map(self, request: Request) -> Result<Map, Self::Error>;
}

pub enum Error {
    PermissionDenied { requested: Access },
    UnenoughSpace { requested: usize, allocated: usize },
    Contention,
    LayoutClosed,
    DuplicateAttachment,
    InvalidHeader,
    LayoutMismatch(LayoutField),
    PoisonedComposition,
    ArithmeticOverflow,
    ParticipantExhausted,
}

impl core::error::Error for Error {}

impl core::fmt::Debug for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PermissionDenied { requested } => {
                write!(f, "Permission denied, requested {requested:?}")
            }
            Self::UnenoughSpace {
                requested,
                allocated,
            } => write!(
                f,
                "Not enough space available, requested {requested}, allocated {allocated}"
            ),
            Self::Contention => f.write_str("Contention"),
            Self::LayoutClosed => f.write_str("Shared layout is closed"),
            Self::DuplicateAttachment => {
                f.write_str("This participant already attached the shared layout")
            }
            Self::InvalidHeader => f.write_str("Header initialization failed"),
            Self::LayoutMismatch(field) => write!(f, "Layout mismatch: {field:?}"),
            Self::PoisonedComposition => f.write_str("Layout composition is poisoned"),
            Self::ArithmeticOverflow => f.write_str("Layout cursor arithmetic overflow"),
            Self::ParticipantExhausted => f.write_str("No shared-memory participant slot is free"),
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

/// Exposes one live contiguous memory extent.
///
/// # Safety
///
/// `start_ptr` and `size` must remain valid for the implementation's lifetime.
pub unsafe trait MemOps {
    fn start_ptr(&self) -> *const u8;
    fn size(&self) -> usize;

    #[inline]
    unsafe fn start_mut_ptr(&self) -> *mut u8 {
        self.start_ptr().cast_mut()
    }

    #[inline]
    unsafe fn offset<T: ?Sized>(&self, ptr: *const T) -> usize {
        unsafe { ptr.byte_offset_from_unsigned(self.start_ptr()) }
    }
}

unsafe impl<M: MemOps> MemOps for &M {
    fn start_ptr(&self) -> *const u8 {
        (*self).start_ptr()
    }

    fn size(&self) -> usize {
        (*self).size()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddrSpan<T> {
    pub start_offset: T,
    pub size: T,
}

impl<T> AddrSpan<T> {
    pub const fn new(start_offset: T, size: T) -> Self {
        Self { start_offset, size }
    }
}

impl AddrSpan<usize> {
    pub const fn null() -> Self {
        Self::new(0, 0)
    }

    pub const fn is_null(&self) -> bool {
        self.start_offset == 0 || self.size == 0
    }

    pub const unsafe fn as_nonnull(&self, base: *const u8) -> NonNull<u8> {
        unsafe { NonNull::new_unchecked(base.add(self.start_offset).cast_mut()) }
    }
}

/// Linear owner of one process-local mapping.
pub struct Map {
    start: NonNull<u8>,
    len: usize,
    access: Access,
    release: Option<unsafe fn(NonNull<u8>, usize) -> bool>,
}

impl core::fmt::Debug for Map {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Map")
            .field("start", &self.start)
            .field("len", &self.len)
            .field("access", &self.access)
            .finish()
    }
}

unsafe impl MemOps for Map {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.start.as_ptr()
    }

    #[inline]
    fn size(&self) -> usize {
        self.len
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = unsafe { release(self.start, self.len) };
        }
    }
}

impl Map {
    /// Creates an owner for an externally established mapping.
    ///
    /// # Safety
    ///
    /// `start..start + len` must remain exclusively owned and valid for the
    /// declared access until `release` is invoked. `release` must release that
    /// exact mapping and must be safe to call exactly once from any thread.
    pub const unsafe fn from_raw_parts(
        start: NonNull<u8>,
        len: usize,
        access: Access,
        release: unsafe fn(NonNull<u8>, usize) -> bool,
    ) -> Self {
        Self {
            start,
            len,
            access,
            release: Some(release),
        }
    }

    #[inline]
    pub fn permits(&self, access: Access) -> Result<(), Error> {
        if !self.access.contains(access) {
            return Err(Error::PermissionDenied { requested: access });
        }
        Ok(())
    }

    fn close(mut self) -> bool {
        let release = self.release.take().expect("live map has release authority");
        unsafe { release(self.start, self.len) }
    }

    #[inline]
    unsafe fn reserve<T: Layout>(&self, offset: usize) -> Result<(*mut T, usize), Error> {
        self.permits(Access::WRITE)?;
        let align = core::mem::align_of::<T>();
        let start = self.start_ptr().addr();
        let candidate = start.checked_add(offset).ok_or(Error::ArithmeticOverflow)?;
        let aligned_address = candidate
            .checked_add(align - 1)
            .map(|value| value & !(align - 1))
            .ok_or(Error::ArithmeticOverflow)?;
        let end = aligned_address
            .checked_add(core::mem::size_of::<T>())
            .ok_or(Error::ArithmeticOverflow)?;
        let aligned = aligned_address
            .checked_sub(start)
            .ok_or(Error::ArithmeticOverflow)?;
        let next = end.checked_sub(start).ok_or(Error::ArithmeticOverflow)?;
        if next > self.size() {
            return Err(Error::UnenoughSpace {
                requested: next,
                allocated: self.size(),
            });
        }
        let ptr = unsafe { self.start_mut_ptr().add(aligned).cast::<T>() };
        #[cfg(feature = "tracing")]
        tracing::debug!("[Area]: reserve offset, old {}, new {}", offset, next);
        Ok((ptr, next))
    }

    #[inline]
    unsafe fn commit<T: AdmitLayout>(
        &self,
        header: *mut T,
        conf: T::Config,
        ctx: LayoutContext,
    ) -> Result<NonNull<T>, Error> {
        self.permits(Access::WRITE)?;
        match unsafe { T::admit(header, conf, ctx, None) } {
            Ok(()) => Ok(unsafe { NonNull::new_unchecked(header) }),
            Err(AdmitError::Contention) => Err(Error::Contention),
            Err(AdmitError::Closed) => Err(Error::LayoutClosed),
            Err(AdmitError::DuplicateMember) => Err(Error::DuplicateAttachment),
            Err(AdmitError::InvalidHeader) => Err(Error::InvalidHeader),
            Err(AdmitError::Mismatch(field)) => Err(Error::LayoutMismatch(field)),
        }
    }
}

/// Immutable process-local authority for one mapped shared-memory region.
///
/// The mapping backend is erased after successful root admission. The only
/// retained backend operation is final unmap.
pub(crate) struct Region {
    map: Option<Map>,
    header: NonNull<RootHeader>,
    member: Member,
    region: RegionId,
    allow_init: bool,
    detached: bool,
}

// Region exposes only atomic root operations; typed projections independently
// require `T: Sync`. Drop has unique ownership, and Map admission requires its
// release function to be callable from any owning thread.
unsafe impl Send for Region {}
unsafe impl Sync for Region {}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseError {
    InUse,
    UnmapFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionError {
    ReadOnly,
    ArithmeticOverflow,
    OutOfBounds,
    Misaligned,
    Admission(AdmitError),
}

unsafe impl MemOps for Region {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.map().start_ptr()
    }

    #[inline]
    fn size(&self) -> usize {
        self.map().size()
    }
}

impl core::fmt::Debug for Region {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Region")
            .field("map", &self.map)
            .field("region", &self.region)
            .finish()
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        self.detach();
        let _ = self.unmap();
    }
}

impl Region {
    fn new(map: Map, admission: RegionAdmission) -> Result<(Arc<Self>, usize), Error> {
        let (ptr, offset) = unsafe { map.reserve::<RootHeader>(0) }?;
        let region = match admission {
            RegionAdmission::Create(id) | RegionAdmission::Expect(id) => id,
            RegionAdmission::Discover => match unsafe { RootHeader::published_region_at(ptr) } {
                Some(region) => region,
                None => return Err(Error::InvalidHeader),
            },
        };
        let allow_init = matches!(admission, RegionAdmission::Create(_));
        let ctx = LayoutContext {
            region,
            offset: 0,
            allow_init,
        };
        let header = unsafe { map.commit(ptr, (), ctx) }?;
        let member = match unsafe { header.as_ref() }.inner.join() {
            Some(member) => member,
            None => return Err(Error::ParticipantExhausted),
        };
        Ok((
            Arc::new(Self {
                map: Some(map),
                header,
                member,
                region,
                allow_init,
                detached: false,
            }),
            offset,
        ))
    }

    fn header(&self) -> &RootHeader {
        unsafe { self.header.as_ref() }
    }

    fn map(&self) -> &Map {
        self.map.as_ref().expect("live region owns its map")
    }

    fn permits(&self, access: Access) -> Result<(), Error> {
        self.map().permits(access)
    }

    unsafe fn reserve<T: Layout>(&self, offset: usize) -> Result<(*mut T, usize), Error> {
        unsafe { self.map().reserve::<T>(offset) }
    }

    unsafe fn commit<T: AdmitLayout>(
        &self,
        pointer: *mut T,
        conf: T::Config,
        ctx: LayoutContext,
    ) -> Result<NonNull<T>, Error> {
        self.permits(Access::WRITE)?;
        match unsafe { T::admit(pointer, conf, ctx, Some(self.member.slot)) } {
            Ok(()) => Ok(unsafe { NonNull::new_unchecked(pointer) }),
            Err(AdmitError::Contention) => Err(Error::Contention),
            Err(AdmitError::Closed) => Err(Error::LayoutClosed),
            Err(AdmitError::DuplicateMember) => Err(Error::DuplicateAttachment),
            Err(AdmitError::InvalidHeader) => Err(Error::InvalidHeader),
            Err(AdmitError::Mismatch(field)) => Err(Error::LayoutMismatch(field)),
        }
    }

    fn unmap(&mut self) -> bool {
        let Some(map) = self.map.take() else {
            return true;
        };
        map.close()
    }

    fn detach(&mut self) {
        if self.detached {
            return;
        }
        let _ = self.header().inner.leave(self.member);
        self.detached = true;
    }

    #[cfg(test)]
    pub(crate) fn close(region: Arc<Self>) -> Result<(), CloseError> {
        let mut region = Arc::try_unwrap(region).map_err(|_| CloseError::InUse)?;
        region.detach();
        if region.unmap() {
            Ok(())
        } else {
            Err(CloseError::UnmapFailed)
        }
    }

    pub fn peer(&self) -> Peer {
        Peer(self.member)
    }

    /// Marks an exact participant generation as permanently unable to access
    /// this region.
    ///
    /// # Safety
    ///
    /// The caller must know that the process represented by `peer` can never
    /// again access this mapping. A timeout, task cancellation, or thread exit
    /// is not sufficient evidence.
    pub(crate) unsafe fn mark_dead(&self, peer: Peer) -> Option<Member> {
        let root = &self.header().inner;
        (root.mark_dead(peer.0) || root.is_dead(peer.0)).then_some(peer.0)
    }

    pub(crate) fn release_dead(&self, member: Member) -> bool {
        self.header().inner.release_dead(member)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer(Member);

impl Peer {
    pub const fn from_parts(slot: u8, generation: usize) -> Self {
        Self(Member { slot, generation })
    }

    pub const fn slot(self) -> u8 {
        self.0.slot
    }

    pub const fn generation(self) -> usize {
        self.0.generation
    }
}

pub(crate) struct Reservation<'a, T: AdmitLayout> {
    layout: &'a mut Build,
    offset: usize,
    next: usize,
    _marker: PhantomData<fn() -> T>,
}

impl<T: AdmitLayout> Reservation<'_, T> {
    #[inline]
    pub fn remaining_after(&self) -> usize {
        self.layout.size().saturating_sub(self.next)
    }

    pub fn commit(self, conf: T::Config) -> Result<Mapped<T>, Error> {
        self.layout.commit_reserved(self.offset, self.next, conf)
    }
}

pub(crate) struct Build {
    area: Arc<Region>,
    offset: usize,
    poisoned: bool,
}

unsafe impl MemOps for Build {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.area.start_ptr()
    }

    #[inline]
    fn size(&self) -> usize {
        self.area.size()
    }
}

impl Build {
    /// Creates a new layout manager from a raw map, initializing the header and offset.
    #[inline]
    pub(crate) fn new(map: Map, admission: RegionAdmission) -> Result<Self, Error> {
        let (area, offset) = Region::new(map, admission)?;
        Ok(Self {
            area,
            offset,
            poisoned: false,
        })
    }

    pub fn region_id(&self) -> RegionId {
        self.area.region
    }

    /// Reserves space for `T`, exclusively borrowing this composition until commit.
    #[inline]
    pub(crate) fn reserve<T: AdmitLayout>(&mut self) -> Result<Reservation<'_, T>, Error> {
        if self.poisoned {
            return Err(Error::PoisonedComposition);
        }
        let (ptr, next) = unsafe { self.area.reserve::<T>(self.offset) }?;
        let offset = unsafe { self.area.offset(ptr) };
        Ok(Reservation {
            layout: self,
            offset,
            next,
            _marker: PhantomData,
        })
    }

    fn commit_reserved<T: AdmitLayout>(
        &mut self,
        offset: usize,
        expected_next: usize,
        conf: T::Config,
    ) -> Result<Mapped<T>, Error> {
        if self.poisoned {
            return Err(Error::PoisonedComposition);
        }
        let (ptr, next) = match unsafe { self.area.reserve::<T>(self.offset) } {
            Ok(value) => value,
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        };
        if unsafe { self.area.offset(ptr) } != offset || next != expected_next {
            self.poisoned = true;
            return Err(Error::InvalidHeader);
        }
        let ctx = LayoutContext {
            region: self.region_id(),
            offset: offset as u64,
            allow_init: self.area.allow_init,
        };
        let ptr = match unsafe { self.area.commit::<T>(ptr, conf, ctx) } {
            Ok(ptr) => ptr,
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        };
        self.offset = next;
        let handle = unsafe { Mapped::from_raw(Arc::clone(&self.area), ptr) };
        Ok(handle)
    }

    /// Reserves and commits space for a type `T` in one step, advancing the offset.
    #[inline]
    pub(crate) fn push<T: AdmitLayout>(&mut self, conf: T::Config) -> Result<Mapped<T>, Error> {
        self.reserve::<T>()?.commit(conf)
    }

    #[cfg(test)]
    pub(crate) fn header(&self) -> &RootHeader {
        self.area.header()
    }

    #[cfg(test)]
    pub(crate) fn close(self) -> Result<(), CloseError> {
        Region::close(self.area)
    }
}

pub(crate) struct Mapped<T: AdmitLayout> {
    handle: Arc<Region>,
    ptr: NonNull<T>,
    attached: bool,
}
// Safety: Region erasure admits only cache-coherent process-shared mappings;
// the projected layout controls whether immutable cross-thread access is safe.
unsafe impl<T: AdmitLayout + Sync> Send for Mapped<T> {}
unsafe impl<T: AdmitLayout + Sync> Sync for Mapped<T> {}

impl<T: AdmitLayout + core::fmt::Debug> core::fmt::Debug for Mapped<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Mapped")
            .field("handle", &self.handle)
            .field("ptr", &self.ptr)
            .finish()
    }
}

impl<T: AdmitLayout> Drop for Mapped<T> {
    fn drop(&mut self) {
        if self.attached {
            unsafe { T::leave(self.ptr.as_ptr(), self.handle.member.slot) };
        }
    }
}

impl<T: AdmitLayout> const Deref for Mapped<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: AdmitLayout> Mapped<T> {
    pub(crate) fn pointer(&self) -> NonNull<T> {
        self.ptr
    }

    pub(crate) fn disarm_detach(&mut self) {
        self.attached = false;
    }

    pub fn region_id(&self) -> RegionId {
        self.handle.region
    }

    pub fn peer(&self) -> Peer {
        self.handle.peer()
    }

    pub(crate) unsafe fn mark_dead(&self, peer: Peer) -> Option<Member> {
        unsafe { self.handle.mark_dead(peer) }
    }

    pub(crate) fn release_dead(&self, member: Member) -> bool {
        self.handle.release_dead(member)
    }

    pub(crate) fn offset_of(
        &self,
        pointer: NonNull<u8>,
        size: usize,
    ) -> Result<usize, ProjectionError> {
        let offset = pointer
            .as_ptr()
            .addr()
            .checked_sub(self.handle.start_ptr().addr())
            .ok_or(ProjectionError::OutOfBounds)?;
        let end = offset
            .checked_add(size)
            .ok_or(ProjectionError::ArithmeticOverflow)?;
        (end <= self.handle.size())
            .then_some(offset)
            .ok_or(ProjectionError::OutOfBounds)
    }

    pub(crate) unsafe fn ref_at<U>(&self, offset: usize) -> Result<&U, ProjectionError> {
        let end = offset
            .checked_add(core::mem::size_of::<U>())
            .ok_or(ProjectionError::ArithmeticOverflow)?;
        if end > self.handle.size() {
            return Err(ProjectionError::OutOfBounds);
        }
        let pointer = unsafe { self.handle.start_ptr().add(offset) }.cast::<U>();
        if !pointer.is_aligned() {
            return Err(ProjectionError::Misaligned);
        }
        Ok(unsafe { &*pointer })
    }
}

impl<T: crate::header::Layout> Mapped<crate::header::RcHeader<T>> {
    pub(crate) fn recover_member(&self, slot: u8) -> bool {
        unsafe { self.ptr.as_ref() }.recover_member(slot)
    }

    pub(crate) fn has_member(&self, slot: u8) -> bool {
        unsafe { self.ptr.as_ref() }.has_member(slot)
    }
}

impl<T: AdmitLayout> Mapped<T> {
    unsafe fn from_raw(handle: Arc<Region>, ptr: NonNull<T>) -> Self {
        Self {
            handle,
            ptr,
            attached: true,
        }
    }

    pub(crate) fn admit_at<U: AdmitLayout>(
        &self,
        offset: usize,
        conf: U::Config,
        allow_init: bool,
    ) -> Result<Mapped<U>, ProjectionError> {
        if self.handle.permits(Access::WRITE).is_err() {
            return Err(ProjectionError::ReadOnly);
        }
        let layout = <U as AdmitLayout>::storage(&conf).ok_or(ProjectionError::Admission(
            crate::header::AdmitError::InvalidHeader,
        ))?;
        let end = offset
            .checked_add(layout.size())
            .ok_or(ProjectionError::ArithmeticOverflow)?;
        if end > self.handle.size() {
            return Err(ProjectionError::OutOfBounds);
        }
        let pointer = unsafe { self.handle.map().start_mut_ptr().add(offset) }.cast::<U>();
        if pointer.addr() % layout.align() != 0 {
            return Err(ProjectionError::Misaligned);
        }
        let offset = u64::try_from(offset).map_err(|_| ProjectionError::ArithmeticOverflow)?;
        let ctx = LayoutContext {
            region: self.handle.region,
            offset,
            allow_init,
        };
        unsafe { U::admit(pointer, conf, ctx, Some(self.handle.member.slot)) }
            .map_err(ProjectionError::Admission)?;
        Ok(unsafe { Mapped::from_raw(self.handle.clone(), NonNull::new_unchecked(pointer)) })
    }
}
