use alloc::sync::Arc;
use core::ptr::NonNull;
use core::{marker::PhantomData, ops::Deref};

use super::MemOps;
use crate::{
    header::{AdmitError, AdmitLayout, Layout, Member, RootHeader},
    mem::{Access, Error},
    schema::{LayoutContext, RegionAdmission, RegionId},
};

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

    fn into_parts(
        mut self,
    ) -> (
        NonNull<u8>,
        usize,
        Access,
        unsafe fn(NonNull<u8>, usize) -> bool,
    ) {
        let release = self.release.take().expect("live map has release authority");
        (self.start, self.len, self.access, release)
    }
}

/// Immutable process-local authority for one mapped shared-memory region.
///
/// The mapping backend is erased after successful root admission. The only
/// retained backend operation is final unmap.
pub struct Region {
    start: NonNull<u8>,
    size: usize,
    writable: bool,
    header: NonNull<RootHeader>,
    member: Member,
    region: RegionId,
    allow_init: bool,
    detached: bool,
    release: Option<unsafe fn(NonNull<u8>, usize) -> bool>,
}

// Region exposes only atomic root operations; typed projections independently
// require `T: Sync`. Drop has unique ownership, and Map admission requires its
// release function to be callable from any owning thread.
unsafe impl Send for Region {}
unsafe impl Sync for Region {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseError {
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
        self.start.as_ptr()
    }

    #[inline]
    fn size(&self) -> usize {
        self.size
    }
}

impl core::fmt::Debug for Region {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Region")
            .field("start", &self.start)
            .field("size", &self.size)
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
        let (start, size, access, release) = map.into_parts();
        Ok((
            Arc::new(Self {
                start,
                size,
                writable: access.contains(Access::WRITE),
                header,
                member,
                region,
                allow_init,
                detached: false,
                release: Some(release),
            }),
            offset,
        ))
    }

    fn header(&self) -> &RootHeader {
        unsafe { self.header.as_ref() }
    }

    fn permits(&self, access: Access) -> Result<(), Error> {
        if access.contains(Access::WRITE) && !self.writable {
            return Err(Error::PermissionDenied { requested: access });
        }
        Ok(())
    }

    unsafe fn reserve<T: Layout>(&self, offset: usize) -> Result<(*mut T, usize), Error> {
        self.permits(Access::WRITE)?;
        let layout = core::alloc::Layout::new::<T>();
        let start = self.start.as_ptr().addr();
        let candidate = start.checked_add(offset).ok_or(Error::ArithmeticOverflow)?;
        let aligned = candidate
            .checked_add(layout.align() - 1)
            .map(|value| value & !(layout.align() - 1))
            .ok_or(Error::ArithmeticOverflow)?;
        let end = aligned
            .checked_add(layout.size())
            .ok_or(Error::ArithmeticOverflow)?;
        let next = end.checked_sub(start).ok_or(Error::ArithmeticOverflow)?;
        if next > self.size {
            return Err(Error::UnenoughSpace {
                requested: next,
                allocated: self.size,
            });
        }
        Ok((aligned as *mut T, next))
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
        let Some(release) = self.release.take() else {
            return true;
        };
        unsafe { release(self.start, self.size) }
    }

    fn detach(&mut self) {
        if self.detached {
            return;
        }
        let _ = self.header().inner.leave(self.member);
        self.detached = true;
    }

    pub fn close(region: Arc<Self>) -> Result<(), CloseError> {
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
    pub unsafe fn assume_dead(&self, peer: Peer) -> Option<Recovery<'_>> {
        let root = &self.header().inner;
        (root.mark_dead(peer.0) || root.is_dead(peer.0)).then_some(Recovery {
            _region: self,
            member: peer.0,
        })
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

#[must_use = "recovery authority must be completed or durably delegated"]
/// Exact authority to recover one participant generation.
///
/// Slot reuse is intentionally unavailable until every layout and operation
/// has been recovered by the coupled reap protocol.
///
/// ```compile_fail
/// fn bypass_scan(recovery: evering::Recovery<'_>) {
///     unsafe { recovery.complete() };
/// }
/// ```
pub struct Recovery<'region> {
    _region: &'region Region,
    member: Member,
}

impl Recovery<'_> {
    pub fn peer(&self) -> Peer {
        Peer(self.member)
    }

    pub(crate) const fn region_id(&self) -> RegionId {
        self._region.region
    }

    pub(crate) const fn slot(&self) -> u8 {
        self.member.slot
    }

    pub(crate) fn release(self) -> Result<(), Self> {
        if self._region.header().inner.release_dead(self.member) {
            Ok(())
        } else {
            Err(self)
        }
    }
}

pub struct Reservation<'a, T: AdmitLayout> {
    layout: &'a mut MapLayout,
    offset: usize,
    next: usize,
    _marker: PhantomData<fn() -> T>,
}

impl<T: AdmitLayout> Reservation<'_, T> {
    #[inline]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    #[inline]
    pub fn available_from_offset(&self) -> usize {
        self.layout.size().saturating_sub(self.offset)
    }

    #[inline]
    pub fn remaining_after(&self) -> usize {
        self.layout.size().saturating_sub(self.next)
    }

    #[inline]
    pub const fn size(&self) -> usize {
        core::mem::size_of::<T>()
    }

    pub fn commit(self, conf: T::Config) -> Result<Mapped<T>, Error> {
        self.layout.commit_reserved(self.offset, self.next, conf)
    }
}

/// Manages incremental typed composition within a memory-mapped area.
///
/// A reservation exclusively borrows its originating cursor:
///
/// ```compile_fail
/// use evering::MapLayout;
///
/// fn overlap(layout: &mut MapLayout) {
///     let first = layout.reserve::<()>().unwrap();
///     let _second = layout.reserve::<()>().unwrap();
///     first.commit(()).unwrap();
/// }
/// ```
///
/// It also cannot outlive that cursor:
///
/// ```compile_fail
/// use evering::MapLayout;
///
/// fn escape(layout: &mut MapLayout) -> impl 'static {
///     layout.reserve::<()>().unwrap()
/// }
/// ```
pub struct MapLayout {
    area: Arc<Region>,
    offset: usize,
    poisoned: bool,
}

unsafe impl MemOps for MapLayout {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.area.start_ptr()
    }

    #[inline]
    fn size(&self) -> usize {
        self.area.size()
    }
}

impl MapLayout {
    pub fn map<S: super::Source>(
        source: S,
        request: super::Request,
        admission: RegionAdmission,
    ) -> Result<Self, super::OpenError<S::Error>> {
        let map = source.map(request).map_err(super::OpenError::Source)?;
        Self::new(map, admission).map_err(super::OpenError::Admission)
    }

    /// Creates a new layout manager from a raw map, initializing the header and offset.
    #[inline]
    pub fn new(map: Map, admission: RegionAdmission) -> Result<Self, Error> {
        let (area, offset) = Region::new(map, admission)?;
        Ok(Self {
            area,
            offset,
            poisoned: false,
        })
    }

    /// Advances the current offset by the specified amount, returning a new layout.
    #[inline]
    pub fn forward(&mut self, forward: usize) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::PoisonedComposition);
        }
        let offset = self
            .offset
            .checked_add(forward)
            .ok_or(Error::ArithmeticOverflow)?;
        if offset > self.size() {
            return Err(Error::OutofSize {
                requested: offset,
                bound: self.size(),
            });
        }
        self.offset = offset;
        Ok(())
    }

    /// Returns the current offset within the memory area.
    #[inline]
    pub const fn cur_offset(&self) -> usize {
        self.offset
    }

    pub fn region_id(&self) -> RegionId {
        self.area.region
    }

    /// Returns the remaining size available for allocation.
    #[inline]
    pub fn rest_size(&self) -> usize {
        self.area.size().saturating_sub(self.offset)
    }

    /// Reserves space for `T`, exclusively borrowing this composition until commit.
    #[inline]
    pub fn reserve<T: AdmitLayout>(&mut self) -> Result<Reservation<'_, T>, Error> {
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
    pub fn push<T: AdmitLayout>(&mut self, conf: T::Config) -> Result<Mapped<T>, Error> {
        self.reserve::<T>()?.commit(conf)
    }

    /// Finalizes the layout and returns the total offset used.
    pub fn finish(self) -> usize {
        self.offset
    }
}

/// A linear mapping owner whose thread-safety follows the projected layout.
///
/// ```compile_fail
/// use evering::Mapped;
///
/// fn needs_clone<T: Clone>() {}
/// fn clone_it<T>() {
///     needs_clone::<Mapped<T>>();
/// }
/// ```
pub struct Mapped<T: ?Sized> {
    handle: Arc<Region>,
    ptr: NonNull<T>,
    detach: Option<unsafe fn(NonNull<T>, u8)>,
}
pub type MapView = Mapped<()>;

// Safety: Region erasure admits only cache-coherent process-shared mappings;
// the projected layout controls whether immutable cross-thread access is safe.
unsafe impl<T: ?Sized + Sync> Send for Mapped<T> {}
unsafe impl<T: ?Sized + Sync> Sync for Mapped<T> {}

/// A zero-atomic borrow of one admitted mapping.
///
/// ```compile_fail
/// use evering::{Mapped, Ref};
///
/// fn close_while_borrowed(mapped: Mapped<()>) {
///     let borrowed: Ref<'_, ()> = mapped.as_ref();
///     mapped.close().unwrap();
///     let _ = *borrowed;
/// }
/// ```
pub struct Ref<'a, T: ?Sized> {
    ptr: NonNull<T>,
    _mapped: PhantomData<&'a Mapped<T>>,
}

impl<T: ?Sized> Clone for Ref<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> Copy for Ref<'_, T> {}

impl<T: ?Sized + core::fmt::Debug> core::fmt::Debug for Mapped<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Mapped")
            .field("handle", &self.handle)
            .field("ptr", &self.ptr)
            .finish()
    }
}

impl<T: ?Sized> Drop for Mapped<T> {
    fn drop(&mut self) {
        if let Some(detach) = self.detach.take() {
            unsafe { detach(self.ptr, self.handle.member.slot) };
        }
    }
}

impl<T: ?Sized> const Deref for Mapped<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: ?Sized> const Deref for Ref<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl TryFrom<MapLayout> for MapView {
    type Error = Error;

    fn try_from(mut value: MapLayout) -> Result<Self, Self::Error> {
        let area = value.push::<()>(())?;
        Ok(area)
    }
}

impl MapView {
    #[inline]
    pub fn header(&self) -> &RootHeader {
        self.handle.header()
    }
}

impl<T: ?Sized> Mapped<T> {
    pub(crate) fn pointer(&self) -> NonNull<T> {
        self.ptr
    }

    pub(crate) fn disarm_detach(&mut self) {
        self.detach = None;
    }

    pub fn close(mut self) -> Result<(), CloseError> {
        if let Some(detach) = self.detach.take() {
            unsafe { detach(self.ptr, self.handle.member.slot) };
        }
        let this = core::mem::ManuallyDrop::new(self);
        let handle = unsafe { core::ptr::read(&this.handle) };
        Region::close(handle)
    }

    pub fn region_id(&self) -> RegionId {
        self.handle.region
    }

    pub fn peer(&self) -> Peer {
        self.handle.peer()
    }

    /// Marks an exact participant generation as permanently unable to access
    /// this mapping.
    ///
    /// # Safety
    ///
    /// The caller must know that `peer` can never access this region again.
    pub unsafe fn assume_dead(&self, peer: Peer) -> Option<Recovery<'_>> {
        unsafe { self.handle.assume_dead(peer) }
    }

    #[cfg(test)]
    pub(crate) fn recovery_for_test(&self, slot: u8) -> Recovery<'_> {
        Recovery {
            _region: &self.handle,
            member: Member {
                slot,
                generation: 1,
            },
        }
    }

    #[inline(always)]
    pub fn as_ref(&self) -> Ref<'_, T> {
        Ref {
            ptr: self.ptr,
            _mapped: PhantomData,
        }
    }

    pub fn map<U>(&self, f: impl FnOnce(&T) -> &U) -> Ref<'_, U> {
        let u = f(self);
        Ref {
            ptr: u.into(),
            _mapped: PhantomData,
        }
    }

    pub fn try_map<E, U>(&self, f: impl FnOnce(&T) -> Result<&U, E>) -> Result<Ref<'_, U>, E> {
        let u = f(self)?;
        Ok(Ref {
            ptr: u.into(),
            _mapped: PhantomData,
        })
    }

    pub fn may_map<U>(&self, f: impl FnOnce(&T) -> Option<&U>) -> Option<Ref<'_, U>> {
        let u = f(self)?;
        Some(Ref {
            ptr: u.into(),
            _mapped: PhantomData,
        })
    }

    pub(crate) fn offset_of(
        &self,
        pointer: NonNull<u8>,
        size: usize,
    ) -> Result<usize, ProjectionError> {
        let offset = pointer
            .as_ptr()
            .addr()
            .checked_sub(self.handle.start.as_ptr().addr())
            .ok_or(ProjectionError::OutOfBounds)?;
        let end = offset
            .checked_add(size)
            .ok_or(ProjectionError::ArithmeticOverflow)?;
        (end <= self.handle.size)
            .then_some(offset)
            .ok_or(ProjectionError::OutOfBounds)
    }

    pub(crate) unsafe fn ref_at<U>(&self, offset: usize) -> Result<&U, ProjectionError> {
        let end = offset
            .checked_add(core::mem::size_of::<U>())
            .ok_or(ProjectionError::ArithmeticOverflow)?;
        if end > self.handle.size {
            return Err(ProjectionError::OutOfBounds);
        }
        let pointer = unsafe { self.handle.start.as_ptr().add(offset) }.cast::<U>();
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
        unsafe fn detach<T: AdmitLayout>(pointer: NonNull<T>, slot: u8) {
            unsafe { T::leave(pointer.as_ptr(), slot) };
        }
        Self {
            handle,
            ptr,
            detach: Some(detach::<T>),
        }
    }

    pub(crate) fn admit_at<U: AdmitLayout>(
        &self,
        offset: usize,
        conf: U::Config,
        allow_init: bool,
    ) -> Result<Mapped<U>, ProjectionError> {
        if !self.handle.writable {
            return Err(ProjectionError::ReadOnly);
        }
        let end = offset
            .checked_add(core::mem::size_of::<U>())
            .ok_or(ProjectionError::ArithmeticOverflow)?;
        if end > self.handle.size {
            return Err(ProjectionError::OutOfBounds);
        }
        let pointer = unsafe { self.handle.start.as_ptr().add(offset) }.cast::<U>();
        if !pointer.is_aligned() {
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
