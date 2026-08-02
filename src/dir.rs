use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::header::{self, Layout};
use crate::schema::{LayoutContext, RegionId, SchemaKey, SharedSchema, schema_id};

const CAPACITY: usize = 32;
const CLEAN: usize = 0;
const HELD: usize = 1;
const EVIDENCE: usize = 2;
const ROLLING_BACK: usize = 3;
const TRANSACTION_BITS: usize = 2;
const VACANT: usize = 0;
const PENDING: usize = 1;
const PUBLISHED: usize = 2;
const CLOSING: usize = 3;
const REMOVING: usize = 4;
const RELEASED: usize = 5;
const QUARANTINED: usize = 6;
const CREATE: usize = 0;
const REMOVE: usize = 1;
const GROW: usize = 2;
const STATE_BITS: usize = 3;
const STATE_MASK: usize = (1 << STATE_BITS) - 1;
const GENERATION_MAX: usize = usize::MAX >> STATE_BITS;

#[cfg(test)]
pub(crate) const CREATE_PENDING: usize = 1;
#[cfg(test)]
pub(crate) const CREATE_ALLOCATED: usize = 2;
#[cfg(test)]
pub(crate) const CREATE_EVIDENCE: usize = 3;
#[cfg(test)]
pub(crate) const CREATE_HEAP_CLEAN: usize = 4;
#[cfg(test)]
pub(crate) const CREATE_PUBLISHED: usize = 5;
#[cfg(test)]
pub(crate) const REMOVE_CLOSING: usize = 6;
#[cfg(test)]
pub(crate) const REMOVE_CLOSED: usize = 7;
#[cfg(test)]
pub(crate) const REMOVE_REMOVING: usize = 8;
#[cfg(test)]
pub(crate) const REMOVE_DEALLOCATED: usize = 9;
#[cfg(test)]
pub(crate) const REMOVE_RELEASED: usize = 10;
#[cfg(test)]
pub(crate) const REMOVE_HEAP_CLEAN: usize = 11;
#[cfg(test)]
pub(crate) const REMOVE_VACANT: usize = 12;
#[cfg(test)]
pub(crate) const GROW_ALLOCATED: usize = 13;
#[cfg(test)]
pub(crate) const GROW_EVIDENCE: usize = 14;
#[cfg(test)]
pub(crate) const GROW_HEAP_CLEAN: usize = 15;
#[cfg(test)]
pub(crate) const GROW_LINKED: usize = 16;
#[cfg(test)]
pub(crate) const GROW_CLEARED: usize = 17;
#[cfg(test)]
static CRASH_AFTER: AtomicUsize = AtomicUsize::new(0);

#[cfg(all(test, unix, feature = "map"))]
pub(crate) fn crash_after_for_test(point: usize) {
    CRASH_AFTER.store(point, Ordering::Relaxed);
}

#[cfg(test)]
fn crash_after(point: usize) {
    if CRASH_AFTER.load(Ordering::Relaxed) == point {
        std::process::exit(80 + point as i32);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Busy(u8),
    Full,
    Stale,
    Admission(crate::mem::ProjectionError),
    Heap(crate::talc::MutationError),
    RolledBack,
    InUse,
    Close(crate::header::CloseError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Recovered {
    RolledBack,
    Released,
    Retained,
    Quarantined,
}

impl MapDirectory {
    pub(crate) fn transaction_owner(&self) -> Option<u8> {
        Directory::word_owner(self.inner.transaction.load(Ordering::Acquire))
    }

    pub(crate) fn scan(
        &self,
        mut visit: impl FnMut(Id<()>, header::RecordedLayout) -> bool,
    ) -> Result<bool, Error> {
        let mut slab_index = 0u32;
        let mut slab = SlabRef::Root(&self.inner);
        loop {
            for (entry_index, entry) in slab.entries().iter().enumerate() {
                let word = entry.state.load(Ordering::Acquire);
                match word & STATE_MASK {
                    VACANT | QUARANTINED => {}
                    PUBLISHED => {
                        let offset = entry.offset.load(Ordering::Relaxed);
                        let record = unsafe { self.ref_at::<header::Record>(offset) }
                            .map_err(Error::Admission)?;
                        let recorded = unsafe {
                            header::recorded_at((record as *const header::Record).cast())
                        }
                        .map_err(crate::mem::ProjectionError::Admission)
                        .map_err(Error::Admission)?;
                        self.offset_of(core::ptr::NonNull::from(record).cast(), recorded.extent)
                            .map_err(Error::Admission)?;
                        if !visit(
                            Id::from_parts(
                                self.region_id(),
                                slab_index,
                                entry_index as u32,
                                word >> STATE_BITS,
                            ),
                            recorded,
                        ) {
                            return Ok(false);
                        }
                    }
                    _ => return Err(Error::InUse),
                }
            }
            match self.next_slab(slab)? {
                Some(next) => slab = next,
                None => return Ok(true),
            }
            slab_index = slab_index.checked_add(1).ok_or(Error::Full)?;
        }
    }

    pub(crate) unsafe fn recover_layout<L: Layout>(
        &self,
        id: Id<()>,
        recorded: header::RecordedLayout,
        dead: u8,
        live: u8,
    ) -> bool {
        if recorded.mismatch::<L>().is_some() {
            return false;
        }
        let Ok(offset) = self.offset(id) else {
            return false;
        };
        let Ok(header) = (unsafe { self.ref_at::<header::RcHeader<L>>(offset) }) else {
            return false;
        };
        let ctx = LayoutContext {
            region: self.region_id(),
            offset: offset as u64,
            allow_init: false,
        };
        if unsafe { header::RcHeader::<L>::inspect_at(header, ctx) }.is_err()
            || !L::recover(header::RecoveryContext {
                layout: &header.inner,
                info: header.layout_info(),
                extent: recorded.extent,
                dead,
                live,
                base: core::ptr::from_ref(header).cast(),
                directory: self,
            })
        {
            return false;
        }
        header.recover_member(dead);
        !header.has_member(dead)
    }

    pub(crate) fn matches<L>(&self, id: Id<L>, layout: crate::schema::LayoutId) -> bool {
        self.offset(id)
            .is_ok_and(|offset| layout.region == self.region_id() && layout.offset == offset as u64)
    }

    pub(crate) fn inspect_layout<L: Layout, R>(
        &self,
        layout: crate::schema::LayoutId,
        inspect: impl FnOnce(&header::RcHeader<L>, usize) -> R,
    ) -> Option<R> {
        if layout.region != self.region_id() {
            return None;
        }
        let mut found = None;
        self.scan(|id, recorded| {
            if recorded.mismatch::<L>().is_none()
                && self
                    .offset(id)
                    .is_ok_and(|offset| offset as u64 == layout.offset)
            {
                found = Some((id, recorded));
            }
            true
        })
        .ok()?;
        let (id, recorded) = found?;
        let offset = self.offset(id).ok()?;
        let header = unsafe { self.ref_at::<header::RcHeader<L>>(offset) }.ok()?;
        let context = LayoutContext {
            region: self.region_id(),
            offset: offset as u64,
            allow_init: false,
        };
        unsafe { header::RcHeader::<L>::inspect_at(header, context) }.ok()?;
        Some(inspect(header, recorded.extent))
    }

    pub(crate) fn open<L: Layout>(
        &self,
        id: Id<L>,
        conf: L::Config,
    ) -> Result<crate::mem::Mapped<header::RcHeader<L>>, Error> {
        let offset = self.offset(id)?;
        self.admit_at::<header::RcHeader<L>>(offset, conf, false)
            .map_err(Error::Admission)
    }

    pub(crate) fn open_recorded<L: Layout, U>(
        &self,
        id: Id<L>,
        discover: impl FnOnce(L::Info, usize) -> Option<(L::Config, U)>,
    ) -> Option<(crate::mem::Mapped<header::RcHeader<L>>, U)> {
        let offset = self.offset(id).ok()?;
        let record = unsafe { self.ref_at::<header::Record>(offset) }.ok()?;
        let recorded = unsafe { header::recorded_at(core::ptr::from_ref(record).cast()) }.ok()?;
        self.offset_of(core::ptr::NonNull::from(record).cast(), recorded.extent)
            .ok()?;
        let header = unsafe { self.ref_at::<header::RcHeader<L>>(offset) }.ok()?;
        let info = unsafe {
            header::RcHeader::<L>::inspect_at(
                header,
                LayoutContext {
                    region: self.region_id(),
                    offset: offset as u64,
                    allow_init: false,
                },
            )
        }
        .ok()?;
        let (conf, value) = discover(info, recorded.extent)?;
        self.admit_at::<header::RcHeader<L>>(offset, conf, false)
            .ok()
            .map(|mapped| (mapped, value))
    }

    #[cfg(test)]
    pub(crate) fn create<L: Layout>(
        &self,
        heap: &crate::talc::MapTalc,
        conf: L::Config,
    ) -> Result<(Id<L>, crate::mem::Mapped<header::RcHeader<L>>), Error> {
        self.create_in(
            heap,
            conf,
            core::alloc::Layout::new::<header::RcHeader<L>>(),
        )
    }

    pub(crate) fn create_in<L: Layout>(
        &self,
        heap: &crate::talc::MapTalc,
        conf: L::Config,
        layout: core::alloc::Layout,
    ) -> Result<(Id<L>, crate::mem::Mapped<header::RcHeader<L>>), Error> {
        if self.region_id() != heap.region_id() {
            return Err(Error::Stale);
        }
        if L::storage(&conf) != Some(layout) {
            return Err(Error::Stale);
        }
        let owner = self.peer().slot();
        let mut transaction = match self.inner.claim(owner) {
            Ok(transaction) => transaction,
            Err(Error::Busy(busy))
                if busy == owner
                    && self.inner.transaction.load(Ordering::Acquire)
                        == Directory::owner_word(owner, ROLLING_BACK) =>
            {
                self.resume_rollback(heap, owner)?;
                return Err(Error::RolledBack);
            }
            Err(error) => return Err(error),
        };
        let slot = match self.reserve() {
            Ok(slot) => slot,
            Err(Error::Full) => {
                if let Err(error) = self.grow(heap, &mut transaction) {
                    transaction.commit();
                    return Err(error);
                }
                match self.reserve() {
                    Ok(slot) => slot,
                    Err(error) => {
                        transaction.commit();
                        return Err(error);
                    }
                }
            }
            Err(error) => {
                transaction.commit();
                return Err(error);
            }
        };
        let entry = match self.entry(slot.slab, slot.entry) {
            Ok(entry) => entry,
            Err(error) => {
                transaction.commit();
                return Err(error);
            }
        };
        entry
            .state
            .store((slot.generation << STATE_BITS) | PENDING, Ordering::Relaxed);
        #[cfg(test)]
        crash_after(CREATE_PENDING);

        let mutation = match heap.begin_mutation() {
            Ok(mutation) => mutation,
            Err(error) => {
                entry.state.store(slot.vacant, Ordering::Relaxed);
                transaction.commit();
                return Err(Error::Heap(error));
            }
        };
        let meta = match unsafe { heap.allocate_during(&mutation, layout) } {
            Ok(meta) => meta,
            Err(error) => {
                mutation.commit();
                entry.state.store(slot.vacant, Ordering::Relaxed);
                transaction.commit();
                return Err(Error::Heap(error));
            }
        };
        #[cfg(test)]
        crash_after(CREATE_ALLOCATED);
        let pointer = heap.pointer(&meta);
        // Safety: `meta` is the live allocation just returned under this
        // mutation; no pointer to its not-yet-published bytes exists.
        unsafe { pointer.as_ptr().write_bytes(0, layout.size()) };
        let offset = match self.offset_of(pointer, layout.size()) {
            Ok(offset) => offset,
            Err(error) => {
                unsafe { heap.deallocate_during(&mutation, pointer, layout) };
                mutation.commit();
                entry.state.store(slot.vacant, Ordering::Relaxed);
                transaction.commit();
                return Err(Error::Admission(error));
            }
        };
        transaction.persist(slot.slab, slot.entry, meta, CREATE);
        #[cfg(test)]
        crash_after(CREATE_EVIDENCE);
        mutation.commit();
        #[cfg(test)]
        crash_after(CREATE_HEAP_CLEAN);

        let mapped = match self.admit_at::<header::RcHeader<L>>(offset, conf, true) {
            Ok(mapped) => mapped,
            Err(error) => {
                let mutation = heap.begin_mutation().map_err(Error::Heap)?;
                let pointer = heap.pointer(transaction.pending());
                unsafe { heap.deallocate_during(&mutation, pointer, layout) };
                entry.state.store(slot.vacant, Ordering::Relaxed);
                transaction.clear_pending();
                mutation.commit();
                transaction.commit();
                return Err(Error::Admission(error));
            }
        };

        entry.offset.store(offset, Ordering::Relaxed);
        entry.state.store(
            (slot.generation << STATE_BITS) | PUBLISHED,
            Ordering::Release,
        );
        #[cfg(test)]
        crash_after(CREATE_PUBLISHED);
        transaction.clear_pending();
        transaction.commit();
        Ok((
            Id {
                region: self.region_id(),
                slab: slot.slab,
                entry: slot.entry as u32,
                generation: slot.generation,
                _layout: PhantomData,
            },
            mapped,
        ))
    }

    pub(crate) fn remove_in<L: Layout>(
        &self,
        heap: &crate::talc::MapTalc,
        id: Id<L>,
        mut mapped: crate::mem::Mapped<header::RcHeader<L>>,
        layout: core::alloc::Layout,
    ) -> Result<(), (Error, crate::mem::Mapped<header::RcHeader<L>>)> {
        if self.region_id() != heap.region_id() {
            return Err((Error::Stale, mapped));
        }
        let header = core::alloc::Layout::new::<header::RcHeader<L>>();
        if layout.size() < header.size() || layout.align() < header.align() {
            return Err((Error::Stale, mapped));
        }
        let offset = match self.offset(id) {
            Ok(offset) => offset,
            Err(error) => return Err((error, mapped)),
        };
        if mapped.region_id() != self.region_id() || mapped.layout_id().offset != offset as u64 {
            return Err((Error::Stale, mapped));
        }
        let mut transaction = match self.inner.claim(self.peer().slot()) {
            Ok(transaction) => transaction,
            Err(error) => return Err((error, mapped)),
        };
        let entry = match self.entry(id.slab, id.entry as usize) {
            Ok(entry) => entry,
            Err(error) => {
                transaction.commit();
                return Err((error, mapped));
            }
        };
        let published = (id.generation << STATE_BITS) | PUBLISHED;
        if entry.state.load(Ordering::Acquire) != published {
            transaction.commit();
            return Err((Error::Stale, mapped));
        }
        let pointer = mapped.pointer().cast::<u8>();
        let Some(meta) = heap.meta_for(pointer, layout.size()) else {
            transaction.commit();
            return Err((Error::Stale, mapped));
        };
        let mutation = match heap.begin_mutation() {
            Ok(mutation) => mutation,
            Err(error) => {
                transaction.commit();
                return Err((Error::Heap(error), mapped));
            }
        };
        entry
            .state
            .store((id.generation << STATE_BITS) | CLOSING, Ordering::Release);
        transaction.persist(id.slab, id.entry as usize, meta, REMOVE);
        #[cfg(test)]
        crash_after(REMOVE_CLOSING);
        if let Err(error) = mapped.close_unique(self.peer().slot()) {
            entry.state.store(published, Ordering::Release);
            transaction.clear_pending();
            mutation.commit();
            transaction.commit();
            return Err((Error::Close(error), mapped));
        }
        mapped.disarm_detach();
        #[cfg(test)]
        crash_after(REMOVE_CLOSED);
        entry
            .state
            .store((id.generation << STATE_BITS) | REMOVING, Ordering::Release);
        #[cfg(test)]
        crash_after(REMOVE_REMOVING);
        unsafe { heap.deallocate_during(&mutation, pointer, layout) };
        #[cfg(test)]
        crash_after(REMOVE_DEALLOCATED);
        entry
            .state
            .store((id.generation << STATE_BITS) | RELEASED, Ordering::Release);
        #[cfg(test)]
        crash_after(REMOVE_RELEASED);
        mutation.commit();
        #[cfg(test)]
        crash_after(REMOVE_HEAP_CLEAN);
        entry
            .state
            .store(id.generation << STATE_BITS, Ordering::Release);
        #[cfg(test)]
        crash_after(REMOVE_VACANT);
        transaction.clear_pending();
        transaction.commit();
        drop(mapped);
        Ok(())
    }

    fn slab(&self, index: u32) -> Result<SlabRef<'_>, Error> {
        let mut slab = SlabRef::Root(&self.inner);
        for _ in 0..index {
            slab = self.next_slab(slab)?.ok_or(Error::Stale)?;
        }
        Ok(slab)
    }

    fn next_slab<'a>(&'a self, slab: SlabRef<'a>) -> Result<Option<SlabRef<'a>>, Error> {
        let offset = slab.next().load(Ordering::Acquire);
        if offset == 0 {
            return Ok(None);
        }
        unsafe { self.ref_at::<Slab>(offset) }
            .map(SlabRef::Linked)
            .map(Some)
            .map_err(Error::Admission)
    }

    fn entry(&self, slab: u32, entry: usize) -> Result<&Entry, Error> {
        self.slab(slab)?.entries().get(entry).ok_or(Error::Stale)
    }

    fn reserve(&self) -> Result<Slot, Error> {
        let mut index = 0u32;
        let mut slab = SlabRef::Root(&self.inner);
        loop {
            for (entry_index, entry) in slab.entries().iter().enumerate() {
                let state = entry.state.load(Ordering::Relaxed);
                if state & STATE_MASK != VACANT {
                    continue;
                }
                let generation = state >> STATE_BITS;
                if generation != GENERATION_MAX {
                    return Ok(Slot {
                        slab: index,
                        entry: entry_index,
                        generation: generation + 1,
                        vacant: state,
                    });
                }
            }
            slab = self.next_slab(slab)?.ok_or(Error::Full)?;
            index = index.checked_add(1).ok_or(Error::Full)?;
        }
    }

    fn tail(&self) -> Result<(u32, SlabRef<'_>), Error> {
        let mut index = 0u32;
        let mut slab = SlabRef::Root(&self.inner);
        loop {
            match self.next_slab(slab)? {
                Some(next) => slab = next,
                None => return Ok((index, slab)),
            }
            index = index.checked_add(1).ok_or(Error::Full)?;
        }
    }

    fn transition(&self) -> Result<Option<(u32, usize)>, Error> {
        let mut index = 0u32;
        let mut slab = SlabRef::Root(&self.inner);
        loop {
            if let Some(entry) = slab.entries().iter().position(|entry| {
                !matches!(
                    entry.state.load(Ordering::Acquire) & STATE_MASK,
                    VACANT | PUBLISHED | QUARANTINED
                )
            }) {
                return Ok(Some((index, entry)));
            }
            match self.next_slab(slab)? {
                Some(next) => slab = next,
                None => return Ok(None),
            }
            index = index.checked_add(1).ok_or(Error::Full)?;
        }
    }

    fn grow(
        &self,
        heap: &crate::talc::MapTalc,
        transaction: &mut Transaction<'_>,
    ) -> Result<(), Error> {
        let (tail_index, tail) = self.tail()?;
        let slab_index = tail_index.checked_add(1).ok_or(Error::Full)?;
        let mutation = heap.begin_mutation().map_err(Error::Heap)?;
        let layout = core::alloc::Layout::new::<Slab>();
        let meta = match unsafe { heap.allocate_during(&mutation, layout) } {
            Ok(meta) => meta,
            Err(error) => {
                mutation.commit();
                return Err(Error::Heap(error));
            }
        };
        let pointer = heap.pointer(&meta);
        unsafe { pointer.cast::<Slab>().as_ptr().write(Slab::new()) };
        #[cfg(test)]
        crash_after(GROW_ALLOCATED);
        let offset = match self.offset_of(pointer, layout.size()) {
            Ok(offset) if offset != 0 => offset,
            Ok(_) => unreachable!("Talc allocation cannot overlap the region root"),
            Err(error) => {
                unsafe { heap.deallocate_during(&mutation, pointer, layout) };
                mutation.commit();
                return Err(Error::Admission(error));
            }
        };
        transaction.persist(slab_index, 0, meta, GROW);
        #[cfg(test)]
        crash_after(GROW_EVIDENCE);
        mutation.commit();
        #[cfg(test)]
        crash_after(GROW_HEAP_CLEAN);
        tail.next().store(offset, Ordering::Release);
        #[cfg(test)]
        crash_after(GROW_LINKED);
        transaction.clear_pending();
        #[cfg(test)]
        crash_after(GROW_CLEARED);
        Ok(())
    }

    fn offset<L>(&self, id: Id<L>) -> Result<usize, Error> {
        if id.region != self.region_id() {
            return Err(Error::Stale);
        }
        let entry = self.entry(id.slab, id.entry as usize)?;
        let state = entry.state.load(Ordering::Acquire);
        if state & STATE_MASK != PUBLISHED || state >> STATE_BITS != id.generation {
            return Err(Error::Stale);
        }
        Ok(entry.offset.load(Ordering::Relaxed))
    }

    pub(crate) fn recover(
        &self,
        heap: &crate::talc::MapTalc,
        dead: u8,
    ) -> Result<Recovered, Error> {
        if self.region_id() != heap.region_id() {
            return Err(Error::Stale);
        }
        use crate::talc::MutationState;

        let word = self.inner.transaction.load(Ordering::Acquire);
        if word == CLEAN {
            return Err(Error::Stale);
        }
        let owner = ((word >> TRANSACTION_BITS).saturating_sub(1)) as u8;
        if owner != dead {
            return Err(Error::Busy(owner));
        }
        let phase = word & ((1 << TRANSACTION_BITS) - 1);
        let kind = self.inner.pending_kind.load(Ordering::Relaxed);
        if matches!(phase, EVIDENCE | ROLLING_BACK) && kind == GROW {
            return self.recover_growth(heap, dead, word, phase, true);
        }
        let location = if matches!(phase, EVIDENCE | ROLLING_BACK) {
            Some((
                self.inner.pending_slab.load(Ordering::Relaxed) as u32,
                self.inner.pending_entry.load(Ordering::Relaxed),
            ))
        } else {
            self.transition()?
        };
        let Some((slab, index)) = location else {
            if phase != HELD {
                return Err(Error::Stale);
            }
            if matches!(heap.mutation_state(), crate::talc::MutationState::Owned(found) if found == dead)
            {
                let _ = heap.poison_owner(dead);
                self.transfer_and_clear(word, dead)?;
                return Ok(Recovered::Quarantined);
            }
            self.transfer_and_clear(word, dead)?;
            return Ok(Recovered::RolledBack);
        };
        let entry = self.entry(slab, index)?;
        let state = entry.state.load(Ordering::Acquire);
        let generation = state >> STATE_BITS;
        let operation = (kind, state & STATE_MASK, phase);
        if matches!(operation, (REMOVE, CLOSING, EVIDENCE | ROLLING_BACK)) {
            if !heap.clear_dead_owner(dead) {
                return match heap.mutation_state() {
                    MutationState::Owned(owner) => Err(Error::Busy(owner)),
                    MutationState::Poisoned => self.quarantine(word, entry),
                    MutationState::Clean => unreachable!(),
                };
            }
            let meta = unsafe { (*self.inner.pending.get()).assume_init_ref() };
            if !unsafe { header::closed_at(heap.pointer(meta).as_ptr()) } {
                let mut transaction = self.transfer(word, dead, phase)?;
                entry
                    .state
                    .store((generation << STATE_BITS) | PUBLISHED, Ordering::Release);
                transaction.clear_pending();
                transaction.commit();
                return Ok(Recovered::RolledBack);
            }
            let (mutation, mut transaction) =
                self.claim_heap_and_transfer(heap, dead, word, phase)?;
            let meta = transaction.pending();
            unsafe {
                heap.deallocate_during(&mutation, heap.pointer(meta), meta.layout());
            }
            entry
                .state
                .store((generation << STATE_BITS) | RELEASED, Ordering::Release);
            mutation.commit();
            entry
                .state
                .store(generation << STATE_BITS, Ordering::Release);
            transaction.clear_pending();
            transaction.commit();
            return Ok(Recovered::Released);
        }
        if matches!(operation, (REMOVE, RELEASED, EVIDENCE | ROLLING_BACK)) {
            if !heap.clear_dead_owner(dead) {
                return match heap.mutation_state() {
                    MutationState::Owned(owner) => Err(Error::Busy(owner)),
                    MutationState::Poisoned => self.quarantine(word, entry),
                    MutationState::Clean => unreachable!(),
                };
            }
            let mut transaction = self.transfer(word, dead, phase)?;
            entry
                .state
                .store(generation << STATE_BITS, Ordering::Release);
            transaction.clear_pending();
            transaction.commit();
            return Ok(Recovered::Released);
        }
        if matches!(operation, (REMOVE, VACANT, EVIDENCE | ROLLING_BACK)) {
            if let MutationState::Owned(owner) = heap.mutation_state()
                && owner != dead
            {
                return Err(Error::Busy(owner));
            }
            let mut transaction = self.transfer(word, dead, phase)?;
            transaction.clear_pending();
            transaction.commit();
            return Ok(Recovered::Released);
        }
        if matches!(heap.mutation_state(), MutationState::Owned(owner) if owner == dead) {
            let _ = heap.poison_owner(dead);
        }
        if matches!(
            operation,
            (CREATE, PENDING, EVIDENCE | ROLLING_BACK)
                | (REMOVE, REMOVING, EVIDENCE | ROLLING_BACK)
        ) {
            match heap.mutation_state() {
                MutationState::Owned(owner) => return Err(Error::Busy(owner)),
                MutationState::Poisoned => return self.quarantine(word, entry),
                MutationState::Clean => {}
            }
        }
        match operation {
            (_, PENDING, HELD) => {
                if generation == 0 {
                    return self.quarantine(word, entry);
                }
                let transaction = self.transfer(word, dead, phase)?;
                entry
                    .state
                    .store((generation - 1) << STATE_BITS, Ordering::Release);
                transaction.commit();
                Ok(Recovered::RolledBack)
            }
            (CREATE, PENDING, EVIDENCE | ROLLING_BACK) => {
                if generation == 0 {
                    return self.quarantine(word, entry);
                }
                let (mutation, mut transaction) =
                    self.claim_heap_and_transfer(heap, dead, word, phase)?;
                let meta = transaction.pending();
                unsafe {
                    heap.deallocate_during(&mutation, heap.pointer(meta), meta.layout());
                }
                entry
                    .state
                    .store((generation - 1) << STATE_BITS, Ordering::Release);
                transaction.clear_pending();
                mutation.commit();
                transaction.commit();
                Ok(Recovered::RolledBack)
            }
            (REMOVE, REMOVING, EVIDENCE | ROLLING_BACK) => {
                let (mutation, mut transaction) =
                    self.claim_heap_and_transfer(heap, dead, word, phase)?;
                let meta = transaction.pending();
                unsafe {
                    heap.deallocate_during(&mutation, heap.pointer(meta), meta.layout());
                }
                entry
                    .state
                    .store((generation << STATE_BITS) | RELEASED, Ordering::Release);
                mutation.commit();
                entry
                    .state
                    .store(generation << STATE_BITS, Ordering::Release);
                transaction.clear_pending();
                transaction.commit();
                Ok(Recovered::Released)
            }
            _ => self.quarantine(word, entry),
        }
    }

    fn recover_growth(
        &self,
        heap: &crate::talc::MapTalc,
        owner: u8,
        word: usize,
        phase: usize,
        dead: bool,
    ) -> Result<Recovered, Error> {
        use crate::talc::MutationState;

        let slab_index = u32::try_from(self.inner.pending_slab.load(Ordering::Relaxed))
            .map_err(|_| Error::Stale)?;
        let predecessor = slab_index.checked_sub(1).ok_or(Error::Stale)?;
        let meta = unsafe { (*self.inner.pending.get()).assume_init_ref() };
        let pointer = heap.pointer(meta);
        let offset = self
            .offset_of(pointer, core::mem::size_of::<Slab>())
            .map_err(Error::Admission)?;
        let link = self.slab(predecessor)?.next().load(Ordering::Acquire);

        if link == offset {
            let mut transaction = self.transfer(word, owner, phase)?;
            transaction.clear_pending();
            transaction.commit();
            return Ok(Recovered::Retained);
        }
        if link != 0 {
            let mut transaction = self.transfer(word, owner, phase)?;
            transaction.clear_pending();
            transaction.commit();
            return Ok(Recovered::Quarantined);
        }

        if dead && matches!(heap.mutation_state(), MutationState::Owned(found) if found == owner) {
            let _ = heap.poison_owner(owner);
        }
        match heap.mutation_state() {
            MutationState::Clean => {
                let (mutation, mut transaction) =
                    self.claim_heap_and_transfer(heap, owner, word, phase)?;
                unsafe {
                    heap.deallocate_during(&mutation, pointer, core::alloc::Layout::new::<Slab>())
                };
                transaction.clear_pending();
                mutation.commit();
                transaction.commit();
                Ok(Recovered::RolledBack)
            }
            MutationState::Owned(found) => Err(Error::Busy(found)),
            MutationState::Poisoned => {
                let mut transaction = self.transfer(word, owner, phase)?;
                transaction.clear_pending();
                transaction.commit();
                Ok(Recovered::Quarantined)
            }
        }
    }

    fn transfer(&self, word: usize, dead: u8, phase: usize) -> Result<Transaction<'_>, Error> {
        let live = self.peer().slot();
        self.inner
            .transaction
            .compare_exchange(
                word,
                Directory::owner_word(live, phase),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|current| Error::Busy(Directory::word_owner(current).unwrap_or(dead)))?;
        Ok(Transaction {
            directory: &self.inner,
            owner: live,
            phase,
        })
    }

    fn transfer_and_clear(&self, word: usize, dead: u8) -> Result<(), Error> {
        self.transfer(word, dead, HELD)?.commit();
        Ok(())
    }

    fn claim_heap_and_transfer<'a>(
        &'a self,
        heap: &'a crate::talc::MapTalc,
        dead: u8,
        word: usize,
        phase: usize,
    ) -> Result<(crate::talc::Mutation<'a>, Transaction<'a>), Error> {
        let mutation = heap.begin_mutation().map_err(Error::Heap)?;
        let transaction = match self.transfer(word, dead, phase) {
            Ok(transaction) => transaction,
            Err(error) => {
                mutation.commit();
                return Err(error);
            }
        };
        Ok((mutation, transaction))
    }

    fn quarantine(&self, transaction: usize, entry: &Entry) -> Result<Recovered, Error> {
        let dead = Directory::word_owner(transaction).ok_or(Error::Stale)?;
        let phase = transaction & ((1 << TRANSACTION_BITS) - 1);
        let held = self.transfer(transaction, dead, phase)?;
        let generation = entry.state.load(Ordering::Acquire) >> STATE_BITS;
        entry
            .state
            .store((generation << STATE_BITS) | QUARANTINED, Ordering::Release);
        debug_assert!(held.commit());
        Ok(Recovered::Quarantined)
    }

    fn resume_rollback(&self, heap: &crate::talc::MapTalc, owner: u8) -> Result<(), Error> {
        let expected = Directory::owner_word(owner, ROLLING_BACK);
        if self.inner.transaction.load(Ordering::Acquire) != expected {
            return Err(Error::Busy(owner));
        }
        match self.inner.pending_kind.load(Ordering::Relaxed) {
            GROW => {
                self.recover_growth(heap, owner, expected, ROLLING_BACK, false)?;
                return Ok(());
            }
            CREATE => {}
            _ => return Err(Error::Busy(owner)),
        }
        let mut transaction = Transaction {
            directory: &self.inner,
            owner,
            phase: ROLLING_BACK,
        };
        let mutation = heap.begin_mutation().map_err(Error::Heap)?;
        let meta = transaction.pending();
        let pointer = heap.pointer(meta);
        unsafe { heap.deallocate_during(&mutation, pointer, meta.layout()) };
        let slab = u32::try_from(self.inner.pending_slab.load(Ordering::Relaxed))
            .map_err(|_| Error::Stale)?;
        let index = self.inner.pending_entry.load(Ordering::Relaxed);
        let entry = self.entry(slab, index)?;
        let state = entry.state.load(Ordering::Relaxed);
        let generation = state >> STATE_BITS;
        entry.state.store(
            generation.saturating_sub(1) << STATE_BITS,
            Ordering::Relaxed,
        );
        transaction.clear_pending();
        mutation.commit();
        transaction.commit();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn abandon_released_for_test<L>(&self, id: Id<L>, dead: u8) {
        let entry = self.entry(id.slab, id.entry as usize).unwrap();
        entry
            .state
            .store((id.generation << STATE_BITS) | RELEASED, Ordering::Release);
        self.inner
            .pending_slab
            .store(id.slab as usize, Ordering::Relaxed);
        self.inner
            .pending_entry
            .store(id.entry as usize, Ordering::Relaxed);
        self.inner.pending_kind.store(REMOVE, Ordering::Relaxed);
        self.inner
            .transaction
            .store(Directory::owner_word(dead, EVIDENCE), Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn abandon_vacant_remove_for_test<L>(&self, id: Id<L>, dead: u8) {
        let entry = self.entry(id.slab, id.entry as usize).unwrap();
        entry
            .state
            .store(id.generation << STATE_BITS, Ordering::Release);
        self.inner
            .pending_slab
            .store(id.slab as usize, Ordering::Relaxed);
        self.inner
            .pending_entry
            .store(id.entry as usize, Ordering::Relaxed);
        self.inner.pending_kind.store(REMOVE, Ordering::Relaxed);
        self.inner
            .transaction
            .store(Directory::owner_word(dead, EVIDENCE), Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn abandon_prepared_remove_for_test<L: Layout>(
        &self,
        heap: &crate::talc::MapTalc,
        id: Id<L>,
        mut mapped: crate::mem::Mapped<header::RcHeader<L>>,
        layout: core::alloc::Layout,
        dead: u8,
        closed: bool,
    ) {
        let mut transaction = self.inner.claim(dead).unwrap();
        let entry = self.entry(id.slab, id.entry as usize).unwrap();
        let pointer = mapped.pointer().cast::<u8>();
        let meta = heap.meta_for(pointer, layout.size()).unwrap();
        entry
            .state
            .store((id.generation << STATE_BITS) | CLOSING, Ordering::Release);
        transaction.persist(id.slab, id.entry as usize, meta, REMOVE);
        heap.abandon_mutation_for_test(dead);
        if closed {
            mapped.close_unique(self.peer().slot()).unwrap();
            mapped.disarm_detach();
        }
        core::mem::forget(transaction);
    }

    #[cfg(test)]
    pub(crate) fn abandon_held_reservation_for_test(&self, dead: u8) {
        let slot = self.reserve().unwrap();
        self.entry(slot.slab, slot.entry)
            .unwrap()
            .state
            .store((slot.generation << STATE_BITS) | PENDING, Ordering::Release);
        self.inner
            .transaction
            .store(Directory::owner_word(dead, HELD), Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn abandon_allocated_create_for_test(&self, dead: u8, meta: crate::talc::Meta) {
        let slot = self.reserve().unwrap();
        self.entry(slot.slab, slot.entry)
            .unwrap()
            .state
            .store((slot.generation << STATE_BITS) | PENDING, Ordering::Release);
        self.inner
            .pending_slab
            .store(slot.slab as usize, Ordering::Relaxed);
        self.inner
            .pending_entry
            .store(slot.entry, Ordering::Relaxed);
        self.inner.pending_kind.store(CREATE, Ordering::Relaxed);
        unsafe { (*self.inner.pending.get()).write(meta) };
        self.inner
            .transaction
            .store(Directory::owner_word(dead, EVIDENCE), Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn abandon_growth_for_test(
        &self,
        heap: &crate::talc::MapTalc,
        dead: u8,
        linked: bool,
        clean_heap: bool,
    ) {
        assert!(!linked || clean_heap);
        let mut transaction = self.inner.claim(dead).unwrap();
        let (tail_index, tail) = self.tail().unwrap();
        let slab_index = tail_index.checked_add(1).unwrap();
        let mutation = heap.begin_mutation().unwrap();
        let layout = core::alloc::Layout::new::<Slab>();
        let meta = unsafe { heap.allocate_during(&mutation, layout) }.unwrap();
        let pointer = heap.pointer(&meta);
        unsafe { pointer.cast::<Slab>().as_ptr().write(Slab::new()) };
        let offset = self.offset_of(pointer, layout.size()).unwrap();
        transaction.persist(slab_index, 0, meta, GROW);
        mutation.commit();
        if !clean_heap {
            heap.abandon_mutation_for_test(dead);
        }
        if linked {
            tail.next().store(offset, Ordering::Release);
        }
        core::mem::forget(transaction);
    }

    #[cfg(test)]
    pub(crate) fn slab_count_for_test(&self) -> u32 {
        self.tail().unwrap().0 + 1
    }

    #[cfg(test)]
    pub(crate) fn abandon_held_heap_for_test(&self, heap: &crate::talc::MapTalc, dead: u8) {
        let transaction = self.inner.claim(dead).unwrap();
        heap.abandon_mutation_for_test(dead);
        core::mem::forget(transaction);
    }
}

pub(crate) struct Id<L> {
    region: RegionId,
    slab: u32,
    entry: u32,
    generation: usize,
    _layout: PhantomData<fn() -> L>,
}

impl<L> Copy for Id<L> {}

impl<L> Clone for Id<L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<L> core::fmt::Debug for Id<L> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Id")
            .field("region", &self.region)
            .field("slab", &self.slab)
            .field("entry", &self.entry)
            .field("generation", &self.generation)
            .finish()
    }
}

impl<L> PartialEq for Id<L> {
    fn eq(&self, other: &Self) -> bool {
        self.region == other.region
            && self.slab == other.slab
            && self.entry == other.entry
            && self.generation == other.generation
    }
}

impl<L> Eq for Id<L> {}

impl<L> Id<L> {
    pub(crate) const fn from_parts(
        region: RegionId,
        slab: u32,
        entry: u32,
        generation: usize,
    ) -> Self {
        Self {
            region,
            slab,
            entry,
            generation,
            _layout: PhantomData,
        }
    }

    pub(crate) const fn parts(self) -> (RegionId, u32, u32, usize) {
        (self.region, self.slab, self.entry, self.generation)
    }
}

#[repr(C)]
struct Entry {
    state: AtomicUsize,
    offset: AtomicUsize,
}

impl Entry {
    const fn vacant() -> Self {
        Self {
            state: AtomicUsize::new(VACANT),
            offset: AtomicUsize::new(0),
        }
    }
}

#[repr(C)]
struct Slab {
    next: AtomicUsize,
    entries: [Entry; CAPACITY],
}

impl Slab {
    const fn new() -> Self {
        Self {
            next: AtomicUsize::new(0),
            entries: [const { Entry::vacant() }; CAPACITY],
        }
    }
}

#[derive(Clone, Copy)]
enum SlabRef<'a> {
    Root(&'a Directory),
    Linked(&'a Slab),
}

impl<'a> SlabRef<'a> {
    fn next(&self) -> &'a AtomicUsize {
        match self {
            Self::Root(root) => &root.next,
            Self::Linked(slab) => &slab.next,
        }
    }

    fn entries(&self) -> &'a [Entry; CAPACITY] {
        match self {
            Self::Root(root) => &root.entries,
            Self::Linked(slab) => &slab.entries,
        }
    }
}

struct Slot {
    slab: u32,
    entry: usize,
    generation: usize,
    vacant: usize,
}

#[repr(C)]
pub(crate) struct Directory {
    transaction: AtomicUsize,
    pending_slab: AtomicUsize,
    pending_entry: AtomicUsize,
    pending_kind: AtomicUsize,
    pending: UnsafeCell<MaybeUninit<crate::talc::Meta>>,
    next: AtomicUsize,
    entries: [Entry; CAPACITY],
}

// Safety: the single Directory transaction exclusively owns `pending`.
unsafe impl Sync for Directory {}

pub(crate) type Header = header::RcHeader<Directory>;
pub(crate) type MapDirectory = crate::mem::Mapped<Header>;

impl SharedSchema for Directory {
    const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.directory"), 4);
}

unsafe impl Layout for Directory {
    type Config = ();
    type Info = ();

    const MAGIC: header::Magic = 0xD1EC;

    fn info(_: &(), _: LayoutContext) {}

    unsafe fn init(destination: *mut Self, _: ()) -> header::Status {
        unsafe {
            destination.write(Self {
                transaction: AtomicUsize::new(CLEAN),
                pending_slab: AtomicUsize::new(0),
                pending_entry: AtomicUsize::new(0),
                pending_kind: AtomicUsize::new(CREATE),
                pending: UnsafeCell::new(MaybeUninit::uninit()),
                next: AtomicUsize::new(0),
                entries: [const { Entry::vacant() }; CAPACITY],
            });
        }
        header::Status::Initialized
    }

    fn attach(&self, _: &()) -> header::Status {
        header::Status::Initialized
    }
}

impl Directory {
    fn owner_word(owner: u8, phase: usize) -> usize {
        ((owner as usize + 1) << TRANSACTION_BITS) | phase
    }

    fn word_owner(word: usize) -> Option<u8> {
        (word != CLEAN).then(|| ((word >> TRANSACTION_BITS).saturating_sub(1)) as u8)
    }

    fn claim(&self, owner: u8) -> Result<Transaction<'_>, Error> {
        match self.transaction.compare_exchange(
            CLEAN,
            Self::owner_word(owner, HELD),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(Transaction {
                directory: self,
                owner,
                phase: HELD,
            }),
            Err(word) => Err(Error::Busy(
                ((word >> TRANSACTION_BITS).saturating_sub(1)) as u8,
            )),
        }
    }
}

struct Transaction<'a> {
    directory: &'a Directory,
    owner: u8,
    phase: usize,
}

impl Transaction<'_> {
    fn persist(&mut self, slab: u32, entry: usize, meta: crate::talc::Meta, kind: usize) {
        self.directory
            .pending_slab
            .store(slab as usize, Ordering::Relaxed);
        self.directory.pending_entry.store(entry, Ordering::Relaxed);
        self.directory.pending_kind.store(kind, Ordering::Relaxed);
        unsafe { (*self.directory.pending.get()).write(meta) };
        self.directory.transaction.store(
            Directory::owner_word(self.owner, EVIDENCE),
            Ordering::Release,
        );
        self.phase = EVIDENCE;
    }

    fn pending(&self) -> &crate::talc::Meta {
        debug_assert!(matches!(self.phase, EVIDENCE | ROLLING_BACK));
        unsafe { (*self.directory.pending.get()).assume_init_ref() }
    }

    fn clear_pending(&mut self) {
        debug_assert!(matches!(self.phase, EVIDENCE | ROLLING_BACK));
        self.phase = HELD;
        self.directory
            .transaction
            .store(Directory::owner_word(self.owner, HELD), Ordering::Release);
    }

    fn commit(self) -> bool {
        let committed = self
            .directory
            .transaction
            .compare_exchange(
                Directory::owner_word(self.owner, self.phase),
                CLEAN,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        core::mem::forget(self);
        committed
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if self.phase == EVIDENCE {
            let _ = self.directory.transaction.compare_exchange(
                Directory::owner_word(self.owner, EVIDENCE),
                Directory::owner_word(self.owner, ROLLING_BACK),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            self.phase = ROLLING_BACK;
        }
    }
}
