//! Owns the linear account of one process-local mapping: the cursor that turns a sequence of composed
//! layouts into addresses, and the arithmetic that reserves a span inside it. `Build`'s `push` and
//! `reserve`, committed through `Reservation::commit`, extend the composition; `Map`'s `reserve` and
//! `commit` do the address arithmetic and the compare-exchange that keeps it honest; `Map::close` hands
//! the account back once, if the extent has an account to hand back — one declared with
//! `Map::from_static` is simply let go. `Region::new` is the one place a mapping names the member it
//! owns, and it takes that word one of two ways: `join` claims whatever slot is next, `materialise`
//! arrives in a slot a live member promised.
//!
//! The cursor advances by `size_of`, never by the extent a configuration says a type needs, so a
//! record whose extent grows with its contents has to be composed last. Nothing asserts that, and a
//! region composed the other way round still opens. The residue a composition records is likewise a
//! promise about the mapping underneath it: `REGION_ALIGN` says what the promise is, so a port can meet
//! it on purpose, and a `Source` written outside this crate can still hand back a `Map` that misses it —
//! which a peer finds out at its own admission, not here.
use alloc::sync::Arc;
use core::ptr::NonNull;
use core::{marker::PhantomData, ops::Deref};

use crate::{
    header::{AdmitError, AdmitLayout, Claim, Layout, Member, RootHeader},
    schema::{LayoutContext, RegionAdmission, RegionId},
};

pub use crate::header::LayoutField;
#[cfg(test)]
pub use alloc::alloc::AllocError;

bitflags::bitflags! {
    /// What a mapping may do to the bytes, and the only access language a port has to answer in.
    ///
    /// The three bits mean different things on the two hosts, and a caller that reads one arm's
    /// behaviour into the other gets a refusal on one and silence on the other. On Unix they become the
    /// protection of *one mapping*, so each process decides for itself and the object underneath
    /// restricts nobody — except that an object made through `os::shared` keeps the set it was made
    /// under as its own bound, which this crate then refuses to widen. On Windows they select the
    /// protection a section is *created* with, which limits every process that maps it, and a view can
    /// only narrow from it.
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Access: u8 {
        /// Read the bytes. The Windows arm requires it twice — of the section as it is created and of
        /// the view as it is opened — and refuses the call itself when it is absent, while the Unix arm
        /// asks for nothing and passes the request straight to `mmap` as protection bits.
        const READ = 0x1;
        /// Write the bytes, which is what admitting a layout to a region requires — every composition
        /// in this crate asks for it, because the header and the directory are written, not read.
        const WRITE = 0x1 << 1;
        /// Execute the bytes. Nothing in this crate asks for it: it exists because a port's host may
        /// grant it and a caller has to be able to say so, and on Windows it changes the protection the
        /// section is created under rather than only the view.
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
/// The `os` adapters report `io::Error` for every platform's mapping failure, so
/// `SessionError<S::Error>` stays one type across them. A target with no mapping
/// call at all — a region fixed in the program's own image — implements this trait
/// by returning a [`Map::from_static`], and is the only kind of implementation the
/// safety note below does not constrain away.
///
/// # Safety
///
/// An implementation must return a valid mapping with the requested access and extent, based at an
/// address that is a multiple of [`REGION_ALIGN`]. That bound is what makes a block address
/// reproducible in a peer: a pool caps its widest class at it, and `header::recorded_at` refuses a
/// member whose own base is not a multiple of the extent alignment the region recorded. A release
/// function must remain callable and release that mapping exactly once from any thread that may own
/// it; an extent with nothing to release is declared with [`Map::from_static`] rather than by a release
/// that lies.
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
    ParticipantReserved,
    ArrivalRefused,
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
            Self::ParticipantReserved => f.write_str(
                "No shared-memory participant slot is free, and some stand promised to a process \
                 that has not arrived",
            ),
            Self::ArrivalRefused => f.write_str(
                "The shared-memory participant slot named for arrival is not promised at that \
                 generation",
            ),
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

/// Alignment every mapping of a region is guaranteed to have in every process that maps it.
///
/// A pool's class blocks are `bytes`-aligned against the absolute address the allocator happened to
/// get, and a peer reading the same record offset only reproduces that alignment if its own base has
/// the same residue — which is what `header::recorded_at` refuses. `os::unix` arranges the residue by
/// placing the file at an address on this bound instead of hoping `mmap` chose one, and
/// `pool::Geometry::build` refuses a wider class than the bound, so the promise is never one a peer
/// cannot keep. Because the bound names what the host's mapping can promise, it is the platform's own
/// property: a caller that wants one geometry everywhere keeps its classes inside the smallest of
/// them. Windows takes the granularity its view placement is documented to round addresses to, which
/// is coarser than any class the Unix-side tests here lay out, and has not been executed. A test
/// backend that hands every session a slice of one buffer cannot vary its residue, so it exercises
/// neither the arrangement nor the refusal.
///
/// This is also the number a `Source` written outside the crate has to meet, which is why it is
/// published rather than only assumed by a safety note: a port arranges its own base, and the multiple
/// of the extent alignment a region recorded is what a joining member is refused for missing.
#[cfg(unix)]
pub const REGION_ALIGN: usize = 1 << 21;
#[cfg(not(unix))]
pub const REGION_ALIGN: usize = 1 << 16;

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

    /// Owns an extent this process was given and can never give back.
    ///
    /// This is the shape of a region that lives in the program's own image: there is no host object to
    /// unmap, so no release function exists to name, and dropping the owner simply lets the bytes go.
    /// A caller that has a real mapping to release uses [`Map::from_raw_parts`] instead — declaring an
    /// extent static when it is not leaves whatever the host allocated behind mapped after this owner
    /// is gone.
    ///
    /// # Safety
    ///
    /// `start..start + len` must stay valid, carry no [`REGION_ALIGN`] residue, and be exclusively
    /// owned for the declared access for as long as this process lives. Nothing here can keep another
    /// owner out, and nothing here will try to release it.
    pub const unsafe fn from_static(start: NonNull<u8>, len: usize, access: Access) -> Self {
        Self {
            start,
            len,
            access,
            release: None,
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
        // A static extent has nothing to give back, so closing it succeeds by being let go.
        let Some(release) = self.release.take() else {
            return true;
        };
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

// Safety: Region exposes only atomic root operations to a shared holder, and its typed projections
// require `T: Sync`. `reserve` is the one method that hands out a mutable raw pointer from `&self`,
// and sharing it is sound because a pointer from it is worthless until `commit` wins the layout's
// admission CAS — the permission bits only say the mapping may be written, and that CAS is what
// says this participant may write it. Drop has unique ownership, and Map admission requires its
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
    fn new(
        map: Map,
        admission: RegionAdmission,
        promised: Option<Peer>,
    ) -> Result<(Arc<Self>, usize), Error> {
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
        let root = &unsafe { header.as_ref() }.inner;
        let member = match promised {
            // Arriving means taking up the one slot somebody promised this generation, and nobody
            // else's: the compare-exchange is what says the promise still stands.
            Some(promised) => {
                let member = promised.0;
                if root.materialise(member) {
                    member
                } else {
                    return Err(Error::ArrivalRefused);
                }
            }
            None => match root.join() {
                Claim::Taken(member) => member,
                Claim::Promised => return Err(Error::ParticipantReserved),
                Claim::Full => return Err(Error::ParticipantExhausted),
            },
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

    /// Hold a member slot for a process this one has not started yet.
    pub(crate) fn reserve_member(&self) -> Claim {
        self.header().inner.reserve()
    }

    /// Give back a slot the promised process never arrived in.
    pub(crate) fn cancel_member(&self, member: Member) -> bool {
        self.header().inner.cancel(member)
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
    #[inline]
    pub(crate) fn new(map: Map, admission: RegionAdmission) -> Result<Self, Error> {
        Self::establish(map, admission, None)
    }

    /// Build as the member a live participant promised, rather than as whoever claims a slot next.
    pub(crate) fn arriving_as(
        map: Map,
        admission: RegionAdmission,
        promised: Peer,
    ) -> Result<Self, Error> {
        Self::establish(map, admission, Some(promised))
    }

    fn establish(
        map: Map,
        admission: RegionAdmission,
        promised: Option<Peer>,
    ) -> Result<Self, Error> {
        let (area, offset) = Region::new(map, admission, promised)?;
        Ok(Self {
            area,
            offset,
            poisoned: false,
        })
    }

    pub fn region_id(&self) -> RegionId {
        self.area.region
    }

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
// Safety: Region erasure admits only cache-coherent process-shared mappings, and the projected
// layout is what makes cross-thread access sound: `Mapped<T>` itself is Sync only for `T: Sync`,
// and the only other way this type reaches memory — `ref_at` — is bounded the same way, so a
// projection cannot hand out a shared reference to a type the promise was never made about.
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

const impl<T: AdmitLayout> Deref for Mapped<T> {
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

    pub(crate) fn reserve_member(&self) -> Claim {
        self.handle.reserve_member()
    }

    pub(crate) fn cancel_member(&self, member: Member) -> bool {
        self.handle.cancel_member(member)
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

    /// Projects `U` at `offset` inside this mapping.
    ///
    /// `U: Sync` is part of the contract, not a convenience: it is `Mapped<T>: Sync` that lets two
    /// threads hold this value at once, and a projection hands out `&U` for a type the compiler never
    /// saw, so a `U` with interior mutability would be shared through a promise made about `T`.
    pub(crate) unsafe fn ref_at<U: Sync>(&self, offset: usize) -> Result<&U, ProjectionError> {
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
