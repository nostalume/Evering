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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenPoolError {
    Unavailable(crate::PoolId),
}

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

pub struct Session {
    dir: dir::MapDirectory,
    alloc: talc::MapTalc,
}

#[must_use = "recovery authority must be completed or durably delegated"]
pub struct Recovery<'a> {
    session: &'a Session,
    member: Member,
}

impl Recovery<'_> {
    pub fn peer(&self) -> Peer {
        Peer::from_parts(self.member.slot, self.member.generation)
    }

    pub fn reap(self) -> Result<(), Self> {
        self.reap_with(&[])
    }

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
        pool::open(&self.dir, id).ok_or(OpenPoolError::Unavailable(id))
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
