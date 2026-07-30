#![allow(unused_imports)]

pub mod talc {
    use core::marker::PhantomData;

    use crate::channel::cross;
    use crate::{
        boxed::PBox,
        channel::Repair,
        dir,
        mem::{self, AllocError, MapLayout, MemOps, Peer, Recovery},
        msg::{Encoded, Repr},
        talc,
        token::{DiscardError, OpenError, PackToken, Shape, TokenOf},
    };

    pub use crate::{
        mem::{Access, Request, Source},
        schema::RegionId,
        talc::{Geometry, GeometryError},
    };

    pub mod channel {
        use crate::{channel::driver::CachePoolHandle, talc, token};

        pub use crate::channel::driver::{
            Completer, Completion, CompletionReject, SubmitCause, Submitter, TrySubmitError,
        };
        pub use crate::channel::{QueueChannel, TryRecvError, TrySendError};
        pub use crate::token::{ReqId, ReqNull};

        pub type Token = token::Token<talc::Meta>;
        pub type CachePool<H, const N: usize> = CachePoolHandle<token::PackToken<H, talc::Meta>, N>;
    }

    pub type Meta = talc::Meta;

    type AllocConfig = talc::Config;
    type AllocHeader = talc::Header;
    type MapAlloc = talc::MapTalc;
    type MsgDuplex<H> = cross::Duplex<H, Meta>;
    type MsgDuplexView<H> = cross::View<H, Meta>;
    type Opened<'a, P, T> = Result<(P, PBox<'a, T>), OpenError<PackToken<P, Meta>>>;
    type RemoveError<H> = (MsgDuplexView<H>, Option<PackToken<H, Meta>>);

    #[derive(Clone)]
    pub struct Heap<'a> {
        alloc: talc::RefTalc<'a>,
    }

    impl Heap<'_> {
        pub fn put<T: Repr>(&self, value: T) -> Result<TokenOf<T, Meta>, PutError<T>> {
            let () = T::VALID;
            let slot = match PBox::try_new_uninit_in(self.alloc.clone()) {
                Ok(slot) => slot,
                Err(error) => return Err(PutError { error, value }),
            };
            Ok(slot.write(value).token_of())
        }

        pub fn copy<T: Repr + Copy>(&self, value: &[T]) -> Result<TokenOf<[T], Meta>, AllocError> {
            let () = T::VALID;
            PBox::try_new_slice_in(value.len(), |index| value[index], self.alloc.clone())
                .map(PBox::token_of)
        }

        pub fn init<T: Repr>(
            &self,
            len: usize,
            make: impl FnMut(usize) -> T,
        ) -> Result<TokenOf<[T], Meta>, AllocError> {
            let () = T::VALID;
            PBox::try_new_slice_in(len, make, self.alloc.clone()).map(PBox::token_of)
        }

        pub fn encode(
            &self,
            schema: crate::SchemaKey,
            bytes: &[u8],
        ) -> Result<PackToken<Encoded, Meta>, AllocError> {
            self.copy(bytes)
                .map(|token| token.pack(Encoded::new(schema)))
        }

        pub fn open_encoded(&self, record: PackToken<Encoded, Meta>) -> Opened<'_, Encoded, [u8]> {
            self.open::<Encoded, [u8]>(record)
        }

        pub fn discard<P: Repr>(
            &self,
            record: PackToken<P, Meta>,
        ) -> Result<P, DiscardError<PackToken<P, Meta>>> {
            record.discard_with(self.alloc.clone())
        }

        pub fn open<P, T>(&self, record: PackToken<P, Meta>) -> Opened<'_, P, T>
        where
            P: Repr,
            T: ?Sized + Repr + Shape,
        {
            record.open::<T, _>(self.alloc.clone())
        }
    }

    pub struct PutError<T> {
        pub error: AllocError,
        pub value: T,
    }

    impl<T> core::fmt::Debug for PutError<T> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("PutError")
                .field("error", &self.error)
                .finish_non_exhaustive()
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct Id<H: Repr> {
        inner: dir::Id<MsgDuplex<H>>,
        capacity: usize,
    }

    impl<H: Repr> Copy for Id<H> {}

    impl<H: Repr> Clone for Id<H> {
        fn clone(&self) -> Self {
            *self
        }
    }

    impl<H: Repr> Id<H> {
        pub const fn new(
            region: RegionId,
            slab: u32,
            entry: u32,
            generation: usize,
            capacity: usize,
        ) -> Self {
            Self {
                inner: dir::Id::from_parts(region, slab, entry, generation),
                capacity,
            }
        }

        pub const fn region(self) -> RegionId {
            self.inner.parts().0
        }

        pub const fn slab(self) -> u32 {
            self.inner.parts().1
        }

        pub const fn entry(self) -> u32 {
            self.inner.parts().2
        }

        pub const fn generation(self) -> usize {
            self.inner.parts().3
        }

        pub const fn capacity(self) -> usize {
            self.capacity
        }
    }

    pub struct SessionBy<H: Repr> {
        _marker: PhantomData<H>,
    }

    pub struct Session<H: Repr> {
        dir: dir::MapDirectory,
        alloc: MapAlloc,
        _marker: PhantomData<fn() -> H>,
    }

    impl<H: Repr> SessionBy<H> {
        pub fn create<S: Source>(
            source: S,
            request: Request,
            region: RegionId,
        ) -> Result<Session<H>, mem::OpenError<S::Error>> {
            Self::admit(source, request, crate::RegionAdmission::Create(region))
        }

        pub fn open<S: Source>(
            source: S,
            request: Request,
            region: RegionId,
        ) -> Result<Session<H>, mem::OpenError<S::Error>> {
            Self::admit(source, request, crate::RegionAdmission::Expect(region))
        }

        fn admit<S: Source>(
            source: S,
            request: Request,
            admission: crate::RegionAdmission,
        ) -> Result<Session<H>, mem::OpenError<S::Error>> {
            let area = MapLayout::map(source, request, admission)?;
            Self::from(area).map_err(mem::OpenError::Admission)
        }

        pub fn from(area: MapLayout) -> Result<Session<H>, mem::Error> {
            let conf = AllocConfig::new(area.size());
            Self::from_config(area, conf)
        }

        pub fn from_geometry(
            area: MapLayout,
            geometry: Geometry,
        ) -> Result<Session<H>, mem::Error> {
            let conf = AllocConfig::new(area.size()).with_geometry(geometry);
            Self::from_config(area, conf)
        }

        pub(crate) fn from_config(
            area: MapLayout,
            conf: AllocConfig,
        ) -> Result<Session<H>, mem::Error> {
            let mut area = area;
            let dir = area.push::<dir::Header>(())?;
            let areserve = area.reserve::<AllocHeader>()?;
            let conf = conf.with_bound(areserve.remaining_after());
            let alloc = areserve.commit(conf)?;
            let alloc = MapAlloc::from_handle(alloc);
            Ok(Session {
                dir,
                alloc,
                _marker: PhantomData,
            })
        }
    }

    impl<H: Repr> TryFrom<MapLayout> for Session<H> {
        type Error = mem::Error;

        fn try_from(value: MapLayout) -> Result<Self, Self::Error> {
            SessionBy::<H>::from(value)
        }
    }

    impl<H: Repr> Session<H> {
        pub fn base_addr(&self) -> usize {
            mem::MemAlloc::base_ptr(&self.alloc).addr()
        }

        pub fn peer(&self) -> Peer {
            self.dir.peer()
        }

        pub fn heap(&self) -> Heap<'_> {
            Heap {
                alloc: self.alloc.as_ref(),
            }
        }

        pub fn prepare(&self, capacity: usize) -> Option<Id<H>> {
            let geometry = MsgDuplex::<H>::geometry(capacity).ok()?;
            let (inner, mapped) = self
                .dir
                .create_in::<MsgDuplex<H>>(
                    &self.alloc,
                    cross::Config::new(capacity),
                    geometry.layout(),
                )
                .ok()?;
            drop(mapped);
            Some(Id { inner, capacity })
        }

        pub fn peek(&self, id: Id<H>) -> Option<MsgDuplexView<H>> {
            self.acquire(id)
        }

        pub fn acquire(&self, id: Id<H>) -> Option<MsgDuplexView<H>> {
            let mapped = self
                .dir
                .open::<MsgDuplex<H>>(id.inner, cross::Config::new(id.capacity))
                .ok()?;
            MsgDuplexView::new(mapped)
        }

        /// Closes, drains, and removes one channel in one bounded attempt.
        ///
        /// On contention the view is returned for retry. If discarding a
        /// queued record fails, that record is returned with the view.
        pub fn remove(&self, id: Id<H>, view: MsgDuplexView<H>) -> Result<(), RemoveError<H>> {
            if view.capacity() != id.capacity
                || !self.dir.matches(id.inner, view.layout_id())
                || !view.is_unique()
            {
                return Err((view, None));
            }
            let heap = self.heap();
            match view.quiesce(|record| {
                heap.discard(record)
                    .map(|_| ())
                    .map_err(|error| error.record)
            }) {
                Ok(true) => {}
                Ok(false) => return Err((view, None)),
                Err(record) => return Err((view, Some(record))),
            }
            let mapped = match view.into_mapped() {
                Ok(mapped) => mapped,
                Err(view) => return Err((view, None)),
            };
            let layout = match MsgDuplex::<H>::geometry(id.capacity) {
                Ok(geometry) => geometry.layout(),
                Err(_) => return Err((MsgDuplexView::new(mapped).unwrap(), None)),
            };
            match self.dir.remove_in(&self.alloc, id.inner, mapped, layout) {
                Ok(()) => Ok(()),
                Err((_, mapped)) => Err((MsgDuplexView::new(mapped).unwrap(), None)),
            }
        }

        /// Marks an exact process generation as permanently unable to access
        /// this session's region.
        ///
        /// # Safety
        ///
        /// The caller must know that `peer` can never access the mapping again.
        pub unsafe fn assume_dead(&self, peer: Peer) -> Option<Recovery<'_>> {
            unsafe { self.dir.assume_dead(peer) }
        }

        /// Marks `peer` dead using terminal evidence from a retained child.
        ///
        /// # Safety
        ///
        /// The caller must prove that `peer` is the exact participant owned by
        /// `exit`, and that no inherited or duplicated mapping authority can
        /// access this session after that child terminated.
        #[cfg(feature = "process")]
        pub unsafe fn assume_exited(
            &self,
            peer: Peer,
            _exit: &crate::process::Exit<'_>,
        ) -> Option<Recovery<'_>> {
            unsafe { self.assume_dead(peer) }
        }

        fn scan_recovery(&self, dead: u8, live: u8, repair: bool) -> bool {
            self.dir
                .scan::<MsgDuplex<H>>(|id| {
                    let Some(capacity) = self.dir.info(id).ok().and_then(cross::Info::capacity)
                    else {
                        return false;
                    };
                    let Ok(mapped) = self
                        .dir
                        .open::<MsgDuplex<H>>(id, cross::Config::new(capacity))
                    else {
                        return false;
                    };
                    if !repair {
                        return !mapped.has_member(dead);
                    }
                    let Some(view) = MsgDuplexView::new(mapped) else {
                        return false;
                    };
                    !matches!(view.repair(dead, live), Repair::Busy(_) | Repair::Corrupted)
                })
                .is_ok_and(|complete| complete)
        }

        /// Repairs every layout owned by `recovery` and releases its participant
        /// slot only after a second complete scan proves no ownership remains.
        pub fn reap<'a>(&'a self, recovery: Recovery<'a>) -> Result<(), Recovery<'a>> {
            if recovery.peer() == self.dir.peer() {
                return Err(recovery);
            }
            let dead = recovery.slot();
            let live = self.dir.peer().slot();
            match self.dir.transaction_owner() {
                None => {}
                Some(owner) if owner == dead => {
                    if self.dir.recover(&self.alloc, &recovery).is_err() {
                        return Err(recovery);
                    }
                }
                Some(_) => return Err(recovery),
            }
            if !self.alloc.finish_recovery(dead) {
                return Err(recovery);
            }
            if !self.scan_recovery(dead, live, true) || !self.scan_recovery(dead, live, false) {
                return Err(recovery);
            }
            self.dir.recover_member(dead);
            if self.dir.has_member(dead) {
                return Err(recovery);
            }
            recovery.release()
        }
    }
}
