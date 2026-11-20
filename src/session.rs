//! Owns no bytes of its own: it is the only type that pairs one directory with one heap under one
//! member identity, and the only one that authorizes the dead-mark which rewrites a member's word in
//! the region header. `create`, `create_with` and `open` join a member; `create_pool`,
//! `create_channel`, `adopt` and `remove` drive the record writes and the heap claim; and a dead
//! member's words move only inside `Recovery::reap_with`.
//!
//! `reap_with` walks the region twice before it clears a member, because the second walk is what
//! proves the first changed nothing — a single walk returns `Ok` for a region that still names the
//! peer it just removed. Those walks sit between `finish_recovery` and `release_dead` in that order,
//! and nothing in the suite exercises the ordering.
use core::{alloc::Layout, mem::MaybeUninit};

use crate::channel::{self as shared, Channel, CloseError, Id, Port};
use crate::{
    boxed::PBox,
    dir,
    header::{Member, RecoveryHandler},
    mem::{self, Build, MemOps, Peer, Request, Source},
    msg::Repr,
    pool, talc,
};

use crate::{
    schema::RegionId,
    talc::{Geometry, MutationError},
};

#[derive(Clone)]
pub struct GeneralHeap<'a> {
    alloc: talc::RefTalc<'a>,
}

impl<'a> GeneralHeap<'a> {
    fn uninit<T>(&self) -> Result<PBox<'a, MaybeUninit<T>>, MutationError> {
        let alloc = self.alloc.clone();
        if size_of::<T>() == 0 {
            Ok(PBox::null(alloc))
        } else {
            alloc
                .allocate(Layout::new::<MaybeUninit<T>>())
                .map(|meta| PBox::from_meta(meta, alloc))
        }
    }

    fn uninit_slice<T>(&self, len: usize) -> Result<PBox<'a, [MaybeUninit<T>]>, MutationError> {
        let alloc = self.alloc.clone();
        let layout =
            Layout::array::<MaybeUninit<T>>(len).map_err(|_| MutationError::LayoutOverflow)?;
        let meta = if layout.size() == 0 {
            talc::Meta::null()
        } else {
            alloc.allocate(layout)?
        };
        Ok(PBox::from_meta_slice(meta, alloc, len))
    }

    pub fn put<T: Repr>(&self, value: T) -> Result<PBox<'a, T>, PutError<T>> {
        let () = T::VALID;
        let slot = match self.uninit() {
            Ok(slot) => slot,
            Err(error) => return Err(PutError { error, value }),
        };
        Ok(slot.write(value))
    }

    pub fn copy<T: Repr + Copy>(&self, value: &[T]) -> Result<PBox<'a, [T]>, MutationError> {
        self.init(value.len(), |index| value[index])
    }

    pub fn init<T: Repr>(
        &self,
        len: usize,
        mut make: impl FnMut(usize) -> T,
    ) -> Result<PBox<'a, [T]>, MutationError> {
        let () = T::VALID;
        let mut values = self.uninit_slice(len)?;
        for (index, value) in values.iter_mut().enumerate() {
            value.write(make(index));
        }
        Ok(unsafe { values.assume_init() })
    }
}

pub struct PutError<T> {
    pub error: MutationError,
    pub value: T,
}

impl<T> core::fmt::Debug for PutError<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PutError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelCreateError {
    InvalidCapacity,
    Exhausted,
}

impl core::fmt::Display for ChannelCreateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::InvalidCapacity => "the capacity is not usable for a duplex queue",
            Self::Exhausted => "the region has no room left for another channel record",
        })
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ChannelCreateError {}

pub enum AdmitError<H: Repr> {
    WrongRegion(Port<H>),
    Stale(Port<H>),
    Occupied(Port<H>),
    Terminal(Port<H>),
    Orphaned(Port<H>),
    Open(Port<H>),
}

impl<H: Repr> core::fmt::Debug for AdmitError<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::WrongRegion(_) => "WrongRegion(..)",
            Self::Stale(_) => "Stale(..)",
            Self::Occupied(_) => "Occupied(..)",
            Self::Terminal(_) => "Terminal(..)",
            Self::Orphaned(_) => "Orphaned(..)",
            Self::Open(_) => "Open(..)",
        })
    }
}

impl<H: Repr> core::fmt::Display for AdmitError<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::WrongRegion(_) => "the port names another region than this session's mapping",
            Self::Stale(_) => "the port names a channel generation the role word no longer holds",
            Self::Occupied(_) => "another member already holds the role the port grants",
            Self::Terminal(_) => "the channel is closed, so its role can no longer be taken",
            Self::Orphaned(_) => "the peer's role is gone, so the channel could not be completed",
            Self::Open(_) => "the channel behind the port is not open to that role",
        })
    }
}

#[cfg(feature = "std")]
impl<H: Repr> std::error::Error for AdmitError<H> {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionOptions {
    heap: Option<Geometry>,
}

impl SessionOptions {
    pub const fn with_heap_geometry(mut self, geometry: Geometry) -> Self {
        self.heap = Some(geometry);
        self
    }
}

#[derive(Debug)]
pub enum SessionError<E> {
    Source(E),
    PermissionDenied { requested: mem::Access },
    InsufficientSpace { requested: usize, allocated: usize },
    Busy,
    Closed,
    DuplicateAttachment,
    InvalidHeader,
    LayoutMismatch(crate::header::LayoutField),
    Poisoned,
    Overflow,
    ParticipantExhausted,
}

impl<E> SessionError<E> {
    fn admission(error: mem::Error) -> Self {
        match error {
            mem::Error::PermissionDenied { requested } => Self::PermissionDenied { requested },
            mem::Error::UnenoughSpace {
                requested,
                allocated,
            } => Self::InsufficientSpace {
                requested,
                allocated,
            },
            mem::Error::Contention => Self::Busy,
            mem::Error::LayoutClosed => Self::Closed,
            mem::Error::DuplicateAttachment => Self::DuplicateAttachment,
            mem::Error::InvalidHeader => Self::InvalidHeader,
            mem::Error::LayoutMismatch(field) => Self::LayoutMismatch(field),
            mem::Error::PoisonedComposition => Self::Poisoned,
            mem::Error::ArithmeticOverflow => Self::Overflow,
            mem::Error::ParticipantExhausted => Self::ParticipantExhausted,
        }
    }
}

impl<E: core::fmt::Debug> core::fmt::Display for SessionError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Source(error) => write!(f, "the provider refused the mapping: {error:?}"),
            Self::PermissionDenied { requested } => {
                write!(f, "the mapping does not permit {requested:?}")
            }
            Self::InsufficientSpace {
                requested,
                allocated,
            } => write!(
                f,
                "{requested} bytes were asked of a region holding {allocated}"
            ),
            Self::Busy => f.write_str("another member held the transaction word; retry"),
            Self::Closed => f.write_str("this region is closed"),
            Self::DuplicateAttachment => f.write_str("this member is already attached"),
            Self::InvalidHeader => f.write_str("no readable header sits at the recorded place"),
            Self::LayoutMismatch(field) => {
                write!(f, "the shared words disagree with the layout at {field:?}")
            }
            Self::Poisoned => f.write_str("this composition was refused as poisoned"),
            Self::Overflow => f.write_str("the requested size or offset overflowed"),
            Self::ParticipantExhausted => f.write_str("the region holds no free member slot"),
        }
    }
}

#[cfg(feature = "std")]
impl<E: core::fmt::Debug> std::error::Error for SessionError<E> {}

/// Why a Pool could not be opened from an id another process published.
///
/// The refusal is named at the point that made it, because the families of cause ask different
/// things of the caller. `Busy` is worth retrying. `Absent` and `WrongRegion` say the id was never
/// this region's to answer for, and retrying cannot change that. `Geometry`, `InvalidHeader`,
/// `OutOfBounds`, `Misaligned` and `LayoutMismatch` say the region's own words disagree with each
/// other, which is what death evidence and `Recovery` settle rather than a retry loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenPoolError {
    /// The id names another region than this session's mapping.
    WrongRegion(crate::PoolId),
    /// No published directory entry carries this id and generation.
    Absent(crate::PoolId),
    /// The record's offset or extent does not fit inside this mapping.
    OutOfBounds(crate::PoolId),
    /// The recorded offset is not aligned for the header a Pool needs to find there.
    Misaligned(crate::PoolId),
    /// This mapping cannot be written, so no member can be admitted to the shared header.
    ReadOnly(crate::PoolId),
    /// Nothing that reads as a header sits at the recorded offset.
    InvalidHeader(crate::PoolId),
    /// A header is there, and this field of it disagrees with the layout a Pool of this id needs.
    LayoutMismatch(crate::PoolId, crate::header::LayoutField),
    /// The header is a Pool header, but its class words do not rebuild the extent it records.
    Geometry(crate::PoolId),
    /// The Pool is closed.
    Closed(crate::PoolId),
    /// This member is already admitted to the Pool's header.
    DuplicateMember(crate::PoolId),
    /// Another member held the header's transaction word; the open may be retried.
    Busy(crate::PoolId),
}

impl OpenPoolError {
    pub(crate) fn of(id: crate::PoolId, cause: crate::dir::OpenCause) -> Self {
        use crate::dir::OpenCause as Cause;
        match cause {
            Cause::WrongRegion => Self::WrongRegion(id),
            Cause::Absent => Self::Absent(id),
            Cause::OutOfBounds => Self::OutOfBounds(id),
            Cause::Misaligned => Self::Misaligned(id),
            Cause::ReadOnly => Self::ReadOnly(id),
            Cause::InvalidHeader => Self::InvalidHeader(id),
            Cause::Mismatch(field) => Self::LayoutMismatch(id, field),
            Cause::Undiscoverable => Self::Geometry(id),
            Cause::Closed => Self::Closed(id),
            Cause::DuplicateMember => Self::DuplicateMember(id),
            Cause::Busy => Self::Busy(id),
        }
    }

    /// The id this refusal is about, whichever cause named itself.
    pub const fn id(&self) -> crate::PoolId {
        match self {
            Self::WrongRegion(id)
            | Self::Absent(id)
            | Self::OutOfBounds(id)
            | Self::Misaligned(id)
            | Self::ReadOnly(id)
            | Self::InvalidHeader(id)
            | Self::LayoutMismatch(id, _)
            | Self::Geometry(id)
            | Self::Closed(id)
            | Self::DuplicateMember(id)
            | Self::Busy(id) => *id,
        }
    }

    /// Whether asking again could change the answer, as opposed to the region needing adjudication.
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Busy(_))
    }
}

impl core::fmt::Display for OpenPoolError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let id = self.id();
        match self {
            Self::WrongRegion(_) => write!(f, "pool {id:?} names another region than this mapping"),
            Self::Absent(_) => write!(f, "no published record names pool {id:?}"),
            Self::OutOfBounds(_) => {
                write!(f, "pool {id:?}'s recorded place does not fit this mapping")
            }
            Self::Misaligned(_) => write!(f, "pool {id:?}'s recorded place is misaligned"),
            Self::ReadOnly(_) => write!(
                f,
                "this mapping is read-only, so no member can join pool {id:?}"
            ),
            Self::InvalidHeader(_) => {
                write!(
                    f,
                    "nothing readable as a header sits at pool {id:?}'s place"
                )
            }
            Self::LayoutMismatch(_, field) => {
                write!(
                    f,
                    "pool {id:?}'s header disagrees with its layout at {field:?}"
                )
            }
            Self::Geometry(_) => write!(
                f,
                "pool {id:?}'s class words do not rebuild the extent its header records"
            ),
            Self::Closed(_) => write!(f, "pool {id:?} is closed"),
            Self::DuplicateMember(_) => {
                write!(f, "this member already holds a place in pool {id:?}")
            }
            Self::Busy(_) => write!(f, "another member was changing pool {id:?}'s header; retry"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for OpenPoolError {}

pub enum RemoveError<H: Repr> {
    Busy(Channel<H>),
    Evidence(Channel<H>),
}

impl<H: Repr> core::fmt::Debug for RemoveError<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Busy(_) => "Busy(..)",
            Self::Evidence(_) => "Evidence(..)",
        })
    }
}

impl<H: Repr> core::fmt::Display for RemoveError<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Busy(_) => {
                "the channel is still held: another handle, the peer's role, or a queued token"
            }
            Self::Evidence(_) => {
                "a queued token can only be released once its owner's death is attested"
            }
        })
    }
}

#[cfg(feature = "std")]
impl<H: Repr> std::error::Error for RemoveError<H> {}

pub struct Session {
    dir: dir::MapDirectory,
    alloc: talc::MapTalc,
}

/// Authority to repair what one dead participant left in this region.
///
/// [`Session::assume_dead`] hands one out after marking an exact peer generation dead. Dropping the
/// handle repairs nothing: the peer stays marked dead with its evidence in place, so a reap either
/// runs here or the handle moves to whichever participant will run it.
///
/// Holding the authority does not mean this process can finish the job. The repairs are per-layout,
/// and the layouts a region can hold are chosen by its users, not by the library: see
/// [`Recovery::reap`] for what the built-ins cover and [`Recovery::reap_with`] for the rest.
#[must_use = "recovery authority must be completed or durably delegated"]
pub struct Recovery<'a> {
    session: &'a Session,
    member: Member,
}

impl Recovery<'_> {
    /// The peer this authority is for: the slot and generation marked dead.
    pub fn peer(&self) -> Peer {
        Peer::from_parts(self.member.slot, self.member.generation)
    }

    /// Reaps with only the built-in handler, which is the Pool's own storage.
    ///
    /// Every published member of the region must be adjudicated for a reap to finish, and each is
    /// matched to a handler by the schema and ABI profile recorded in its header. A member that
    /// matches no handler fails the reap rather than being skipped, so `reap()` cannot finish a
    /// region that holds anything beyond Pool storage — the member a `Channel<H>` publishes is a
    /// `Duplex<H>`, and its handler is [`crate::layout::recovery_handler`]. Without it the reap
    /// settles what it can and returns the authority, so the caller either supplies the handler or
    /// leaves the member for a participant that has it.
    pub fn reap(self) -> Result<(), Self> {
        self.reap_with(&[])
    }

    /// Adjudicates the dead peer's leftover state, extending the built-in handler with `handlers`.
    ///
    /// One call runs in this order. The directory transaction is settled first, and only a
    /// transaction that names the dead peer is repairable here: one held by any live participant —
    /// including the caller's own, mid-send — fails the reap untouched. The allocator's in-flight
    /// mutation is then closed, or poisoned if it cannot be attributed. Then every published member
    /// is visited, each handed to exactly one handler whose schema and profile match it; an
    /// ambiguous match is refused the same way a missing one is. The walk must complete twice, so a
    /// repair that leaves another member unsettled is caught by the second pass instead of being
    /// reported as done. Finally the member is removed from every header that still names it and
    /// its directory slot is released.
    ///
    /// `Ok(())` means the region no longer names `peer()` anywhere and its blocks are claimable.
    /// `Err(self)` means something in that sequence was not this process's to decide. Nothing is
    /// rewound when a reap fails: each member repair that already ran is durable, so a retry — by
    /// this participant or one holding the handle later — continues from what is left rather than
    /// starting over. A reap that keeps failing with the same cause is a region holding a layout no
    /// caller has supplied a handler for.
    pub fn reap_with(self, handlers: &[RecoveryHandler]) -> Result<(), Self> {
        let session = self.session;
        if self.peer() == session.dir.peer() {
            return Err(self);
        }
        let dead = self.member.slot;
        let live = session.dir.peer().slot();
        match session.dir.transaction_owner() {
            None => {}
            Some(owner) if owner == dead => {
                // The cause is discarded deliberately. Every answer `recover` gives from here ends
                // the reap the same way — hand the authority back so a participant that can name the
                // handler tries again — and `reap_with` returns the session rather than an error, so
                // there is no place to put a cause a caller could act on. A typed recovery failure
                // reopens when one caller's branch actually differs by cause, not before.
                if session.dir.recover(&session.alloc, dead).is_err() {
                    return Err(self);
                }
            }
            Some(_) => return Err(self),
        }
        if !session.alloc.finish_recovery(dead)
            || !session.scan_recovery(dead, live, handlers)
            || !session.scan_recovery(dead, live, handlers)
        {
            return Err(self);
        }
        session.dir.recover_member(dead);
        if session.dir.has_member(dead) || !session.dir.release_dead(self.member) {
            Err(self)
        } else {
            Ok(())
        }
    }
}

impl Session {
    pub fn create<S: Source>(
        source: S,
        request: Request,
        region: RegionId,
    ) -> Result<Self, SessionError<S::Error>> {
        Self::create_with(source, request, region, SessionOptions::default())
    }

    pub fn create_with<S: Source>(
        source: S,
        request: Request,
        region: RegionId,
        options: SessionOptions,
    ) -> Result<Self, SessionError<S::Error>> {
        Self::admit(
            source,
            request,
            crate::schema::RegionAdmission::Create(region),
            options.heap,
        )
    }

    pub fn open<S: Source>(
        source: S,
        request: Request,
        region: RegionId,
    ) -> Result<Self, SessionError<S::Error>> {
        Self::admit(
            source,
            request,
            crate::schema::RegionAdmission::Expect(region),
            None,
        )
    }

    fn admit<S: Source>(
        source: S,
        request: Request,
        admission: crate::schema::RegionAdmission,
        geometry: Option<Geometry>,
    ) -> Result<Self, SessionError<S::Error>> {
        let map = source.map(request).map_err(SessionError::Source)?;
        let area = Build::new(map, admission).map_err(SessionError::admission)?;
        match geometry {
            Some(geometry) => Self::from_geometry(area, geometry),
            None => Self::from(area),
        }
        .map_err(SessionError::admission)
    }

    pub(crate) fn from(area: Build) -> Result<Self, mem::Error> {
        let conf = talc::Config::new(area.size());
        Self::from_config(area, conf)
    }

    pub(crate) fn from_geometry(area: Build, geometry: Geometry) -> Result<Self, mem::Error> {
        let conf = talc::Config::new(area.size()).with_geometry(geometry);
        Self::from_config(area, conf)
    }

    pub(crate) fn from_config(area: Build, conf: talc::Config) -> Result<Self, mem::Error> {
        let mut area = area;
        let dir = area.push::<dir::Header>(())?;
        let areserve = area.reserve::<talc::Header>()?;
        let conf = conf.with_bound(areserve.remaining_after());
        let alloc = areserve.commit(conf)?;
        let alloc = talc::MapTalc::from_handle(alloc);
        Ok(Session { dir, alloc })
    }

    /// The address this process's mapping of the region starts at.
    ///
    /// This is a fact about the local mapping, not about the region: every member maps the same
    /// bytes wherever it chooses, so the number identifies nothing across a boundary — `RegionId`
    /// does that, and `Session::open` takes it. Where a mapping lands is asked of the backend
    /// before it maps (`os::unix::FdMap::at`; Windows leaves the choice to the kernel), never
    /// derived from a peer's answer. A test that wants to prove a region works under two different
    /// bases uses this only as raw arithmetic, to place the second mapping away from the first.
    pub fn base_addr(&self) -> usize {
        self.alloc.base_ptr().addr()
    }

    pub fn peer(&self) -> Peer {
        self.dir.peer()
    }

    pub fn heap(&self) -> GeneralHeap<'_> {
        GeneralHeap {
            alloc: self.alloc.as_ref(),
        }
    }

    pub fn create_pool(
        &self,
        extent: usize,
        range: Option<crate::BlockRange>,
    ) -> Result<crate::Pool, crate::PoolCreateError> {
        pool::create(&self.dir, &self.alloc, extent, range)
    }

    pub fn open_pool(&self, id: crate::PoolId) -> Result<crate::Pool, OpenPoolError> {
        pool::open(&self.dir, id)
    }

    #[cfg(test)]
    pub(crate) fn abandon_heap_for_test(&self, owner: u8) {
        self.alloc.abandon_mutation_for_test(owner);
    }

    pub fn create_channel<H: Repr>(
        &self,
        capacity: usize,
    ) -> Result<(Channel<H>, Port<H>), ChannelCreateError> {
        let geometry = shared::Duplex::<H>::geometry(capacity)
            .map_err(|_| ChannelCreateError::InvalidCapacity)?;
        let (inner, mapped) = self
            .dir
            .create_in::<shared::Duplex<H>>(&self.alloc, capacity, geometry.layout)
            .map_err(|_| ChannelCreateError::Exhausted)?;
        let id = Id { inner, capacity };
        let (channel, generation) = shared::Channel::creator(mapped, geometry, id);
        Ok((channel, Port::new(id, 1, generation)))
    }

    pub fn adopt<H: Repr>(&self, port: Port<H>) -> Result<Channel<H>, AdmitError<H>> {
        if port.id.region() != self.dir.region_id() {
            return Err(AdmitError::WrongRegion(port));
        }
        let mapped = match self
            .dir
            .open::<shared::Duplex<H>>(port.id.inner, port.id.capacity)
        {
            Ok(mapped) => mapped,
            Err(_) => {
                return Err(AdmitError::Open(port));
            }
        };
        match shared::Channel::adopt(mapped, &port) {
            Ok(channel) => Ok(channel),
            Err(error) => {
                let error = match error {
                    shared::RoleAdmitError::Stale => AdmitError::Stale(port),
                    shared::RoleAdmitError::Occupied => AdmitError::Occupied(port),
                    shared::RoleAdmitError::Terminal => AdmitError::Terminal(port),
                    shared::RoleAdmitError::Orphaned => AdmitError::Orphaned(port),
                    shared::RoleAdmitError::Open => AdmitError::Open(port),
                };
                Err(error)
            }
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn contains_channel<H: Repr>(&self, id: Id<H>) -> bool {
        self.dir
            .open::<shared::Duplex<H>>(id.inner, id.capacity)
            .is_ok()
    }

    /// Closes, drains, and removes one uniquely held channel.
    ///
    /// On contention the channel is returned for retry. If reclaiming a
    /// queued token fails, that token is returned with the channel.
    pub fn remove<H: Repr>(&self, channel: Channel<H>) -> Result<(), RemoveError<H>> {
        let id = channel.id();
        if !channel.is_unique() || !self.dir.matches(id.inner, channel.layout_id()) {
            return Err(RemoveError::Busy(channel));
        }
        match channel.close_with(|token| pool::release_token(&self.dir, token, None)) {
            Ok(()) => {}
            Err(CloseError::Busy) => return Err(RemoveError::Busy(channel)),
            Err(CloseError::Evidence) => return Err(RemoveError::Evidence(channel)),
        }
        // Closing the issuer first makes a racing Port adoption roll back
        // before this observation can authorize layout reclamation.
        if !channel.peer_is_unowned() {
            return Err(RemoveError::Busy(channel));
        }
        let (mapped, index, generation, owner) = match channel.into_closed_mapped() {
            Ok(parts) => parts,
            Err(channel) => return Err(RemoveError::Busy(channel)),
        };
        let geometry = match shared::Duplex::<H>::geometry(id.capacity) {
            Ok(geometry) => geometry,
            Err(_) => unreachable!("an admitted channel has validated geometry"),
        };
        match self
            .dir
            .remove_in(&self.alloc, id.inner, mapped, geometry.layout)
        {
            Ok(()) => Ok(()),
            Err((_, mapped)) => Err(RemoveError::Busy(shared::Channel::closed(
                mapped, geometry, id, index, generation, owner,
            ))),
        }
    }

    /// Marks an exact process generation as permanently unable to access
    /// this session's region.
    ///
    /// Returns the [`Recovery`] authority for that member, or `None` when the slot is neither live nor
    /// already dead at exactly `peer`'s generation: a free slot, a superseded generation, or a retired
    /// one. A `None` is not evidence that the peer is gone, only that this region has no member to
    /// reap under that identity.
    ///
    /// # Safety
    ///
    /// The caller must know that `peer` can never access the mapping again.
    pub unsafe fn assume_dead(&self, peer: Peer) -> Option<Recovery<'_>> {
        let member = unsafe { self.dir.mark_dead(peer) }?;
        Some(Recovery {
            session: self,
            member,
        })
    }

    fn scan_recovery(&self, dead: u8, live: u8, handlers: &[RecoveryHandler]) -> bool {
        let builtins = [RecoveryHandler::of::<pool::Storage>()];
        self.dir
            .scan(|id, recorded| {
                let matches = |item: &&RecoveryHandler| {
                    item.schema == recorded.schema && item.profile == recorded.profile
                };
                let mut selected = builtins.iter().chain(handlers).filter(matches);
                let Some(handler) = selected.next() else {
                    return false;
                };
                if selected.next().is_some() {
                    return false;
                }
                unsafe { (handler.run)(&self.dir, id, recorded, dead, live) }
            })
            .is_ok_and(|complete| complete)
    }
}
